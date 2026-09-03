//! Deterministic, token-free review-authority onboarding.
//!
//! The binary owns one small scaffold so an agent can discover and reproduce a supported setup
//! without pasting a prompt or hand-writing a digest. Once emitted, every byte is ordinary
//! project-owned authority: `af onboard` validates it, but never silently overwrites it.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use review_config::lock::{Lockfile, Pin, Registry};
use review_config::{
    ArgSpec, BudgetSpec, BudgetUnit, CheckSpec, CommandSpec, ConvergenceSpec, Definition, EdgeSpec,
    GateExecutionSpec, GateModeSpec, IsolationSpec, NodeKindSpec, NodeSpec, PortContractSpec,
    PortSpec, ProvenanceSpec, SandboxProviderSpec, SeveritySpec, SubjectSpec, TypedPortSpec,
};
use review_core::{PortCardinality, SnapshotAffinity, SubjectKind, contract};
use serde::Serialize;

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
            Self::Claude => "opus (high effort)",
            Self::Codex => "machine-configured Codex model",
        }
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
    profile: RunnerProfile,
    gates: Vec<Gate>,
    apply: bool,
    refresh_lock: bool,
    migrate: bool,
    json: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ReviewerSummary {
    node: String,
    package: String,
    runner: String,
    model: String,
}

/// One legacy `.review/` pipeline: the format upgrades it still needs, or just received.
#[derive(Debug, Clone, Serialize)]
struct LegacyPipeline {
    path: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pending: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    applied: Vec<String>,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pipelines: Vec<LegacyPipeline>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_af_version: Option<String>,
}

struct Bundle {
    files: BTreeMap<String, Vec<u8>>,
    worker_files: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    report: Report,
}

pub(super) fn run_cli(args: crate::cli::OnboardArgs) -> Result<(), String> {
    let options = options_from_cli(args)?;
    let report = execute(&options)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
        );
    } else {
        print_human(&report);
    }
    Ok(())
}

