//! Deterministic, token-free review-authority onboarding.
//!
//! The binary owns one small scaffold so an agent can discover and reproduce a supported setup
//! without pasting a prompt or hand-writing a digest. Once emitted, every byte is ordinary
//! project-owned authority: `af onboard` validates it, but never silently overwrites it.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use review_config::lock::{AfPin, Lockfile, Pin, Registry};
use review_config::{
    ArgSpec, BudgetSpec, BudgetUnit, CheckSpec, CommandSpec, ConvergenceSpec, Definition, EdgeSpec,
    GateExecutionSpec, GateModeSpec, IsolationSpec, NodeKindSpec, NodeSpec, PortSpec,
    ProvenanceSpec, SandboxProviderSpec, SeveritySpec, SubjectSpec, TypedPortSpec,
};
use review_core::{PortCardinality, SnapshotAffinity, SubjectKind, contract};
use serde::Serialize;

mod tasks;

const MAX_DISCOVERY_FILE_BYTES: u64 = 1024 * 1024;
const PROFILE: &str = "multi-review@1";
const PIPELINE_NAME: &str = "review";
const PIPELINE_VERSION: &str = "1.0.0";
const WORKER_VERSION: &str = "1.0.0";

const CORRECTNESS_PROMPT: &str = r#"# Correctness reviewer

Review the exact kernel-selected Subject for concrete correctness defects at high depth. The
materialized working directory is the head Snapshot and is yours alone to explore. For a Diff
Subject, review the Base-to-head behavior named by the supplied exact Change Set and trace changed
contracts through their immediate producers and consumers.

Look for, in order of importance:

1. Behavior that contradicts the stated requirement, public contract, schema, or durable state.
2. State-transition, replay, concurrency, and crash-consistency paths that can disagree.
3. Error, timeout, budget, and isolation paths that silently pass or lose evidence.
4. Compatibility gaps where a changed interface leaves a caller, fixture, or persisted version.
5. Missing tests only when they expose one specific unverified failure path.

Report only concrete correctness defects with a reproducible path to the wrong result. Do not
report style, naming, speculative refactors, or performance-only optimization. Every Finding needs
a concrete fix.
"#;

const ARCHITECTURE_PROMPT: &str = r#"# Architecture reviewer

Review the exact kernel-selected Subject for architectural defects at high depth. The materialized
working directory is the head Snapshot and is yours alone to explore. For a Diff Subject, review
the Base-to-head behavior named by the supplied exact Change Set.

Look for, in order of importance:

1. Responsibilities leaking across module or service boundaries and inverted dependencies.
2. Invariants or state that multiple components believe they own.
3. A second implementation shape that duplicates an established concept and will drift.
4. Public contracts changed without every immediate producer and consumer.
5. Security or operability consequences caused specifically by the changed boundary.

Do not report style, formatting, naming, or general redesign wishes. Report only a concrete defect
introduced or exposed by this Subject, explain the failure path, and give a bounded fix.
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunnerProfile {
    Mixed,
    Claude,
    Codex,
}

impl RunnerProfile {
    fn name(self) -> &'static str {
        match self {
            Self::Mixed => "mixed",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    fn runner(self, worker: &str) -> RunnerKind {
        match (self, worker) {
            (Self::Mixed, "correctness") | (Self::Claude, _) => RunnerKind::Claude,
            (Self::Mixed, _) | (Self::Codex, _) => RunnerKind::Codex,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunnerKind {
    Claude,
    Codex,
}

impl RunnerKind {
    fn program(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    fn model(self) -> &'static str {
        match self {
            Self::Claude => "claude-opus-5-5 (high effort)",
            Self::Codex => "gpt-6-sol (high effort)",
        }
    }

    /// The exact model ID and effort level the generated Worker binds.
    fn binding(self) -> (&'static str, &'static str) {
        match self {
            Self::Claude => ("claude-opus-5-5", "high"),
            Self::Codex => ("gpt-6-sol", "high"),
        }
    }

    /// The generated binding passes the static rules of the one validation authority: no
    /// alias, a versioned exact ID and an effort level the Provider kind accepts (ADR-0148).
    fn check_models(self) -> Result<(), String> {
        use review_runner_claude::models::{Kind, ProviderRef, check_static};
        let kind = match self {
            Self::Claude => Kind::Claude,
            Self::Codex => Kind::Codex,
        };
        let (model, effort) = self.binding();
        check_static(kind, model, Some(effort))
            .map_err(|refusal| refusal.hint(ProviderRef { id: None, kind }, model, Some(effort)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct Gate {
    name: String,
    program: String,
    args: Vec<String>,
    source: String,
}

impl Gate {
    fn detected(name: &str, program: &str, args: &[&str], source: &str) -> Self {
        Self {
            name: name.to_string(),
            program: program.to_string(),
            args: args.iter().map(|value| (*value).to_string()).collect(),
            source: source.to_string(),
        }
    }

    fn command_line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(shell_words::quote)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone)]
struct Options {
    repo: PathBuf,
    tasks: bool,
    /// `--runner`; without it a new bundle's profile follows the registered Provider kinds.
    runner: Option<RunnerProfile>,
    /// `--probe`: run the token-free Provider status probe for the providers block.
    probe: bool,
    gates: Vec<Gate>,
    apply: bool,
    refresh_lock: bool,
    /// `--af VERSION`: dispatch already ran this command under that release; here it is only
    /// checked, because a pin can be moved forward only by the release that will hold it.
    af: Option<String>,
    json: bool,
    long: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ReviewerSummary {
    node: String,
    package: String,
    runner: String,
    model: String,
}

#[derive(Debug, Clone, Serialize)]
struct Report {
    status: String,
    profile: String,
    repository: String,
    pipeline: String,
    reviewers: Vec<ReviewerSummary>,
    gates: Vec<Gate>,
    attempt_tokens: Option<u64>,
    run_tokens: Option<u64>,
    clean_rounds: u32,
    max_rounds: u32,
    topology: Vec<String>,
    warnings: Vec<String>,
    files: Vec<String>,
    next_steps: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_af_version: Option<String>,
    /// The targets whose archive digests the lock's `af` pin records.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    lock_af_targets: Vec<String>,
    /// Which runner profile a new bundle uses and why, for the `profile` line. Text only: the
    /// JSON document keeps its shape.
    #[serde(skip)]
    runner_note: Option<String>,
    /// One line per model Worker: the Provider kind it needs and what is registered, for the
    /// `providers` block. Text only, like `runner_note`.
    #[serde(skip)]
    providers: Vec<String>,
}

/// The pin this binary writes into a lock: its own release with every target's archive digest
/// from the release's verified `SHA256SUMS`, or at least this target's from its receipt. A build
/// without an install receipt pins nothing: there is no released byte to pin. The release's
/// digest for this target must agree with the receipt, or the lock would pin bytes other than
/// the ones that wrote it.
fn running_af_pin() -> Result<(Option<AfPin>, Vec<String>), String> {
    let version = env!("CARGO_PKG_VERSION");
    let Some(receipt) = crate::selfmgmt::self_receipt() else {
        let receipt = crate::selfmgmt::receipt_path()
            .map(|path| format!(" at {}", path.display()))
            .unwrap_or_default();
        return Ok((
            None,
            vec![format!(
                "{} has no install receipt{receipt} (a source build or a launcher copy), so the lock pins no af release; install a release with `af self install {version}` and rerun to pin it",
                no_receipt_subject()
            )],
        ));
    };
    let own = format!("sha256:{}", receipt.sha256);
    let own_is_digest =
        receipt.sha256.len() == 64 && receipt.sha256.bytes().all(|byte| byte.is_ascii_hexdigit());
    let mut pin = AfPin::version_only(&receipt.version);
    let mut warnings = Vec::new();
    match crate::selfmgmt::release_digests(&receipt.version) {
        Ok((digests, _)) => {
            match digests.get(&receipt.target) {
                Some(listed) if own_is_digest && *listed != own => {
                    return Err(format!(
                        "the release lists {listed} for {} but this af {} was installed from {own}; the asset may have been re-uploaded — reinstall with `af self remove {1} && af self install {1}` before pinning",
                        receipt.target, receipt.version
                    ));
                }
                Some(_) => {}
                None if own_is_digest => {
                    warnings.push(format!(
                        "the release lists no archive for {}; the lock pins this binary's own digest for it",
                        receipt.target
                    ));
                }
                None => {}
            }
            pin.digests = digests;
            if own_is_digest {
                pin.digests.insert(receipt.target.clone(), own);
            }
        }
        Err(error) => {
            if own_is_digest {
                pin.digests.insert(receipt.target.clone(), own);
            }
            warnings.push(format!(
                "{error}; the lock records this target only ({}) — rerun `af onboard --refresh-lock` online to record every target",
                receipt.target
            ));
        }
    }
    Ok((Some(pin), warnings))
}

fn pin_targets(pin: Option<&AfPin>) -> Vec<String> {
    pin.map(|pin| pin.digests.keys().cloned().collect())
        .unwrap_or_default()
}

struct Bundle {
    files: BTreeMap<String, Vec<u8>>,
    worker_files: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    report: Report,
}

pub(super) fn run_cli(args: crate::cli::OnboardArgs) -> Result<(), String> {
    let options = options_from_cli(args)?;
    if options.tasks {
        return tasks::run(&options);
    }
    let report = execute(&options)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
        );
    } else {
        print_human(&report, options.long);
    }
    Ok(())
}

fn options_from_cli(args: crate::cli::OnboardArgs) -> Result<Options, String> {
    let runner = args.runner.map(|runner| match runner {
        crate::cli::RunnerArg::Mixed => RunnerProfile::Mixed,
        crate::cli::RunnerArg::Claude => RunnerProfile::Claude,
        crate::cli::RunnerArg::Codex => RunnerProfile::Codex,
    });
    let mut gates = Vec::new();
    let mut gate_names = BTreeSet::new();
    for value in &args.gate {
        let gate = parse_gate(value)?;
        if !gate_names.insert(gate.name.clone()) {
            return Err(format!("duplicate Gate name `{}`", gate.name));
        }
        gates.push(gate);
    }
    let options = Options {
        repo: args.repo,
        tasks: args.tasks,
        runner,
        probe: args.probe,
        gates,
        apply: args.apply,
        refresh_lock: args.refresh_lock,
        af: args
            .af
            .map(|version| version.trim_start_matches('v').to_string()),
        json: args.json,
        long: args.long,
    };
    if let Some(requested) = &options.af
        && requested != env!("CARGO_PKG_VERSION")
    {
        return Err(format!(
            "--af {requested} asks release {requested} to run this command, but af {} is running: the dispatch to it was refused — fix: af self install {requested}",
            env!("CARGO_PKG_VERSION")
        ));
    }
    if options.apply && options.refresh_lock {
        return Err("--apply and --refresh-lock are mutually exclusive".to_string());
    }
    if options.tasks && options.probe {
        return Err(
            "--probe is for review onboarding; use `af provider status` to inspect Task Providers"
                .into(),
        );
    }
    if options.refresh_lock && !options.gates.is_empty() {
        return Err(
            "--gate cannot be combined with --refresh-lock; edit the pipeline first".into(),
        );
    }
    if options.refresh_lock && options.runner.is_some() {
        return Err(
            "--runner cannot be combined with --refresh-lock; edit Worker manifests first".into(),
        );
    }
    Ok(options)
}

fn parse_gate(value: &str) -> Result<Gate, String> {
    let (name, command) = value
        .split_once('=')
        .ok_or("a Gate must be NAME=COMMAND, for example `check=make check`")?;
    if !safe_name(name) {
        return Err(format!("Gate name `{name}` is not one safe identifier"));
    }
    let words = shell_words::split(command)
        .map_err(|error| format!("parsing Gate `{name}` command: {error}"))?;
    let (program, args) = words
        .split_first()
        .ok_or_else(|| format!("Gate `{name}` has an empty command"))?;
    Ok(Gate {
        name: name.to_string(),
        program: program.clone(),
        args: args.to_vec(),
        source: "explicit --gate".to_string(),
    })
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'-' | b'_' | b'.'))
        })
}

fn execute(options: &Options) -> Result<Report, String> {
    let repo = std::fs::canonicalize(&options.repo)
        .map_err(|error| format!("opening repository {}: {error}", options.repo.display()))?;
    if !repo.join(".git").exists() {
        return Err(format!(
            "{} is not a Git repository root (no .git entry)",
            repo.display()
        ));
    }
    let authority = repo.join(".af");
    match std::fs::symlink_metadata(&authority) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err("refusing symlinked review authority `.af`".to_string())
        }
        Ok(metadata) if !metadata.is_dir() => {
            Err("review authority `.af` exists but is not a directory".to_string())
        }
        Ok(_) => {
            if options.apply {
                return Err(
                    "`.af/` already exists; plain `af onboard` validates it and never overwrites it"
                        .to_string(),
                );
            }
            if options.refresh_lock {
                return refresh_lock(&repo);
            }
            if !options.gates.is_empty() || options.runner.is_some() {
                return Err(
                    "--gate and --runner configure only a new bundle; existing authority is never silently changed"
                        .into(),
                );
            }
            let mut report = inspect_existing(&repo, "onboarded")?;
            let registry = crate::providers::read_registry_entries();
            report.providers = providers_block(&report.reviewers, &registry, options.probe);
            report.next_steps = validated_next_steps(&report.reviewers, &registry);
            Ok(report)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if options.refresh_lock {
                return Err("cannot refresh a lock before `.af/` exists".to_string());
            }
            let gates = if options.gates.is_empty() {
                discover_gates(&repo)?
            } else {
                options.gates.clone()
            };
            if gates.is_empty() {
                return Err(
                    "no unambiguous acceptance Gate found; supply a trusted literal such as `--gate 'check=make check'`"
                        .to_string(),
                );
            }
            let registry = crate::providers::read_registry_entries();
            let (profile, runner_note) = pick_runner(options.runner, &registry);
            let mut bundle = build_bundle(&repo, profile, gates, options.af.as_deref())?;
            validate_bundle(&bundle)?;
            bundle.report.runner_note = Some(runner_note);
            bundle.report.providers =
                providers_block(&bundle.report.reviewers, &registry, options.probe);
            if bundle
                .report
                .providers
                .iter()
                .any(|line| line.contains(NONE_REGISTERED))
            {
                bundle.report.next_steps.push(REGISTER_A_PROVIDER.into());
            }
            if options.apply {
                apply_bundle(&repo, &bundle)?;
                bundle.report.status = "created".to_string();
                bundle.report.next_steps = created_next_steps();
            }
            Ok(bundle.report)
        }
        Err(error) => Err(format!("inspecting {}: {error}", authority.display())),
    }
}

