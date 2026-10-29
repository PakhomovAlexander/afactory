//! Campaigns: every review Campaign this machine's Campaign Stores hold (docs/design/tui.md
//! section 5.6), read by the functions behind `af review campaigns --json` and `af review report
//! --json`.
//!
//! The Stores are the default Campaign root, the Store a plain `af review run` keeps for each
//! repository under `review/local/`, and every review Store and Campaign root the Store registry
//! (`stores.toml`) records. The folder lists each Campaign with its newest Round, the state of
//! that Round's reviewer nodes as chips, its verdict, what it spent and when its Store was last
//! written; one Campaign's pane shows its Ledger by reviewer and the `af review` commands that
//! apply to it. Nothing is written. While the pane shows, a thread reads the Stores again: about
//! once a second while a Round is open, the Tasks pane's cadence, and every five seconds while
//! none is, so a Campaign started meanwhile appears without a key.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use review_core::store_writer::StoreProvenance;
use serde_json::Value;

use super::tasks::{array, bounded, fit};
use super::{Effect, Pane, Row};
use crate::storage::registry::{self, StoreKind};
use crate::tui::keymap::Key;
use crate::tui::paint::{Paint, Span, Tone};
use crate::tui::scope::{Scope, ScopeKind};
use crate::tui::tree::Item;

/// How often the pane reads the Stores again while a Round is open.
const LIVE: Duration = Duration::from_secs(1);
/// How often it reads them while no Round is open: a new Campaign appears within this.
const IDLE: Duration = Duration::from_secs(5);
/// The bar's columns, its separator excluded.
const BAR_INNER: usize = crate::tui::BAR_WIDTH - 1;
/// The columns of the main pane beside the bar at 100 columns.
const MAIN: usize = 100 - crate::tui::BAR_WIDTH;
/// The columns a Campaign's label takes in the folder's rows.
const LABEL: usize = 20;
/// The columns a reviewer node's name takes in NODES.
const NODE: usize = 18;
/// The bar id of the Stores other af releases wrote, listed as one entry.
const EARLIER: &str = "!earlier";
/// The bar id prefix of a Campaign Store this release cannot read.
const UNREADABLE: &str = "!unreadable:";

/// Where the pane finds Campaigns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    /// A directory of Campaign Stores, enumerated as `af review campaigns --state-root` does:
    /// the default root, or one `--state-root DIR` registered.
    Root(PathBuf),
    /// `review/local/`: one Store per repository a plain `af review run` reviewed, each read on
    /// its own.
    Local(PathBuf),
    /// One Campaign Store a `--state DIR` registered.
    Store(PathBuf),
}

impl Source {
    fn path(&self) -> &Path {
        match self {
            Source::Root(path) | Source::Local(path) | Source::Store(path) => path,
        }
    }
}

/// The Campaign sources of this machine, and why the Store registry could not add its own,
/// if it could not. A missing state root names no source at all.
pub(crate) fn sources(scope: &Scope) -> (Vec<Source>, Option<String>) {
    let Some(state) = &scope.state else {
        return (Vec::new(), None);
    };
    let review = state.join("af").join("review");
    let mut sources = vec![
        Source::Root(review.join("campaigns")),
        Source::Local(review.join("local")),
    ];
    let mut warning = None;
    match registry::read(&registry::location(state)) {
        Ok(entries) => {
            for entry in entries {
                let source = match entry.kind {
                    StoreKind::Review => Source::Store(entry.path),
                    StoreKind::ReviewRoot => Source::Root(entry.path),
                    StoreKind::Task | StoreKind::Project => continue,
                };
                if !sources.contains(&source) {
                    sources.push(source);
                }
            }
        }
        Err(error) => warning = Some(format!("the Store registry cannot be read: {error}")),
    }
    (sources, warning)
}

/// One Campaign as `af review campaigns` lists it.
pub(crate) struct Campaign {
    /// Its Store.
    pub(crate) state: PathBuf,
    shown: String,
    view: crate::CampaignView,
    /// Its Task accounting as `af review report --json` prints it; the node chips read it.
    accounting: Value,
    /// `--campaign LABEL`, and `--state DIR` unless the default root resolves it by its label.
    selector: Vec<String>,
}

impl Campaign {
    fn label(&self) -> &str {
        &self.view.label
    }

    /// The word on its chip and the chip's tone: an open Round is running work, a closed one
    /// is its verdict, and a Campaign without a Round is new.
    fn state(&self) -> (&'static str, Option<Tone>) {
        if self.view.round_open {
            return ("open", Some(Tone::Active));
        }
        match self.view.verdict.as_deref() {
            Some(verdict) if verdict.starts_with("pass") => ("pass", Some(Tone::Ok)),
            Some(verdict) if verdict.starts_with("incomplete") => ("incomplete", Some(Tone::Fail)),
            Some(_) => ("fail", Some(Tone::Fail)),
            None if self.view.latest_round.is_some() => ("closed", None),
            None => ("new", None),
        }
    }

    fn round(&self) -> String {
        match self.view.latest_round {
            Some((round, epoch)) => format!("round {round} epoch {epoch}"),
            None => "no Round yet".to_owned(),
        }
    }

