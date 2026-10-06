use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::task::host::*;
use crate::task::*;
use review_config::task::catalog::*;
use review_core::Producer;
use review_core::task::execution::{TaskInvocationV1, TaskOutputV1};
use review_core::task::pipeline::*;
use review_core::task::plan::*;
use review_core::task::*;
use review_graph::task::{CompiledTask, OperatorAttemptCost, OperatorSignature};
use review_store::store::task::execution::PreparedTaskAttempt;
use review_store::{Cas, EventStore};
use serde_json::json;

use review_core::task::report::TaskRunReportV1;
use review_graph::RunReport;

// Reuse the exact admitted command Task fixture, not a manufactured projection.
include!("../../tests/support/task_runtime_fixture.rs");

fn with_report(entry_ms: u64, test: impl FnOnce(&mut TaskRuntime<'_, '_>, &RunReport)) {
    let mut f = Fixture::new(SUCCESS);
    let host = CapturedTaskHost::capture_with_models(
        &f.cas,
        &f.compiler,
        &f.task,
        &f.plan,
        f.graph.clone(),
        &EmptyTaskEnvironment,
        &DocumentDomain,
        &BTreeMap::new(),
    )
    .unwrap();
    let authority = CapturedTaskAuthority::new(&f.compiler, &host, &NoTaskDeveloper);
    let lease = f
        .store
        .open_task(&f.cas, &f.revision_id, "report-entry", 60_000)
        .unwrap();
    f.store
        .propose_task_plan(&f.cas, &lease, &f.plan_id, &authority)
        .unwrap();
    f.store.admit_task_plan(&f.cas, &lease, &authority).unwrap();
    let report = {
        let runtime =
            TaskRuntime::new(&mut f.store, &f.cas, lease.clone(), &authority, &host).unwrap();
        runtime.execute().unwrap()
    };
    assert!(report.complete());
    // execute's heartbeat has joined. Release and reacquire through the production API
    // to set entry timing: renewal correctly refuses to shorten an existing expiry.
    f.store.release_task_lease(&f.cas, &lease).unwrap();
    let lease = f
        .store
        .take_task_lease(&f.cas, &f.task.task_id, "report-entry-next", entry_ms)
        .unwrap();
    let mut runtime = TaskRuntime::new(&mut f.store, &f.cas, lease, &authority, &host).unwrap();
    // Direct capture cannot be rescued by a scheduler-dependent heartbeat tick.
    test(&mut runtime, &report);
}

#[test]
fn due_report_entry_renews_before_prefix_without_reexecuting_or_recharging() {
    with_report(8_000, |runtime, report| {
        let before = runtime.projection().unwrap();
        let before_execution = before.execution.as_ref().unwrap();
        let before_expiry = runtime
            .store
            .lock()
            .unwrap()
            .task_lease_state(&runtime.lease)
            .unwrap();
        let id = runtime.capture_run_report(report, None).unwrap();
        let captured: TaskRunReportV1 =
            serde_json::from_value(runtime.cas.get_artifact(&id).unwrap().payload).unwrap();
        assert_eq!(
            captured.through_sequence,
            before.next_sequence + 1,
            "the due renewal must precede the newly read report prefix"
        );
        let after = runtime.projection().unwrap();
        assert_eq!(
            after.next_sequence,
            before.next_sequence + 2,
            "exactly one renewal and one report; no duplicate effects"
        );
        assert_eq!(after.run_reports.last(), Some(&id));
        let after_execution = after.execution.as_ref().unwrap();
        assert_eq!(
            after_execution.budget.begun_attempts(),
            before_execution.budget.begun_attempts()
        );
        assert_eq!(
            after_execution.budget.committed_tokens(),
            before_execution.budget.committed_tokens()
        );
        assert_eq!(
            after_execution.budget.reserved_tokens(),
            before_execution.budget.reserved_tokens()
        );
        assert_eq!(
            format!("{:?}", after_execution.attempt_accounting()),
            format!("{:?}", before_execution.attempt_accounting())
        );
        assert_eq!(
            serde_json::to_value(&after_execution.outputs).unwrap(),
            serde_json::to_value(&before_execution.outputs).unwrap()
        );
        assert!(
            runtime
                .store
                .lock()
                .unwrap()
                .task_lease_state(&runtime.lease)
                .unwrap()
                > before_expiry
        );
    });
}

#[test]
fn report_entry_with_full_lease_does_not_renew_or_change_accounting() {
    with_report(60_000, |runtime, report| {
        let before = runtime.projection().unwrap();
        let expiry = runtime
            .store
            .lock()
            .unwrap()
            .task_lease_state(&runtime.lease)
            .unwrap();
        let id = runtime.capture_run_report(report, None).unwrap();
        let captured: TaskRunReportV1 =
            serde_json::from_value(runtime.cas.get_artifact(&id).unwrap().payload).unwrap();
        let after = runtime.projection().unwrap();
        assert_eq!(captured.through_sequence, before.next_sequence);
        assert_eq!(after.next_sequence, before.next_sequence + 1);
        assert_eq!(
            runtime
                .store
                .lock()
                .unwrap()
                .task_lease_state(&runtime.lease)
                .unwrap(),
            expiry
        );
        assert_eq!(
            format!("{:?}", after.execution.unwrap().attempt_accounting()),
            format!("{:?}", before.execution.unwrap().attempt_accounting())
        );
    });
}

#[test]
fn report_entry_refuses_released_replaced_and_expired_writers_without_append() {
    for mode in ["released", "replaced", "expired"] {
        with_report(60_000, |runtime, report| {
            let successor = {
                let mut store = runtime.store.lock().unwrap();
                if mode == "expired" {
                    store
                        .release_task_lease(runtime.cas, &runtime.lease)
                        .unwrap();
                    // Install only a genuine capability returned by the fenced Store API.
                    runtime.lease = store
                        .take_task_lease(runtime.cas, runtime.lease.task_id(), "expiring", 1)
                        .unwrap();
                    // Wait for the actual lease boundary, not an arbitrary scheduling sleep.
                    let stop = std::time::Instant::now() + std::time::Duration::from_secs(1);
                    while store.task_lease_state(&runtime.lease).is_ok() {
                        assert!(
                            std::time::Instant::now() < stop,
                            "one-ms lease did not expire"
                        );
                        std::thread::yield_now();
                    }
                    None
                } else {
                    store
                        .release_task_lease(runtime.cas, &runtime.lease)
                        .unwrap();
                    if mode == "replaced" {
                        Some(
                            store
                                .take_task_lease(
                                    runtime.cas,
                                    runtime.lease.task_id(),
                                    "successor",
                                    60_000,
                                )
                                .unwrap(),
                        )
                    } else {
                        None
                    }
                }
            };
            let before = runtime.projection().unwrap();
            assert!(runtime.capture_run_report(report, None).is_err(), "{mode}");
            let after = runtime.projection().unwrap();
            assert_eq!(
                after.next_sequence, before.next_sequence,
                "{mode}: no renewal/report appended"
            );
            assert_eq!(after.run_reports, before.run_reports);
            assert_eq!(
                format!("{:?}", after.execution.unwrap().attempt_accounting()),
                format!("{:?}", before.execution.unwrap().attempt_accounting())
            );
            if let Some(successor) = successor {
                assert!(
                    runtime
                        .store
                        .lock()
                        .unwrap()
                        .task_lease_state(&successor)
                        .is_ok()
                );
            }
        });
    }
}