/// The widest line of the text preview: the terminal's width, at most 80 columns, so the default
/// preview reads on one 80x24 screen.
fn preview_width() -> usize {
    crate::human::width().min(80)
}

/// One line of the text preview, wrapped at its spaces to [`preview_width`], every line after
/// the first indented by `indent`.
fn print_wrapped(line: &str, indent: usize) {
    println!("{}", crate::human::wrap(line, preview_width(), indent));
}

/// A `providers` line: whole when it fits, else what is registered and, on a line of its own,
/// the arrow to what to run.
fn print_provider_line(line: &str) {
    let whole = format!("  {line}");
    match line.split_once(PROVIDER_ARROW) {
        Some((state, fix)) if whole.chars().count() > preview_width() => {
            print_wrapped(&format!("  {state}"), 4);
            print_wrapped(&format!("    ← {fix}"), 6);
        }
        _ => print_wrapped(&whole, 4),
    }
}

/// A next step: whole when it fits; a step that ends in an `af` command between backticks
/// (`Review this plan, then run `af onboard ... --apply`.`) as its prose and then the command,
/// continued between its arguments, so a copied command still runs; any other step wrapped.
fn print_next_step(number: usize, step: &str) {
    let width = preview_width();
    let line = format!("  {number}. {step}");
    if line.chars().count() <= width {
        println!("{line}");
        return;
    }
    if let Some((prose, command)) = step
        .strip_suffix("`.")
        .and_then(|rest| rest.rsplit_once(" `"))
        .filter(|(_, command)| command.starts_with("af ") && !command.contains('`'))
    {
        let prose = prose.trim_end_matches(':');
        print_wrapped(&format!("  {number}. {prose}:"), 5);
        println!(
            "{}",
            crate::human::fit_command(&format!("     {command}"), width)
        );
        return;
    }
    print_wrapped(&line, 5);
}

/// The subject of the warning of a build without an install receipt.
fn no_receipt_subject() -> String {
    format!("this af {}", env!("CARGO_PKG_VERSION"))
}

/// A warning as the default preview says it: a build without an install receipt in one line,
/// its receipt path and fix left to `--long` and `--json`; any other warning whole.
fn brief_warning(warning: &str) -> String {
    if warning.starts_with(&format!("{} has no install receipt", no_receipt_subject())) {
        return "no install receipt: the lock pins no af release; --long says more".into();
    }
    warning.to_owned()
}

fn print_human(report: &Report, long: bool) {
    println!("af onboard: {}", report.status);
    println!("repository  {}", report.repository);
    match &report.runner_note {
        Some(note) => print_wrapped(&format!("profile     {} · {note}", report.profile), 12),
        None => println!("profile     {}", report.profile),
    }
    // The default view counts the files `--long` lists after the topology.
    match report.files.len() {
        files if files > 0 && !long => println!(
            "pipeline    {}; {files} files under .af/, --long lists each",
            report.pipeline
        ),
        _ => println!("pipeline    {}", report.pipeline),
    }
    if let Some(version) = &report.lock_af_version {
        print_wrapped(
            &format!(
                "lock        pinned by af {version}{}",
                if report.lock_af_targets.is_empty() {
                    " (no archive digests)".to_string()
                } else {
                    format!(" (digests: {})", report.lock_af_targets.join(", "))
                }
            ),
            12,
        );
    }
    if long {
        println!("topology");
        for line in &report.topology {
            println!("  {line}");
        }
    }
    println!("reviewers");
    for reviewer in &report.reviewers {
        print_wrapped(
            &format!(
                "  {}: {} / {} ({})",
                reviewer.node, reviewer.package, reviewer.runner, reviewer.model
            ),
            4,
        );
    }
    if !report.providers.is_empty() {
        println!("providers");
        for line in &report.providers {
            print_provider_line(line);
        }
    }
    let gate_line =
        |gate: &Gate| format!("{}: {} [{}]", gate.name, gate.command_line(), gate.source);
    match report.gates.as_slice() {
        [gate] => print_wrapped(&format!("gates       {}", gate_line(gate)), 14),
        gates => {
            println!("gates");
            for gate in gates {
                print_wrapped(&format!("  {}", gate_line(gate)), 4);
            }
        }
    }
    let budget = match (report.attempt_tokens, report.run_tokens) {
        (Some(attempt), Some(run)) => format!("{attempt} tokens/Attempt; {run} tokens/Round"),
        _ => "uncapped by pipeline authority".to_string(),
    };
    if long {
        println!("budget      {budget}");
        println!(
            "convergence {} clean Round; {} Round maximum",
            report.clean_rounds, report.max_rounds
        );
    } else {
        print_wrapped(
            &format!("budget      {budget}; {} Rounds at most", report.max_rounds),
            12,
        );
    }
    for warning in &report.warnings {
        let warning = if long {
            warning.clone()
        } else {
            brief_warning(warning)
        };
        print_wrapped(&format!("warning     {warning}"), 12);
    }
    if long && !report.files.is_empty() {
        println!("files");
        for path in &report.files {
            println!("  {path}");
        }
    }
    println!("next");
    // The preview's providers block already names `af provider detect` where it says none, so
    // its text leaves the step that points at the block to `--json`.
    let steps = report
        .next_steps
        .iter()
        .filter(|step| !(report.status == "preview" && *step == REGISTER_A_PROVIDER));
    for (index, step) in steps.enumerate() {
        print_next_step(index + 1, step);
    }
}

