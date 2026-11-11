//! Deterministic, token-free review-authority onboarding.
//!
//! The binary owns one small scaffold so an agent can discover and reproduce a supported setup
//! without pasting a prompt or hand-writing a digest. Once emitted, every byte is ordinary
//! project-owned authority: `af onboard` validates it, but never silently overwrites it.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use af_pipeline_config::lock::{AfPin, Lockfile};
use serde::Serialize;

mod tasks;

const MAX_DISCOVERY_FILE_BYTES: u64 = 1024 * 1024;
const PROFILE: &str = "native-review@1";
const PIPELINE_NAME: &str = "onboard/review-code";
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

    /// The exact model ID and effort level the generated Worker binds.
    fn binding(self) -> (&'static str, &'static str) {
        match self {
            Self::Claude => ("claude-opus-5-5", "high"),
            Self::Codex => ("gpt-6.1-terra", "high"),
        }
    }

    /// The generated binding passes the static rules of the one validation authority: no
    /// alias, a versioned exact ID and an effort level the Provider kind accepts (ADR-0148).
    fn check_models(self) -> Result<(), String> {
        use af_runner::models::{Kind, ProviderRef, check_static};
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
/// the ones that wrote it. A nightly pins nothing either: a nightly is never pinned (ADR-0267).
fn running_af_pin() -> Result<(Option<AfPin>, Vec<String>), String> {
    let version = env!("CARGO_PKG_VERSION");
    let Some(receipt) = crate::self_management::self_receipt() else {
        let receipt = crate::self_management::receipt_path()
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
    if crate::self_management::is_nightly_version(&receipt.version) {
        return Ok((
            None,
            vec![format!(
                "this af {} is a nightly, and a nightly is never pinned, so the lock pins no af release; `af self channel stable` installs the newest release, which pins itself when you rerun",
                receipt.version
            )],
        ));
    }
    let own = format!("sha256:{}", receipt.sha256);
    let own_is_digest =
        receipt.sha256.len() == 64 && receipt.sha256.bytes().all(|byte| byte.is_ascii_hexdigit());
    let mut pin = AfPin::version_only(&receipt.version);
    let mut warnings = Vec::new();
    match crate::self_management::release_digests(&receipt.version) {
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
        (Some(attempt), Some(run)) => format!("{attempt} tokens/Attempt; {run} tokens/Task"),
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
        .and_then(|n| n.to_str())
        .unwrap_or("project");
    let mut files = tasks::files(&gates, profile)?;
    let (af_pin, warnings) = running_af_pin()?;
    let mut lock = Lockfile::empty();
    lock.af = af_pin.clone();
    files.insert(".af/af.lock".into(), lock.to_toml().into_bytes());
    files.insert(
        ".af/af.toml".into(),
        format!("{}{}", project_file(project_name), tasks::DEFAULTS).into_bytes(),
    );
    files.insert(
        ".af/README.md".into(),
        guide_file(project_name, profile, &gates).into_bytes(),
    );
    let native = crate::task_commands::onboarding::inspect_files(&files)?;
    let report = Report {
        status: "preview".into(),
        profile: PROFILE.into(),
        repository: repo.display().to_string(),
        pipeline: PIPELINE_NAME.into(),
        reviewers: native
            .reviewers
            .into_iter()
            .map(|(node, package, runner, model)| ReviewerSummary {
                node,
                package,
                runner,
                model,
            })
            .collect(),
        gates,
        attempt_tokens: Some(400_000),
        run_tokens: Some(6_000_000),
        clean_rounds: 1,
        max_rounds: 2,
        topology: vec!["checks -> bugs, correctness -> reduce -> review, findings, history".into()],
        warnings,
        files: files.keys().cloned().collect(),
        next_steps: vec![
            format!(
                "Review this plan, then run `{}`.",
                apply_command(repo, profile, &[], af.unwrap_or(env!("CARGO_PKG_VERSION")))?
            ),
            "Preview `af setup agents`, then install it with `af setup agents --write`.".into(),
            "Review and commit the generated `.af/` diff on the trusted base branch.".into(),
            "Run `af onboard` again to validate the authority and print the workflow.".into(),
        ],
        lock_af_version: af_pin.as_ref().map(|p| p.version.clone()),
        lock_af_targets: pin_targets(af_pin.as_ref()),
        runner_note: None,
        providers: vec![],
    };
    // Preserve every explicit Gate in the reproducible apply command.
    let mut report = report;
    report.next_steps[0] = format!(
        "Review this plan, then run `{}`.",
        apply_command(
            repo,
            profile,
            &report.gates,
            af.unwrap_or(env!("CARGO_PKG_VERSION"))
        )?
    );
    Ok(Bundle { files, report })
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

fn project_file(project_name: &str) -> String {
    #[derive(Serialize)]
    struct ProjectFile<'a> {
        version: u32,
        project: Project<'a>,
    }

    #[derive(Serialize)]
    struct Project<'a> {
        name: &'a str,
        min_af: &'a str,
    }

    toml::to_string_pretty(&ProjectFile {
        version: 2,
        project: Project {
            name: project_name,
            min_af: "0.6",
        },
    })
    .expect("the built-in project file serializes")
}

fn guide_file(project_name: &str, _profile: RunnerProfile, _gates: &[Gate]) -> String {
    format!(
        r#"# {project_name}: native Review and implementation Tasks

Review and commit this entire authority bundle before planning. The common committed
`.af/task-catalog.toml` pins current `pipeline.toml` and `worker.toml` packages;
`.af/code-policy.toml` declares the acceptance checks. No Campaign configuration is emitted.

## First Review

Copy `.af/task-packages/onboard/review-code/task.json` to `../review.json` and give it a fresh
Task ID. Make a source change, then run:

```sh
af review plan --file ../review.json --authority HEAD --base HEAD --uncommitted
af review resume review --confirm-plan PLAN_ID
af review inspect review --json
af review ledger review --json
af review report review --json
```

Planning records a Task without dispatching Workers. Inspect its exact plan before approving
execution. `--authority` selects trusted committed policy independently of `--candidate REV`
and `--base REV`; `--uncommitted` selects the working tree instead of a candidate commit.
Empty Diff planning and execution refuse before checks, Provider admission or Workers.
Use the catalog's machine-local Provider aliases, or a `--bindings FILE` document with
`[providers]` entries keyed by `onboard/bugs` and `onboard/correctness`. `af provider detect`
and `af provider status` are token-free; login runs at the operator's terminal.

The example's `strategy` is `light`: one complete Round. Heavy review is an explicit native
Pipeline with bounded successor Rounds and captured convergence policy; there is no `--heavy`
Campaign switch. See `af help review` and the Review Tasks guide for native composition.
Resume a recorded Task with `af review resume review --confirm-plan PLAN_ID`.
Continue reviewed implementation work with `af review continue NEXT_ID --from TASK_ID` using
its captured continuation contract. These commands never reset prior accounting or authority.

## Model-driven Tasks

`[task.defaults]` selects `onboard/implementation-reviewed` and its bounded independent
implementation, review and evaluation. A three-field Task file can use those defaults with
`af task plan --file ticket.json`. Review the plan before running it. A sealed Proposal may be
exported with `af review export TASK_ID PROPOSAL_ID`; export grants no delivery or publication
authority. Branches, commits, pushes and pull requests remain separate human actions.

Existing authority is validated by `af onboard` and `af doctor`. To re-pin intentional current
package edits, run `af onboard --refresh-lock`, inspect the catalog diff and commit it. No
Campaign reader or migration is installed; replace retired `.af/pipelines/` configuration
with the common Task catalog after reviewing a fresh scaffold in a separate checkout.
"#
    )
}

fn validate_bundle(bundle: &Bundle) -> Result<(), String> {
    crate::task_commands::onboarding::inspect_files(&bundle.files).map(drop)
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

/// The Provider kind a model Worker's runner program needs; `None` for a command Worker.
fn provider_kind(runner: &str) -> Option<&'static str> {
    match runner {
        "claude" => Some("claude"),
        "codex" => Some("codex"),
        "pi" => Some("pi"),
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
                let setup = if kind == "pi" {
                    format!(
                        "af provider setup {id} --kind pi --backend openai-codex --auth-dir /absolute/private/pi-profile --sdk-entrypoint /installed/node_modules/@earendil-works/pi-coding-agent/dist/index.js"
                    )
                } else {
                    format!("af provider detect, or af provider setup {id} --kind {kind} --login")
                };
                lines.push(format!(
                    "{node} needs a {kind} Provider: {NONE_REGISTERED}{PROVIDER_ARROW}{setup}"
                ));
            }
            Some(first) => {
                let listed = ids
                    .iter()
                    .map(|id| format!("{id} ({})", state(id)))
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(format!(
                    "{node} needs a {kind} Provider: {listed}{PROVIDER_ARROW}bind onboard/{node}={first} in --bindings FILE"
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
    let mut steps =
        vec!["Run `af provider status`; Provider credentials remain machine-local.".into()];
    if reviewers
        .iter()
        .filter_map(|r| provider_kind(&r.runner))
        .any(|kind| registry.ids_of(kind).is_empty())
    {
        steps.push(REGISTER_A_PROVIDER.into());
    }
    steps.extend([
        "Copy .af/task-packages/onboard/review-code/task.json to ../review.json and choose a fresh Task ID.".into(),
        "Make a change, then plan its review token-free: `af review plan --file ../review.json --authority HEAD --base HEAD --uncommitted`.".into(),
        "Use --bindings FILE to remap catalog Provider aliases; inspect the exact plan before execution.".into(),
        "Inspect `af review ledger TASK_ID --json` and `af review report TASK_ID --json`; publish nothing without human authorization.".into(),
    ]);
    steps
}

/// Where onboarding's reader stops on a committed `.af/`: the file, the line its TOML error
/// names (syntax or type), and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthorityStop {
    pub(crate) file: PathBuf,
    pub(crate) line: Option<usize>,
    pub(crate) message: String,
    /// The reader stopped because `file` no longer matches its current Task catalog pin:
    /// restoring it, or re-pinning it with `af onboard
    /// --refresh-lock` when it changed on purpose, is what clears it.
    pub(crate) repin: bool,
}

/// Follow the same bounded current Task authority reader as onboarding and doctor. A typed
/// failure names its file and TOML line; no retired Review Definition or route is executed.
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

/// The same native authority validator used by plain onboarding and machine doctor.
fn authority_steps(repo: &Path, af: &Path) -> Result<(), AuthorityStop> {
    crate::task_commands::onboarding::inspect(repo)
        .map(drop)
        .map_err(|error| AuthorityStop {
            file: error.file.unwrap_or_else(|| af.join("task-catalog.toml")),
            line: error.line,
            message: error.message,
            repin: error.repin,
        })
}

pub(crate) fn read_authority_text(path: &Path) -> Result<String, String> {
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
    let bytes = af_store::local_fs::read_bounded(path, MAX_DISCOVERY_FILE_BYTES)
        .map_err(|error| format!("reading authority {}: {error}", path.display()))?;
    String::from_utf8(bytes)
        .map_err(|error| format!("reading authority {}: {error}", path.display()))
}

fn inspect_existing(repo: &Path, status: &str) -> Result<Report, String> {
    let native = crate::task_commands::onboarding::inspect(repo).map_err(|e| {
        let location = e
            .file
            .as_ref()
            .map_or(String::new(), |p| p.display().to_string());
        let line = e.line.map_or(String::new(), |line| format!(" line {line}"));
        format!("{location}{line}: {}", e.message)
    })?;
    let lock = optional_release_lock(&repo.join(".af/af.lock"))?;
    let warnings = crate::project_policy::check_lock_af_version(&lock, ".af/af.lock")?
        .into_iter()
        .collect();
    Ok(Report {
        status: status.into(),
        profile: "project-owned native Task authority".into(),
        repository: repo.display().to_string(),
        pipeline: PIPELINE_NAME.into(),
        reviewers: native
            .reviewers
            .into_iter()
            .map(|(node, package, runner, model)| ReviewerSummary {
                node,
                package,
                runner,
                model,
            })
            .collect(),
        gates: native
            .gates
            .into_iter()
            .map(|(name, program, args)| Gate {
                name,
                program,
                args,
                source: "project authority".into(),
            })
            .collect(),
        attempt_tokens: None,
        run_tokens: None,
        clean_rounds: native.clean_rounds,
        max_rounds: native.max_rounds,
        topology: vec!["Native Task checks, review, Findings and exact accounting".into()],
        warnings,
        files: native.files,
        lock_af_version: lock.af_version().map(str::to_string),
        lock_af_targets: pin_targets(lock.af.as_ref()),
        runner_note: None,
        providers: vec![],
        next_steps: vec![],
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
    let native = crate::task_commands::onboarding::inspect(repo).map_err(|e| e.message)?;
    let container = native.container;
    Ok(AuthorityFacts {
        gates: report
            .gates
            .into_iter()
            .map(|gate| (gate.name, gate.program, gate.args))
            .collect(),
        container,
        workers: native.workers,
    })
}

fn optional_release_lock(path: &Path) -> Result<Lockfile, String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Lockfile::empty()),
        Err(e) => Err(e.to_string()),
        Ok(_) => Lockfile::from_toml(&read_authority_text(path)?).map_err(|e| e.to_string()),
    }
}

fn refresh_lock(repo: &Path) -> Result<Report, String> {
    let path = repo.join(".af/af.lock");
    let mut lock = optional_release_lock(&path)?;
    // Release dispatch, receipts, archive digests and the explicit --af refresh retain the
    // same owner. Current Task package pins live in the common catalog instead.
    let (pin, mut warnings) = if path.exists() {
        running_af_pin()?
    } else {
        (
            None,
            vec!["No release lock is present; refreshed only current Task package pins".into()],
        )
    };
    let changed = pin
        .as_ref()
        .is_some_and(|pin| lock.af.as_ref() != Some(pin));
    if let Some(pin) = pin {
        lock.af = Some(pin);
    } else if let Some(version) = lock.af_version() {
        warnings.push(format!("kept the existing pin af {version}"));
    }
    crate::task_commands::onboarding::refresh(repo)?;
    if changed {
        atomic_replace_authority(&path, lock.to_toml().as_bytes())?;
    }
    let mut report = inspect_existing(repo, "lock_refreshed")?;
    report.warnings.extend(warnings);
    report.next_steps = validated_next_steps(
        &report.reviewers,
        &crate::providers::read_registry_entries(),
    );
    Ok(report)
}

pub(crate) fn atomic_replace_authority(path: &Path, bytes: &[u8]) -> Result<(), String> {
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
pub(crate) fn fixture_files(repo: &Path) -> BTreeMap<String, Vec<u8>> {
    build_bundle(
        repo,
        RunnerProfile::Codex,
        vec![Gate::detected("check", "/usr/bin/true", &[], "fixture")],
        None,
    )
    .unwrap()
    .files
}
