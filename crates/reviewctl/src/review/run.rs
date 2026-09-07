//! `af review run`, `plan`, and `render` — the pinned-authority run path — plus the Provider
//! doctor that shares its admission step.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use review_attempt::{Budget, BudgetLedger, Scope};
use review_core::EventType;
use review_graph::NodeOutcome;
use review_pipeline::{Kernel, RoundAuthority, RunVerdict};
use review_runner::ReviewerAdapter;
use review_source_git::Repo;
use review_store::{Cas, EventStore, Verdict};

use crate::review::evidence::latest_round_evidence;
use crate::review::report::print_scope_authority_warnings;
use crate::{
    CampaignMode, Options, authority, caches, candidate_identity, project, providers, run_progress,
};

pub(crate) fn print_render(options: &Options) -> Result<(), String> {
    let scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
    let cas = Cas::open(scratch.path().join("cas")).map_err(|error| error.to_string())?;
    let repository = std::fs::canonicalize(&options.repo)
        .map_err(|error| format!("opening repository {}: {error}", options.repo.display()))?;
    let git_home = scratch.path().join("git-home");
    std::fs::create_dir_all(&git_home).map_err(|error| error.to_string())?;
    let repo = Repo::open(repository, git_home)
        .with_timeout(authority::requested_git_timeout(options.git_timeout));
    let view = authority::render(options, &cas, &repo)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string(&view).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    // The header goes to stderr and the exact bytes to stdout, so `> prompt.md` is exact.
    eprintln!("review render (token-free; no Campaign state)");
    eprintln!("node      {} ({})", view.node, view.runner);
    if let Some(package) = &view.package {
        eprintln!(
            "package   {} {} {}",
            package["name"].as_str().unwrap_or("?"),
            package["version"].as_str().unwrap_or("?"),
            package["digest"].as_str().unwrap_or("?")
        );
    }
    eprintln!(
        "transport {}; {} bytes; about {} tokens{}",
        match view.transport {
            review_runner::InputTransport::Prompt => "prompt on stdin",
            review_runner::InputTransport::Json => "typed JSON on stdin",
        },
        view.bytes,
        view.estimated_tokens,
        match (view.cap_tokens, view.fits) {
            (Some(cap), Some(true)) => format!("; fits its {cap}-token Attempt cap"),
            (Some(cap), Some(false)) => format!("; EXCEEDS its {cap}-token Attempt cap"),
            _ => String::new(),
        }
    );
    for entry in &view.manifest.entries {
        eprintln!(
            "context   {} {} bytes ({})",
            entry.name, entry.rendered_bytes, entry.required_by
        );
    }
    for item in &view.not_rendered {
        eprintln!("omitted   {item}");
    }
    eprintln!("effects   no state, Gates, Provider calls, Workers, or token spend");
    let mut stdout = std::io::stdout().lock();
    std::io::Write::write_all(&mut stdout, &view.raw).map_err(|error| error.to_string())?;
    std::io::Write::flush(&mut stdout).map_err(|error| error.to_string())?;
    Ok(())
}