fn discover_gates(repo: &Path) -> Result<Vec<Gate>, String> {
    for makefile in ["GNUmakefile", "Makefile", "makefile"] {
        let path = repo.join(makefile);
        if path.is_file() {
            let text = read_bounded_text(&path)?;
            if has_make_target(&text, "check") {
                return Ok(vec![Gate::detected(
                    "check",
                    "make",
                    &["check"],
                    &format!("detected {makefile} target"),
                )]);
            }
        }
    }
    if repo.join("scripts/verify.sh").is_file() {
        return Ok(vec![Gate::detected(
            "verify",
            "bash",
            &["scripts/verify.sh"],
            "detected scripts/verify.sh",
        )]);
    }
    if repo.join("Cargo.toml").is_file() {
        let args = if repo.join("Cargo.lock").is_file() {
            vec!["test", "--locked"]
        } else {
            vec!["test"]
        };
        return Ok(vec![Gate::detected(
            "cargo-test",
            "cargo",
            &args,
            "detected Cargo.toml",
        )]);
    }
    if repo.join("go.mod").is_file() {
        return Ok(vec![Gate::detected(
            "go-test",
            "go",
            &["test", "./..."],
            "detected go.mod",
        )]);
    }
    let package_json = repo.join("package.json");
    if package_json.is_file() {
        let value: serde_json::Value = serde_json::from_str(&read_bounded_text(&package_json)?)
            .map_err(|error| format!("parsing {}: {error}", package_json.display()))?;
        let test_script = value
            .get("scripts")
            .and_then(|scripts| scripts.get("test"))
            .and_then(serde_json::Value::as_str);
        if test_script.is_some_and(|script| {
            let script = script.trim();
            !(script.is_empty()
                || script.contains("Error: no test specified") && script.contains("exit 1"))
        }) {
            let (name, program, args, source) = if repo.join("pnpm-lock.yaml").is_file() {
                (
                    "pnpm-test",
                    "pnpm",
                    vec!["test"],
                    "detected pnpm test script",
                )
            } else if repo.join("yarn.lock").is_file() {
                (
                    "yarn-test",
                    "yarn",
                    vec!["test"],
                    "detected yarn test script",
                )
            } else if repo.join("bun.lock").is_file() || repo.join("bun.lockb").is_file() {
                ("bun-test", "bun", vec!["test"], "detected Bun test script")
            } else {
                ("npm-test", "npm", vec!["test"], "detected npm test script")
            };
            return Ok(vec![Gate::detected(name, program, &args, source)]);
        }
    }
    Ok(Vec::new())
}

fn has_make_target(text: &str, target: &str) -> bool {
    text.lines().any(|line| {
        if line.starts_with(char::is_whitespace) || line.trim_start().starts_with('#') {
            return false;
        }
        let Some((targets, _)) = line.split_once(':') else {
            return false;
        };
        targets.split_whitespace().any(|name| name == target)
    })
}

fn read_bounded_text(path: &Path) -> Result<String, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("reading {} metadata: {error}", path.display()))?;
    if metadata.len() > MAX_DISCOVERY_FILE_BYTES {
        return Err(format!(
            "{} is {} bytes; onboarding reads at most {} bytes from one discovery file",
            path.display(),
            metadata.len(),
            MAX_DISCOVERY_FILE_BYTES
        ));
    }
    // Bounded again while reading: a file that grew since its size was read is not read whole.
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(
            std::fs::File::open(path)
                .map_err(|error| format!("reading {}: {error}", path.display()))?,
            MAX_DISCOVERY_FILE_BYTES + 1,
        ),
        &mut bytes,
    )
    .map_err(|error| format!("reading {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_DISCOVERY_FILE_BYTES {
        return Err(format!(
            "{} grew beyond {MAX_DISCOVERY_FILE_BYTES} bytes while onboarding read it",
            path.display()
        ));
    }
    String::from_utf8(bytes).map_err(|error| format!("reading {}: {error}", path.display()))
}

fn build_bundle(
    repo: &Path,
    profile: RunnerProfile,
    gates: Vec<Gate>,
    af: Option<&str>,
) -> Result<Bundle, String> {
    let project_name = repo
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("project");
    let definition = build_definition(&gates);
    let warnings = validate_budget_arithmetic(&definition)?;
    let pipeline = toml::to_string_pretty(&definition).map_err(|error| error.to_string())?;

    let mut worker_files = BTreeMap::new();
    for (name, prompt) in [
        ("correctness", CORRECTNESS_PROMPT),
        ("architecture", ARCHITECTURE_PROMPT),
    ] {
        // The generated defaults pass the one validation authority, as every binding must
        // (ADR-0148).
        profile.runner(name).check_models()?;
        worker_files.insert(
            name.to_string(),
            BTreeMap::from([
                (
                    "reviewer.toml".to_string(),
                    worker_manifest(name, profile.runner(name)).into_bytes(),
                ),
                ("reviewer.md".to_string(), prompt.as_bytes().to_vec()),
            ]),
        );
    }

    let (af_pin, pin_warnings) = running_af_pin()?;
    let mut lockfile = Lockfile::empty();
    lockfile.af = af_pin.clone();
    for (name, files) in &worker_files {
        lockfile.workers.insert(
            name.clone(),
            Lockfile::pin_package_files(name, files).map_err(|error| error.to_string())?,
        );
    }
    lockfile.pipelines.insert(
        PIPELINE_NAME.to_string(),
        Pin {
            version: PIPELINE_VERSION.to_string(),
            digest: review_store::canonical::blob_content_id(pipeline.as_bytes()),
        },
    );

    let af_toml = project_file(project_name);
    let guide = guide_file(project_name, profile, &gates);
    let mut files = BTreeMap::from([
        (".af/README.md".to_string(), guide.into_bytes()),
        (".af/af.lock".to_string(), lockfile.to_toml().into_bytes()),
        (".af/af.toml".to_string(), af_toml.into_bytes()),
        (
            ".af/pipelines/review.toml".to_string(),
            pipeline.into_bytes(),
        ),
    ]);
    for (name, package) in &worker_files {
        for (path, bytes) in package {
            files.insert(format!(".af/workers/{name}/{path}"), bytes.clone());
        }
    }

    let reviewers = reviewer_summaries(profile);
    let mut warnings = warnings;
    warnings.extend(pin_warnings);
    let apply_command = apply_command(
        repo,
        profile,
        &gates,
        af.unwrap_or(env!("CARGO_PKG_VERSION")),
    )?;
    let report = Report {
        status: "preview".to_string(),
        profile: PROFILE.to_string(),
        repository: repo.display().to_string(),
        pipeline: ".af/pipelines/review.toml".to_string(),
        reviewers,
        gates,
        attempt_tokens: Some(300_000),
        run_tokens: Some(1_000_000),
        clean_rounds: 1,
        max_rounds: 2,
        topology: topology_lines(&definition),
        warnings,
        files: files.keys().cloned().collect(),
        lock_af_version: af_pin.as_ref().map(|pin| pin.version.clone()),
        lock_af_targets: pin_targets(af_pin.as_ref()),
        runner_note: None,
        providers: Vec::new(),
        next_steps: vec![
            format!("Review this plan, then run `{apply_command}`."),
            "Preview `af setup agents`, then install it with `af setup agents --write`.".into(),
            "Review and commit the generated `.af/` diff on the trusted base branch.".into(),
            "Run `af onboard` again to validate the authority and print the workflow.".into(),
        ],
    };
    Ok(Bundle {
        files,
        worker_files,
        report,
    })
}

fn apply_command(
    repo: &Path,
    profile: RunnerProfile,
    gates: &[Gate],
    af: &str,
) -> Result<String, String> {
    let repo = repo.to_str().ok_or_else(|| {
        format!(
            "repository path {} is not valid UTF-8 and cannot be represented in a copyable shell command",
            repo.display()
        )
    })?;
    let mut command = format!(
        "af onboard --repo {} --runner {}",
        shell_words::quote(repo),
        profile.name()
    );
    for gate in gates {
        let literal = format!("{}={}", gate.name, gate.command_line());
        command.push_str(" --gate ");
        command.push_str(&shell_words::quote(&literal));
    }
    command.push_str(" --af ");
    command.push_str(&shell_words::quote(af));
    command.push_str(" --apply");
    Ok(command)
}