    /// `tokens tok  wall`, from the Task accounting and the Round walls `af review report`
    /// prints.
    fn spend(&self) -> String {
        let tokens = self
            .view
            .chargeable_tokens()
            .map_or("-".to_owned(), |tokens| tokens.to_string());
        let wall = self
            .view
            .wall_ms
            .map_or("-".to_owned(), crate::human::duration);
        format!("{tokens} tok  wall {wall}")
    }

    fn written(&self) -> String {
        self.view
            .last_activity_unix_ms
            .map_or("-".to_owned(), crate::human::utc_time)
    }

    /// `af review WORDS` with this Campaign's selector, as a `:` line.
    fn line(&self, verb: &str, extra: &[&str]) -> String {
        let mut words: Vec<String> = vec!["review".to_owned(), verb.to_owned()];
        words.extend(self.selector.iter().cloned());
        words.extend(extra.iter().map(|word| (*word).to_owned()));
        shell_words::join(words)
    }
}

/// How one reviewer node of the newest Round stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NodeState {
    pub(crate) node: String,
    /// The outcome of its latest Attempt, as `af review report --json` names it.
    pub(crate) outcome: String,
    pub(crate) tokens: u128,
    pub(crate) wall_ms: Option<u64>,
}

impl NodeState {
    pub(crate) fn tone(&self) -> Option<Tone> {
        match self.outcome.as_str() {
            "running" | "reserved" => Some(Tone::Active),
            "selected" => Some(Tone::Ok),
            "failed" | "abandoned" | "fenced" => Some(Tone::Fail),
            _ => None,
        }
    }
}

/// The reviewer nodes of `round`, the Campaign's newest Round, in the order their first Attempt
/// started, each with the outcome of its latest Attempt and what all of them spent. Only that
/// Round's Attempts count: a Round that started and has none yet has no node, never the
/// previous Round's. The latest Attempt is the one that started last — an Attempt still
/// running, which has no wall yet, after every one that has — never the last in the
/// accounting's order, which is by Attempt ID.
pub(crate) fn nodes(accounting: &Value, round: Option<(u32, u32)>) -> Vec<NodeState> {
    let Some((round, epoch)) = round else {
        return Vec::new();
    };
    let mut attempts: Vec<(usize, &Value)> = Vec::new();
    for task in array(accounting) {
        for attempt in array(&task["attempts"]) {
            let of_round = attempt["round"].as_u64() == Some(u64::from(round))
                && attempt["epoch"].as_u64().unwrap_or(1) == u64::from(epoch);
            if attempt["category"] == "business" && of_round {
                attempts.push((attempts.len(), attempt));
            }
        }
    }
    // When each started: an Attempt without a wall is still running, so it is the newest.
    let started = |attempt: &Value| {
        attempt["wall"]["started_unix_ms"]
            .as_u64()
            .unwrap_or(u64::MAX)
    };
    attempts.sort_by_key(|(at, attempt)| (started(attempt), *at));
    let mut nodes: Vec<NodeState> = Vec::new();
    for (_, attempt) in attempts {
        let name = attempt["review_node"]
            .as_str()
            .or_else(|| attempt["node"].as_str())
            .unwrap_or("-");
        let tokens: u128 = attempt["chargeable_tokens"]
            .as_str()
            .and_then(|tokens| tokens.parse().ok())
            .unwrap_or(0);
        let wall = attempt["wall"]["elapsed_ms"].as_u64();
        let outcome = attempt["outcome"].as_str().unwrap_or("-").to_owned();
        match nodes.iter_mut().find(|node| node.node == name) {
            Some(node) => {
                node.outcome = outcome;
                node.tokens = node.tokens.saturating_add(tokens);
                node.wall_ms = match (node.wall_ms, wall) {
                    (Some(sum), Some(more)) => Some(sum.saturating_add(more)),
                    (sum, more) => sum.or(more),
                };
            }
            None => nodes.push(NodeState {
                node: name.to_owned(),
                outcome,
                tokens,
                wall_ms: wall,
            }),
        }
    }
    nodes
}

/// A Campaign Store this release refused to read.
struct Problem {
    state: PathBuf,
    shown: String,
    reason: String,
    /// Who wrote it, when another release did: such Stores are one summary row.
    release: Option<StoreProvenance>,
}

/// What one source holds.
struct Group {
    /// The source it was read from, which places it among the groups.
    source: Source,
    shown: String,
    campaigns: Vec<Campaign>,
    problems: Vec<Problem>,
    /// The source as a whole cannot be read.
    error: Option<String>,
}

/// Paths as the pane shows them, and the default root a bare `--campaign LABEL` reaches.
#[derive(Clone)]
struct Places {
    home: Option<PathBuf>,
    default_root: Option<PathBuf>,
}

impl Places {
    fn of(scope: &Scope) -> Places {
        let default_root = scope.state.as_ref().and_then(|state| {
            crate::resolve_filesystem_path(&state.join("af/review/campaigns")).ok()
        });
        Places {
            home: scope.home.clone(),
            default_root,
        }
    }

