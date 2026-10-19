//! Every git process `af project` and registration start (ADR-0151, ADR-0026).
//!
//! Each runs through `review_process` in its own process group, with a deadline that ends the
//! whole group, and in delivery's isolated command (`crate::task::isolated_git`): a cleared
//! environment, a private empty `HOME`, no system or global configuration, no hooks, no
//! transport, no prompt. Capture reads the same way through `review_source_git::Repo`, under its
//! own deadline. No process of these modules is started any other way.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use review_process::{ExitPolicy, SupervisedError, SupervisedOutput, run_supervised_with_policy};
use review_source_git::Repo;

/// The deadline of one plumbing command: delivery's.
const PLUMBING_DEADLINE: Duration = Duration::from_secs(30);
/// The deadline of one command of a committed capture, which reads every object of a tree.
const CAPTURE_DEADLINE: Duration =
    Duration::from_secs(review_source_git::DEFAULT_GIT_TIMEOUT_SECONDS);
/// A debug build's fixture may lower both deadlines, so a test whose subject is the deadline
/// does not wait for the production one. Release binaries never read it, and git never sees it.
#[cfg(debug_assertions)]
const TEST_DEADLINE_MS: &str = "AF_TEST_PROJECT_GIT_DEADLINE_MS";

/// `production`, or the lower deadline a debug build's fixture set.
fn deadline(production: Duration) -> Duration {
    #[cfg(debug_assertions)]
    {
        lowered(production, std::env::var(TEST_DEADLINE_MS).ok().as_deref())
    }
    #[cfg(not(debug_assertions))]
    {
        production
    }
}

/// The setting can only lower: a value of 0, above `production`, or not a number changes nothing.
#[cfg(debug_assertions)]
pub(super) fn lowered(production: Duration, setting: Option<&str>) -> Duration {
    setting
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value >= 1)
        .map_or(production, |value| {
            production.min(Duration::from_millis(value))
        })
}

/// git in one directory, with a private home that lives as long as any of its clones.
#[derive(Clone)]
pub(crate) struct Git {
    directory: PathBuf,
    home: Arc<tempfile::TempDir>,
}

impl Git {
    pub(crate) fn open(directory: &Path) -> Result<Self, String> {
        let home = tempfile::Builder::new()
            .prefix("af-project-git-home-")
            .tempdir()
            .map_err(|error| format!("creating git's private home: {error}"))?;
        Ok(Self {
            directory: directory.to_path_buf(),
            home: Arc::new(home),
        })
    }

    /// The same git, in another directory.
    pub(crate) fn at(&self, directory: &Path) -> Self {
        Self {
            directory: directory.to_path_buf(),
            home: Arc::clone(&self.home),
        }
    }

    /// Run git; its exit status is the caller's to judge. Only a process that could not be
    /// started, outlived its deadline or held its pipes is an error, which names what happened.
    pub(crate) fn run<I, S>(
        &self,
        args: I,
        input: Option<Vec<u8>>,
    ) -> Result<SupervisedOutput, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_within(args, input, deadline(PLUMBING_DEADLINE))
            .map_err(|stopped| stopped.message)
    }

    /// [`Git::run`] under `limit` instead of the plumbing deadline: the Surveyor gives each
    /// command what remains of its one `max_wall_ms` (ADR-0152). A git stopped at `limit` is
    /// told apart from every other failure.
    pub(crate) fn run_within<I, S>(
        &self,
        args: I,
        input: Option<Vec<u8>>,
        limit: Duration,
    ) -> Result<SupervisedOutput, Stopped>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<OsString> = args
            .into_iter()
            .map(|argument| argument.as_ref().to_os_string())
            .collect();
        let mut command = crate::task::isolated_git(&self.directory, self.home.path());
        command.args(&args);
        run_supervised_with_policy(&mut command, input, limit, ExitPolicy::KillProcessGroup)
            .map_err(|error| match error {
                SupervisedError::TimedOut { .. } => Stopped {
                    deadline: true,
                    message: format!(
                        "git {} did not finish within its deadline of {limit:?}; it was stopped \
                         with its process group, so nothing of it still runs; run the command \
                         again once git answers",
                        rendered(&args)
                    ),
                },
                error => Stopped {
                    deadline: false,
                    message: format!("git {}: {error}", rendered(&args)),
                },
            })
    }

    /// Run git and require success: its trimmed stdout, or its stderr as the error.
    pub(crate) fn require<I, S>(&self, args: I, input: Option<Vec<u8>>) -> Result<String, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<OsString> = args
            .into_iter()
            .map(|argument| argument.as_ref().to_os_string())
            .collect();
        let output = self.run(&args, input)?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(format!(
                "git {}: {}",
                rendered(&args),
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    /// The sorted, distinct root commits of `commit` joined by `,`: what `Repo::repository_id`
    /// computes for `HEAD`, for any commit.
    pub(crate) fn roots(&self, commit: &str) -> Result<String, String> {
        let listed = self.require(
            [
                "rev-list",
                "--max-parents=0",
                "--end-of-options",
                commit,
                "--",
            ],
            None,
        )?;
        let mut roots: Vec<&str> = listed
            .lines()
            .map(str::trim)
            .filter(|root| !root.is_empty())
            .collect();
        roots.sort_unstable();
        roots.dedup();
        if roots.is_empty() {
            return Err(format!("git rev-list found no root commit of {commit}"));
        }
        Ok(roots.join(","))
    }

    /// The repository's common directory, made absolute.
    pub(crate) fn common_dir(&self) -> Result<PathBuf, String> {
        let common = self.require(["rev-parse", "--git-common-dir"], None)?;
        Ok(self.directory.join(common))
    }

    /// The read-only capture boundary of `review_source_git`, under the capture deadline.
    pub(crate) fn repo(&self) -> Repo {
        self.repo_within(deadline(CAPTURE_DEADLINE))
    }

    /// The capture boundary under `limit`, for a capture inside the Surveyor's deadline.
    pub(crate) fn repo_within(&self, limit: Duration) -> Repo {
        Repo::open(&self.directory, self.home.path()).with_timeout(limit)
    }
}

/// Why a git started under [`Git::run_within`] gave no output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stopped {
    /// It reached its deadline and was stopped with its process group.
    pub(crate) deadline: bool,
    pub(crate) message: String,
}

fn rendered(args: &[OsString]) -> String {
    args.iter()
        .map(|argument| argument.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}