fn build_definition(gates: &[Gate]) -> Definition {
    let checks = gates
        .iter()
        .map(|gate| CheckSpec {
            name: gate.name.clone(),
            command: CommandSpec {
                program: gate.program.clone(),
                args: gate
                    .args
                    .iter()
                    .map(|value| ArgSpec {
                        value: value.clone(),
                        provenance: ProvenanceSpec::Literal,
                    })
                    .collect(),
            },
            required: true,
        })
        .collect();

    let gate = NodeSpec {
        id: "gate".into(),
        kind: NodeKindSpec::Gate,
        demands: None,
        inputs: Vec::new(),
        outputs: vec![typed_port("decision", contract::GATE_DECISION_V1)],
        gated_by: None,
        runner: None,
        package: None,
        execution: None,
        slicing: None,
        closeout_for: None,
        budget: None,
        warm: None,
    };
    let generation = NodeSpec {
        id: "generation".into(),
        kind: NodeKindSpec::Generation,
        demands: None,
        inputs: Vec::new(),
        outputs: vec![
            prior_finding_set_port("findings"),
            typed_port("change_set", contract::CHANGE_SET_V1),
        ],
        gated_by: None,
        runner: None,
        package: None,
        execution: None,
        slicing: None,
        closeout_for: None,
        budget: None,
        warm: None,
    };
    let reviewers: Vec<NodeSpec> = ["correctness", "architecture"]
        .into_iter()
        .map(|id| NodeSpec {
            id: id.into(),
            kind: NodeKindSpec::Reviewer,
            demands: Some(review_core::DemandRequirement::Required),
            inputs: vec![
                typed_port("gate", contract::GATE_DECISION_V1),
                prior_finding_set_port("prior_findings"),
                typed_port("change_set", contract::CHANGE_SET_V1),
            ],
            outputs: vec![typed_port("result", contract::REVIEWER_RESULT_V2)],
            gated_by: Some("gate".into()),
            runner: None,
            package: Some(id.into()),
            execution: None,
            slicing: None,
            closeout_for: None,
            budget: None,
            warm: None,
        })
        .collect();
    let gather = NodeSpec {
        id: "gather".into(),
        kind: NodeKindSpec::Gather,
        demands: None,
        inputs: vec![
            typed_port("correctness", contract::REVIEWER_RESULT_V2),
            typed_port("architecture", contract::REVIEWER_RESULT_V2),
        ],
        outputs: vec![typed_port("reports", contract::REPORT_SET_V1)],
        gated_by: None,
        runner: None,
        package: None,
        execution: None,
        slicing: None,
        closeout_for: None,
        budget: None,
        warm: None,
    };
    let ledger = NodeSpec {
        id: "ledger".into(),
        kind: NodeKindSpec::Ledger,
        demands: None,
        inputs: vec![typed_port("reports", contract::REPORT_SET_V1)],
        outputs: vec![
            typed_port("findings", contract::FINDING_SET_V1),
            typed_port("demands", contract::DEMAND_SET_V1),
        ],
        gated_by: None,
        runner: None,
        package: None,
        execution: None,
        slicing: None,
        closeout_for: None,
        budget: None,
        warm: None,
    };

    let mut nodes = vec![gate, generation];
    nodes.extend(reviewers);
    nodes.extend([gather, ledger]);
    let mut edges = Vec::new();
    for reviewer in ["correctness", "architecture"] {
        edges.extend([
            edge("generation", "findings", reviewer, "prior_findings"),
            edge("generation", "change_set", reviewer, "change_set"),
            edge("gate", "decision", reviewer, "gate"),
            edge(reviewer, "result", "gather", reviewer),
        ]);
    }
    edges.push(edge("gather", "reports", "ledger", "reports"));

    Definition {
        version: 3,
        subject: Some(SubjectSpec {
            kind: SubjectKind::Diff,
        }),
        checks,
        check_timeout_seconds: Some(3600),
        gate: Some(GateExecutionSpec {
            provider: SandboxProviderSpec::TrustedLocal,
            required_isolation: IsolationSpec::None,
            mode: GateModeSpec::EphemeralWrite,
            image: None,
            caches: Vec::new(),
            build_caches: Vec::new(),
            build_cache_max_bytes: None,
            build_cache_max_entries: None,
            memory: None,
            cpus: None,
        }),
        nodes,
        edges,
        convergence: ConvergenceSpec {
            clean_rounds: 1,
            max_rounds: 2,
            gate: SeveritySpec::Major,
        },
        budgets: Some(BudgetSpec {
            unit: BudgetUnit::Tokens,
            attempt: 300_000,
            run: 1_000_000,
            fan_out: None,
        }),
        integration: None,
    }
}

fn typed_port(name: &str, artifact_type: &str) -> TypedPortSpec {
    TypedPortSpec {
        name: name.to_string(),
        artifact_type: artifact_type.to_string(),
        cardinality: PortCardinality::One,
        optional: false,
        snapshot_affinity: SnapshotAffinity::SameSubject,
    }
}

fn prior_finding_set_port(name: &str) -> TypedPortSpec {
    TypedPortSpec {
        name: name.to_string(),
        artifact_type: contract::FINDING_SET_V1.to_string(),
        cardinality: PortCardinality::One,
        optional: true,
        snapshot_affinity: SnapshotAffinity::Any,
    }
}

fn edge(from_node: &str, from_port: &str, to_node: &str, to_port: &str) -> EdgeSpec {
    EdgeSpec {
        from: PortSpec {
            node: from_node.to_string(),
            port: from_port.to_string(),
        },
        to: PortSpec {
            node: to_node.to_string(),
            port: to_port.to_string(),
        },
    }
}

fn topology_lines(definition: &Definition) -> Vec<String> {
    let mut lines = definition
        .nodes
        .iter()
        .map(|node| format!("node {} [{:?}]", node.id, node.kind).to_lowercase())
        .collect::<Vec<_>>();
    lines.extend(definition.edges.iter().map(|edge| {
        format!(
            "{}.{} -> {}.{}",
            edge.from.node, edge.from.port, edge.to.node, edge.to.port
        )
    }));
    lines
}

fn validate_budget_arithmetic(definition: &Definition) -> Result<Vec<String>, String> {
    let Some(budget) = definition.budgets else {
        return Ok(Vec::new());
    };
    let workers: Vec<&NodeSpec> = definition
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKindSpec::Reviewer)
        .collect();
    let static_workers = workers.len();
    let model_workers = workers.iter().filter(|node| node.package.is_some()).count();
    // Each Worker's own cap where declared, the pipeline attempt cap elsewhere.
    let caps: Vec<u64> = workers
        .iter()
        .map(|node| {
            node.budget
                .map_or(budget.attempt, |node_budget| node_budget.attempt)
        })
        .collect();
    let reservation = caps.iter().try_fold(0_u64, |sum, cap| {
        sum.checked_add(*cap)
            .ok_or("static Worker budget arithmetic overflow")
    })?;
    let required = reservation
        .checked_add(model_workers as u64)
        .ok_or("static Worker Provider budget arithmetic overflow")?;
    if required > budget.run {
        return Err(format!(
            "run budget {} cannot admit the first Attempt of each of {static_workers} static Workers (requires {required}: {reservation} reserved together plus {model_workers} Provider admission floors)",
            budget.run
        ));
    }
    let largest = caps.iter().copied().max().unwrap_or(budget.attempt);
    let with_one_retry = required
        .checked_add(largest)
        .ok_or("static Worker retry budget arithmetic overflow")?;
    if with_one_retry > budget.run {
        Ok(vec![format!(
            "run budget {} admits the initial {static_workers} Workers but has no headroom for one {largest}-token retry",
            budget.run
        )])
    } else {
        Ok(Vec::new())
    }
}

fn worker_manifest(name: &str, runner: RunnerKind) -> String {
    #[derive(Serialize)]
    struct Manifest<'a> {
        name: &'a str,
        version: &'a str,
        subjects: [&'a str; 1],
        runner: ManifestRunner,
    }

    #[derive(Serialize)]
    struct ManifestRunner {
        program: String,
        args: Vec<ManifestArg>,
    }

    #[derive(Serialize)]
    struct ManifestArg {
        value: String,
    }

    let (model, effort) = runner.binding();
    let values: Vec<String> = match runner {
        RunnerKind::Claude => vec![
            "--model".into(),
            model.into(),
            "--effort".into(),
            effort.into(),
        ],
        // The Codex adapter owns the `exec` invocation, sandbox, output, and stdin flags.
        // Package args are model flags only.
        RunnerKind::Codex => vec![
            "--model".into(),
            model.into(),
            "-c".into(),
            format!("model_reasoning_effort=\"{effort}\""),
        ],
    };
    let manifest = Manifest {
        name,
        version: WORKER_VERSION,
        subjects: ["diff"],
        runner: ManifestRunner {
            program: runner.program().to_string(),
            args: values
                .into_iter()
                .map(|value| ManifestArg { value })
                .collect(),
        },
    };
    toml::to_string_pretty(&manifest).expect("the built-in Worker manifest serializes")
}

fn project_file(project_name: &str) -> String {
    #[derive(Serialize)]
    struct ProjectFile<'a> {
        version: u32,
        project: Project<'a>,
        defaults: Defaults<'a>,
    }

    #[derive(Serialize)]
    struct Project<'a> {
        name: &'a str,
        min_af: &'a str,
    }

    #[derive(Serialize)]
    struct Defaults<'a> {
        pipeline: &'a str,
    }

    toml::to_string_pretty(&ProjectFile {
        version: 1,
        project: Project {
            name: project_name,
            min_af: "0.6",
        },
        defaults: Defaults {
            pipeline: PIPELINE_NAME,
        },
    })
    .expect("the built-in project file serializes")
}

