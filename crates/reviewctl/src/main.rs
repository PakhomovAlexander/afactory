//! `reviewctl` — reviews from a definition file to a verdict, and the campaign loop.
//!
//! Three subcommands:
//!
//! - `run` captures the repository HEAD as an immutable snapshot, loads the pipeline through
//!   its lockfile, binds each packaged reviewer to the adapter its runner names, executes
//!   under the definition's budgets, and prints what happened — every node, every finding,
//!   the spend, the verdict. With `--campaign NAME` the run joins a persistent ledger: each
//!   run is a new round, and every reviewer receives the campaign's prior findings as a
//!   labelled data artifact.
//! - `ledger` prints a campaign's findings, one per line, machine-readably.
//! - `resolve` records the operator's disposition of one finding (fixed, wontfix, ...) in the
//!   campaign's ledger — the step between fixing and the round that verifies the fix.
//!
//! Nothing here mutates any repository. A run reads a repo and writes its own state
//! directory; `resolve` writes only that state; publishing results anywhere is a human's
//! explicit action.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use review_config::Definition;
use review_config::lock::{Lockfile, Registry};
use review_graph::{NodeOutcome, Scheduler};
use review_pipeline::{Kernel, RunVerdict, run_verdict};
use review_runner::ReviewerAdapter;
use review_source_git::{Capture, Repo};
use review_store::{Cas, EventStore, Ingest, Ledger, NewEvent, Status};

struct Options {
    repo: PathBuf,
    pipeline: PathBuf,
    state: Option<PathBuf>,
    campaign: Option<String>,
    focus: Option<String>,
    timeout: Duration,
}

impl Options {
    /// Where this run's state lives: explicit `--state` wins; a campaign gets its own
    /// directory; anything else shares `local`.
    fn state_dir(&self) -> PathBuf {
        match (&self.state, &self.campaign) {
            (Some(state), _) => state.clone(),
            (None, Some(campaign)) => PathBuf::from(format!(".review/runs/{campaign}")),
            (None, None) => PathBuf::from(".review/runs/local"),
        }
    }
}

struct LedgerOptions {
    state: Option<PathBuf>,
    campaign: String,
}

struct ResolveOptions {
    state: Option<PathBuf>,
    campaign: String,
    key: String,
    status: String,
    note: Option<String>,
}

fn usage() -> ! {
    eprintln!(
        "usage: reviewctl run     [--repo DIR] [--pipeline FILE] [--state DIR] \
         [--campaign NAME] [--focus TEXT] [--timeout-secs N]\n\
        \x20      reviewctl ledger  --campaign NAME [--state DIR]\n\
        \x20      reviewctl resolve --campaign NAME [--state DIR] KEY STATUS [--note TEXT]\n\
         \n\
         STATUS is one of: open fixed rejected wontfix contested"
    );
    std::process::exit(2);
}

fn campaign_run_id(campaign: &str) -> String {
    format!("campaign-{campaign}")
}

fn campaign_state(state: &Option<PathBuf>, campaign: &str) -> PathBuf {
    state
        .clone()
        .unwrap_or_else(|| PathBuf::from(format!(".review/runs/{campaign}")))
}

fn parse_run(mut args: std::env::Args) -> Options {
    let mut options = Options {
        repo: PathBuf::from("."),
        pipeline: PathBuf::from(".review/pipelines/heavy.toml"),
        state: None,
        campaign: None,
        focus: None,
        timeout: Duration::from_secs(1800),
    };
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--repo" => options.repo = PathBuf::from(value()),
            "--pipeline" => options.pipeline = PathBuf::from(value()),
            "--state" => options.state = Some(PathBuf::from(value())),
            "--campaign" => options.campaign = Some(value()),
            "--focus" => options.focus = Some(value()),
            "--timeout-secs" => {
                options.timeout = Duration::from_secs(value().parse().unwrap_or_else(|_| usage()))
            }
            _ => usage(),
        }
    }
    options
}

fn parse_ledger(mut args: std::env::Args) -> LedgerOptions {
    let mut state = None;
    let mut campaign = None;
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            _ => usage(),
        }
    }
    LedgerOptions {
        state,
        campaign: campaign.unwrap_or_else(|| usage()),
    }
}