    fn show(&self, path: &Path) -> String {
        let relative = self
            .home
            .as_deref()
            .and_then(|home| path.strip_prefix(home).ok());
        match relative {
            Some(relative) if relative.as_os_str().is_empty() => "~".to_owned(),
            Some(relative) => format!("~/{}", relative.display()),
            None => path.display().to_string(),
        }
    }

    fn campaign(&self, state: PathBuf, view: crate::CampaignView) -> Campaign {
        let accounting = serde_json::to_value(&view.task_accounting).unwrap_or(Value::Null);
        let selector = view.selector(&state, self.default_root.as_deref());
        Campaign {
            shown: self.show(&state),
            state,
            view,
            accounting,
            selector,
        }
    }

    fn problem(&self, state: PathBuf, reason: String) -> Problem {
        let release = crate::store_release::foreign(&state, &reason);
        let reason = match &release {
            Some(provenance) => {
                crate::store_release::refusal(&state, provenance, "af review campaigns")
            }
            None => reason,
        };
        Problem {
            shown: self.show(&state),
            state,
            reason,
            release,
        }
    }
}

/// Read one Campaign Store on its own; `None` when the directory holds no Store. A Store whose
/// log cannot even be looked at (permission denied, an I/O error) is a problem naming its path
/// and the error, never left out as if it were no Campaign.
fn read_dir(places: &Places, state: &Path) -> Option<Result<Campaign, Problem>> {
    let database = state.join("events.sqlite");
    match std::fs::symlink_metadata(&database) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            let reason = format!("{} cannot be read: {error}", database.display());
            return Some(Err(places.problem(state.to_path_buf(), reason)));
        }
    }
    Some(match crate::read_campaign_dir(state) {
        Ok(view) => Ok(places.campaign(state.to_path_buf(), view)),
        Err(reason) => Err(places.problem(state.to_path_buf(), reason)),
    })
}

fn read_source(places: &Places, source: &Source) -> Group {
    let mut group = Group {
        source: source.clone(),
        shown: places.show(source.path()),
        campaigns: Vec::new(),
        problems: Vec::new(),
        error: None,
    };
    match source {
        Source::Root(path) => match crate::enumerate_campaigns(path, false) {
            Ok(enumeration) => {
                let root = crate::resolve_filesystem_path(path).unwrap_or_else(|_| path.clone());
                for view in enumeration.campaigns {
                    let state = root.join(&view.state_dir);
                    group.campaigns.push(places.campaign(state, view));
                }
                for problem in enumeration.problems {
                    let state = root.join(&problem.directory);
                    group.problems.push(Problem {
                        shown: places.show(&state),
                        state,
                        reason: problem.reason,
                        release: problem.release,
                    });
                }
            }
            Err(error) => group.error = Some(error),
        },
        Source::Local(path) => {
            let entries = match std::fs::read_dir(path) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return group,
                Err(error) => {
                    group.error = Some(format!("{}: {error}", path.display()));
                    return group;
                }
            };
            let mut dirs: Vec<PathBuf> = entries
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .map(|entry| entry.path())
                .collect();
            dirs.sort();
            for dir in dirs {
                match read_dir(places, &dir) {
                    Some(Ok(campaign)) => group.campaigns.push(campaign),
                    Some(Err(problem)) => group.problems.push(problem),
                    None => {}
                }
            }
        }
        Source::Store(path) => match read_dir(places, path) {
            Some(Ok(campaign)) => group.campaigns.push(campaign),
            Some(Err(problem)) => group.problems.push(problem),
            None => {}
        },
    }
    group
}

/// What one read of every source found, and the opened Campaign's report.
/// One message of a read: a source's group as soon as it is read, then the end of the read.
enum Read {
    /// One source, read.
    Group(Group),
    /// Every source was read; the opened Campaign's report, read last.
    Done {
        report: Option<(PathBuf, Result<Value, String>)>,
    },
}

/// Read every source, a Store listed twice only the first time, and the opened Campaign's
/// report again, on the calling thread: each group goes through `sender` the moment its
/// source is read, the report last. A receiver gone away ends the read early.
fn read(
    sources: &[Source],
    places: &Places,
    opened: Option<(PathBuf, String)>,
    sender: &mpsc::Sender<Read>,
) {
    let mut seen = BTreeSet::new();
    for source in sources {
        let mut group = read_source(places, source);
        group
            .campaigns
            .retain(|campaign| seen.insert(campaign.state.clone()));
        group
            .problems
            .retain(|problem| seen.insert(problem.state.clone()));
        if sender.send(Read::Group(group)).is_err() {
            return;
        }
    }
    let report = opened.map(|(state, label)| {
        let report = crate::browse_report(&state, &label);
        (state, report)
    });
    let _ = sender.send(Read::Done { report });
}

/// The opened Campaign: its report, and what Enter does on each of its rows.
struct Detail {
    state: PathBuf,
    report: Result<Value, String>,
}