fn guide_file(project_name: &str, profile: RunnerProfile, gates: &[Gate]) -> String {
    let gate_lines = gates
        .iter()
        .map(|gate| format!("- `{}`: `{}`", gate.name, gate.command_line()))
        .collect::<Vec<_>>()
        .join("\n");
    let reviewer_lines = reviewer_summaries(profile)
        .into_iter()
        .map(|reviewer| {
            format!(
                "- `{}` uses package `{}` through {} ({})",
                reviewer.node, reviewer.package, reviewer.runner, reviewer.model
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        r#"# Afactory review authority

This directory is the committed, project-owned review authority for `{project_name}`. It was
generated by `af onboard`; the binary does not own it after creation and never silently rewrites
it. Review every change here like executable policy.

## Quickstart for the owner

1. Run `af onboard` to preview the authority. It uses no model tokens and writes nothing.
2. Review the preview, then run `af onboard --apply`. It uses no model tokens and writes only
   absent files under `.af/`; it does not run repository checks (Gates).
3. Review the generated files and commit them with `git add .af && git commit -m 'Add af review authority'`.
   Git writes the commit; no model tokens are spent. The committed revision is the authority
   `af review plan --policy-rev HEAD` will read.

## What runs

```text
exact Base..head Change Set + prior Findings       required acceptance Gate
                    |                                       |
                    +-------------------+-------------------+
                                        |
                         +--------------+--------------+
                         |                             |
                  correctness                     architecture
                         |                             |
                         +----------- gather ----------+
                                        |
                                      Ledger
```

{reviewer_lines}

The reviewers receive the exact Diff Subject, bounded kernel artifacts, and their own package.
They do not receive one another's transcript. Results meet at the deterministic gather barrier.
The Campaign stops after one clean Round or two Rounds total, with caps of 300,000 tokens per
Attempt and 1,000,000 tokens per Round.

Plain `af review run` is light regardless of that maximum: it permits one closed Round, then tells
the agent to fix Findings and run the deterministic project gate without another Campaign. Only a
human-requested `--heavy` Campaign uses the complete two-Round convergence window. The selected
mode is pinned and must be repeated when resuming the Campaign.

## Worker data authorization

Trusting this configured authority and intentionally running `af review run` authorizes Afactory
to send each configured Worker exactly its declared, bounded inputs for every Attempt and later
Round in that Campaign. Agents should not ask for additional per-Worker, per-Attempt, or per-Round
confirmation. This does not authorize undeclared context, changed Provider bindings, comments,
commits, pushes, pull requests, publication, or other remote side effects.

The generated pipeline explicitly binds its Gate to `trusted_local` with required isolation
`none`. Each Gate receives a disposable `ephemeral-write` clone, so build/scaffold writes cannot
taint reviewer clones, but this is not a security boundary against untrusted project commands.
Before reviewing untrusted code, change the binding to provider `container`, add a project
toolchain image pinned as `name@sha256:<digest>`, require isolation `container`, refresh the lock,
and verify the live container probes.

Required Gate commands (declared as literal trusted argv; onboarding does not execute them):

{gate_lines}

## Agent workflow for an existing pull request

1. Run `af onboard` at the trusted base checkout. It validates the graph and every exact digest.
2. Run `af provider status`; if a Worker's kind has no Provider, `af provider detect` or
   `af provider setup <id> --kind <kind> --login` at your terminal. Choose the machine-local
   Provider ID for each Worker without writing a token into this repository.
3. Fetch the pull request with the normal repository tooling, create a disposable worktree at its
   head, and identify the trusted base revision.
4. From that worktree run `af review plan`. If it refuses, run its returned `next_action`
   exactly as printed; the command supplies explicit trusted selectors and Provider bindings only
   when af can determine them unambiguously. Inspect the resulting plan. Then run the returned
   `af review run` command, adding `--campaign pr-<number>` and `--json` if wanted. For Provider
   admission before execution, run `af provider doctor` with the same explicit selectors,
   bindings, and Campaign name. Finish with:

   ```sh
   af review ledger --campaign pr-<number> --json
   af review show --campaign pr-<number> FINDING_KEY --json
   af review report --campaign pr-<number> --json
   ```

5. Read the ledger, inspect each review result (Finding) by key, and read the report. Do not comment on the pull request, commit, push, merge, or mutate
   external state unless a human explicitly authorizes that separate action.

The command above is light by default. After a finding-bearing result, fix the Findings, run the
project's deterministic Gate command, and stop. Do not open a follow-up review Campaign. Add
`--heavy` only when a human explicitly requests convergence review.

`--policy-rev` must name a trusted committed revision containing this `.af/` directory; `--base`
independently names the revision against which the candidate is compared. The review command
captures both immutably; uncommitted pull-request content cannot alter its Gate, Worker prompts,
models, topology, budgets, or pins.

## Changing authority

Edit the ordinary files under `.af/`, then explicitly run `af onboard --refresh-lock`. That
command re-pins the selected pipeline, every pipeline a route or the oversized policy names, and
the Worker packages they reference; it drops a pin whose `.af/workers/NAME` package or
`.af/pipelines/NAME.toml` file no longer exists. Review the authority and lock diff together,
run `af onboard` again, then commit through the project's normal controls. Never weaken a Gate
or boundary merely to obtain a passing review. The lock also binds a receipted `af` release by
version and archive digest: ordinary commands dispatch to that pinned release. To move it
intentionally, use `af onboard --refresh-lock --af <version>`; review and commit the resulting
lock diff. A source build without an install receipt cannot create a verified release pin and
leaves an existing one unchanged; package digests are still checked. A source build of the
pinned version runs as built: with no receipt, it has no bytes to compare with the pinned
digest.

## Agents without a terminal

An agent cannot complete a Provider login: `af provider setup --login` runs only at a human's
interactive terminal, because the OAuth URL and code it prints are credentials in transit. For
missing or broken auth, an explicitly authorized private host can use `af provider auth` (see
docs/provider-auth-host.md in the af repository) and send the requesting human only the official
browser handoff. Login completion is unverified: do not claim paid admission or Task resumption
from status.
"#
    )
}

fn reviewer_summaries(profile: RunnerProfile) -> Vec<ReviewerSummary> {
    ["correctness", "architecture"]
        .into_iter()
        .map(|name| {
            let runner = profile.runner(name);
            ReviewerSummary {
                node: name.to_string(),
                package: name.to_string(),
                runner: runner.program().to_string(),
                model: runner.model().to_string(),
            }
        })
        .collect()
}

fn validate_bundle(bundle: &Bundle) -> Result<(), String> {
    let lock_text = bundle_text(&bundle.files, ".af/af.lock")?;
    let pipeline_text = bundle_text(&bundle.files, ".af/pipelines/review.toml")?;
    let project_text = bundle_text(&bundle.files, ".af/af.toml")?;
    let selected = selected_pipeline(project_text)?;
    if selected != PIPELINE_NAME {
        return Err(format!(
            "generated project selected pipeline `{selected}` instead of `{PIPELINE_NAME}`"
        ));
    }
    let lock = Lockfile::from_toml(lock_text).map_err(|error| error.to_string())?;
    validate_pipeline_pin(&lock, PIPELINE_NAME, pipeline_text.as_bytes())?;
    let registry = Registry::captured(bundle.worker_files.clone());
    let definition = Definition::from_toml(pipeline_text).map_err(|error| error.to_string())?;
    validate_budget_arithmetic(&definition)?;
    let loaded = definition
        .load_with(&lock, &registry)
        .map_err(|error| error.to_string())?;
    if loaded.packages().len() != 2 || loaded.checks().is_empty() {
        return Err("generated authority did not bind two Workers and at least one Gate".into());
    }
    Ok(())
}

fn bundle_text<'a>(files: &'a BTreeMap<String, Vec<u8>>, path: &str) -> Result<&'a str, String> {
    std::str::from_utf8(
        files
            .get(path)
            .ok_or_else(|| format!("generated bundle omitted `{path}`"))?,
    )
    .map_err(|error| format!("generated `{path}` is not UTF-8: {error}"))
}

fn apply_bundle(repo: &Path, bundle: &Bundle) -> Result<(), String> {
    let temporary = tempfile::Builder::new()
        .prefix(".af-onboard-")
        .tempdir_in(repo)
        .map_err(|error| format!("staging review authority: {error}"))?;
    for (path, bytes) in &bundle.files {
        let relative = Path::new(path)
            .strip_prefix(".af")
            .map_err(|_| format!("generated path `{path}` does not live under `.af/`"))?;
        let target = temporary.path().join(relative);
        let parent = target
            .parent()
            .ok_or_else(|| format!("generated path `{path}` has no parent"))?;
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|error| format!("creating {}: {error}", target.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("writing {}: {error}", target.display()))?;
        file.sync_all()
            .map_err(|error| format!("syncing {}: {error}", target.display()))?;
    }
    std::fs::File::open(temporary.path())
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("syncing staged review authority: {error}"))?;
    let staged = temporary.keep();
    let authority = repo.join(".af");
    match rustix::fs::renameat_with(
        rustix::fs::CWD,
        &staged,
        rustix::fs::CWD,
        &authority,
        rustix::fs::RenameFlags::NOREPLACE,
    ) {
        Ok(()) => {
            std::fs::File::open(repo)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("syncing repository after onboarding: {error}"))?;
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staged);
            if authority.exists() {
                Err("`.af/` appeared during onboarding; no authority was overwritten".into())
            } else {
                Err(format!("installing review authority atomically: {error}"))
            }
        }
    }
}

fn created_next_steps() -> Vec<String> {
    vec![
        "Review and commit the generated `.af/` authority on the trusted base branch.".into(),
        "Run `af onboard` again; it must validate every selected pipeline and Worker pin.".into(),
        REGISTER_A_PROVIDER.into(),
        "Run `af provider status`, then follow `.af/README.md` to review a pull request.".into(),
    ]
}

/// What a providers line says when no Provider of the Worker's kind is registered.
const NONE_REGISTERED: &str = "none registered";
/// Between what a providers line found and what to run about it.
const PROVIDER_ARROW: &str = "   ← ";
const REGISTER_A_PROVIDER: &str =
    "Register a Provider if the providers block says none: af provider detect";

/// The runner profile of a new bundle and the `profile` line's reason for it: `--runner` when
/// given, else the only Provider kind registered on this machine, else `mixed`.
fn pick_runner(
    explicit: Option<RunnerProfile>,
    registry: &crate::providers::RegistryRead,
) -> (RunnerProfile, String) {
    if let Some(profile) = explicit {
        return (profile, format!("runner {} (--runner)", profile.name()));
    }
    let claude = !registry.ids_of("claude").is_empty();
    let codex = !registry.ids_of("codex").is_empty();
    let (profile, why) = match (claude, codex) {
        (true, false) => (RunnerProfile::Claude, "the only Provider kind registered"),
        (false, true) => (RunnerProfile::Codex, "the only Provider kind registered"),
        (true, true) => (RunnerProfile::Mixed, "both Provider kinds registered"),
        (false, false) => (RunnerProfile::Mixed, "no Provider registered yet"),
    };
    (profile, format!("runner {} ({why})", profile.name()))
}

/// The `--provider NODE=ID` bindings of a new bundle's first review, as the preview selects them:
/// the runner profile `pick_runner` chooses without `--runner`, and for each reviewer the first
/// registered Provider of its kind. `None` when the registry cannot be read or a reviewer's kind
/// has no Provider registered, so no binding can be named. `af help start` prints these.
pub(crate) fn first_review_bindings(
    registry: &crate::providers::RegistryRead,
) -> Option<Vec<(&'static str, String)>> {
    if registry.warning.is_some() {
        return None;
    }
    let (profile, _) = pick_runner(None, registry);
    ["correctness", "architecture"]
        .into_iter()
        .map(|node| {
            let kind = profile.runner(node).program();
            registry
                .ids_of(kind)
                .first()
                .map(|id| (node, (*id).to_string()))
        })
        .collect()
}

/// The Provider kind a model Worker's runner program needs; `None` for a command Worker.
fn provider_kind(runner: &str) -> Option<&'static str> {
    match runner {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        _ => None,
    }
}

