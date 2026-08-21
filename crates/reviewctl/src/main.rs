//! `reviewctl` — reviews from a definition file to a verdict, and the campaign loop.
//!
//! Six subcommands:
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
//! - `tui` drafts an explicit configuration patch for the pipeline's existing reviewer packages
//!   and launches the same pinned-authority `run` path from an alternate-screen interface.
//!
//! Nothing here mutates any repository. A run reads a repo and writes its own state
//! directory; `resolve` writes only that state; publishing results anywhere is a human's
//! explicit action.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use review_core::{EventType, RunFailureReasonV2, RunReportPayloadV2, RunVerdictV2};
use review_graph::NodeOutcome;
use review_pipeline::{Kernel, RunVerdict};
use review_runner::ReviewerAdapter;
use review_source_git::Repo;
use review_store::{Cas, EventStore, Ingest, Ledger, Status};

mod authority;
mod tui;

#[derive(Clone)]
struct Options {
    repo: PathBuf,
    pipeline: PathBuf,
    state: Option<PathBuf>,
    campaign: Option<String>,
    focus: Option<String>,
    authority: Option<String>,
    uncommitted: bool,
    restart_round: bool,
    timeout: Option<Duration>,
}

impl Options {
    /// Resolve state once for both execution and presentation. Relative paths use the process
    /// working directory, preserving the CLI's historical meaning, and repository-contained
    /// state is confined to the review tree's `runs` directory.
    fn resolved_state_dir(&self) -> Result<PathBuf, String> {
        if let Some(campaign) = &self.campaign {
            validate_campaign_name(campaign)?;
        }
        let requested = match (&self.state, &self.campaign) {
            (Some(state), _) => state.clone(),
            (None, Some(campaign)) => PathBuf::from(format!(".review/runs/{campaign}")),
            (None, None) => PathBuf::from(".review/runs/local"),
        };
        let state = resolve_filesystem_path(&requested)?;
        let repository = std::fs::canonicalize(&self.repo)
            .map_err(|error| format!("opening repository {}: {error}", self.repo.display()))?;
        if state.starts_with(&repository) {
            let pipeline = if self.pipeline.is_absolute() {
                self.pipeline.clone()
            } else {
                repository.join(&self.pipeline)
            };
            let pipeline = pipeline
                .strip_prefix(&repository)
                .map_err(|_| "the pipeline path must be inside --repo".to_string())?
                .to_str()
                .ok_or_else(|| "the pipeline path must be UTF-8".to_string())?;
            let review_root = repository.join(authority::review_dir(pipeline)?);
            refuse_repository_symlinks(&repository, &review_root.join("runs"))?;
            let allowed = resolve_filesystem_path(&review_root.join("runs"))?;
            if !state.starts_with(&allowed) {
                return Err(format!(
                    "state {} overlaps captured repository content; use state outside --repo or below {}",
                    state.display(),
                    review_root.join("runs").display()
                ));
            }
        }
        Ok(state)
    }
}

fn validate_campaign_name(campaign: &str) -> Result<(), String> {
    let mut components = Path::new(campaign).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(format!(
            "campaign name `{campaign}` must be one safe path component"
        ));
    }
    Ok(())
}

fn refuse_repository_symlinks(repository: &Path, path: &Path) -> Result<(), String> {
    let relative = path
        .strip_prefix(repository)
        .map_err(|_| format!("path {} is outside --repo", path.display()))?;
    let mut current = repository.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "repository-contained state path {} is a symlink",
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(format!("opening {}: {error}", current.display())),
        }
    }
    Ok(())
}

fn resolve_filesystem_path(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("reading current directory: {error}"))?
            .join(path)
    };
    let absolute = normalize_absolute(&absolute)?;
    let mut existing = absolute.clone();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| format!("path {} has no existing ancestor", absolute.display()))?
            .to_os_string();
        suffix.push(name);
        existing
            .pop()
            .then_some(())
            .ok_or_else(|| format!("path {} has no existing ancestor", absolute.display()))?;
    }
    let mut resolved = std::fs::canonicalize(&existing)
        .map_err(|error| format!("opening {}: {error}", existing.display()))?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    normalize_absolute(&resolved)
}