fn parse_resolve(mut args: std::env::Args) -> ResolveOptions {
    let mut state = None;
    let mut campaign = None;
    let mut note = None;
    let mut positional: Vec<String> = Vec::new();
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            "--note" => note = Some(value()),
            other if !other.starts_with("--") => positional.push(other.to_string()),
            _ => usage(),
        }
    }
    let (Some(campaign), [key, status]) = (campaign, positional.as_slice()) else {
        usage()
    };
    ResolveOptions {
        state,
        campaign,
        key: key.clone(),
        status: status.clone(),
        note,
    }
}

fn main() {
    let mut args = std::env::args();
    args.next();
    let result = match args.next().as_deref() {
        Some("run") => run(&parse_run(args)),
        Some("ledger") => print_ledger(&parse_ledger(args)),
        Some("resolve") => resolve(&parse_resolve(args)),
        _ => usage(),
    };
    if let Err(error) = result {
        eprintln!("reviewctl: {error}");
        std::process::exit(1);
    }
}

fn open_campaign_store(state: &Path) -> Result<EventStore, String> {
    if !state.join("events.sqlite").exists() {
        return Err(format!(
            "no campaign state at {}; a campaign starts with `reviewctl run --campaign`",
            state.display()
        ));
    }
    EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())
}

fn print_ledger(options: &LedgerOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign);
    let store = open_campaign_store(&state)?;
    let ledger =
        Ledger::rebuild(&store, &campaign_run_id(&options.campaign)).map_err(|e| e.to_string())?;
    for finding in ledger.findings() {
        println!(
            "{}\t{}\t{}\t{}:{}\t{}",
            finding.key,
            format!("{:?}", finding.severity).to_lowercase(),
            finding.status.as_str(),
            finding.file,
            finding.line.map_or("-".to_string(), |l| l.to_string()),
            finding.title
        );
    }
    eprintln!(
        "round {}; {} findings, {} open",
        ledger.round,
        ledger.len(),
        ledger
            .findings()
            .iter()
            .filter(|f| f.status == Status::Open)
            .count()
    );
    Ok(())
}

fn resolve(options: &ResolveOptions) -> Result<(), String> {
    let status = Status::parse(&options.status)
        .ok_or_else(|| format!("unknown status `{}`", options.status))?;
    let state = campaign_state(&options.state, &options.campaign);
    let mut store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let run_id = campaign_run_id(&options.campaign);
    let mut ingest = Ingest::new(&mut store, &cas, run_id).map_err(|e| e.to_string())?;
    if ingest.ledger().get(&options.key).is_none() {
        return Err(format!("no finding with key {}", options.key));
    }
    ingest
        .resolve(&options.key, status, options.note.as_deref())
        .map_err(|e| e.to_string())?;
    let now = ingest
        .ledger()
        .get(&options.key)
        .map(|f| f.status.as_str())
        .unwrap_or("?");
    println!("resolved {} -> {}", options.key, now);
    Ok(())
}