/// The `providers` block: for every model Worker, the Provider kind it needs, the registered
/// Providers of that kind and the `--provider` binding to pass, or how to register one. It
/// reads the registry only; `probe` adds each Provider's login state from the token-free status
/// probe `af provider status` runs.
fn providers_block(
    reviewers: &[ReviewerSummary],
    registry: &crate::providers::RegistryRead,
    probe: bool,
) -> Vec<String> {
    let probed = if probe && !registry.registered.is_empty() {
        crate::providers::probed_statuses()
    } else {
        Vec::new()
    };
    let state = |id: &str| -> String {
        if !probe {
            return "--probe checks its login".to_string();
        }
        probed
            .iter()
            .find(|status| status.id == id)
            .map_or_else(|| "not probed".to_string(), |status| status.status.clone())
    };
    let mut lines = Vec::new();
    if let Some(warning) = &registry.warning {
        lines.push(format!(
            "the Provider registry cannot be read: {warning}{PROVIDER_ARROW}af provider status"
        ));
    }
    // The same names-only scan `af provider status` and `af doctor` list (ADR-0206).
    if !registry.unregistered.is_empty() {
        let found: Vec<String> = registry
            .unregistered
            .iter()
            .map(|dir| crate::providers::detect::display_path(dir))
            .collect();
        lines.push(format!(
            "found but not registered: {} (not probed){PROVIDER_ARROW}af provider detect",
            found.join(", ")
        ));
    }
    let taken: BTreeSet<String> = registry
        .registered
        .iter()
        .map(|entry| entry.id.clone())
        .collect();
    for reviewer in reviewers {
        let Some(kind) = provider_kind(&reviewer.runner) else {
            continue;
        };
        let node = &reviewer.node;
        let ids = registry.ids_of(kind);
        match ids.first() {
            None => {
                let id = crate::providers::available_provider_id(kind, &taken);
                lines.push(format!(
                    "{node} needs a {kind} Provider: {NONE_REGISTERED}{PROVIDER_ARROW}af provider detect, or af provider setup {id} --kind {kind} --login"
                ));
            }
            Some(first) => {
                let listed = ids
                    .iter()
                    .map(|id| format!("{id} ({})", state(id)))
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!(
                    "{node} needs a {kind} Provider: {listed}{PROVIDER_ARROW}bind with --provider {node}={first}"
                ));
            }
        }
    }
    lines
}

/// What to do once existing authority validates: the token-free plan of an uncommitted change,
/// its `--provider` bindings filled with the registered IDs where they exist.
fn validated_next_steps(
    reviewers: &[ReviewerSummary],
    registry: &crate::providers::RegistryRead,
) -> Vec<String> {
    let mut plan = "af review plan --policy-rev HEAD --base HEAD --uncommitted".to_string();
    let mut missing = false;
    for reviewer in reviewers {
        let Some(kind) = provider_kind(&reviewer.runner) else {
            continue;
        };
        let id = match registry.ids_of(kind).first() {
            Some(id) => (*id).to_string(),
            None => {
                missing = true;
                format!("<{kind}-provider-id>")
            }
        };
        plan.push_str(&format!(" --provider {}={id}", reviewer.node));
    }
    let mut steps =
        vec!["Run `af provider status`; Provider credentials remain machine-local.".to_string()];
    if missing {
        steps.push(REGISTER_A_PROVIDER.into());
    }
    steps.extend([
        format!("Make a change, then plan its review token-free: `{plan}`."),
        "Follow `.af/README.md` to capture a pull request in a disposable worktree.".into(),
        "Run `af review ledger` and `af review report`; publish nothing without human authorization."
            .into(),
    ]);
    steps
}

struct ExistingAuthority {
    project: crate::project::ProjectFile,
    selected: String,
    pipeline_path: PathBuf,
    pipeline_text: String,
    definition: Definition,
    lock: Lockfile,
}

fn parse_existing(repo: &Path) -> Result<ExistingAuthority, String> {
    let project_path = repo.join(".af/af.toml");
    let project_text = read_authority_text(&project_path)?;
    let project = crate::project::ProjectFile::parse(&project_text)?;
    let selected = project.review_pipeline().to_string();
    let pipeline_path = repo.join(format!(".af/pipelines/{selected}.toml"));
    let pipeline_text = read_authority_text(&pipeline_path)?;
    let definition = Definition::from_toml(&pipeline_text).map_err(|error| error.to_string())?;
    let lock_path = repo.join(".af/af.lock");
    let lock = Lockfile::from_toml(&read_authority_text(&lock_path)?)
        .map_err(|error| error.to_string())?;
    Ok(ExistingAuthority {
        project,
        selected,
        pipeline_path,
        pipeline_text,
        definition,
        lock,
    })
}

/// Where onboarding's reader stops on a committed `.af/`: the file, the line its TOML error
/// names (syntax or type), and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorityStop {
    pub(crate) file: PathBuf,
    pub(crate) line: Option<usize>,
    pub(crate) message: String,
    /// The reader stopped because `file` no longer matches the pin `.af/af.lock` holds for it
    /// (or the lock pins what is not there): restoring it, or re-pinning it with `af onboard
    /// --refresh-lock` when it changed on purpose, is what clears it.
    pub(crate) repin: bool,
}

/// Where [`inspect_existing`] stops on `repo`'s `.af/`, found by taking its own steps in its own
/// order: the project file, the pipeline it selects and the lock, each parsed as its type; the
/// lock's pins; the selected pipeline's pin; its Worker packages through the one
/// [`Registry`] reader, each package's pin verified before its `reviewer.toml` is parsed as a
/// typed manifest; then each other pipeline a route or the oversized policy can select. The
/// stop names the file the failing step read, with the line a TOML error (syntax or type)
/// names, and only ever a path below `.af/`: a package is found by its validated name, never by
/// the pipeline's string as a path. `None` when the reader does not stop, or stops on a file it
/// cannot name.
pub(crate) fn authority_stop(repo: &Path) -> Option<AuthorityStop> {
    let af = repo.join(".af");
    let stop = authority_steps(repo, &af).err()?;
    // Nothing outside `.af/` is ever named.
    let inside = stop.file.starts_with(&af)
        && !stop
            .file
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir));
    inside.then_some(stop)
}

/// The steps of [`authority_stop`]: `Err` with the first one that stops.
fn authority_steps(repo: &Path, af: &Path) -> Result<(), AuthorityStop> {
    let stop = |file: &Path, line, message: &str, repin| AuthorityStop {
        file: file.to_path_buf(),
        line,
        message: message.lines().next().unwrap_or_default().trim().to_owned(),
        repin,
    };
    // The line a TOML error names, read by the type's own deserializer, so a typed error
    // (`version = "one"`) is located as well as a syntax error.
    type Located = Option<(Option<usize>, String)>;
    fn located<T: serde::de::DeserializeOwned>(text: &str) -> Located {
        let error = toml::from_str::<T>(text).err()?;
        let start = error.span().map(|span| span.start.min(text.len()));
        let line = start.map(|start| text[..start].matches('\n').count() + 1);
        Some((line, error.message().to_owned()))
    }
    let typed = |path: &Path,
                 parse: &dyn Fn(&str) -> Result<(), String>,
                 locate: &dyn Fn(&str) -> Located|
     -> Result<String, AuthorityStop> {
        let text =
            read_authority_text(path).map_err(|message| stop(path, None, &message, false))?;
        parse(&text).map_err(|message| match locate(&text) {
            Some((line, located)) => stop(path, line, &located, false),
            None => stop(path, None, &message, false),
        })?;
        Ok(text)
    };
    let project_path = af.join("af.toml");
    let project_text = typed(
        &project_path,
        &|text| crate::project::ProjectFile::parse(text).map(drop),
        &located::<crate::project::ProjectFile>,
    )?;
    let project = crate::project::ProjectFile::parse(&project_text)
        .map_err(|message| stop(&project_path, None, &message, false))?;
    let selected = project.review_pipeline().to_string();
    let pipeline_path = repo.join(crate::project::pipeline_path_for(&selected));
    let pipeline = |path: &Path| {
        typed(
            path,
            &|text| {
                Definition::from_toml(text)
                    .map(drop)
                    .map_err(|e| e.to_string())
            },
            &located::<Definition>,
        )
    };
    let pipeline_text = pipeline(&pipeline_path)?;
    let lock_path = af.join("af.lock");
    let lock_text = typed(
        &lock_path,
        &|text| {
            Lockfile::from_toml(text)
                .map(drop)
                .map_err(|e| e.to_string())
        },
        &located::<Lockfile>,
    )?;
    let lock = Lockfile::from_toml(&lock_text)
        .map_err(|error| stop(&lock_path, None, &error.to_string(), false))?;
    validate_no_stale_pins(repo, &lock)
        .map_err(|message| stop(&lock_path, None, &message, true))?;
    crate::project::check_lock_af_version(&lock, ".af/af.lock")
        .map_err(|message| stop(&lock_path, None, &message, true))?;
    let registry = Registry::new(af.join("workers"));
    let load = |path: &Path, name: &str, text: &str| -> Result<(), AuthorityStop> {
        validate_pipeline_pin(&lock, name, text.as_bytes())
            .map_err(|message| stop(path, None, &message, true))?;
        let definition = Definition::from_toml(text)
            .map_err(|error| stop(path, None, &error.to_string(), false))?;
        let packages: Vec<String> = definition
            .nodes
            .iter()
            .filter_map(|node| node.package.clone())
            .collect();
        match definition.load_with(&lock, &registry) {
            Ok(_) => Ok(()),
            Err(review_config::ConfigError::Lock(error)) => {
                Err(package_stop(af, path, &packages, &error))
            }
            Err(error) => Err(stop(path, None, &error.to_string(), false)),
        }
    };
    load(&pipeline_path, &selected, &pipeline_text)?;
    for candidate in project.pipeline_candidates() {
        if candidate == selected {
            continue;
        }
        let path = repo.join(crate::project::pipeline_path_for(&candidate));
        // The reader checks a route target's pin before it parses it.
        let text =
            read_authority_text(&path).map_err(|message| stop(&path, None, &message, false))?;
        validate_pipeline_pin(&lock, &candidate, text.as_bytes())
            .map_err(|message| stop(&path, None, &message, true))?;
        pipeline(&path)?;
        load(&path, &candidate, &text)?;
    }
    Ok(())
}