struct Job {
    receiver: Receiver<Read>,
    /// The Campaign the read was started for: its report is dropped when another is opened
    /// meanwhile; the groups are listed either way.
    for_campaign: Option<String>,
    /// The opening the read was started under: a report read under an earlier opening of
    /// the Campaign is dropped, and the one that opening read stands.
    opening: u64,
    /// A load or `R` started it: its groups are wanted whether or not the pane shows, so
    /// closing the pane or opening a Campaign keeps it. A live reread is dropped by both.
    explicit: bool,
}

#[derive(Default)]
pub(crate) struct CampaignsPane {
    title: String,
    /// The scope loaded last, by what makes it another one: another scope lists nothing of
    /// the old; the same scope with other sources keeps its groups until the read answers.
    place: Option<(ScopeKind, PathBuf)>,
    sources: Vec<Source>,
    places: Option<Places>,
    /// Why the registry added no source, shown above the Campaigns.
    warning: Option<String>,
    groups: Vec<Group>,
    /// The Stores the read in flight answered so far, by state path: a Campaign or problem
    /// of a Store no source answered yet is retained from the last read until one does, or
    /// until the read is done.
    answered: BTreeSet<PathBuf>,
    /// The bar id of the opened Campaign.
    selected: Option<String>,
    detail: Option<Detail>,
    /// The main pane shows this pane.
    opened: bool,
    job: Option<Job>,
    /// An explicit read `read_now` recorded, which the next `poll` starts.
    pending: bool,
    /// How many times a Campaign was opened: a read's report is taken only from a read
    /// started under the opening that shows now.
    opening: u64,
    next: Option<Instant>,
    /// The spinner frame the bar shows while a read runs.
    spinner: usize,
    /// Live reads started, for tests.
    #[cfg(test)]
    reads: usize,
    /// Reads started on a thread, for tests: a load records one and starts none.
    #[cfg(test)]
    started: usize,
    rows: Vec<Row>,
    /// The `:` line Enter puts up on a row of the opened Campaign.
    lines: BTreeMap<usize, String>,
}

impl CampaignsPane {
    fn campaigns(&self) -> impl Iterator<Item = &Campaign> {
        self.groups.iter().flat_map(|group| group.campaigns.iter())
    }

    fn campaign(&self, id: &str) -> Option<&Campaign> {
        self.campaigns()
            .find(|campaign| campaign.state.display().to_string() == id)
    }

    /// The label a bar id names, for the breadcrumb and `y`.
    pub(crate) fn label(&self, id: &str) -> Option<String> {
        self.campaign(id)
            .map(|campaign| campaign.label().to_owned())
    }

    /// Live reads started so far.
    #[cfg(test)]
    pub(crate) fn reads(&self) -> usize {
        self.reads
    }

    fn opened_campaign(&self) -> Option<(PathBuf, String)> {
        let campaign = self.campaign(self.selected.as_deref()?)?;
        Some((campaign.state.clone(), campaign.label().to_owned()))
    }

    /// A load or `R`: every source is read again, on a thread, each group listed as it is read.
    /// The read is recorded here and started at the next `poll`, which the event loop reaches
    /// after the frame that follows: no Store is read before that frame, the browser's first
    /// after a load (ADR-0240). A read in flight is dropped.
    fn read_now(&mut self) {
        self.job = None;
        self.pending = true;
    }

