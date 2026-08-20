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
use review_core::{
    EventType, RunFailureReasonV2, RunReportPayloadV2, RunVerdictV2, run_report_closes_round,
};
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
    long: bool,
}

struct ShowOptions {
    state: Option<PathBuf>,
    campaign: String,
    key: String,
}

struct ReportOptions {
    state: Option<PathBuf>,
    campaign: String,
    format: String,
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
        \x20      reviewctl ledger  --campaign NAME [--state DIR] [--long]\n\
        \x20      reviewctl show    --campaign NAME [--state DIR] KEY\n\
        \x20      reviewctl report  --campaign NAME [--state DIR] [--format md]\n\
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
    let mut long = false;
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            "--long" => long = true,
            _ => usage(),
        }
    }
    LedgerOptions {
        state,
        campaign: campaign.unwrap_or_else(|| usage()),
        long,
    }
}

fn parse_show(mut args: std::env::Args) -> ShowOptions {
    let mut state = None;
    let mut campaign = None;
    let mut key = None;
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            other if !other.starts_with("--") && key.is_none() => key = Some(other.to_string()),
            _ => usage(),
        }
    }
    ShowOptions {
        state,
        campaign: campaign.unwrap_or_else(|| usage()),
        key: key.unwrap_or_else(|| usage()),
    }
}

fn parse_report(mut args: std::env::Args) -> ReportOptions {
    let mut state = None;
    let mut campaign = None;
    let mut format = "md".to_string();
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--state" => state = Some(PathBuf::from(value())),
            "--campaign" => campaign = Some(value()),
            "--format" => format = value(),
            _ => usage(),
        }
    }
    if format != "md" {
        usage();
    }
    ReportOptions {
        state,
        campaign: campaign.unwrap_or_else(|| usage()),
        format,
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
        Some("show") => show(&parse_show(args)),
        Some("report") => print_report(&parse_report(args)),
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
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let ledger = Ledger::rebuild(&store, &cas, &campaign_run_id(&options.campaign))
        .map_err(|e| e.to_string())?;
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
        if options.long {
            print_indented("body", &finding.body);
            print_indented(
                "fix",
                finding
                    .fix
                    .as_deref()
                    .unwrap_or("(unavailable: artifact-less legacy import)"),
            );
        }
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

fn print_indented(label: &str, value: &str) {
    let mut lines = value.lines();
    println!("  {label}: {}", lines.next().unwrap_or_default());
    for line in lines {
        println!("    {line}");
    }
}

fn show(options: &ShowOptions) -> Result<(), String> {
    let state = campaign_state(&options.state, &options.campaign);
    let store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let ledger = Ledger::rebuild(&store, &cas, &campaign_run_id(&options.campaign))
        .map_err(|e| e.to_string())?;
    let finding = ledger
        .get(&options.key)
        .ok_or_else(|| format!("no finding with key {}", options.key))?;

    println!("{} [{}]", finding.title, finding.key);
    println!(
        "severity={} status={} location={}:{}",
        format!("{:?}", finding.severity).to_lowercase(),
        finding.status.as_str(),
        finding.file,
        finding
            .line
            .map_or("-".to_string(), |line| line.to_string())
    );
    for (index, attached) in finding.reports.iter().enumerate() {
        println!(
            "\nreport {}: reviewer={} round={} severity={} id={}",
            index + 1,
            attached.source,
            attached.round,
            format!("{:?}", attached.severity).to_lowercase(),
            if attached.report_id.is_empty() {
                "(unavailable: legacy import)"
            } else {
                &attached.report_id
            }
        );
        if attached.report_id.is_empty() {
            print_indented("body", &finding.body);
            print_indented("fix", "(unavailable: artifact-less legacy import)");
            println!(
                "  confidence: {}",
                finding
                    .confidence
                    .map_or("(unavailable)".to_string(), |value| value.to_string())
            );
        } else {
            let report = cas
                .get_json(&attached.report_id)
                .map_err(|e| format!("reading report {}: {e}", attached.report_id))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
            );
        }
    }

    println!("\nhistory:");
    for transition in &finding.history {
        println!(
            "  round {}: {:?}{}",
            transition.round,
            transition.kind,
            transition
                .note
                .as_deref()
                .map(|note| format!(" - {note}"))
                .unwrap_or_default()
        );
    }
    println!(
        "current note: {}",
        finding.current_note().unwrap_or("(none)")
    );
    Ok(())
}

