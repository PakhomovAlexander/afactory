//! Kill the installed CLI between durable dispatch and output, then recover with the real lease.
use super::*;
use review_core::task::execution::TaskAttemptResultV1;
use review_core::task::{TaskAcceptanceV1, TaskExecutionV1, TaskPhaseV1, TaskResultV1};
use review_graph::task::{CompiledOperator, CompiledTask};
use review_store::store::task::TaskProjection;
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct BoundedCli {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl BoundedCli {
    fn spawn(directory: &Path, label: &str, repo: &Path, home: &Path, args: &[&str]) -> Self {
        let stdout = directory.join(format!("{label}.stdout"));
        let stderr = directory.join(format!("{label}.stderr"));
        let child = Command::new(env!("CARGO_BIN_EXE_af"))
            .current_dir(repo)
            .env("HOME", home)
            .args(["review"])
            .args(args)
            .stdout(Stdio::from(std::fs::File::create(&stdout).unwrap()))
            .stderr(Stdio::from(std::fs::File::create(&stderr).unwrap()))
            .spawn()
            .unwrap();
        Self {
            child,
            stdout,
            stderr,
        }
    }

    fn output(&self, status: ExitStatus) -> Output {
        Output {
            status,
            stdout: std::fs::read(&self.stdout).unwrap(),
            stderr: std::fs::read(&self.stderr).unwrap(),
        }
    }

    fn wait(&mut self) -> Output {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return self.output(status);
            }
            assert!(
                Instant::now() < deadline,
                "bounded Review CLI did not exit: {}",
                std::fs::read_to_string(&self.stderr).unwrap()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for BoundedCli {
    fn drop(&mut self) {
        // Covers assertion failures as well as success. The only fixture Worker descendant
        // sleeps for two seconds and then exits; no unbounded helper survives a killed CLI.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn projection(state: &Path) -> Option<TaskProjection> {
    let cas = Cas::open_existing(state.join("cas")).ok()?;
    let store = EventStore::open_read_only(state.join("events.sqlite")).ok()?;
    let tasks = store.task_ids(&cas).ok()?;
    match tasks.as_slice() {
        [] => None,
        [id] => store.task_projection(&cas, id).unwrap(),
        _ => panic!("one Campaign created multiple common Tasks"),
    }
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

#[test]
fn killed_review_resumes_original_task_after_real_lease_expiry_with_one_bounded_retry() {
    let directory = tempfile::tempdir().unwrap();
    let (repo, home, _, state) = fixture(directory.path(), true);
    let args = [
        "run",
        "--campaign",
        "timing",
        "--state",
        state.to_str().unwrap(),
        "--pipeline",
        ".af/pipelines/review.toml",
        "--policy-rev",
        "HEAD",
    ];
    let mut original_cli = BoundedCli::spawn(directory.path(), "original", &repo, &home, &args);
    let started_deadline = Instant::now() + Duration::from_secs(12);
    let (reviewer_node, gate_node) = loop {
        if let Some(status) = original_cli.child.try_wait().unwrap() {
            let output = original_cli.output(status);
            panic!(
                "original Review exited before interruption: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        if let Some(task) = projection(&state)
            && let Some(execution) = &task.execution
            && let Some(plan_id) = &task.plan_id
        {
            let cas = Cas::open_existing(state.join("cas")).unwrap();
            let plan: review_core::task::plan::ExecutionPlanV1 =
                serde_json::from_value(cas.get_artifact(plan_id).unwrap().payload).unwrap();
            let graph: CompiledTask =
                serde_json::from_value(cas.get_artifact(&plan.compiled_graph_id).unwrap().payload)
                    .unwrap();
            let node = |name| {
                graph
                    .nodes
                    .iter()
                    .find_map(|(id, node)| match &node.operator {
                        CompiledOperator::ReviewDomain { review_node, .. }
                            if review_node == name =>
                        {
                            Some(id.clone())
                        }
                        _ => None,
                    })
                    .unwrap()
            };
            let reviewer = node("reviewer");
            if execution.attempt_accounting().iter().any(|attempt| {
                attempt.reservation.node == reviewer && attempt.started && attempt.result.is_none()
            }) {
                assert_eq!(graph.allowances[&reviewer].max_attempts, 2);
                break (reviewer, node("gate"));
            }
        }
        assert!(
            Instant::now() < started_deadline,
            "Reviewer never durably started"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    original_cli.child.kill().unwrap();
    let killed = original_cli.child.wait().unwrap();
    assert_eq!(
        killed.signal(),
        Some(9),
        "fixture must exercise real SIGKILL"
    );

    let original = projection(&state).unwrap();
    let original_execution = original.execution.as_ref().unwrap();
    let original_attempts = original_execution.attempt_accounting();
    let interrupted = original_attempts
        .iter()
        .find(|a| a.reservation.node == reviewer_node)
        .unwrap();
    assert!(interrupted.started);
    assert!(
        interrupted.result.is_none(),
        "kill must precede settlement/publication"
    );
    let gate = original_attempts
        .iter()
        .find(|a| a.reservation.node == gate_node)
        .unwrap();
    assert!(matches!(
        gate.result,
        Some(TaskAttemptResultV1::Succeeded { .. })
    ));
    assert_eq!(original_execution.budget.begun_attempts(), 2);
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let store = EventStore::open_read_only(state.join("events.sqlite")).unwrap();
    let task_run = review_store::store::task::task_run_id(&original.task_id).unwrap();
    let before_task = store.replay(&task_run).unwrap();
    let before_review = store.replay("campaign-timing").unwrap();
    assert!(!before_review.iter().any(|e| e.event_type.is_run_report()));
    let rows = store.task_attempt_wall(&task_run).unwrap();
    let observed_floor = rows
        .iter()
        .filter(|row| row.attempt_id == interrupted.attempt_id)
        .filter_map(|row| {
            row.usage
                .as_ref()
                .map(|usage| usage.chargeable_tokens.get())
        })
        .max()
        .unwrap_or(0)
        .max(interrupted.charged_tokens);
    let expected_recovery_charge = observed_floor.max(u128::from(interrupted.reservation.tokens));
    let lease_until = original.lease_until_unix_ms();
    assert!(now_ms() < lease_until);
    // Prove the real writer used the unchanged fifteen-second lease, including any heartbeat.
    let lease_record = before_task
        .iter()
        .rev()
        .find(|event| event.payload["change"]["lease_until_unix_ms"].as_u64() == Some(lease_until))
        .unwrap();
    assert_eq!(
        lease_until - lease_record.payload["now_unix_ms"].as_u64().unwrap(),
        15_000
    );
    let wait_deadline = Instant::now() + Duration::from_secs(16);
    while now_ms() <= lease_until {
        assert!(
            Instant::now() < wait_deadline,
            "recorded lease did not expire on the real clock"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        store.replay(&task_run).unwrap(),
        before_task,
        "no synthetic lease release or renewal"
    );
    let mut resumed_cli = BoundedCli::spawn(directory.path(), "resumed", &repo, &home, &args);
    checked(resumed_cli.wait(), 0);

    let resumed = projection(&state).unwrap();
    assert_eq!(resumed.task_id, original.task_id);
    assert_eq!(resumed.revision_id, original.revision_id);
    assert_eq!(
        resumed.revision, original.revision,
        "Source, Round roots, policy, limits and deadline must be identical"
    );
    assert_eq!(resumed.plan_id, original.plan_id);
    assert!(resumed.review_handoffs.is_empty());
    let execution = resumed.execution.as_ref().unwrap();
    let attempts = execution.attempt_accounting();
    assert_eq!(execution.budget.begun_attempts(), 3);
    assert_eq!(attempts.len(), 3);
    let old = attempts
        .iter()
        .find(|a| a.attempt_id == interrupted.attempt_id)
        .unwrap();
    assert_eq!(old.reservation, interrupted.reservation);
    assert_eq!(old.invocation_id, interrupted.invocation_id);
    assert_eq!(old.charged_tokens, expected_recovery_charge);
    let Some(TaskAttemptResultV1::Abandoned { diagnostic_id }) = &old.result else {
        panic!("missing common disappearance recovery: {:?}", old.result)
    };
    let diagnostic = cas.get_json(diagnostic_id).unwrap();
    assert_eq!(diagnostic["attempt_id"], old.attempt_id);
    assert_eq!(diagnostic["reason"], "previous Task writer disappeared");
    let retry = attempts
        .iter()
        .find(|a| a.reservation.node == reviewer_node && a.attempt_id != old.attempt_id)
        .unwrap();
    assert_eq!(retry.invocation_id, old.invocation_id);
    assert_eq!(retry.plan_id, old.plan_id);
    assert!(matches!(
        retry.result,
        Some(TaskAttemptResultV1::Succeeded { .. })
    ));
    assert_eq!(
        attempts
            .iter()
            .filter(|a| a.reservation.node == gate_node)
            .count(),
        1
    );
    let retained_gate = attempts
        .iter()
        .find(|a| a.attempt_id == gate.attempt_id)
        .unwrap();
    assert_eq!(retained_gate.result, gate.result);
    assert_eq!(retained_gate.reservation, gate.reservation);
    assert_eq!(
        execution.budget.committed_tokens(),
        attempts.iter().map(|a| a.charged_tokens).sum::<u128>()
    );
    let TaskPhaseV1::Finished { result_id } = &resumed.phase else {
        panic!("resumed Task did not finish")
    };
    let result: TaskResultV1 =
        serde_json::from_value(cas.get_artifact(result_id).unwrap().payload).unwrap();
    assert_eq!(result.execution, TaskExecutionV1::Completed);
    assert_eq!(result.acceptance, TaskAcceptanceV1::Satisfied);
    let review = store.replay("campaign-timing").unwrap();
    for kind in [
        review_core::EventType::RoundStartedV1,
        review_core::EventType::SourceCapturedV1,
        review_core::EventType::CheckCompletedV1,
    ] {
        assert_eq!(
            review.iter().filter(|e| e.event_type == kind).count(),
            1,
            "{kind:?} must not repeat"
        );
    }
    assert!(review.starts_with(&before_review));
    assert!(
        !review
            .iter()
            .any(|e| e.event_type.as_str().starts_with("Attempt"))
    );
    let reports: Vec<_> = review
        .iter()
        .filter(|e| e.event_type.is_run_report())
        .collect();
    assert_eq!(reports.len(), 1);
    let report: review_core::RunReportPayloadV6 =
        serde_json::from_value(reports[0].payload.clone()).unwrap();
    report.validate().unwrap();
    assert_eq!(report.verdict, review_core::RunVerdictV3::Pass {});
    assert_eq!(
        report.spent_tokens.get(),
        execution.budget.committed_tokens()
    );
    let before_refusal = store.replay(&task_run).unwrap();
    let mut refused_cli = BoundedCli::spawn(directory.path(), "refused", &repo, &home, &args);
    let refused = checked(refused_cli.wait(), 1);
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("already completed its single review Round")
    );
    assert_eq!(store.task_ids(&cas).unwrap(), vec![original.task_id]);
    assert_eq!(store.replay(&task_run).unwrap(), before_refusal);
    assert_eq!(store.replay("campaign-timing").unwrap(), review);
}