    /// Start a read of every source on a thread; a read in flight is dropped, its thread ends
    /// at its next send. Only `poll` starts a read, the explicit one `read_now` recorded or a
    /// live one, so no read begins before the frame painted last (ADR-0240).
    fn start(&mut self, explicit: bool) {
        self.pending = false;
        let Some(places) = self.places.clone() else {
            self.job = None;
            return;
        };
        let (sources, opened) = (self.sources.clone(), self.opened_campaign());
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || read(&sources, &places, opened, &sender));
        #[cfg(test)]
        {
            self.started += 1;
        }
        self.answered.clear();
        self.job = Some(Job {
            receiver,
            for_campaign: self.selected.clone(),
            opening: self.opening,
            explicit,
        });
    }

    /// How many reads were started on a thread: a load records one and starts none.
    #[cfg(test)]
    pub(crate) fn started(&self) -> usize {
        self.started
    }

    /// A read in flight whose messages a test sends by hand, for the Campaign opened now.
    #[cfg(test)]
    fn fed(&mut self) -> mpsc::Sender<Read> {
        let (sender, receiver) = mpsc::channel();
        self.pending = false;
        self.answered.clear();
        self.job = Some(Job {
            receiver,
            for_campaign: self.selected.clone(),
            opening: self.opening,
            explicit: true,
        });
        sender
    }

    /// The groups in the order of their sources; one of no source last.
    fn order(&mut self) {
        let sources = &self.sources;
        let order = |source: &Source| {
            sources
                .iter()
                .position(|s| s == source)
                .unwrap_or(usize::MAX)
        };
        self.groups.sort_by_key(|group| order(&group.source));
    }

    /// Whether the read in flight, if any, was started by a load or `R`, or one is recorded.
    fn explicit_read(&self) -> bool {
        self.pending || self.job.as_ref().is_some_and(|job| job.explicit)
    }

    /// One message of the read in flight: the report is for the Campaign the read was started
    /// for, and dropped when another was opened meanwhile; a group is listed either way.
    fn collect(&mut self, read: Read) {
        let current = self
            .job
            .as_ref()
            .is_some_and(|job| job.for_campaign == self.selected && job.opening == self.opening);
        let read = match read {
            Read::Done { report: Some(_) } if !current => Read::Done { report: None },
            other => other,
        };
        self.apply(read);
    }

    /// Wait for the read in flight, if any, and list everything it finds: a test's event loop.
    #[cfg(test)]
    pub(crate) fn settle(&mut self) {
        if self.pending {
            self.start(true);
        }
        loop {
            let received = match &self.job {
                Some(job) => job.receiver.recv(),
                None => return,
            };
            match received {
                Ok(read) => self.collect(read),
                Err(_) => self.job = None,
            }
        }
    }

    fn apply(&mut self, read: Read) {
        match read {
            // A source's group takes the place of what the last read found for it, among the
            // groups in source order. A Store is listed once, where the source that answered
            // it first in this read put it: a Campaign a root listed may be a registered
            // Store's answer this time, and the other way round, and a healthy Campaign
            // becomes the refusal a source reports for its Store the moment that arrives. A
            // Store no source answered yet stays where the last read listed it, until one
            // does or the read is done: a changed source list never unlists a Campaign before
            // a source answers for its Store, and an opened one stays opened meanwhile.
            Read::Group(group) => {
                let answered: BTreeSet<PathBuf> = group
                    .campaigns
                    .iter()
                    .map(|campaign| campaign.state.clone())
                    .chain(group.problems.iter().map(|problem| problem.state.clone()))
                    .collect();
                for listed in &mut self.groups {
                    listed
                        .campaigns
                        .retain(|campaign| !answered.contains(&campaign.state));
                    listed
                        .problems
                        .retain(|problem| !answered.contains(&problem.state));
                }
                self.answered.extend(answered);
                match self.groups.iter().position(|g| g.source == group.source) {
                    Some(listed) => {
                        // What the last read listed under this source and no source answered
                        // yet stays, behind what this read found.
                        let old = std::mem::replace(&mut self.groups[listed], group);
                        let group = &mut self.groups[listed];
                        group.campaigns.extend(old.campaigns);
                        group.problems.extend(old.problems);
                    }
                    None => self.groups.push(group),
                }
                self.order();
            }
            Read::Done { report } => {
                // Every source answered: a Store none answered goes, and so does a group of a
                // source this read did not name.
                let (sources, answered) = (&self.sources, &self.answered);
                for group in &mut self.groups {
                    group
                        .campaigns
                        .retain(|campaign| answered.contains(&campaign.state));
                    group
                        .problems
                        .retain(|problem| answered.contains(&problem.state));
                }
                self.groups.retain(|group| sources.contains(&group.source));
                self.order();
                if let Some((state, report)) = report {
                    self.detail = Some(Detail { state, report });
                }
                self.job = None;
            }
        }
        // A Campaign no longer listed takes its pane with it: the folder shows instead.
        if let Some(id) = self.selected.clone()
            && self.campaign(&id).is_none()
        {
            self.selected = None;
            self.detail = None;
        }
        self.rebuild();
    }

    /// How often the pane reads the Stores again while it shows: once a second while a Round is
    /// open in a listed Campaign, every five seconds otherwise; `None` while it does not show.
    pub(crate) fn cadence(&self) -> Option<Duration> {
        if !self.opened {
            return None;
        }
        let open = self.campaigns().any(|campaign| campaign.view.round_open);
        Some(if open { LIVE } else { IDLE })
    }

    /// Make the next poll read again, as if its interval had passed.
    #[cfg(test)]
    pub(crate) fn due(&mut self) {
        self.next = Some(Instant::now());
    }

    fn rebuild(&mut self) {
        self.lines.clear();
        let opened = self.selected.as_deref().and_then(|id| self.campaign(id));
        self.rows = match (opened, &self.detail) {
            (Some(campaign), Some(detail)) if detail.state == campaign.state => {
                let (rows, lines) = detail_rows(campaign, &detail.report);
                self.lines = lines;
                rows
            }
            _ => self.folder_rows(),
        };
    }

    fn folder_rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::painted(&self.title, Paint::Title), Row::blank()];
        if let Some(warning) = &self.warning {
            rows.extend(Row::errors("warning", bounded(warning, MAIN - 10, "")));
            rows.push(Row::blank());
        }
        let listed = self.groups.iter().any(|group| {
            !group.campaigns.is_empty() || !group.problems.is_empty() || group.error.is_some()
        });
        if !listed {
            rows.push(Row::plain("No review Campaign is recorded here yet."));
            rows.push(Row::plain(
                "af review run starts one; af help review explains Campaigns.",
            ));
            return rows;
        }
        let earlier: Vec<&Problem> = self
            .groups
            .iter()
            .flat_map(|group| group.problems.iter())
            .filter(|problem| problem.release.is_some())
            .collect();
        if !earlier.is_empty() {
            // One row for all of them, by the releases that wrote them, with the command that
            // lists them: never one error each.
            let summary = crate::store_release::summary(
                earlier
                    .iter()
                    .filter_map(|problem| problem.release.as_ref()),
                "af review campaigns",
            )
            .unwrap_or_default();
            for line in summary.lines() {
                // A line of the summary keeps its indent on each row it wraps to.
                let text = line.trim_start();
                let indent = &line[..line.len() - text.len()];
                for row in bounded(text, MAIN, indent) {
                    rows.push(Row::painted(row, Paint::Muted));
                }
            }
            rows.push(Row::blank());
        }
        for group in &self.groups {
            let problems: Vec<&Problem> = group
                .problems
                .iter()
                .filter(|problem| problem.release.is_none())
                .collect();
            if group.campaigns.is_empty() && problems.is_empty() && group.error.is_none() {
                continue;
            }
            for line in bounded(&group.shown, MAIN, "") {
                rows.push(Row::plain(line));
            }
            if let Some(error) = &group.error {
                let cause = format!("these Campaign Stores cannot be listed: {error}");
                rows.extend(Row::errors("error", bounded(&cause, MAIN - 8, "")));
            }
            for campaign in &group.campaigns {
                rows.extend(campaign_rows(campaign));
            }
            for problem in problems {
                let cause = format!("{}: {}", problem.shown, problem.reason);
                rows.extend(Row::errors("error", bounded(&cause, MAIN - 8, "")));
            }
            rows.push(Row::blank());
        }
        rows
    }
}