fn run(options: &Options) -> Result<(), String> {
    // Load the pipeline through its lockfile — the same trust path a test run takes.
    let review_dir = options
        .pipeline
        .parent()
        .and_then(|p| p.parent())
        .ok_or("the pipeline file must live under .review/pipelines/")?
        .to_path_buf();
    let text = std::fs::read_to_string(&options.pipeline).map_err(|e| e.to_string())?;
    let lock_text =
        std::fs::read_to_string(review_dir.join("review.lock")).map_err(|e| e.to_string())?;
    let lockfile = Lockfile::from_toml(&lock_text).map_err(|e| e.to_string())?;
    let registry = Registry::new([review_dir.join("reviewers")]);
    let loaded = Definition::from_toml(&text)
        .map_err(|e| e.to_string())?
        .load_with(&lockfile, &registry)
        .map_err(|e| e.to_string())?;

    // Capture HEAD. Committed content only: a run reviews an immutable snapshot, and if the
    // change you want reviewed is not committed, that is the message rather than a workaround.
    let state = options.state_dir();
    std::fs::create_dir_all(&state).map_err(|e| e.to_string())?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let mut store = EventStore::open(state.join("events.sqlite")).map_err(|e| e.to_string())?;
    let home = std::env::var("HOME").map_err(|e| e.to_string())?;
    let repo = Repo::open(&options.repo, &home);
    let snapshot = Capture::new(&repo, &cas)
        .committed("HEAD")
        .map_err(|e| format!("capturing HEAD: {e}"))?;
    // A campaign is one logical review across rounds, so it is one run id and one ledger; a
    // plain run is identified by what it reviewed.
    let run_id = match &options.campaign {
        Some(campaign) => campaign_run_id(campaign),
        None => format!("run-{}", &snapshot.content_digest[7..19]),
    };
    println!("snapshot {}", snapshot.content_digest);
    println!("run      {run_id}");

    // Campaign continuation: a repeat run is a new round, and the ledger as it stands travels
    // to every reviewer as a labelled artifact — round N+1 re-examines round N's claims
    // instead of taking a fresh look that happens to share a repository.
    let mut prior_artifact: Option<String> = None;
    if options.campaign.is_some() {
        // Advance only past rounds that actually *closed* — a run that reached a real verdict,
        // not a crash, a failed reviewer, or an exit-4 incomplete run. Counting closed rounds
        // (rather than "are there any events?") means an infra failure re-runs the same round
        // instead of consuming `max_rounds`, and it is correct even after an advance-then-crash:
        // the target round is a function of closed rounds, so a bumped-but-unclosed round is
        // simply re-entered.
        let closed_rounds = store
            .replay(&run_id)
            .map_err(|e| e.to_string())?
            .iter()
            .filter(|e| {
                e.event_type == "RunReport@1"
                    && !e
                        .payload
                        .get("verdict")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .starts_with("Incomplete")
            })
            .count() as u32;
        let target_round = closed_rounds + 1;
        {
            let mut ingest =
                Ingest::new(&mut store, &cas, run_id.clone()).map_err(|e| e.to_string())?;
            while ingest.ledger().round < target_round {
                ingest.advance().map_err(|e| e.to_string())?;
            }
            println!("round    {}", ingest.ledger().round);
        }

        let ledger = Ledger::rebuild(&store, &run_id).map_err(|e| e.to_string())?;
        // Only claims a reviewer can still act on: declined findings (rejected / wontfix) are
        // the operator's terminal decision, and the ledger never reopens them — handing them
        // back under the re-examination contract only invites re-litigation that cannot change
        // status and spends review budget. Open, fixed, and contested claims all can move.
        let rows: Vec<serde_json::Value> = ledger
            .findings()
            .iter()
            .filter(|f| !matches!(f.status, Status::Rejected | Status::Wontfix))
            .map(|f| {
                serde_json::json!({
                    "key": f.key,
                    "severity": format!("{:?}", f.severity).to_lowercase(),
                    "status": f.status.as_str(),
                    "file": f.file,
                    "line": f.line,
                    "title": f.title,
                    "body": f.body,
                    "source": f.source,
                    "last_seen_round": f.last_seen_round,
                })
            })
            .collect();
        if !rows.is_empty() {
            println!("prior    {} findings carried", rows.len());
            prior_artifact = Some(
                cas.put_json(&serde_json::json!({
                    "round": ledger.round,
                    "prior_findings": rows,
                }))
                .map_err(|e| e.to_string())?,
            );
        }
    }

    // The capture is the run's first event: a log that cannot say what was reviewed cannot
    // rebuild the run that reviewed it.
    let manifest_artifact = cas
        .put_json(&serde_json::to_value(&snapshot.manifest).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let tree_id = snapshot
        .source_revision
        .as_deref()
        .and_then(|rev| repo.line(&["rev-parse", &format!("{rev}^{{tree}}")]).ok())
        .unwrap_or_default();
    store
        .append(
            &run_id,
            &cas,
            NewEvent::new(
                "SourceCaptured@1",
                snapshot.to_payload(&tree_id, Some(&manifest_artifact)),
            )
            .referencing(vec![manifest_artifact]),
        )
        .map_err(|e| e.to_string())?;

    // Bind reviewers: a packaged reviewer gets the adapter its runner names; an inline
    // command runs as itself.
    let auth = (
        std::env::var("CLAUDE_CONFIG_DIR").unwrap_or_else(|_| format!("{home}/.claude")),
        std::env::var("USER").map_err(|e| format!("USER: {e}"))?,
        home.clone(),
    );
    let mut kernel = Kernel::new(&cas, &mut store, &run_id, snapshot.manifest.clone())
        .with_checks(loaded.checks);
    if let Some(artifact) = prior_artifact {
        kernel = kernel.with_prior_findings(artifact);
    }
    if let Some(budgets) = loaded.budgets {
        println!(
            "budgets  {} per attempt, {} per run (tokens)",
            budgets.attempt, budgets.run
        );
        kernel = kernel.with_budgets(budgets.attempt, budgets.run);
    }
    let mut bound: BTreeMap<String, String> = BTreeMap::new();
    for (node, command) in &loaded.reviewers {
        let adapter: Box<dyn ReviewerAdapter> = match loaded.packages.get(node) {
            Some(package) => {
                let program = std::path::Path::new(&command.program)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                match program.as_str() {
                    "claude" => {
                        let mut adapter = review_runner_claude::ClaudeAdapter::from_package(
                            package,
                            options.timeout,
                        )
                        .map_err(|e| format!("{node}: {e}"))?
                        .with_auth(
                            auth.0.clone(),
                            auth.1.clone(),
                            auth.2.clone(),
                        );
                        if let Some(focus) = &options.focus {
                            adapter = adapter.with_focus(focus);
                        }
                        Box::new(adapter)
                    }
                    "codex" => {
                        let mut adapter = review_runner_codex::CodexAdapter::from_package(
                            package,
                            options.timeout,
                        )
                        .map_err(|e| format!("{node}: {e}"))?
                        .with_codex_home(format!("{home}/.codex"));
                        if let Some(focus) = &options.focus {
                            adapter = adapter.with_focus(focus);
                        }
                        Box::new(adapter)
                    }
                    other => {
                        return Err(format!(
                            "node `{node}`: no adapter drives `{other}`; this reviewctl knows \
                             claude and codex"
                        ));
                    }
                }
            }
            None => Box::new(command.clone()),
        };
        bound.insert(node.clone(), command.program.clone());
        kernel = kernel.with_adapter(node.clone(), adapter);
    }
    for (node, program) in &bound {
        println!("reviewer {node} -> {program}");
    }

    // Run, and say what happened to every node — a suppressed node in silence would read as
    // "nothing to report".
    let report = Scheduler::new(&loaded.plan).run(&kernel);
    println!();
    for (node, outcome) in &report.outcomes {
        match outcome {
            NodeOutcome::Completed { .. } => println!("  done      {node}"),
            NodeOutcome::Failed { error } => println!("  FAILED    {node}: {error}"),
            NodeOutcome::Suppressed { reason } => {
                println!("  never-ran {node}: {reason:?}")
            }
        }
    }

    let ledger = kernel.ledger();
    println!();
    println!("findings {}", ledger.len());
    for finding in ledger.findings() {
        println!(
            "  [{:?}] {}:{} — {} ({:?})",
            finding.severity,
            finding.file,
            finding.line.map_or("?".to_string(), |l| l.to_string()),
            finding.title,
            finding.status
        );
    }
    if let Some(spent) = kernel.spent() {
        println!("spent    {spent} tokens");
    }

    let convergence = kernel.convergence(loaded.convergence);
    let verdict = run_verdict(&report, &convergence);
    kernel.publish_report(&report, &verdict)?;
    println!("verdict  {verdict:?}");
    match verdict {
        RunVerdict::Pass => Ok(()),
        RunVerdict::Fail(_) => {
            std::process::exit(3);
        }
        RunVerdict::Incomplete { .. } => {
            std::process::exit(4);
        }
    }
}