fn print_report(options: &ReportOptions) -> Result<(), String> {
    debug_assert_eq!(options.format, "md");
    let state = campaign_state(&options.state, &options.campaign);
    let store = open_campaign_store(&state)?;
    let cas = Cas::open(state.join("cas")).map_err(|e| e.to_string())?;
    let run_id = campaign_run_id(&options.campaign);
    let ledger = Ledger::rebuild(&store, &cas, &run_id).map_err(|e| e.to_string())?;
    let events = store.replay(&run_id).map_err(|e| e.to_string())?;
    let reports: Vec<_> = events
        .iter()
        .filter(|event| {
            matches!(
                event.event_type,
                EventType::RunReportV1 | EventType::RunReportV2
            )
        })
        .collect();

    println!("# Review campaign `{}`", options.campaign);
    println!();
    println!("- Runs recorded: {}", reports.len());
    println!("- Ledger round: {}", ledger.round);
    println!(
        "- Final verdict: {}",
        reports
            .last()
            .map(|event| report_verdict(event))
            .transpose()?
            .unwrap_or_else(|| "not recorded".to_string())
    );
    println!();
    println!("## Runs");
    println!();
    println!("| Run | Verdict | Tokens |");
    println!("| ---: | --- | ---: |");
    for (index, event) in reports.iter().enumerate() {
        println!(
            "| {} | {} | {} |",
            index + 1,
            report_verdict(event)?,
            event.payload["spent_tokens"]
                .as_u64()
                .map_or("-".to_string(), |tokens| tokens.to_string())
        );
    }

    println!();
    println!("## Findings");
    for severity in ["blocker", "major", "minor"] {
        println!();
        println!("### {}", title_case(severity));
        let matching: Vec<_> = ledger
            .findings()
            .into_iter()
            .filter(|finding| format!("{:?}", finding.severity).to_lowercase() == severity)
            .collect();
        if matching.is_empty() {
            println!();
            println!("None.");
            continue;
        }
        for finding in matching {
            println!();
            println!(
                "- **[{}] {}** (`{}`) at `{}:{}`",
                finding.status.as_str(),
                finding.title,
                finding.key,
                finding.file,
                finding
                    .line
                    .map_or("-".to_string(), |line| line.to_string())
            );
            println!("  - Body: {}", markdown_line(&finding.body));
            println!(
                "  - Fix: {}",
                markdown_line(
                    finding
                        .fix
                        .as_deref()
                        .unwrap_or("unavailable: artifact-less legacy import")
                )
            );
            let evidence = finding
                .reports
                .iter()
                .map(|report| {
                    if report.report_id.is_empty() {
                        format!("{} round {} (legacy import)", report.source, report.round)
                    } else {
                        format!(
                            "{} round {} `{}`",
                            report.source, report.round, report.report_id
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            println!("  - Reports: {evidence}");
            for transition in &finding.history {
                println!(
                    "  - Resolution/history, round {}: {:?} - {}",
                    transition.round,
                    transition.kind,
                    markdown_line(transition.note.as_deref().unwrap_or("(no note)"))
                );
            }
        }
    }
    Ok(())
}

fn report_verdict(event: &review_core::RunEvent) -> Result<String, String> {
    match event.event_type {
        EventType::RunReportV1 => event.payload["verdict"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "RunReport@1 has no string verdict".to_string()),
        EventType::RunReportV2 => {
            let report: RunReportPayloadV2 =
                serde_json::from_value(event.payload.clone()).map_err(|e| e.to_string())?;
            Ok(match report.verdict {
                RunVerdictV2::Pass => "pass".to_string(),
                RunVerdictV2::Fail {
                    reason: RunFailureReasonV2::NotConverged,
                } => "fail (not_converged)".to_string(),
                RunVerdictV2::Fail {
                    reason: RunFailureReasonV2::Exhausted,
                } => "fail (exhausted)".to_string(),
                RunVerdictV2::Incomplete { missing_nodes } => {
                    format!("incomplete ({} missing nodes)", missing_nodes.len())
                }
            })
        }
        _ => Err(format!("{} is not a run report", event.event_type)),
    }
}

fn markdown_line(value: &str) -> String {
    value.lines().collect::<Vec<_>>().join(" ")
}

fn title_case(value: &str) -> String {
    let mut characters = value.chars();
    match characters.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + characters.as_str(),
        None => String::new(),
    }
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
    let git_home = state.join("git-home");
    std::fs::create_dir_all(&git_home).map_err(|e| e.to_string())?;
    let repo = Repo::open(&options.repo, &git_home);
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
        let events = store.replay(&run_id).map_err(|e| e.to_string())?;
        let mut closed_rounds = 0_u32;
        for event in &events {
            if run_report_closes_round(event)
                .map_err(|e| format!("decoding {}: {e}", event.event_type))?
                .unwrap_or(false)
            {
                closed_rounds += 1;
            }
        }
        let target_round = closed_rounds + 1;
        {
            let mut ingest =
                Ingest::new(&mut store, &cas, run_id.clone()).map_err(|e| e.to_string())?;
            while ingest.ledger().round < target_round {
                ingest.advance().map_err(|e| e.to_string())?;
            }
            println!("round    {}", ingest.ledger().round);
        }

        let ledger = Ledger::rebuild(&store, &cas, &run_id).map_err(|e| e.to_string())?;
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
                EventType::SourceCapturedV1,
                snapshot.to_payload(&tree_id, Some(&manifest_artifact)),
            )
            .referencing(vec![manifest_artifact]),
        )
        .map_err(|e| e.to_string())?;

    // Bind reviewers: a packaged reviewer gets the adapter its runner names; an inline
    // command runs as itself.
    let auth = (
        std::env::var("CLAUDE_CONFIG_DIR").ok(),
        std::env::var("USER").ok(),
        home.clone(),
    );
    let mut kernel = Kernel::new(&cas, &mut store, &run_id, snapshot.manifest.clone())
        .with_checks(loaded.checks);
    if let Some(artifact) = prior_artifact {
        kernel = kernel.with_prior_findings(artifact);
    }
    if let Some(budgets) = loaded.budgets {
        println!(
            "budgets  {} attempt reservation, {} run admission cap (chargeable tokens)",
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
                        let user = auth.1.clone().ok_or_else(|| {
                            format!("node `{node}`: Claude subscription auth requires USER")
                        })?;
                        let mut adapter = review_runner_claude::ClaudeAdapter::from_package(
                            package,
                            options.timeout,
                        )
                        .map_err(|e| format!("{node}: {e}"))?
                        .with_auth(auth.0.clone(), user, auth.2.clone());
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