/// A Campaign in the folder: its label, newest Round, state and verdict; what it spent and when
/// its Store was last written; its reviewer nodes as chips.
fn campaign_rows(campaign: &Campaign) -> Vec<Row> {
    let label = clip(campaign.label(), LABEL);
    let (word, tone) = campaign.state();
    let round = match campaign.view.latest_round {
        Some((round, epoch)) => format!("round {round}.{epoch}"),
        None => "round -".to_owned(),
    };
    let verdict = campaign.view.verdict.as_deref().unwrap_or("-");
    let lead = format!("  {label:<LABEL$}  {round:<10}");
    let mut rows = vec![Row {
        spans: vec![
            Span::new(lead, Paint::Plain),
            chip(word, tone),
            Span::new(format!(" {verdict}"), Paint::Plain),
        ],
    }];
    rows.push(Row::plain(format!(
        "      spend {}  last write {}",
        campaign.spend(),
        campaign.written()
    )));
    let nodes = nodes(&campaign.accounting, campaign.view.latest_round);
    if !nodes.is_empty() {
        let mut spans = vec![Span::new("      nodes", Paint::Plain)];
        for node in &nodes {
            spans.push(Span::new(" ", Paint::Plain));
            spans.push(chip(&node.node, node.tone()));
        }
        rows.push(Row { spans });
    }
    rows
}

/// A state word on its tone's chip, or plain and spaced like one when it has no tone.
fn chip(word: &str, tone: Option<Tone>) -> Span {
    match tone {
        Some(tone) => Span::chip(word, tone),
        None => Span::new(format!(" {word} "), Paint::Plain),
    }
}

/// `text` in at most `width` columns, a cut marked with `~`.
fn clip(text: &str, width: usize) -> String {
    let text = crate::tui::paint::ascii(text);
    if text.len() <= width {
        return text;
    }
    match width {
        0 => String::new(),
        _ => format!("{}~", &text[..width - 1]),
    }
}