fn normalize_absolute(path: &Path) -> Result<PathBuf, String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::Normal(_) => normalized.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return Err(format!(
                        "path {} escapes its filesystem root",
                        path.display()
                    ));
                }
            }
        }
    }
    if !normalized.is_absolute() {
        return Err(format!(
            "path {} did not resolve absolutely",
            path.display()
        ));
    }
    Ok(normalized)
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
         [--campaign NAME] [--authority REV] [--uncommitted] [--restart-round] [--focus TEXT] [--timeout-secs N]\n\
        \x20      reviewctl tui     [--repo DIR] [--pipeline FILE] [--state DIR] \
         [--campaign NAME] [--authority REV] [--uncommitted] [--restart-round] [--focus TEXT] [--timeout-secs N]\n\
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

fn campaign_state(state: &Option<PathBuf>, campaign: &str) -> Result<PathBuf, String> {
    validate_campaign_name(campaign)?;
    resolve_filesystem_path(
        &state
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!(".review/runs/{campaign}"))),
    )
}

fn parse_run(mut args: std::env::Args) -> Options {
    let mut options = Options {
        repo: PathBuf::from("."),
        pipeline: PathBuf::from(".review/pipelines/heavy.toml"),
        state: None,
        campaign: None,
        focus: None,
        authority: None,
        uncommitted: false,
        restart_round: false,
        timeout: None,
    };
    while let Some(flag) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| usage());
        match flag.as_str() {
            "--repo" => options.repo = PathBuf::from(value()),
            "--pipeline" => options.pipeline = PathBuf::from(value()),
            "--state" => options.state = Some(PathBuf::from(value())),
            "--campaign" => options.campaign = Some(value()),
            "--focus" => options.focus = Some(value()),
            "--authority" => options.authority = Some(value()),
            "--uncommitted" => options.uncommitted = true,
            "--restart-round" => options.restart_round = true,
            "--timeout-secs" => {
                options.timeout = Some(Duration::from_secs(
                    value().parse().unwrap_or_else(|_| usage()),
                ))
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
        Some("run") => run(&parse_run(args)).map(exit_for_verdict),
        Some("ledger") => print_ledger(&parse_ledger(args)),
        Some("show") => show(&parse_show(args)),
        Some("report") => print_report(&parse_report(args)),
        Some("resolve") => resolve(&parse_resolve(args)),
        Some("tui") => tui::launch(parse_run(args)),
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
    let state = campaign_state(&options.state, &options.campaign)?;
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
    let state = campaign_state(&options.state, &options.campaign)?;
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
    let state = campaign_state(&options.state, &options.campaign)?;
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
    let state = campaign_state(&options.state, &options.campaign)?;
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

fn run(options: &Options) -> Result<RunVerdict, String> {
    let state = options.resolved_state_dir()?;
    std::fs::create_dir_all(&state).map_err(|error| error.to_string())?;
    let cas = Cas::open(state.join("cas")).map_err(|error| error.to_string())?;
    let mut store =
        EventStore::open(state.join("events.sqlite")).map_err(|error| error.to_string())?;
    let home = std::env::var("HOME").map_err(|error| error.to_string())?;
    let git_home = state.join("git-home");
    std::fs::create_dir_all(&git_home).map_err(|error| error.to_string())?;
    let repo = Repo::open(&options.repo, &git_home);

    let authority::PreparedRun {
        loaded,
        snapshot,
        run_id,
        focus,
        timeout,
        authority,
    } = authority::prepare(options, &cas, &mut store, &repo)?;
    println!("run      {run_id}");

    let auth = (
        std::env::var("CLAUDE_CONFIG_DIR").ok(),
        std::env::var("USER").ok(),
        home.clone(),
    );
    let mut kernel = Kernel::from_loaded(&cas, &mut store, &run_id, snapshot, &loaded, authority)?
        .with_checks(loaded.checks().to_vec());
    if let Some(budgets) = loaded.budgets() {
        println!(
            "budgets  {} attempt reservation, {} run admission cap (chargeable tokens)",
            budgets.attempt, budgets.run
        );
        kernel = kernel.with_budgets(budgets.attempt, budgets.run);
    }

    let mut bound: BTreeMap<String, String> = BTreeMap::new();
    for (node, command) in loaded.reviewers() {
        let adapter: Box<dyn ReviewerAdapter> = match loaded.packages().get(node) {
            Some(package) => {
                let program = std::path::Path::new(&command.program)
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string())
                    .unwrap_or_default();
                match program.as_str() {
                    "claude" => {
                        let user = auth.1.clone().ok_or_else(|| {
                            format!("node `{node}`: Claude subscription auth requires USER")
                        })?;
                        let mut adapter =
                            review_runner_claude::ClaudeAdapter::from_package(package, timeout)
                                .map_err(|error| format!("{node}: {error}"))?
                                .with_auth(auth.0.clone(), user, auth.2.clone());
                        if let Some(focus) = &focus {
                            adapter = adapter.with_focus(focus);
                        }
                        Box::new(adapter)
                    }
                    "codex" => {
                        let mut adapter =
                            review_runner_codex::CodexAdapter::from_package(package, timeout)
                                .map_err(|error| format!("{node}: {error}"))?
                                .with_codex_home(format!("{home}/.codex"));
                        if let Some(focus) = &focus {
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

    let report = loaded.run(&kernel).map_err(|error| error.to_string())?;
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
            "  [{:?}] {}:{} - {} ({:?})",
            finding.severity,
            finding.file,
            finding
                .line
                .map_or("?".to_string(), |line| line.to_string()),
            finding.title,
            finding.status
        );
    }
    if let Some(spent) = kernel.spent() {
        println!("spent    {spent} tokens");
    }

    let verdict = kernel.publish_report(&report, *loaded.convergence())?;
    println!("verdict  {verdict:?}");
    Ok(verdict)
}

fn exit_for_verdict(verdict: RunVerdict) {
    match verdict {
        RunVerdict::Pass => {}
        RunVerdict::Fail(_) => std::process::exit(3),
        RunVerdict::Incomplete { .. } => std::process::exit(4),
    }
}

#[cfg(test)]
mod option_tests {
    use super::{Options, validate_campaign_name};

    #[test]
    fn campaign_names_cannot_redirect_state() {
        for invalid in ["", "../reviewers/architecture", "nested/name", "."] {
            assert!(validate_campaign_name(invalid).is_err());
        }
        for valid in ["heavy", "reviewctl-tui", "round_4", "v2.1-audit"] {
            assert!(validate_campaign_name(valid).is_ok());
        }
    }

    #[cfg(unix)]
    #[test]
    fn repository_runs_symlink_cannot_redirect_state() {
        use std::os::unix::fs::symlink;

        let repository = std::env::temp_dir().join(format!(
            "reviewctl-state-symlink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(repository.join(".review/reviewers")).unwrap();
        symlink("reviewers", repository.join(".review/runs")).unwrap();
        let options = Options {
            repo: repository.clone(),
            pipeline: ".review/pipelines/heavy.toml".into(),
            state: Some(repository.join(".review/runs/architecture")),
            campaign: Some("architecture".to_string()),
            focus: None,
            authority: None,
            uncommitted: false,
            restart_round: false,
            timeout: None,
        };
        let error = options.resolved_state_dir().unwrap_err();
        assert!(error.contains("is a symlink"));
        std::fs::remove_dir_all(repository).unwrap();
    }
}