pub(crate) fn print_plan(options: &Options) -> Result<(), String> {
    let scratch = tempfile::tempdir().map_err(|error| error.to_string())?;
    let cas = Cas::open(scratch.path().join("cas")).map_err(|error| error.to_string())?;
    let repository = std::fs::canonicalize(&options.repo)
        .map_err(|error| format!("opening repository {}: {error}", options.repo.display()))?;
    let git_home = scratch.path().join("git-home");
    std::fs::create_dir_all(&git_home).map_err(|error| error.to_string())?;
    let repo = Repo::open(repository, git_home)
        .with_timeout(authority::requested_git_timeout(options.git_timeout));
    let plan = authority::plan(options, &cas, &repo)?;
    if options.json {
        println!(
            "{}",
            serde_json::to_string(&plan).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    let selectors = &plan["selectors"];
    let resolved = &plan["resolved"];
    println!("review plan (token-free; no Campaign state)");
    if let Some(authority) = selectors["compatibility_authority"].as_str() {
        if plan["subject"]["kind"].as_str() == Some("diff") {
            println!(
                "compat   --authority {authority} => --policy-rev {authority} --base {authority}"
            );
        } else {
            println!("compat   --authority {authority} => --policy-rev {authority}");
        }
    }
    println!(
        "policy   {} => {}",
        selectors["policy_rev"].as_str().unwrap_or("?"),
        resolved["policy_snapshot_id"].as_str().unwrap_or("?")
    );
    if let Some(base) = selectors["base"].as_str() {
        println!(
            "base     {base} => {}",
            resolved["base_snapshot_id"].as_str().unwrap_or("?")
        );
    }
    println!(
        "candidate {} => {}",
        selectors["candidate"].as_str().unwrap_or("?"),
        resolved["candidate_snapshot_id"].as_str().unwrap_or("?")
    );
    println!(
        "subject  {} ({} paths, {} patch bytes{})",
        plan["subject"]["kind"].as_str().unwrap_or("?"),
        plan["subject"]["changed_paths"]
            .as_array()
            .map_or(0, Vec::len),
        plan["subject"]["patch_bytes"].as_u64().unwrap_or(0),
        if plan["subject"]["empty"].as_bool() == Some(true) {
            "; EMPTY — run will refuse"
        } else {
            ""
        }
    );
    for path in plan["subject"]["changed_paths"]
        .as_array()
        .into_iter()
        .flatten()
    {
        println!("  change  {}", path.as_str().unwrap_or("?"));
    }
    if let Some(route) = plan["route"].as_object() {
        println!(
            "route    {} => {}{}{}",
            route
                .get("policy")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?"),
            route
                .get("pipeline_path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("?"),
            route
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(|name| format!(" ({name})"))
                .unwrap_or_default(),
            route
                .get("replaced")
                .and_then(serde_json::Value::as_str)
                .map(|replaced| format!("; replaced {replaced}: its Worker input exceeded the cap"))
                .unwrap_or_default()
        );
    }
    println!("topology");
    for node in plan["pipeline"]["topology"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
    {
        println!(
            "  node {} [{}]",
            node["id"].as_str().unwrap_or("?"),
            node["kind"].as_str().unwrap_or("?")
        );
    }
    for edge in plan["pipeline"]["topology"]["edges"]
        .as_array()
        .into_iter()
        .flatten()
    {
        println!(
            "  {}.{} -> {}.{}",
            edge["from"]["node"].as_str().unwrap_or("?"),
            edge["from"]["port"].as_str().unwrap_or("?"),
            edge["to"]["node"].as_str().unwrap_or("?"),
            edge["to"]["port"].as_str().unwrap_or("?")
        );
    }
    for gate in plan["pipeline"]["gates"].as_array().into_iter().flatten() {
        println!(
            "gate     {} ({})",
            gate["name"].as_str().unwrap_or("?"),
            if gate["required"].as_bool() == Some(true) {
                "required"
            } else {
                "optional"
            }
        );
    }
    if let Some(budgets) = plan["pipeline"]["budgets"].as_object() {
        println!(
            "budgets  {} tokens/Attempt; {} tokens/Round",
            budgets
                .get("attempt")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0),
            budgets
                .get("run")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        );
    }
    let mut parallel = format!(
        "parallel {} Workers at once (pipeline max_parallel)",
        plan["pipeline"]["max_parallel"].as_u64().unwrap_or(0)
    );
    if let Some(fanout) = plan["pipeline"]["scatter_fanout"]
        .as_object()
        .filter(|fanout| !fanout.is_empty())
    {
        parallel.push_str("; shards ");
        parallel.push_str(
            &fanout
                .iter()
                .map(|(node, bound)| format!("{node} up to {bound} at once"))
                .collect::<Vec<_>>()
                .join(", "),
        );
    }
    println!("{parallel}");
    if let Some(reservations) = plan["pipeline"]["reservations"]
        .as_array()
        .filter(|reservations| !reservations.is_empty())
    {
        let parts = reservations
            .iter()
            .map(|reservation| {
                format!(
                    "{} {} ({})",
                    reservation["node"].as_str().unwrap_or("?"),
                    reservation["tokens"].as_u64().unwrap_or(0),
                    reservation["source"].as_str().unwrap_or("?")
                )
            })
            .collect::<Vec<_>>();
        println!(
            "reserve  {}; max simultaneous {}",
            parts.join(" · "),
            plan["pipeline"]["max_simultaneous_reservation"]
                .as_u64()
                .unwrap_or(0)
        );
        for reservation in reservations {
            let Some(bytes) = reservation["input_bytes"].as_u64() else {
                continue;
            };
            println!(
                "input    {} {bytes} bytes (about {} tokens){}",
                reservation["node"].as_str().unwrap_or("?"),
                reservation["input_tokens"].as_u64().unwrap_or(0),
                if reservation["fits"].as_bool() == Some(false) {
                    " — EXCEEDS its Attempt cap; run will refuse"
                } else {
                    ""
                }
            );
        }
    }
    for provider in plan["providers"].as_array().into_iter().flatten() {
        println!(
            "provider {} -> {}",
            provider["node"].as_str().unwrap_or("?"),
            provider["binding"].as_str().unwrap_or("MISSING")
        );
    }
    println!("effects  no state, Gates, Provider calls, Workers, or token spend");
    Ok(())
}

struct ReviewAuthority {
    authority_snapshot_id: String,
    campaign_manifest_id: String,
    subject_id: String,
    head_snapshot_id: String,
    round: u32,
    epoch: u32,
}

fn verdict_value(verdict: &RunVerdict) -> serde_json::Value {
    match verdict {
        RunVerdict::Pass => serde_json::json!({"kind": "clean"}),
        RunVerdict::Fail(Verdict::NotConverged) => {
            serde_json::json!({"kind": "fail", "reason": "not_converged"})
        }
        RunVerdict::Fail(Verdict::Exhausted) => {
            serde_json::json!({"kind": "fail", "reason": "exhausted"})
        }
        RunVerdict::Fail(Verdict::Converged) => {
            serde_json::json!({"kind": "fail", "reason": "invalid_converged_failure"})
        }
        RunVerdict::Incomplete { missing } => serde_json::json!({
            "kind": "incomplete",
            "missing_nodes": missing.iter().map(|(node, reason)| {
                serde_json::json!({"node": node, "reason": reason})
            }).collect::<Vec<_>>(),
        }),
    }
}

fn next_action_value(mode: CampaignMode, verdict: &RunVerdict) -> serde_json::Value {
    match (mode, verdict) {
        (_, RunVerdict::Pass) => serde_json::json!({
            "kind": "done",
            "start_another_campaign": false,
        }),
        (CampaignMode::Light, RunVerdict::Fail(_)) => serde_json::json!({
            "kind": "fix_then_gate",
            "start_another_campaign": false,
            "message": "Fix the concrete findings, run the deterministic project gate, then stop. Do not start another review Campaign; --heavy requires an explicit human choice.",
        }),
        (CampaignMode::Light, RunVerdict::Incomplete { .. }) => serde_json::json!({
            "kind": "resume_incomplete_round",
            "start_another_campaign": false,
            "message": "Resume this exact incomplete light Round. Do not start a replacement Campaign.",
        }),
        (CampaignMode::Heavy, RunVerdict::Fail(Verdict::NotConverged)) => serde_json::json!({
            "kind": "continue_campaign",
            "start_another_campaign": false,
        }),
        (CampaignMode::Heavy, RunVerdict::Fail(Verdict::Exhausted)) => serde_json::json!({
            "kind": "human_decision",
            "start_another_campaign": false,
            "message": "The heavy Campaign exhausted its pinned convergence window. Do not start another Campaign without an explicit human decision.",
        }),
        (CampaignMode::Heavy, RunVerdict::Fail(Verdict::Converged)) => serde_json::json!({
            "kind": "invalid_verdict",
            "start_another_campaign": false,
        }),
        (CampaignMode::Heavy, RunVerdict::Incomplete { .. }) => serde_json::json!({
            "kind": "resume_incomplete_round",
            "start_another_campaign": false,
        }),
    }
}

fn aggregate_usage(attempts: &[review_pipeline::AttemptEvidence]) -> review_runner::TokenUsage {
    fn sum(
        attempts: &[review_pipeline::AttemptEvidence],
        select: impl Fn(&review_runner::TokenUsage) -> Option<u64>,
    ) -> Option<u64> {
        attempts
            .iter()
            .filter_map(|attempt| select(&attempt.usage))
            .reduce(u64::saturating_add)
    }
    review_runner::TokenUsage {
        input_tokens: sum(attempts, |usage| usage.input_tokens),
        output_tokens: sum(attempts, |usage| usage.output_tokens),
        cache_read_tokens: sum(attempts, |usage| usage.cache_read_tokens),
        cache_write_tokens: sum(attempts, |usage| usage.cache_write_tokens),
        reasoning_tokens: sum(attempts, |usage| usage.reasoning_tokens),
        chargeable_tokens: attempts
            .iter()
            .map(|attempt| attempt.cost_tokens)
            .fold(0, u64::saturating_add),
    }
}

fn packaged_runner(command: &review_core::Command) -> String {
    Path::new(&command.program)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// `static_reservation_tokens` is the most the static Workers can hold reserved at once — the
/// `max_parallel` largest first-Attempt reservations (each node's own cap where declared, the
/// pipeline attempt cap elsewhere), with a Scatter node counted at its whole declared fan-out
/// because it reserves every shard up front. See `project::max_simultaneous_reservation`.
pub(crate) fn require_static_attempt_capacity(
    static_reservation_tokens: u64,
    run_tokens: u64,
    static_workers: usize,
    max_parallel: usize,
    committed_tokens: u64,
) -> Result<(), String> {
    let required = committed_tokens
        .checked_add(static_reservation_tokens)
        .ok_or("Provider spend plus static Worker budget arithmetic overflow")?;
    if required > run_tokens {
        let concurrent = max_parallel.min(static_workers);
        return Err(format!(
            "Provider admission committed {committed_tokens} tokens, leaving insufficient run budget for the {concurrent} of {static_workers} required static Workers that can hold a first Attempt reserved at once under this pipeline's max_parallel = {max_parallel} ({static_reservation_tokens} tokens, counting a Scatter node's whole fan-out): cap {run_tokens}, required {required}"
        ));
    }
    Ok(())
}

fn admit_review_providers(
    options: &Options,
    state: &Path,
    cas: &Cas,
    store: &mut EventStore,
    run_id: &str,
    loaded: &review_config::Loaded,
    authority: &RoundAuthority,
) -> Result<BTreeMap<String, providers::ProviderAdmission>, String> {
    for node in options.provider_bindings.keys() {
        if !loaded.reviewers().contains_key(node) {
            return Err(format!("--provider names unknown reviewer node `{node}`"));
        }
        if !loaded.packages().contains_key(node) {
            return Err(format!(
                "node `{node}` is an inline command; --provider is only valid for packaged Workers"
            ));
        }
    }
    for (node, package) in loaded.packages() {
        let runner = packaged_runner(&package.runner);
        if matches!(runner.as_str(), "claude" | "codex")
            && !options.provider_bindings.contains_key(node)
        {
            return Err(format!(
                "model-backed Worker `{node}` ({runner}) requires explicit `--provider {node}=PROVIDER_ID`; run `af provider doctor` with the same Campaign and selectors first"
            ));
        }
    }
    for node in store
        .provider_operation_nodes(run_id, authority.round_event_id())
        .map_err(|error| error.to_string())?
    {
        if !options.provider_bindings.contains_key(&node) {
            return Err(format!(
                "node `{node}` has Provider Admission state in this Round; repeat its explicit --provider binding"
            ));
        }
    }
    let expected_operations = options
        .provider_bindings
        .iter()
        .map(|(node, provider_id)| {
            let reviewer = loaded
                .reviewers()
                .get(node)
                .expect("provider binding node was validated");
            providers::operation_id_for(provider_id, node, reviewer, authority)
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if let Some(stale) = options
        .provider_resumes
        .keys()
        .find(|operation| !expected_operations.contains(*operation))
    {
        return Err(format!(
            "--resume-provider `{stale}` is stale or does not belong to a configured provider operation"
        ));
    }
    let replayed_spend = store
        .round_committed_tokens(run_id, authority.round_event_id())
        .map_err(|error| error.to_string())?;
    let mut provider_budget = BudgetLedger::default().with_committed(Scope::Run, replayed_spend);
    if let Some(budgets) = loaded.budgets() {
        provider_budget = provider_budget.with_limit(Scope::Run, Budget::of(budgets.run));
    }
    let mut resumes = options.provider_resumes.clone();
    let mut structural_probes = BTreeSet::new();
    let mut admissions = BTreeMap::new();
    for (node, provider_id) in &options.provider_bindings {
        let command = loaded
            .reviewers()
            .get(node)
            .expect("provider binding node was validated");
        let admission = providers::admit(
            provider_id,
            providers::AdmissionRequest {
                node_id: node,
                reviewer: command,
                state_dir: state,
                run_id,
                authority,
                cas,
                store,
                resumes: &mut resumes,
                budget: &mut provider_budget,
                structural_probes: &mut structural_probes,
            },
        )?;
        admissions.insert(node.clone(), admission);
    }
    if let Some((operation, _)) = resumes.first_key_value() {
        return Err(format!(
            "--resume-provider `{operation}` is stale or does not belong to a configured provider operation"
        ));
    }
    let has_dispatched_worker = store
        .replay(run_id)
        .map_err(|error| error.to_string())?
        .iter()
        .any(|event| {
            event.event_type == EventType::AttemptDispatchedV1
                && event.causation_id.as_deref() == Some(authority.round_event_id())
        });
    if !has_dispatched_worker && let Some(budgets) = loaded.budgets() {
        let committed = store
            .round_committed_tokens(run_id, authority.round_event_id())
            .map_err(|error| error.to_string())?;
        let static_reservation = project::max_simultaneous_reservation(loaded)?.unwrap_or(0);
        require_static_attempt_capacity(
            static_reservation,
            budgets.run,
            loaded.reviewers().len(),
            loaded.max_parallel(),
            committed,
        )?;
    }
    Ok(admissions)
}

pub(crate) fn provider_doctor(options: &Options) -> Result<(), String> {
    if options.campaign.is_none() {
        return Err("provider doctor requires `--campaign NAME` so admission evidence can be reused by review run".into());
    }
    let state = options.resolved_state_dir()?;
    std::fs::create_dir_all(&state).map_err(|error| error.to_string())?;
    let cas = Cas::open(state.join("cas")).map_err(|error| error.to_string())?;
    let mut store =
        EventStore::open(state.join("events.sqlite")).map_err(|error| error.to_string())?;
    let git_home = state.join("git-home");
    std::fs::create_dir_all(&git_home).map_err(|error| error.to_string())?;
    let repo = Repo::open(&options.repo, &git_home)
        .with_timeout(authority::requested_git_timeout(options.git_timeout));
    let authority::PreparedRun {
        loaded,
        run_id,
        authority,
        ..
    } = authority::prepare(options, &cas, &mut store, &repo)?;
    let admissions = admit_review_providers(
        options, &state, &cas, &mut store, &run_id, &loaded, &authority,
    )?;
    let spent = store
        .round_committed_tokens(&run_id, authority.round_event_id())
        .map_err(|error| error.to_string())?;
    if options.json {
        println!(
            "{}",
            serde_json::json!({
                "schema": "af/provider-doctor@1",
                "ready": true,
                "run_id": run_id,
                "round": authority.round(),
                "epoch": authority.epoch(),
                "admitted_nodes": admissions.keys().collect::<Vec<_>>(),
                "committed_tokens": spent,
                "gates_run": false,
                "workers_dispatched": false,
            })
        );
    } else {
        println!("provider doctor: ready");
        for node in admissions.keys() {
            println!("  admitted {node}");
        }
        println!("  committed {spent} tokens; no Gates or Workers ran");
    }
    Ok(())
}

pub(crate) fn run(options: &Options) -> Result<RunVerdict, String> {
    let state = options.resolved_state_dir()?;
    std::fs::create_dir_all(&state).map_err(|error| error.to_string())?;
    let cas = Cas::open(state.join("cas")).map_err(|error| error.to_string())?;
    let mut store =
        EventStore::open(state.join("events.sqlite")).map_err(|error| error.to_string())?;
    let home = std::env::var("HOME").map_err(|error| error.to_string())?;
    let git_home = state.join("git-home");
    std::fs::create_dir_all(&git_home).map_err(|error| error.to_string())?;
    let repo = Repo::open(&options.repo, &git_home)
        .with_timeout(authority::requested_git_timeout(options.git_timeout));

    let authority::PreparedRun {
        loaded,
        snapshot,
        run_id,
        focus,
        timeout,
        check_timeout,
        git_timeout,
        convergence,
        authority,
        ledger_projection,
    } = authority::prepare(options, &cas, &mut store, &repo)?;
    let authority_receipt = ReviewAuthority {
        authority_snapshot_id: authority.authority_snapshot_id().to_string(),
        campaign_manifest_id: authority.campaign_manifest_id().to_string(),
        subject_id: authority.subject_id().to_string(),
        head_snapshot_id: authority.head_snapshot_id().to_string(),
        round: authority.round(),
        epoch: authority.epoch(),
    };
    run_progress(
        options,
        format_args!(
            "mode     {} ({} clean, {} max Round{})",
            options.mode.as_str(),
            convergence.clean_rounds,
            convergence.max_rounds,
            if convergence.max_rounds == 1 { "" } else { "s" }
        ),
    );
    run_progress(options, format_args!("run      {run_id}"));
    run_progress(
        options,
        format_args!(
            "timeouts reviewer {}s, checks {}s, git capture {}s (pinned)",
            timeout.as_secs(),
            check_timeout.as_secs(),
            git_timeout.as_secs()
        ),
    );
    run_progress(
        options,
        format_args!(
            "parallel {} Workers at once (pinned pipeline max_parallel)",
            loaded.max_parallel()
        ),
    );

    // Every model Worker's first-Attempt input, measured against the cap its dispatch reserves,
    // before any Gate, Provider admission, or Worker: an input that alone exhausts the cap is
    // refused here, with nothing charged, rather than after paying for the whole prompt.
    {
        let manifest: review_core::CampaignManifestV1 = serde_json::from_value(
            cas.get_json(authority.campaign_manifest_id())
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let pipeline_bytes = cas
            .get(&manifest.pipeline.artifact_id)
            .map_err(|error| error.to_string())?;
        let definition = review_config::Definition::from_toml(
            std::str::from_utf8(&pipeline_bytes)
                .map_err(|error| format!("pinned pipeline is not UTF-8: {error}"))?,
        )
        .map_err(|error| error.to_string())?;
        let change_set = authority.change_set().map_or(
            authority::ChangeSetSource::None,
            authority::ChangeSetSource::Resolved,
        );
        let sizes = authority::worker_input_sizes(
            &cas,
            &definition,
            &loaded,
            change_set,
            focus.as_deref(),
        )?;
        for size in &sizes {
            run_progress(
                options,
                format_args!(
                    "input    {} {} bytes (about {} tokens){}",
                    size.node,
                    size.input_bytes,
                    size.input_tokens,
                    size.cap_tokens
                        .map(|cap| format!(" of a {cap}-token Attempt cap"))
                        .unwrap_or_default()
                ),
            );
        }
        authority::refuse_unfit_inputs(&sizes)?;
    }

    let admissions = admit_review_providers(
        options, &state, &cas, &mut store, &run_id, &loaded, &authority,
    )?;

    let auth = (std::env::var("USER").ok(), home.clone());
    let mut kernel = Kernel::from_loaded(&cas, &mut store, &run_id, snapshot, &loaded, authority)?
        .with_ledger_projection(ledger_projection)?
        .with_checks(loaded.checks().to_vec())
        .with_cache_source_resolver(caches::resolve_kind)
        .with_check_timeout(check_timeout);
    if let Some(budgets) = loaded.budgets() {
        run_progress(
            options,
            format_args!(
                "budgets  {} attempt reservation, {} run admission cap (chargeable tokens)",
                budgets.attempt, budgets.run
            ),
        );
        kernel = kernel.with_budgets(budgets.attempt, budgets.run);
    }

    let mut bound: BTreeMap<String, String> = BTreeMap::new();
    for (node, command) in loaded.reviewers() {
        let adapter: Box<dyn ReviewerAdapter> = match loaded.packages().get(node) {
            Some(package) => {
                let program = packaged_runner(command);
                match program.as_str() {
                    "claude" => {
                        let user = auth.0.clone().ok_or_else(|| {
                            format!("node `{node}`: Claude subscription auth requires USER")
                        })?;
                        let mut adapter =
                            review_runner_claude::ClaudeAdapter::from_package(package, timeout)
                                .map_err(|error| format!("{node}: {error}"))?
                                .with_auth(
                                    Some(
                                        admissions
                                            .get(node)
                                            .expect("model-backed package was explicitly admitted")
                                            .auth_dir_string()?,
                                    ),
                                    user,
                                    auth.1.clone(),
                                );
                        if let Some(focus) = &focus {
                            adapter = adapter.with_focus(focus);
                        }
                        Box::new(adapter)
                    }
                    "codex" => {
                        let codex_home = admissions
                            .get(node)
                            .expect("model-backed package was explicitly admitted")
                            .auth_dir_string()?;
                        let mut adapter =
                            review_runner_codex::CodexAdapter::from_package(package, timeout)
                                .map_err(|error| format!("{node}: {error}"))?
                                .with_codex_home(codex_home);
                        if let Some(focus) = &focus {
                            adapter = adapter.with_focus(focus);
                        }
                        Box::new(adapter)
                    }
                    other => {
                        return Err(format!(
                            "node `{node}`: no adapter drives `{other}`; this af release knows \
                             claude and codex"
                        ));
                    }
                }
            }
            None => Box::new(review_runner::CommandAdapter::new(command.clone(), timeout)),
        };
        bound.insert(node.clone(), command.program.clone());
        kernel = kernel.with_adapter(node.clone(), adapter);
    }
    for (node, program) in &bound {
        run_progress(options, format_args!("reviewer {node} -> {program}"));
    }

    let report = loaded.run(&kernel).map_err(|error| error.to_string())?;
    run_progress(options, format_args!(""));
    for (node, outcome) in &report.outcomes {
        match outcome {
            NodeOutcome::Completed { .. } => {
                run_progress(options, format_args!("  done      {node}"))
            }
            NodeOutcome::Failed { error, .. } => {
                run_progress(options, format_args!("  FAILED    {node}: {error}"))
            }
            NodeOutcome::Suppressed { reason } => run_progress(
                options,
                format_args!(
                    "  never-ran {node}: {}",
                    authority::serde_name(&review_pipeline::run_suppression_reason(*reason))
                ),
            ),
        }
    }

    let ledger = kernel.ledger();
    print_scope_authority_warnings(&ledger);
    run_progress(options, format_args!(""));
    let finding_views = ledger.finding_views();
    run_progress(options, format_args!("findings {}", finding_views.len()));
    for finding in finding_views {
        run_progress(
            options,
            format_args!(
                "  [{}] {}:{} - {} ({})",
                authority::serde_name(&finding.severity),
                finding.file,
                finding
                    .line
                    .map_or("?".to_string(), |line| line.to_string()),
                finding.title,
                finding.status.as_str()
            ),
        );
    }
    let open_or_stale_demand_ids = ledger
        .demand_views()
        .into_iter()
        .filter(|demand| {
            demand.requirement == review_core::DemandRequirement::Required
                && matches!(
                    demand.status,
                    review_core::DemandStatus::Open | review_core::DemandStatus::Stale
                )
        })
        .map(|demand| demand.demand_id)
        .collect::<Vec<_>>();
    run_progress(
        options,
        format_args!(
            "demands  {} open/stale (required)",
            open_or_stale_demand_ids.len()
        ),
    );
    let spent_tokens = kernel.spent();
    if let Some(spent) = spent_tokens {
        run_progress(options, format_args!("spent    {spent} tokens"));
    }

    let verdict = kernel.publish_report(&report, convergence)?;
    let attempts = kernel.selected_attempt_evidence()?;
    drop(kernel);
    let events = store.replay(&run_id).map_err(|error| error.to_string())?;
    let latest_evidence = latest_round_evidence(&events, &cas)?;
    if !options.json
        && let Some(evidence) = latest_evidence.as_ref()
        && evidence.ledger_was_not_produced()
        && !evidence.available_node_results.is_empty()
    {
        run_progress(options, format_args!(""));
        run_progress(
            options,
            format_args!("recorded, not gathered (Ledger was not produced):"),
        );
        for result in &evidence.available_node_results {
            run_progress(
                options,
                format_args!(
                    "  {} attempt {} artifact {} spend {} severities [{}]",
                    result.node,
                    result.attempt_id,
                    result.result_artifact_id,
                    result.spend_tokens,
                    result.severities.join(", ")
                ),
            );
            for finding in &result.findings {
                run_progress(
                    options,
                    format_args!(
                        "    [{}] {}",
                        finding["severity"].as_str().unwrap_or("unknown"),
                        finding["title"].as_str().unwrap_or("untitled finding")
                    ),
                );
            }
        }
    }
    if options.json {
        let candidate = candidate_identity()?;
        let findings = ledger
            .finding_views()
            .into_iter()
            .map(|finding| {
                serde_json::json!({
                    "key": finding.key,
                    "severity": authority::serde_name(&finding.severity),
                    "effective_severity": finding.convergence_severity
                        .map(|severity| authority::serde_name(&severity)),
                    "status": finding.status.as_str(),
                    "scope": finding.convergence_scope_label(),
                    "file": finding.file,
                    "line": finding.line,
                    "title": finding.title,
                    "aliases": finding.aliases,
                })
            })
            .collect::<Vec<_>>();
        let node_outcomes = report
            .outcomes
            .iter()
            .map(|(node, outcome)| match outcome {
                NodeOutcome::Completed { outputs } => serde_json::json!({
                    "node": node,
                    "kind": "completed",
                    "output_artifacts": outputs.values().flatten().collect::<Vec<_>>(),
                }),
                NodeOutcome::Failed { error, .. } => serde_json::json!({
                    "node": node,
                    "kind": "failed",
                    "error": error,
                }),
                NodeOutcome::Suppressed { reason } => serde_json::json!({
                    "node": node,
                    "kind": "suppressed",
                    "reason": authority::serde_name(&review_pipeline::run_suppression_reason(*reason)),
                }),
            })
            .collect::<Vec<_>>();
        let rendered_bytes = attempts
            .iter()
            .map(|attempt| attempt.context_manifest.rendered_bytes)
            .fold(0, u64::saturating_add);
        let estimated_tokens = attempts
            .iter()
            .map(|attempt| attempt.context_manifest.estimated_tokens)
            .fold(0, u64::saturating_add);
        let usage = aggregate_usage(&attempts);
        let attempt_values = attempts
            .iter()
            .map(|attempt| {
                serde_json::json!({
                    "node": &attempt.node,
                    "attempt_id": &attempt.attempt_id,
                    "cost_tokens": attempt.cost_tokens,
                    "usage": &attempt.usage,
                    "context_manifest": &attempt.context_manifest,
                    "raw_artifact": &attempt.raw_artifact,
                    "result_artifact": &attempt.result_artifact,
                })
            })
            .collect::<Vec<_>>();
        let mut outcome_value = serde_json::json!({
            "schema": "af/review-outcome@1",
            "campaign_mode": options.mode.as_str(),
            "candidate": {
                "version": candidate.version,
                "executable": candidate.executable,
                "binary_sha256": candidate.binary_sha256,
            },
            "run_id": run_id,
            "authority": {
                "authority_snapshot_id": authority_receipt.authority_snapshot_id,
                "campaign_manifest_id": authority_receipt.campaign_manifest_id,
                "subject_id": authority_receipt.subject_id,
                "head_snapshot_id": authority_receipt.head_snapshot_id,
                "round": authority_receipt.round,
                "epoch": authority_receipt.epoch,
            },
            "node_outcomes": node_outcomes,
            "blocked_gates": report.blocked_gates,
            "attempts": attempt_values,
            "totals": {
                "context": {
                    "rendered_bytes": rendered_bytes,
                    "estimated_tokens": estimated_tokens,
                },
                "usage": usage,
                "spent_tokens": spent_tokens,
                "open_required_demands": open_or_stale_demand_ids.len(),
                "open_or_stale_demand_ids": open_or_stale_demand_ids,
            },
            "findings": findings,
            "outcome": verdict_value(&verdict),
            "next_action": next_action_value(options.mode, &verdict),
        });
        if let Some(evidence) = latest_evidence
            && evidence.ledger_was_not_produced()
        {
            let object = outcome_value.as_object_mut().expect("outcome object");
            object.insert(
                "ledger_production".into(),
                serde_json::Value::String(evidence.ledger_production.into()),
            );
            object.insert(
                "available_node_results".into(),
                serde_json::to_value(evidence.available_node_results)
                    .map_err(|error| error.to_string())?,
            );
        }
        println!(
            "{}",
            serde_json::to_string(&outcome_value).map_err(|error| error.to_string())?
        );
    } else {
        run_progress(options, format_args!("verdict  {verdict:?}"));
    }
    match (options.mode, &verdict) {
        (CampaignMode::Light, RunVerdict::Fail(_)) => run_progress(
            options,
            format_args!(
                "next     fix the findings, run the deterministic project gate, then stop; do not start another Campaign (use --heavy only by explicit human choice)"
            ),
        ),
        (CampaignMode::Light, RunVerdict::Incomplete { .. }) => run_progress(
            options,
            format_args!("next     resume this exact incomplete light Round"),
        ),
        _ => {}
    }
    Ok(verdict)
}