/// One Campaign's pane: its state, spend and Store; the reviewer nodes of its newest Round;
/// its Ledger by reviewer; the `af review` commands that apply. Returns the rows and the `:`
/// line Enter puts up on each row that has one.
fn detail_rows(
    campaign: &Campaign,
    report: &Result<Value, String>,
) -> (Vec<Row>, BTreeMap<usize, String>) {
    let mut lines = BTreeMap::new();
    // The opaque id as other ids print: its kind and eight hex digits.
    let id: String = campaign.view.id.chars().take(10).collect();
    let title = format!("CAMPAIGN  {}  {id}", clip(campaign.label(), MAIN - 26));
    let mut rows = vec![Row::painted(title, Paint::Title)];
    let (word, tone) = campaign.state();
    let verdict = campaign.view.verdict.as_deref().unwrap_or("-");
    rows.push(Row {
        spans: vec![
            Span::new("STATE", Paint::Plain),
            chip(word, tone),
            Span::new(
                format!(" {}  verdict {verdict}", campaign.round()),
                Paint::Plain,
            ),
        ],
    });
    rows.push(Row::plain(format!(
        "SPEND  {}  last write {}",
        campaign.spend(),
        campaign.written()
    )));
    for (index, line) in bounded(&campaign.shown, MAIN - 7, "")
        .into_iter()
        .enumerate()
    {
        let lead = if index == 0 { "STORE  " } else { "       " };
        rows.push(Row::plain(format!("{lead}{line}")));
    }
    rows.push(Row::blank());
    let nodes = nodes(&campaign.accounting, campaign.view.latest_round);
    let heading = match campaign.view.latest_round {
        Some((round, epoch)) => format!("NODES  round {round} epoch {epoch}  (af review report)"),
        None => "NODES  (af review report)".to_owned(),
    };
    rows.push(Row::painted(heading, Paint::Title));
    if nodes.is_empty() {
        let none = if campaign.view.round_open {
            "  waiting: no reviewer Attempt of this Round recorded yet"
        } else {
            "  no reviewer Attempt recorded yet"
        };
        rows.push(Row::painted(none, Paint::Muted));
    }
    for node in &nodes {
        let wall = node.wall_ms.map_or(String::new(), crate::human::duration);
        let name = clip(&node.node, NODE);
        let text = format!(" {name:<NODE$} {:>10} tok  {wall}", node.tokens);
        rows.push(Row {
            spans: vec![
                Span::new(" ", Paint::Plain),
                chip(&node.outcome, node.tone()),
                Span::new(text.trim_end(), Paint::Plain),
            ],
        });
    }
    rows.push(Row::blank());
    match report {
        Ok(report) => ledger_rows(campaign, report, &mut rows, &mut lines),
        Err(error) => {
            rows.push(Row::painted("LEDGER  (af review ledger)", Paint::Title));
            let cause = format!("the report cannot be read: {error}");
            rows.extend(Row::errors("error", bounded(&cause, MAIN - 8, "")));
        }
    }
    rows.push(Row::blank());
    rows.push(Row::painted(
        "COMMANDS  Enter puts one on the : line",
        Paint::Title,
    ));
    let mut commands = vec![
        campaign.line("report", &[]),
        campaign.line("ledger", &["--long"]),
    ];
    // `--restart-round` supersedes an open Round; without one it is refused.
    if campaign.view.round_open {
        commands.push(campaign.line("run", &["--restart-round"]));
    }
    for command in commands {
        lines.insert(rows.len(), command.clone());
        rows.push(Row::plain(format!("  :{command}")));
    }
    let show = format!("{} ", campaign.line("show", &[]));
    lines.insert(rows.len(), show.clone());
    rows.push(Row::plain(format!(
        "  :{show}KEY   (Enter on a finding fills KEY)"
    )));
    (rows, lines)
}

/// LEDGER: the report's verdict and findings summary, then its findings grouped by the reviewer
/// that reported each, with the `af review show` line Enter puts up for each.
fn ledger_rows(
    campaign: &Campaign,
    report: &Value,
    rows: &mut Vec<Row>,
    lines: &mut BTreeMap<usize, String>,
) {
    let round = report["ledger_round"].as_u64().unwrap_or(0);
    rows.push(Row::painted(
        format!("LEDGER  round {round}  (af review ledger)"),
        Paint::Title,
    ));
    let verdict = report["final_verdict"].as_str().unwrap_or("not recorded");
    rows.push(Row::plain(format!("  verdict {verdict}")));
    let summary = &report["findings_summary"];
    let count = |key: &str| summary[key].as_u64().unwrap_or(0);
    let findings = array(&report["findings"]);
    rows.push(Row::plain(format!(
        "  {} findings: {} open, {} pending, {} fixed, {} rejected, {} wontfix, {} contested",
        findings.len(),
        count("open"),
        count("pending_verification"),
        count("fixed"),
        count("rejected"),
        count("wontfix"),
        count("contested"),
    )));
    let mut by_reviewer: BTreeMap<&str, Vec<&Value>> = BTreeMap::new();
    for finding in findings {
        let source = finding["source"].as_str().unwrap_or("-");
        by_reviewer.entry(source).or_default().push(finding);
    }
    for (reviewer, findings) in by_reviewer {
        let noun = if findings.len() == 1 {
            "finding"
        } else {
            "findings"
        };
        rows.push(Row::plain(format!(
            "  {}  {} {noun}",
            clip(reviewer, 40),
            findings.len()
        )));
        for finding in findings {
            let key = finding["key"].as_str().unwrap_or("-");
            let line = finding["line"]
                .as_i64()
                .map_or(String::new(), |line| format!(":{line}"));
            let place = format!("{}{line}", finding["file"].as_str().unwrap_or("-"));
            let text = format!(
                "    {}  {:<7} {:<10} {}  {}",
                super::tasks::short(Some(key)),
                finding["severity"].as_str().unwrap_or("-"),
                finding["status"].as_str().unwrap_or("-"),
                clip(&place, 24),
                finding["title"].as_str().unwrap_or(""),
            );
            lines.insert(rows.len(), campaign.line("show", &[key]));
            rows.push(Row::plain(text));
        }
    }
}

impl Pane for CampaignsPane {
    fn load(&mut self, scope: &Scope) -> Result<(), String> {
        let (sources, warning) = sources(scope);
        let place = (scope.kind, scope.root.clone());
        if self.place.as_ref() != Some(&place) {
            // Another scope: nothing read for the old one stays listed or opened. The same
            // scope with other sources (a Store registered meanwhile) keeps every group
            // listed until the read answers for it, so an opened Campaign is never reported
            // gone because its source is not read yet.
            self.groups.clear();
            self.selected = None;
            self.detail = None;
        }
        self.place = Some(place);
        self.sources = sources;
        // The groups retained from the last read follow the sources as they stand now,
        // before the read answers for any of them.
        self.order();
        self.warning = warning;
        self.places = Some(Places::of(scope));
        self.title = format!("CAMPAIGNS  {}: {}", scope.word(), scope.name());
        self.read_now();
        Ok(())
    }