/// The file a Worker package refusal of [`Lockfile`] resolution names, below `.af/workers/`
/// only: the package by its validated name, a file of it by the path the [`Registry`] reader
/// refused, or `pipeline` for a package name no registry holds. A package that no longer
/// matches its pin names its directory, or its manifest with the line when that does not parse
/// either, so the fix names both steps.
fn package_stop(
    af: &Path,
    pipeline: &Path,
    packages: &[String],
    error: &review_config::lock::LockError,
) -> AuthorityStop {
    use review_config::lock::LockError;
    let message = error.to_string();
    let stop = |file: PathBuf, line, repin| AuthorityStop {
        file,
        line,
        message: message.lines().next().unwrap_or_default().trim().to_owned(),
        repin,
    };
    // Only a name the reader's own `locate` accepts is joined to the registry root.
    let package = |name: &str| {
        (!name.is_empty()
            && name.bytes().enumerate().all(|(index, byte)| {
                byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'-' | b'_'))
            }))
        .then(|| af.join("workers").join(name))
    };
    let manifest_line = |root: &Path| {
        let path = root.join("reviewer.toml");
        let text = read_authority_text(&path).ok()?;
        let error = toml::from_str::<review_config::lock::PackageManifest>(&text).err()?;
        let start = error.span().map(|span| span.start.min(text.len()));
        let line = start.map(|start| text[..start].matches('\n').count() + 1);
        Some((path, line, error.message().to_owned()))
    };
    match error {
        LockError::DigestMismatch { name, .. } => match package(name) {
            Some(root) => match manifest_line(&root) {
                // Changed and malformed: the manifest's own error first, then the pin.
                Some((file, line, parse)) => AuthorityStop {
                    message: parse,
                    ..stop(file, line, true)
                },
                None => stop(root, None, true),
            },
            None => stop(pipeline.to_path_buf(), None, false),
        },
        // The pipeline names a package the lock does not pin: a name to correct there (one
        // that is no package name at all included), or a package to pin.
        LockError::NotLocked { .. } => stop(pipeline.to_path_buf(), None, false),
        LockError::VersionMismatch { .. } => stop(af.join("af.lock"), None, true),
        LockError::MalformedDigest { .. } | LockError::Floating { .. } => {
            stop(af.join("af.lock"), None, false)
        }
        LockError::InvalidName { .. } => stop(pipeline.to_path_buf(), None, false),
        LockError::NotFound { name, .. } => stop(
            package(name).unwrap_or_else(|| pipeline.to_path_buf()),
            None,
            false,
        ),
        LockError::Symlink { name, path }
        | LockError::UnsupportedFileType { name, path }
        | LockError::UnsupportedPath { name, path } => {
            let root = package(name);
            match root {
                Some(root) if path.starts_with(&root) => stop(path.clone(), None, false),
                Some(root) => stop(root, None, false),
                None => stop(pipeline.to_path_buf(), None, false),
            }
        }
        LockError::MissingManifest { name, .. } | LockError::UnsupportedSubject { name, .. } => {
            match package(name) {
                Some(root) => stop(root.join("reviewer.toml"), None, false),
                None => stop(pipeline.to_path_buf(), None, false),
            }
        }
        LockError::NameMismatch { requested, .. } => match package(requested) {
            Some(root) => stop(root.join("reviewer.toml"), None, false),
            None => stop(pipeline.to_path_buf(), None, false),
        },
        // A manifest the typed reader refused: the first package, in the pipeline's order,
        // whose `reviewer.toml` the message names, at the line its typed parse names.
        LockError::Parse(_) => {
            let named = packages
                .iter()
                .find(|name| message.contains(&format!("reviewer.toml for `{name}`")));
            match named.and_then(|name| package(name)) {
                Some(root) => match manifest_line(&root) {
                    Some((file, line, parse)) => AuthorityStop {
                        message: parse,
                        ..stop(file, line, false)
                    },
                    None => stop(root.join("reviewer.toml"), None, false),
                },
                None => stop(pipeline.to_path_buf(), None, false),
            }
        }
        // A package the bounded walk refused, too large or unreadable: onboarding stopped
        // reading that package, which editing the pipeline cannot change.
        LockError::TooLarge { name, at, .. } => match package(name) {
            Some(root)
                if Path::new(at)
                    .components()
                    .all(|part| matches!(part, std::path::Component::Normal(_))) =>
            {
                stop(root.join(at), None, false)
            }
            Some(root) => stop(root, None, false),
            None => stop(pipeline.to_path_buf(), None, false),
        },
        LockError::Io { path, .. } if path.starts_with(af.join("workers")) => {
            stop(path.clone(), None, false)
        }
        _ => stop(pipeline.to_path_buf(), None, false),
    }
}

fn selected_pipeline(text: &str) -> Result<String, String> {
    Ok(crate::project::ProjectFile::parse(text)?
        .review_pipeline()
        .to_string())
}

fn read_authority_text(path: &Path) -> Result<String, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("reading authority {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "authority {} must be a regular file, not a symlink or special file",
            path.display()
        ));
    }
    if metadata.len() > MAX_DISCOVERY_FILE_BYTES {
        return Err(format!(
            "authority {} is {} bytes; onboarding reads at most {} bytes",
            path.display(),
            metadata.len(),
            MAX_DISCOVERY_FILE_BYTES
        ));
    }
    let bytes = review_store::local_fs::read_bounded(path, MAX_DISCOVERY_FILE_BYTES)
        .map_err(|error| format!("reading authority {}: {error}", path.display()))?;
    String::from_utf8(bytes)
        .map_err(|error| format!("reading authority {}: {error}", path.display()))
}

fn validate_pipeline_pin(lock: &Lockfile, name: &str, bytes: &[u8]) -> Result<(), String> {
    let pin = lock
        .pipelines
        .get(name)
        .ok_or_else(|| format!("pipeline `{name}` is not pinned in `.af/af.lock`"))?;
    let found = review_store::canonical::blob_content_id(bytes);
    if pin.digest != found {
        return Err(format!(
            "pipeline `{name}` does not match `.af/af.lock`: locked {}, found {found}",
            pin.digest
        ));
    }
    Ok(())
}

fn inspect_existing(repo: &Path, status: &str) -> Result<Report, String> {
    let authority = parse_existing(repo)?;
    validate_no_stale_pins(repo, &authority.lock)?;
    let lock_note = crate::project::check_lock_af_version(&authority.lock, ".af/af.lock")?;
    validate_pipeline_pin(
        &authority.lock,
        &authority.selected,
        authority.pipeline_text.as_bytes(),
    )?;
    let registry = Registry::new(repo.join(".af/workers"));
    let loaded = authority
        .definition
        .clone()
        .load_with(&authority.lock, &registry)
        .map_err(|error| error.to_string())?;
    // Every pipeline a route or the oversized policy can select is authority too: present,
    // pinned, and loadable, or the route would fail at the first matching change.
    for candidate in authority.project.pipeline_candidates() {
        if candidate == authority.selected {
            continue;
        }
        let path = repo.join(crate::project::pipeline_path_for(&candidate));
        let text = read_authority_text(&path)
            .map_err(|error| format!("route target `{candidate}`: {error}"))?;
        validate_pipeline_pin(&authority.lock, &candidate, text.as_bytes())?;
        Definition::from_toml(&text)
            .map_err(|error| format!("route target `{candidate}`: {error}"))?
            .load_with(&authority.lock, &registry)
            .map_err(|error| format!("route target `{candidate}`: {error}"))?;
    }

    let gates = authority
        .definition
        .checks
        .iter()
        .map(|check| Gate {
            name: check.name.clone(),
            program: check.command.program.clone(),
            args: check
                .command
                .args
                .iter()
                .map(|argument| argument.value.clone())
                .collect(),
            source: "project authority".into(),
        })
        .collect();
    let reviewers = loaded
        .packages()
        .iter()
        .map(|(node, package)| ReviewerSummary {
            node: node.clone(),
            package: package.name.clone(),
            runner: package.runner.program.clone(),
            model: runner_model(&package.runner),
        })
        .collect();
    let (attempt_tokens, run_tokens) = authority
        .definition
        .budgets
        .as_ref()
        .map(|budgets| (Some(budgets.attempt), Some(budgets.run)))
        .unwrap_or((None, None));
    let mut warnings = validate_budget_arithmetic(&authority.definition)?;
    warnings.extend(lock_note);
    let mut files = vec![
        ".af/af.lock".to_string(),
        ".af/af.toml".to_string(),
        authority
            .pipeline_path
            .strip_prefix(repo)
            .unwrap_or(&authority.pipeline_path)
            .display()
            .to_string(),
    ];
    if repo.join(".af/README.md").is_file() {
        files.push(".af/README.md".to_string());
    }
    for package in loaded.packages().values() {
        for path in package.files().keys() {
            files.push(format!(".af/workers/{}/{path}", package.name));
        }
    }
    files.sort();
    files.dedup();
    Ok(Report {
        status: status.to_string(),
        profile: "project-owned authority".into(),
        repository: repo.display().to_string(),
        pipeline: format!(".af/pipelines/{}.toml", authority.selected),
        reviewers,
        gates,
        attempt_tokens,
        run_tokens,
        clean_rounds: authority.definition.convergence.clean_rounds,
        max_rounds: authority.definition.convergence.max_rounds,
        topology: topology_lines(&authority.definition),
        warnings,
        files,
        lock_af_version: authority.lock.af_version().map(str::to_string),
        lock_af_targets: pin_targets(authority.lock.af.as_ref()),
        runner_note: None,
        providers: Vec::new(),
        // `execute` fills in the steps that name this machine's Providers.
        next_steps: Vec::new(),
    })
}