fn options_from_cli(args: crate::cli::OnboardArgs) -> Result<Options, String> {
    let profile = match args.runner {
        crate::cli::RunnerArg::Mixed => RunnerProfile::Mixed,
        crate::cli::RunnerArg::Claude => RunnerProfile::Claude,
        crate::cli::RunnerArg::Codex => RunnerProfile::Codex,
    };
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
        profile,
        gates,
        apply: args.apply,
        refresh_lock: args.refresh_lock,
        migrate: args.migrate,
        json: args.json,
    };
    if options.apply && options.refresh_lock {
        return Err("--apply and --refresh-lock are mutually exclusive".to_string());
    }
    if options.refresh_lock && !options.gates.is_empty() {
        return Err(
            "--gate cannot be combined with --refresh-lock; edit the pipeline first".into(),
        );
    }
    if options.refresh_lock && options.profile != RunnerProfile::Mixed {
        return Err(
            "--runner cannot be combined with --refresh-lock; edit Worker manifests first".into(),
        );
    }
    if options.migrate && options.refresh_lock {
        return Err("--migrate and --refresh-lock are mutually exclusive".to_string());
    }
    if options.migrate && (!options.gates.is_empty() || options.profile != RunnerProfile::Mixed) {
        return Err(
            "--gate and --runner configure only a new `.af/` bundle; --migrate upgrades existing `.review/` policy in place".into(),
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
            if options.migrate {
                return Err(
                    "`.af/` is current authority; --migrate applies only to legacy `.review/` policy"
                        .to_string(),
                );
            }
            if options.apply {
                return Err(
                    "`.af/` already exists; plain `af onboard` validates it and never overwrites it"
                        .to_string(),
                );
            }
            if options.refresh_lock {
                return refresh_lock(&repo);
            }
            if !options.gates.is_empty() || options.profile != RunnerProfile::Mixed {
                return Err(
                    "--gate and --runner configure only a new bundle; existing authority is never silently changed"
                        .into(),
                );
            }
            inspect_existing(&repo, "onboarded")
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(legacy) = legacy_authority(&repo)? {
                return inspect_legacy(&repo, &legacy, options);
            }
            if options.migrate {
                return Err(
                    "--migrate applies to legacy `.review/` authority; this repository has none"
                        .to_string(),
                );
            }
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
            let mut bundle = build_bundle(&repo, options.profile, gates)?;
            validate_bundle(&bundle)?;
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

fn print_human(report: &Report) {
    println!("af onboard: {}", report.status);
    println!("repository  {}", report.repository);
    println!("profile     {}", report.profile);
    println!("pipeline    {}", report.pipeline);
    if !report.pipelines.is_empty() {
        println!("pipelines");
        for pipeline in &report.pipelines {
            println!("  {}", pipeline.path);
            for change in &pipeline.pending {
                println!("    pending  {change}");
            }
            for change in &pipeline.applied {
                println!("    applied  {change}");
            }
        }
    }
    if let Some(version) = &report.lock_af_version {
        println!("lock        pinned by af {version}");
    }
    println!("topology");
    for line in &report.topology {
        println!("  {line}");
    }
    println!("reviewers");
    for reviewer in &report.reviewers {
        println!(
            "  {}: {} / {} ({})",
            reviewer.node, reviewer.package, reviewer.runner, reviewer.model
        );
    }
    println!("gates");
    for gate in &report.gates {
        println!("  {}: {} [{}]", gate.name, gate.command_line(), gate.source);
    }
    match (report.attempt_tokens, report.run_tokens) {
        (Some(attempt), Some(run)) => {
            println!("budget      {attempt} tokens/Attempt; {run} tokens/Round")
        }
        _ => println!("budget      uncapped by pipeline authority"),
    }
    println!(
        "convergence {} clean Round; {} Round maximum",
        report.clean_rounds, report.max_rounds
    );
    for warning in &report.warnings {
        println!("warning     {warning}");
    }
    if !report.files.is_empty() {
        println!("files");
        for path in &report.files {
            println!("  {path}");
        }
    }
    println!("next");
    for (index, step) in report.next_steps.iter().enumerate() {
        println!("  {}. {step}", index + 1);
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
    std::fs::read_to_string(path).map_err(|error| format!("reading {}: {error}", path.display()))
}

fn build_bundle(repo: &Path, profile: RunnerProfile, gates: Vec<Gate>) -> Result<Bundle, String> {
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

    let mut lockfile = Lockfile::empty();
    lockfile.af_version = Some(env!("CARGO_PKG_VERSION").to_string());
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
        pipelines: Vec::new(),
        lock_af_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        next_steps: vec![
            format!(
                "Review this plan, then run `af onboard --repo {} --runner {} --apply`.",
                shell_words::quote(&repo.to_string_lossy()),
                profile.name()
            ),
            "Review and commit the generated `.af/` diff on the trusted base branch.".into(),
            "Run `af onboard` again to validate authority and print the operating workflow.".into(),
        ],
    };
    Ok(Bundle {
        files,
        worker_files,
        report,
    })
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

fn typed_port(name: &str, artifact_type: &str) -> PortContractSpec {
    PortContractSpec::Typed(TypedPortSpec {
        name: name.to_string(),
        artifact_type: artifact_type.to_string(),
        cardinality: PortCardinality::One,
        optional: false,
        snapshot_affinity: SnapshotAffinity::SameSubject,
    })
}

fn prior_finding_set_port(name: &str) -> PortContractSpec {
    PortContractSpec::Typed(TypedPortSpec {
        name: name.to_string(),
        artifact_type: contract::FINDING_SET_V1.to_string(),
        cardinality: PortCardinality::One,
        optional: true,
        snapshot_affinity: SnapshotAffinity::Any,
    })
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
            "run budget {} cannot admit the first Attempt of each of {static_workers} static Workers (requires {required}: {reservation} reserved together plus {model_workers} Provider smoke floors)",
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

    let values: &[&str] = match runner {
        RunnerKind::Claude => &["--model", "opus", "--effort", "high"],
        // The Codex adapter owns the `exec` invocation, sandbox, output, and stdin flags.
        // Package args are model flags only; an empty list deliberately inherits the
        // machine-configured model.
        RunnerKind::Codex => &[],
    };
    let manifest = Manifest {
        name,
        version: WORKER_VERSION,
        subjects: ["diff"],
        runner: ManifestRunner {
            program: runner.program().to_string(),
            args: values
                .iter()
                .map(|value| ManifestArg {
                    value: (*value).to_string(),
                })
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
        worker: BTreeMap<&'a str, Worker<'a>>,
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

    #[derive(Serialize)]
    struct Worker<'a> {
        package: &'a str,
    }

    let mut workers = BTreeMap::new();
    workers.insert(
        "architecture",
        Worker {
            package: "architecture",
        },
    );
    workers.insert(
        "correctness",
        Worker {
            package: "correctness",
        },
    );
    toml::to_string_pretty(&ProjectFile {
        version: 1,
        project: Project {
            name: project_name,
            min_af: "0.6",
        },
        defaults: Defaults {
            pipeline: PIPELINE_NAME,
        },
        worker: workers,
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
2. Run `af provider status`; choose the machine-local Provider ID for each Worker without writing
   a token into this repository.
3. Fetch the pull request with the normal repository tooling, create a disposable worktree at its
   head, and identify the trusted base revision.
4. From that worktree run:

   ```sh
   af review plan --policy-rev <trusted-policy-revision> --base <trusted-base-revision> --uncommitted \
     --provider correctness=<provider-id> --provider architecture=<provider-id>
   af provider doctor --campaign pr-<number> --policy-rev <trusted-policy-revision> \
     --base <trusted-base-revision> --uncommitted \
     --provider correctness=<provider-id> --provider architecture=<provider-id>
   af review run --campaign pr-<number> --policy-rev <trusted-policy-revision> \
     --base <trusted-base-revision> --uncommitted \
     --provider correctness=<provider-id> --provider architecture=<provider-id> --json
   af review ledger --campaign pr-<number> --long
   af review report --campaign pr-<number> --format md
   ```

5. Read the Ledger and report. Do not comment on the pull request, commit, push, merge, or mutate
   external state unless a human explicitly authorizes that separate action.

The command above is light by default. After a finding-bearing result, fix the Findings, run the
project's deterministic Gate command, and stop. Do not open a follow-up review Campaign. Add
`--heavy` only when a human explicitly requests convergence review.

`--policy-rev` must name a trusted committed revision containing this `.af/` directory; `--base`
independently names the revision against which the candidate is compared. The review command
captures both immutably; uncommitted pull-request content cannot alter its Gate, Worker prompts,
models, topology, budgets, or pins. Compatibility `--authority REV` expands visibly to both
selectors, but explicit selectors are preferred.

## Changing authority

Edit the ordinary files under `.af/`, then explicitly run `af onboard --refresh-lock`. That
command recomputes only the selected pipeline pin and the Worker packages it references. Review
the authority and lock diff together, run `af onboard` again, then commit through the project's
normal controls. Never weaken a Gate or boundary merely to obtain a passing review. The lock
also records the `af` release that wrote it: a newer `af` proceeds and notes the difference, an
older `af` refuses until the lock is re-pinned.
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
        "Run `af provider status`, then follow `.af/README.md` to review a pull request.".into(),
    ]
}

struct ExistingAuthority {
    selected: String,
    pipeline_path: PathBuf,
    pipeline_text: String,
    definition: Definition,
    lock: Lockfile,
}

fn parse_existing(repo: &Path) -> Result<ExistingAuthority, String> {
    let project_path = repo.join(".af/af.toml");
    let project_text = read_authority_text(&project_path)?;
    let selected = selected_pipeline(&project_text)?;
    let pipeline_path = repo.join(format!(".af/pipelines/{selected}.toml"));
    let pipeline_text = read_authority_text(&pipeline_path)?;
    let definition = Definition::from_toml(&pipeline_text).map_err(|error| error.to_string())?;
    let lock_path = repo.join(".af/af.lock");
    let lock = Lockfile::from_toml(&read_authority_text(&lock_path)?)
        .map_err(|error| error.to_string())?;
    Ok(ExistingAuthority {
        selected,
        pipeline_path,
        pipeline_text,
        definition,
        lock,
    })
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
    std::fs::read_to_string(path)
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
    let registry = Registry::new([repo.join(".af/workers")]);
    let loaded = authority
        .definition
        .clone()
        .load_with(&authority.lock, &registry)
        .map_err(|error| error.to_string())?;

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
        pipelines: Vec::new(),
        lock_af_version: authority.lock.af_version.clone(),
        next_steps: vec![
            "Run `af provider status`; Provider credentials remain machine-local.".into(),
            "Follow `.af/README.md` to capture a pull request in a disposable worktree.".into(),
            "Run `af review ledger` and `af review report`; publish nothing without human authorization."
                .into(),
        ],
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
    let referenced: BTreeSet<String> = authority
        .definition
        .nodes
        .iter()
        .filter_map(|node| node.package.clone())
        .collect();
    if referenced.is_empty() {
        return Err("selected pipeline references no Worker package".into());
    }
    let registry = Registry::new([repo.join(".af/workers")]);
    let mut refreshed = authority.lock.clone();
    refreshed.af_version = Some(env!("CARGO_PKG_VERSION").to_string());
    refreshed
        .workers
        .retain(|name, _| repo.join(".af/workers").join(name).is_dir());
    refreshed
        .reviewers
        .retain(|name, _| repo.join(".af/workers").join(name).is_dir());
    refreshed
        .pipelines
        .retain(|name, _| repo.join(format!(".af/pipelines/{name}.toml")).is_file());
    for name in referenced {
        let pin = Lockfile::pin(&name, &registry).map_err(|error| error.to_string())?;
        refreshed.workers.insert(name.clone(), pin);
        refreshed.reviewers.remove(&name);
    }
    let version = refreshed
        .pipelines
        .get(&authority.selected)
        .map(|pin| pin.version.clone())
        .unwrap_or_else(|| PIPELINE_VERSION.to_string());
    refreshed.pipelines.insert(
        authority.selected.clone(),
        Pin {
            version,
            digest: review_store::canonical::blob_content_id(authority.pipeline_text.as_bytes()),
        },
    );
    authority
        .definition
        .clone()
        .load_with(&refreshed, &registry)
        .map_err(|error| error.to_string())?;
    atomic_replace_authority(&repo.join(".af/af.lock"), refreshed.to_toml().as_bytes())?;
    inspect_existing(repo, "lock_refreshed")
}

fn validate_no_stale_pins(repo: &Path, lock: &Lockfile) -> Result<(), String> {
    for name in lock.workers.keys().chain(lock.reviewers.keys()) {
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

/// Legacy `.review/` authority: the layout consumers pinned before `.af/` existed.
struct LegacyAuthority {
    root: PathBuf,
    pipelines: Vec<PathBuf>,
}

/// The repository's legacy authority, if it carries `.review/` and no `.af/`.
fn legacy_authority(repo: &Path) -> Result<Option<LegacyAuthority>, String> {
    let root = repo.join(".review");
    match std::fs::symlink_metadata(&root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err("refusing symlinked review authority `.review`".to_string())
        }
        Ok(metadata) if !metadata.is_dir() => {
            Err("review authority `.review` exists but is not a directory".to_string())
        }
        Ok(_) => {
            let pipelines_dir = root.join("pipelines");
            let mut pipelines: Vec<PathBuf> = match std::fs::read_dir(&pipelines_dir) {
                Ok(entries) => entries
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<Result<_, _>>()
                    .map_err(|error| format!("listing {}: {error}", pipelines_dir.display()))?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(error) => {
                    return Err(format!("listing {}: {error}", pipelines_dir.display()));
                }
            };
            pipelines.retain(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "toml")
            });
            pipelines.sort();
            if pipelines.is_empty() {
                return Err(
                    "`.review/` carries no pipelines/*.toml; nothing to validate or migrate".into(),
                );
            }
            Ok(Some(LegacyAuthority { root, pipelines }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("inspecting {}: {error}", root.display())),
    }
}

/// Validates legacy `.review/` policy against this release's pipeline format and, with
/// `--migrate --apply`, rewrites each outdated pipeline in place — additively, never touching
/// reviewer packages, budgets, convergence, checks, or edges. `.review/review.lock` pins only
/// reviewer packages, so no lock changes.
fn inspect_legacy(
    repo: &Path,
    legacy: &LegacyAuthority,
    options: &Options,
) -> Result<Report, String> {
    if options.refresh_lock {
        return Err(
            "--refresh-lock applies to `.af/af.lock`; legacy `.review/review.lock` pins only reviewer packages"
                .into(),
        );
    }
    if !options.gates.is_empty() || options.profile != RunnerProfile::Mixed {
        return Err(
            "--gate and --runner configure only a new `.af/` bundle; this repository carries legacy `.review/` authority"
                .into(),
        );
    }
    if options.apply && !options.migrate {
        return Err(
            "`.review/` already carries review authority; run `af onboard --migrate [--apply]` to validate or upgrade it in place. Scaffolding `.af/` beside it would leave two authorities"
                .into(),
        );
    }
    let lock = Lockfile::from_toml(&read_authority_text(&legacy.root.join("review.lock"))?)
        .map_err(|error| error.to_string())?;
    let registry = Registry::new([legacy.root.join("reviewers")]);
    let lock_note = crate::project::check_lock_af_version(&lock, ".review/review.lock")?;

    let mut pipelines = Vec::new();
    let mut first: Option<(Definition, review_config::Loaded)> = None;
    let mut pending_total = 0_usize;
    let mut applied_total = 0_usize;
    for path in &legacy.pipelines {
        let relative = path
            .strip_prefix(repo)
            .unwrap_or(path)
            .display()
            .to_string();
        let text = read_authority_text(path)?;
        let needed = review_config::pipeline_edit::legacy_upgrades(&text)
            .map_err(|error| format!("{relative}: {error}"))?;
        let describe = |upgrades: &[review_config::pipeline_edit::LegacyUpgrade]| {
            upgrades
                .iter()
                .map(|upgrade| upgrade.describe().to_string())
                .collect::<Vec<_>>()
        };
        let (text, pending, applied) = if options.migrate && options.apply && !needed.is_empty() {
            let rewritten = review_config::pipeline_edit::apply_legacy_upgrades(&text)
                .map_err(|error| format!("{relative}: {error}"))?
                .ok_or_else(|| format!("{relative}: upgrades were pending but nothing changed"))?;
            Definition::from_toml(&rewritten)
                .map_err(|error| format!("{relative}: {error}"))?
                .load_with(&lock, &registry)
                .map_err(|error| format!("{relative}: upgraded pipeline does not load: {error}"))?;
            atomic_replace_authority(path, rewritten.as_bytes())?;
            (rewritten, Vec::new(), describe(&needed))
        } else {
            (text, describe(&needed), Vec::new())
        };
        let definition =
            Definition::from_toml(&text).map_err(|error| format!("{relative}: {error}"))?;
        let loaded = definition
            .clone()
            .load_with(&lock, &registry)
            .map_err(|error| format!("{relative}: {error}"))?;
        pending_total += pending.len();
        applied_total += applied.len();
        pipelines.push(LegacyPipeline {
            path: relative,
            pending,
            applied,
        });
        if first.is_none() {
            first = Some((definition, loaded));
        }
    }
    let (definition, loaded) = first.expect("legacy authority lists at least one pipeline");
    let status = if pending_total > 0 {
        "legacy-outdated"
    } else if applied_total > 0 {
        "migrated"
    } else {
        "legacy"
    };

    let gates = definition
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
    let (attempt_tokens, run_tokens) = definition
        .budgets
        .as_ref()
        .map(|budgets| (Some(budgets.attempt), Some(budgets.run)))
        .unwrap_or((None, None));
    let mut warnings = validate_budget_arithmetic(&definition)?;
    warnings.extend(lock_note);
    let mut files = vec![".review/review.lock".to_string()];
    files.extend(pipelines.iter().map(|pipeline| pipeline.path.clone()));
    for package in loaded.packages().values() {
        for path in package.files().keys() {
            files.push(format!(".review/reviewers/{}/{path}", package.name));
        }
    }
    files.sort();
    files.dedup();
    let repository = repo.display().to_string();
    let next_steps = match status {
        "legacy-outdated" => vec![
            format!(
                "Run `af onboard --repo {repository} --migrate --apply` to rewrite the listed pipelines in place; reviewer packages, budgets, and convergence stay as they are."
            ),
            "Review and commit the `.review/` diff on the trusted base branch.".into(),
            "Run `af review plan` against the committed policy before the next Campaign.".into(),
        ],
        "migrated" => vec![
            "Review and commit the `.review/` diff on the trusted base branch.".into(),
            "Run `af onboard` again to confirm nothing is pending, then `af review plan`.".into(),
        ],
        _ => vec![
            "Policy is current for this release; run `af review plan` after every pin bump.".into(),
            "Moving to `.af/` is a separate decision: remove `.review/` first, then `af onboard --apply` scaffolds a fresh bundle."
                .into(),
        ],
    };
    let pipeline = pipelines[0].path.clone();
    Ok(Report {
        status: status.to_string(),
        profile: "legacy .review/ authority".into(),
        repository,
        pipeline,
        reviewers,
        gates,
        attempt_tokens,
        run_tokens,
        clean_rounds: definition.convergence.clean_rounds,
        max_rounds: definition.convergence.max_rounds,
        topology: topology_lines(&definition),
        warnings,
        files,
        pipelines,
        lock_af_version: lock.af_version.clone(),
        next_steps,
    })
}

#[cfg(test)]
mod tests {
    use super::{build_definition, runner_model, validate_budget_arithmetic};
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
    fn budget_must_admit_every_static_worker_once() {
        let mut definition = build_definition(&[]);
        definition.budgets.as_mut().unwrap().run = 600_000;
        let error = validate_budget_arithmetic(&definition).unwrap_err();
        assert!(error.contains("requires 600002"), "{error}");
    }
}