    fn items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        // The indent and the fold glyph, and a last column for the chip's closing space.
        let width = BAR_INNER.saturating_sub(2 * 2 + 2 + 1);
        for group in &self.groups {
            for campaign in &group.campaigns {
                let (word, tone) = campaign.state();
                items.push(Item {
                    id: campaign.state.display().to_string(),
                    label: fit(campaign.label(), "", word, width),
                    muted: false,
                    tone,
                    children: None,
                });
            }
            for problem in group.problems.iter().filter(|p| p.release.is_none()) {
                let name = problem
                    .state
                    .file_name()
                    .map_or(problem.shown.clone(), |name| {
                        name.to_string_lossy().into_owned()
                    });
                items.push(Item {
                    id: format!("{UNREADABLE}{}", problem.state.display()),
                    label: clip(&format!("! {name} unreadable"), width + 1),
                    muted: true,
                    tone: None,
                    children: None,
                });
            }
        }
        let count = self
            .groups
            .iter()
            .flat_map(|group| group.problems.iter())
            .filter(|problem| problem.release.is_some())
            .count();
        if count > 0 {
            let stores = if count == 1 { "Store" } else { "Stores" };
            items.push(Item {
                id: EARLIER.to_owned(),
                label: format!("! {count} old {stores}"),
                muted: true,
                tone: None,
                children: None,
            });
        }
        items
    }

    fn open(&mut self, item: Option<&str>) {
        if item != self.selected.as_deref() && !self.explicit_read() {
            // A slow live read of the previously opened Campaign must not delay the new one's;
            // a load's read lists every source and goes on.
            self.job = None;
        }
        // Every opening counts: a read's report from an earlier one never stands over this
        // one's.
        self.opening = self.opening.wrapping_add(1);
        self.opened = true;
        self.detail = None;
        self.selected = item
            .filter(|id| self.campaign(id).is_some())
            .map(str::to_owned);
        if let Some((state, label)) = self.opened_campaign() {
            let report = crate::browse_report(&state, &label);
            self.detail = Some(Detail { state, report });
        }
        self.next = self.cadence().map(|cadence| Instant::now() + cadence);
        self.rebuild();
    }

    fn close(&mut self) {
        self.opened = false;
        if !self.explicit_read() {
            self.job = None;
        }
    }

    fn rows(&self) -> &[Row] {
        &self.rows
    }

    fn key(&mut self, key: Key, row: usize) -> Result<Option<Effect>, String> {
        match key {
            Key::Enter if self.selected.is_some() => match self.lines.get(&row) {
                // Never submitted: the user reads the line and presses Enter, or Esc.
                Some(line) => Ok(Some(Effect::Prefill(line.clone()))),
                None => Err(
                    "Enter puts the command of a COMMANDS or finding row on the : line".to_owned(),
                ),
            },
            _ => Ok(None),
        }
    }

    fn rename(&mut self, scope: &Scope) {
        self.title = format!("CAMPAIGNS  {}: {}", scope.word(), scope.name());
        self.rebuild();
    }

    fn legend(&self) -> &'static str {
        if self.selected.is_some() {
            "Enter command to : line  y yank name  R re-read  Tab bar"
        } else {
            "j/k move  R re-read  Tab bar  :cmd  q quit"
        }
    }

    fn status(&self, row: usize) -> Option<String> {
        self.lines
            .get(&row)
            .map(|line| format!("Enter puts :{} on the : line", line.trim_end()))
    }

    fn poll(&mut self) -> bool {
        // The read a load or `R` recorded starts here, after the frame painted last.
        if self.pending {
            self.start(true);
        }
        // Every group the read in flight has delivered, then the end of the read.
        let mut changed = false;
        while let Some(job) = &self.job {
            match job.receiver.try_recv() {
                Ok(read) => {
                    self.collect(read);
                    changed = true;
                }
                Err(TryRecvError::Empty) => {
                    self.spinner = self.spinner.wrapping_add(1);
                    return changed;
                }
                Err(TryRecvError::Disconnected) => {
                    self.job = None;
                    return changed;
                }
            }
        }
        if changed {
            return true;
        }
        let Some(cadence) = self.cadence() else {
            return false;
        };
        let now = Instant::now();
        if self.next.is_some_and(|next| now < next) {
            return false;
        }
        self.next = Some(now + cadence);
        #[cfg(test)]
        {
            self.reads += 1;
        }
        self.start(false);
        false
    }

    fn busy(&self) -> Option<char> {
        let spinner = super::SPINNER[self.spinner % super::SPINNER.len()];
        (self.pending || self.job.is_some()).then_some(spinner)
    }

    fn yank(&self, _row: usize) -> Option<String> {
        let id = self.selected.as_deref()?;
        self.label(id)
    }
}

#[cfg(test)]
mod tests;