/// A repository's committed review authority as `af doctor` reads it, validated exactly as
/// plain `af onboard` validates it; reading it writes nothing.
pub(crate) struct AuthorityFacts {
    /// Each Gate check: its name, program and arguments.
    pub(crate) gates: Vec<(String, String, Vec<String>)>,
    /// The Gate runs in a container.
    pub(crate) container: bool,
    /// Each Worker node and the runner program it runs: `claude`, `codex` or a command.
    pub(crate) workers: Vec<(String, String)>,
}

pub(crate) fn authority_facts(repo: &Path) -> Result<AuthorityFacts, String> {
    let report = inspect_existing(repo, "onboarded")?;
    let container = parse_existing(repo)?
        .definition
        .gate
        .is_some_and(|gate| gate.provider == SandboxProviderSpec::Container);
    Ok(AuthorityFacts {
        gates: report
            .gates
            .into_iter()
            .map(|gate| (gate.name, gate.program, gate.args))
            .collect(),
        container,
        workers: report
            .reviewers
            .into_iter()
            .map(|reviewer| (reviewer.node, reviewer.runner))
            .collect(),
    })
}

fn runner_model(command: &review_core::Command) -> String {
    let values = command
        .args
        .iter()
        .map(|argument| argument.value.as_str())
        .collect::<Vec<_>>();
    let model = values
        .windows(2)
        .find_map(|pair| matches!(pair[0], "--model" | "-m").then_some(pair[1]));
    let effort = values
        .windows(2)
        .find_map(|pair| (pair[0] == "--effort").then_some(pair[1]));
    let codex_effort = values.windows(2).find_map(|pair| {
        (pair[0] == "-c")
            .then(|| pair[1].strip_prefix("model_reasoning_effort="))
            .flatten()
            .map(|value| value.trim_matches(['\'', '"']))
    });
    match (model, effort.or(codex_effort)) {
        (Some(model), Some(effort)) => format!("{model} ({effort} effort)"),
        (Some(model), None) => model.to_string(),
        (None, _) => "machine-configured model".into(),
    }
}

fn refresh_lock(repo: &Path) -> Result<Report, String> {
    let authority = parse_existing(repo)?;
    let mut candidates: Vec<(String, String)> =
        vec![(authority.selected.clone(), authority.pipeline_text.clone())];
    for candidate in authority.project.pipeline_candidates() {
        if candidate == authority.selected {
            continue;
        }
        let path = repo.join(crate::project::pipeline_path_for(&candidate));
        let text = read_authority_text(&path)
            .map_err(|error| format!("route target `{candidate}`: {error}"))?;
        candidates.push((candidate, text));
    }
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    for (name, text) in &candidates {
        let definition =
            Definition::from_toml(text).map_err(|error| format!("pipeline `{name}`: {error}"))?;
        referenced.extend(
            definition
                .nodes
                .iter()
                .filter_map(|node| node.package.clone()),
        );
    }
    if referenced.is_empty() {
        return Err("selected pipeline references no Worker package".into());
    }
    let registry = Registry::new(repo.join(".af/workers"));
    let mut refreshed = authority.lock.clone();
    // Only a released, receipted binary re-pins; a source build keeps whatever pin is there.
    let (af_pin, mut pin_warnings) = running_af_pin()?;
    match af_pin {
        Some(pin) => refreshed.af = Some(pin),
        None => {
            if let Some(kept) = refreshed.af_version() {
                pin_warnings.push(format!("kept the existing pin af {kept}"));
            }
        }
    }
    refreshed
        .workers
        .retain(|name, _| repo.join(".af/workers").join(name).is_dir());
    refreshed
        .pipelines
        .retain(|name, _| repo.join(format!(".af/pipelines/{name}.toml")).is_file());
    for name in referenced {
        let pin = Lockfile::pin(&name, &registry).map_err(|error| error.to_string())?;
        refreshed.workers.insert(name, pin);
    }
    for (name, text) in &candidates {
        let version = refreshed
            .pipelines
            .get(name)
            .map(|pin| pin.version.clone())
            .unwrap_or_else(|| PIPELINE_VERSION.to_string());
        refreshed.pipelines.insert(
            name.clone(),
            Pin {
                version,
                digest: review_store::canonical::blob_content_id(text.as_bytes()),
            },
        );
    }
    for (name, text) in &candidates {
        Definition::from_toml(text)
            .map_err(|error| format!("pipeline `{name}`: {error}"))?
            .load_with(&refreshed, &registry)
            .map_err(|error| format!("pipeline `{name}`: {error}"))?;
    }
    atomic_replace_authority(&repo.join(".af/af.lock"), refreshed.to_toml().as_bytes())?;
    let mut report = inspect_existing(repo, "lock_refreshed")?;
    report.warnings.extend(pin_warnings);
    report.next_steps = validated_next_steps(
        &report.reviewers,
        &crate::providers::read_registry_entries(),
    );
    Ok(report)
}

fn validate_no_stale_pins(repo: &Path, lock: &Lockfile) -> Result<(), String> {
    for name in lock.workers.keys() {
        if !repo.join(".af/workers").join(name).is_dir() {
            return Err(format!(
                "stale Worker pin `{name}` has no `.af/workers/{name}` package; run `af onboard --refresh-lock`"
            ));
        }
    }
    for name in lock.pipelines.keys() {
        if !repo.join(format!(".af/pipelines/{name}.toml")).is_file() {
            return Err(format!(
                "stale pipeline pin `{name}` has no `.af/pipelines/{name}.toml`; run `af onboard --refresh-lock`"
            ));
        }
    }
    Ok(())
}

fn atomic_replace_authority(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("inspecting {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "refusing to replace authority file {} because it is not a regular file",
            path.display()
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| format!("authority file {} has no parent", path.display()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("staging authority file: {error}"))?;
    temporary
        .as_file()
        .set_permissions(metadata.permissions())
        .map_err(|error| format!("preserving authority file permissions: {error}"))?;
    temporary
        .write_all(bytes)
        .map_err(|error| format!("writing staged authority file: {error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("syncing staged authority file: {error}"))?;
    temporary
        .persist(path)
        .map_err(|error| format!("replacing authority file atomically: {}", error.error))?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("syncing authority directory: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        RunnerKind, RunnerProfile, build_definition, runner_model, validate_budget_arithmetic,
        worker_manifest,
    };
    use review_core::{Arg, Command};

    #[test]
    fn model_summary_understands_codex_short_flag() {
        let command = Command::new(
            "codex",
            vec![
                Arg::literal("-m"),
                Arg::literal("gpt-5.6-terra"),
                Arg::literal("-c"),
                Arg::literal("model_reasoning_effort=\"xhigh\""),
            ],
        );
        assert_eq!(runner_model(&command), "gpt-5.6-terra (xhigh effort)");
    }

    #[test]
    fn generated_workers_pin_current_default_models() {
        let claude = worker_manifest("correctness", RunnerKind::Claude);
        assert!(claude.contains("value = \"claude-opus-5-5\""), "{claude}");
        assert!(claude.contains("value = \"high\""), "{claude}");

        let codex = worker_manifest("architecture", RunnerKind::Codex);
        assert!(codex.contains("value = \"gpt-6-sol\""), "{codex}");
        assert!(
            codex.contains("value = 'model_reasoning_effort=\"high\"'"),
            "{codex}"
        );
    }

    /// Every `--runner` profile's generated Workers pass the one validation authority's static
    /// rules, and the package argument parser of their adapter accepts them (ADR-0148).
    #[test]
    fn every_runner_profile_binds_a_spelling_the_validation_authority_accepts() {
        for profile in [
            RunnerProfile::Mixed,
            RunnerProfile::Claude,
            RunnerProfile::Codex,
        ] {
            for name in ["correctness", "architecture"] {
                let runner = profile.runner(name);
                assert_eq!(runner.check_models(), Ok(()), "{profile:?} {name}");
                let manifest: review_config::lock::PackageManifest =
                    toml::from_str(&worker_manifest(name, runner)).unwrap();
                let settings =
                    review_config::lock::reviewer_runner_settings_from_manifest(&manifest).unwrap();
                assert_eq!(
                    (settings.model.as_str(), settings.effort.as_str()),
                    runner.binding()
                );
            }
        }
    }

    #[test]
    fn budget_must_admit_every_static_worker_once() {
        let mut definition = build_definition(&[]);
        definition.budgets.as_mut().unwrap().run = 600_000;
        let error = validate_budget_arithmetic(&definition).unwrap_err();
        assert!(error.contains("requires 600002"), "{error}");
    }

    /// #331 UX close-out: a selected Worker package the bounded reader refuses, too large or
    /// unreadable, stops onboarding at that package, so doctor names the package's file, never
    /// the pipeline whose correction could not clear it; an unsafe `at` names the package root.
    #[test]
    fn a_package_too_large_or_unreadable_is_named_at_the_package() {
        use review_config::lock::LockError;
        let af = std::path::Path::new("/r/.af");
        let pipeline = af.join("pipelines/review.toml");
        let packages = vec!["bugs".to_owned()];
        let too_large = |at: &str| LockError::TooLarge {
            name: "bugs".into(),
            root: af.join("workers/bugs"),
            at: at.into(),
            files: 1,
            bytes: 1,
            file_bytes: 1,
        };
        let stop = super::package_stop(af, &pipeline, &packages, &too_large("prompts/big.md"));
        assert_eq!(stop.file, af.join("workers/bugs/prompts/big.md"));
        let stop = super::package_stop(af, &pipeline, &packages, &too_large("../escape"));
        assert_eq!(stop.file, af.join("workers/bugs"));
        let unreadable = LockError::Io {
            path: af.join("workers/bugs/reviewer.toml"),
            error: "Permission denied".into(),
        };
        let stop = super::package_stop(af, &pipeline, &packages, &unreadable);
        assert_eq!(stop.file, af.join("workers/bugs/reviewer.toml"));
        // An I/O failure outside the packages is the pipeline's, as before.
        let elsewhere = LockError::Io {
            path: std::path::PathBuf::from("/elsewhere"),
            error: "Permission denied".into(),
        };
        assert_eq!(
            super::package_stop(af, &pipeline, &packages, &elsewhere).file,
            pipeline
        );
    }
}
