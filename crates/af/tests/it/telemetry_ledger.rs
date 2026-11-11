//! Design §7.5 and §15 P4: the Store commit mirror, Finding and Demand summaries, the two
//! accounting series and `af telemetry ledger`, end to end over a Store on disk: the kernel
//! commits through its own API with the mirror observing, and the reader answers from the
//! telemetry directory alone.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use af_contracts::task::usage::{TaskTokenUsageV3, TaskUsageObservationV1};
use af_runtime::telemetry::StoreMirror;
use af_store::EventStore;
use af_store::store::observer::{Commit, CommitObserver, WallCommit};
use af_telemetry::ledger::Ledger;
use af_telemetry::level::Level;
use af_telemetry::record::SealExit;
use af_telemetry::segment::writer::{Writer, WriterOptions};
use af_telemetry::subscriber::{TelemetrySubscriber, Typed};
use af_telemetry::view::{self, Resolved};
use serde_json::Value;

#[path = "telemetry_ledger/task_fixture.rs"]
pub(crate) mod task_fixture;

use task_fixture::TaskFixture;

/// One execution recording a run: its writer, the subscriber the kernel's spans reach, and the
/// `af.command` > `af.run` spans every commit of the execution happens under.
pub struct Recording {
    pub writer: Writer,
    dispatch: tracing::Dispatch,
    command: tracing::Span,
    run: tracing::Span,
}

impl Recording {
    /// An execution of the run `run_id` of the Store at `store`, writing to `root` at `level`.
    pub fn start(root: &Path, level: Level, store: &Path, run_id: &str, ordinal: u64) -> Self {
        Self::with_options(WriterOptions::new(root, level), store, run_id, ordinal)
    }

    pub fn with_options(
        mut options: WriterOptions,
        store: &Path,
        run_id: &str,
        ordinal: u64,
    ) -> Self {
        let level = options.level;
        options.redactor = af_telemetry::redact::Redactor::new();
        let writer = Writer::new(options).unwrap();
        let dispatch = tracing::Dispatch::new(TelemetrySubscriber::new(writer.clone()));
        let store_id = af_telemetry::trace::store_id(store).unwrap();
        // Native Review uses the same Task run identity as every other Task.
        let kind = if run_id.starts_with("task:") {
            "task"
        } else {
            "review"
        };
        let (command, run) = tracing::dispatcher::with_default(&dispatch, || {
            let command = tracing::info_span!("af.command", process.command = "af");
            let store = Typed::identity(store_id.clone()).unwrap();
            let run_id = Typed::identity(run_id).unwrap();
            let run = tracing::info_span!(
                parent: &command,
                "af.run",
                af.store_id = store.field(),
                af.run_id = run_id.field(),
                af.run.kind = Typed::enumerated(kind).field(),
                af.execution.ordinal = ordinal,
                af.ledger.coverage = tracing::field::Empty,
            );
            // As the Task runtime opens it: the mirror records from `spans` on.
            if level >= Level::Spans {
                run.record("af.ledger.coverage", "complete");
            }
            (command, run)
        });
        Self {
            writer,
            dispatch,
            command,
            run,
        }
    }

    /// The mirror of this execution, recording through its writer on every thread.
    pub fn mirror(&self) -> Arc<StoreMirror> {
        Arc::new(StoreMirror::with_writer(self.writer.clone()))
    }

    /// Run `work` inside the execution's run span.
    pub fn within<T>(&self, work: impl FnOnce() -> T) -> T {
        tracing::dispatcher::with_default(&self.dispatch, || {
            let _command = self.command.enter();
            let _run = self.run.enter();
            work()
        })
    }

    /// End the spans and seal the segment.
    pub fn finish(self) {
        let Self {
            writer,
            dispatch,
            command,
            run,
        } = self;
        tracing::dispatcher::with_default(&dispatch, || {
            drop(run);
            drop(command);
        });
        writer.finish(SealExit::Ok).unwrap();
    }
}

/// The ledger of `run_id` in the telemetry directory `root`, read from it alone.
pub fn read_ledger(root: &Path, run_id: &str) -> Ledger {
    let (index, read, unreadable) = view::open(root).unwrap();
    let Resolved::Run {
        store_id,
        trace_id,
        kind,
    } = view::resolve(&index, run_id, None).unwrap()
    else {
        panic!("run {run_id} is not recorded in {}", root.display());
    };
    af_telemetry::ledger::ledger(
        &index,
        root,
        read,
        unreadable,
        (&store_id, run_id, &trace_id, kind),
    )
    .unwrap()
}

/// `af telemetry … --dir root`, with a private home.
pub fn af(root: &Path, args: &[&str]) -> std::process::Output {
    let mut command = crate::common::af();
    command.args(args).arg("--dir").arg(root);
    command.output().unwrap()
}

/// `af telemetry ledger RUN --dir root --json`, validated against its schema.
pub fn ledger_json(root: &Path, run_id: &str) -> Value {
    let output = af(root, &["telemetry", "ledger", run_id, "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let document: Value = serde_json::from_slice(&output.stdout).unwrap();
    crate::schemas::valid(
        &crate::schemas::validator("telemetry-ledger-v1.json"),
        &document,
    );
    document
}

fn usage(input: Option<u128>, charge: u128) -> TaskTokenUsageV3 {
    TaskTokenUsageV3 {
        input_tokens: input.map(Into::into),
        chargeable_tokens: charge.into(),
        ..TaskTokenUsageV3::default()
    }
}

/// A telemetry directory and a Store directory beside it.
fn directories() -> (tempfile::TempDir, PathBuf, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    let store = tempfile::tempdir_in(temp.path()).unwrap();
    (temp, root, store)
}

/// Refused generic appends cannot appear in the Task commit mirror.
#[test]
fn telemetry_mirror_skips_rolled_back_transactions() {
    let (_temp, root, store_dir) = directories();
    let run_id = af_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
    let recording = Recording::start(&root, Level::Spans, store_dir.path(), &run_id, 1);
    let mirror = recording.mirror();
    let mut fixture = recording
        .within(|| TaskFixture::new(store_dir, |store| store.observe_commits(mirror.clone())));
    let logs = |store: &EventStore| {
        [run_id.clone(), "campaign-accounting".to_string()].map(|log| store.replay(&log).unwrap())
    };
    let before = logs(&fixture.store);
    recording.within(|| {
        let captured = af_store::NewEvent::new(
            af_contracts::EventType::SourceCapturedV1,
            serde_json::json!({}),
        );
        fixture
            .store
            .append(&run_id, &fixture.cas, captured.clone())
            .unwrap_err();
        fixture
            .store
            .append_batch(
                "campaign-accounting",
                &fixture.cas,
                &[
                    captured,
                    af_store::NewEvent::new(
                        af_contracts::EventType::GenerationAdvancedV1,
                        serde_json::json!({ "round": 9 }),
                    ),
                ],
            )
            .unwrap_err();
    });
    assert_eq!(logs(&fixture.store), before, "both rolled back whole");
    recording.finish();
    let ledger = read_ledger(&root, &run_id);
    assert_eq!(ledger.coverage, "complete", "{:?}", ledger.coverage_reasons);
    for log in &ledger.logs {
        let committed = fixture.store.replay(&log.run_id).unwrap();
        assert_eq!(log.mirrored, committed.len() as u64, "{}", log.run_id);
        assert_eq!(log.last_sequence + 1, committed.len() as u64);
    }
    assert_eq!(
        ledger.logs.len(),
        1,
        "only the committed native Task log: {:?}",
        ledger.logs
    );
    assert!(ledger.logs.iter().all(|log| log.run_id == run_id));
    assert!(
        fixture
            .store
            .replay("campaign-accounting")
            .unwrap()
            .is_empty()
    );
}

/// Tells the mirror of every commit, first corrupting the stored bytes of the CAS object it was
/// armed with on the first commit after it was armed: after the Store validated the commit's
/// artifacts and before the mirror reads them.
struct CorruptBeforeMirror {
    mirror: Arc<StoreMirror>,
    armed: std::sync::Mutex<Option<PathBuf>>,
}

impl CommitObserver for CorruptBeforeMirror {
    fn committed(&self, commit: &Commit<'_>) {
        if let Some(object) = self.armed.lock().unwrap().take() {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&object, std::fs::Permissions::from_mode(0o600)).unwrap();
            let mut bytes = std::fs::read(&object).unwrap();
            bytes[0] = if bytes[0] == b' ' { b'\t' } else { b' ' };
            std::fs::write(&object, bytes).unwrap();
        }
        self.mirror.committed(commit);
    }

    fn wall_recorded(&self, commit: &WallCommit<'_>) {
        self.mirror.wall_recorded(commit);
    }
}

/// Design §7.5: a committed usage observation whose raw native observation the CAS holds but
/// cannot read (corrupt, not absent) is the mirror's failure, never a blob to pass over: one
/// `observer` problem names the artifact, the ledger reads incomplete, and the charge snapshot
/// keeps the projection's figures without a native observation.
#[test]
fn telemetry_mirror_corrupt_usage_artifact_marks_coverage_incomplete() {
    let (_temp, root, store_dir) = directories();
    let run_id = af_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
    let recording = Recording::start(&root, Level::Spans, store_dir.path(), &run_id, 1);
    let observer = Arc::new(CorruptBeforeMirror {
        mirror: recording.mirror(),
        armed: std::sync::Mutex::new(None),
    });
    let mut f = recording
        .within(|| TaskFixture::new(store_dir, |store| store.observe_commits(observer.clone())));
    let (attempt, artifact) = recording.within(|| {
        let attempt = f.start("write");
        let artifact = f.native(
            &attempt,
            "write",
            &TaskUsageObservationV1 {
                reported_usage: Some(usage(Some(6), 6)),
                charge_complete: true,
            },
        );
        f.renew();
        let hex = artifact.strip_prefix("sha256:").unwrap();
        *observer.armed.lock().unwrap() = Some(
            f.directory
                .path()
                .join("cas/objects")
                .join(&hex[..2])
                .join(&hex[2..]),
        );
        // The Store validates the observation and commits; the observer finds it corrupt.
        f.observe_raw(&attempt, usage(Some(6), 6), vec![artifact.clone()]);
        assert!(
            observer.armed.lock().unwrap().is_none(),
            "corrupted on commit"
        );
        assert!(f.cas.get_optional_artifact(&artifact).is_err());
        (attempt, artifact)
    });
    recording.finish();
    let ledger = read_ledger(&root, &run_id);
    assert_eq!(ledger.coverage, "incomplete");
    assert!(
        ledger
            .coverage_reasons
            .iter()
            .any(|reason| reason.contains("Store mirror problem")),
        "{:?}",
        ledger.coverage_reasons
    );
    let (index, _, _) = view::open(&root).unwrap();
    let problems: Vec<(String, String)> = index
        .connection()
        .prepare("SELECT what, reason FROM problems ORDER BY seq")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(index);
    let named = format!("raw artifact {}…", &artifact[..23]);
    assert!(
        problems.iter().any(|(what, reason)| what == "observer"
            && reason.contains(&named)
            && reason.contains("usage_observed")
            && reason.contains("do not match its digest")),
        "{named}: {problems:?}"
    );
    let charge = ledger
        .accounting
        .attempts
        .iter()
        .find(|recorded| recorded.attempt_id == attempt)
        .and_then(|recorded| recorded.charge.clone())
        .expect("the charge snapshot is kept");
    assert_eq!(charge.source, "usage_observed");
    assert_eq!(charge.counters.chargeable_tokens.as_deref(), Some("6"));
    assert_eq!(charge.counters.input_tokens.as_deref(), Some("6"));
    assert_eq!(charge.native, None, "no native observation");
}

/// One committed usage record naming two raw artifacts, a valid native observation and a raw
/// blob, the one at `corrupt` (0 or 1) the blob, corrupted after the Store validated the
/// record: the mirror reads both, one `observer` problem names the corrupt one alone, the
/// ledger reads incomplete, and the charge snapshot carries no native observation although
/// the observation is valid.
fn corrupt_beside_a_valid_observation(corrupt: usize) {
    let (_temp, root, store_dir) = directories();
    let run_id = af_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
    let recording = Recording::start(&root, Level::Spans, store_dir.path(), &run_id, 1);
    let observer = Arc::new(CorruptBeforeMirror {
        mirror: recording.mirror(),
        armed: std::sync::Mutex::new(None),
    });
    let mut f = recording
        .within(|| TaskFixture::new(store_dir, |store| store.observe_commits(observer.clone())));
    let (attempt, raw) = recording.within(|| {
        let attempt = f.start("write");
        let valid = f.native(
            &attempt,
            "write",
            &TaskUsageObservationV1 {
                reported_usage: Some(usage(Some(6), 6)),
                charge_complete: true,
            },
        );
        // A record names one observation at most; the other raw artifact is a plain blob.
        let damaged = f
            .cas
            .put_json(&serde_json::json!({ "native": "transcript of write" }))
            .unwrap();
        let mut raw = vec![valid];
        raw.insert(corrupt, damaged);
        f.renew();
        let hex = raw[corrupt].strip_prefix("sha256:").unwrap();
        *observer.armed.lock().unwrap() = Some(
            f.directory
                .path()
                .join("cas/objects")
                .join(&hex[..2])
                .join(&hex[2..]),
        );
        f.observe_raw(&attempt, usage(Some(6), 6), raw.clone());
        assert!(
            observer.armed.lock().unwrap().is_none(),
            "corrupted on commit"
        );
        assert!(f.cas.get_optional_artifact(&raw[corrupt]).is_err());
        assert!(f.cas.get_optional_artifact(&raw[1 - corrupt]).is_ok());
        (attempt, raw)
    });
    recording.finish();
    let ledger = read_ledger(&root, &run_id);
    assert_eq!(ledger.coverage, "incomplete");
    assert!(
        ledger
            .coverage_reasons
            .iter()
            .any(|reason| reason.contains("Store mirror problem")),
        "{:?}",
        ledger.coverage_reasons
    );
    let (index, _, _) = view::open(&root).unwrap();
    let problems: Vec<(String, String)> = index
        .connection()
        .prepare("SELECT what, reason FROM problems ORDER BY seq")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(index);
    let named = |id: &str| format!("raw artifact {}…", &id[..23]);
    let naming = |id: &str| {
        problems
            .iter()
            .filter(|(what, reason)| {
                what == "observer"
                    && reason.contains(&named(id))
                    && reason.contains("usage_observed")
                    && reason.contains("do not match its digest")
            })
            .count()
    };
    assert_eq!(naming(&raw[corrupt]), 1, "{problems:?}");
    assert_eq!(naming(&raw[1 - corrupt]), 0, "{problems:?}");
    let charge = ledger
        .accounting
        .attempts
        .iter()
        .find(|recorded| recorded.attempt_id == attempt)
        .and_then(|recorded| recorded.charge.clone())
        .expect("the charge snapshot is kept");
    assert_eq!(charge.source, "usage_observed");
    assert_eq!(charge.counters.chargeable_tokens.as_deref(), Some("6"));
    assert_eq!(charge.counters.input_tokens.as_deref(), Some("6"));
    assert_eq!(charge.native, None, "no native observation");
}

/// Design §7.5: a corrupt raw artifact after a valid native observation in one committed
/// record is read and reported, never passed over because an observation came first.
#[test]
fn telemetry_mirror_corrupt_artifact_after_a_valid_observation_marks_coverage_incomplete() {
    corrupt_beside_a_valid_observation(1);
}

/// Design §7.5: a corrupt raw artifact before a valid native observation in one committed
/// record withholds the observation it reported a failure beside.
#[test]
fn telemetry_mirror_corrupt_artifact_before_a_valid_observation_marks_coverage_incomplete() {
    corrupt_beside_a_valid_observation(0);
}

// ------------------------------------------------------------------------------------------
// the charge and wall series

/// The figures `af task show --json` prints for the fixture's Task.
fn task_show(fixture: &TaskFixture) -> Value {
    let output = crate::common::af()
        .args(["task", "show", task_fixture::TASK_ID, "--json", "--state"])
        .arg(fixture.directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Every Attempt's reader figure equals the kernel's, and the run total equals the Task's
/// committed charge.
fn assert_figures(ledger: &Ledger, fixture: &TaskFixture) {
    let (attempts, committed, unknown) = fixture.kernel();
    for attempt in &ledger.accounting.attempts {
        let charge = attempt.charge.as_ref().expect("a charge snapshot");
        let (kernel, cause) = attempts[&attempt.attempt_id];
        assert_eq!(
            charge.counters.chargeable_tokens.as_deref(),
            Some(kernel.to_string().as_str()),
            "{}",
            attempt.attempt_id
        );
        assert_eq!(
            charge.unknown_cause.as_deref(),
            cause,
            "{}",
            attempt.attempt_id
        );
        assert_eq!(
            charge.observation.as_deref() == Some("unknown"),
            cause.is_some()
        );
    }
    let started: Vec<&String> = attempts
        .iter()
        .filter(|(_, (charge, cause))| *charge > 0 || cause.is_some())
        .map(|(id, _)| id)
        .collect();
    for id in started {
        assert!(
            ledger
                .accounting
                .attempts
                .iter()
                .any(|attempt| &attempt.attempt_id == id && attempt.charge.is_some()),
            "{id} has a snapshot"
        );
    }
    assert_eq!(
        ledger
            .accounting
            .totals
            .counters
            .chargeable_tokens
            .as_deref(),
        Some(committed.to_string().as_str())
    );
    assert_eq!(ledger.accounting.totals.unknown_usage_attempts, unknown);
}

/// The accounting scenario of §7.5 at `level`, read after each step and at the end: decreasing,
/// absent and resumed late observations, several sidecar writes with no Task event between, a
/// sidecar write after a committed floor, a recovery whose observed usage is below the
/// reservation, an abandoned Attempt that reported nothing, and a reservation released by the
/// recovery. Returns the final ledger and the fixture.
fn accounting_scenario(
    level: Level,
    check_each_step: bool,
) -> (Ledger, TaskFixture, tempfile::TempDir) {
    let (temp, root, store_dir) = directories();
    let run_id = af_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
    let recording = Recording::start(&root, level, store_dir.path(), &run_id, 1);
    let mirror = recording.mirror();
    let mut f = recording
        .within(|| TaskFixture::new(store_dir, |store| store.observe_commits(mirror.clone())));
    let check = |f: &TaskFixture| {
        if check_each_step {
            recording.writer.flush();
            assert_figures(&read_ledger(&root, &run_id), f);
        }
    };
    // Every Attempt is reserved before any usage passes its reservation of 10: an overrun
    // stops dispatch.
    let (first, second, abandoned) = recording.within(|| {
        let first = f.start("write");
        let second = f.start("second");
        let abandoned = f.start("probe");
        f.observe(&first, usage(Some(6), 6));
        check(&f);
        // A decreasing observation: the kernel keeps its floor.
        f.observe(&first, usage(Some(4), 4));
        check(&f);
        // An observation without component counters.
        f.observe(&first, usage(None, 5));
        check(&f);
        // Several sidecar writes with no Task event between, after the committed floor.
        f.wall(&f.store, &first, "write", usage(Some(3), 3));
        f.wall(&f.store, &first, "write", usage(Some(8), 8));
        check(&f);
        f.settle_failed(&first, usage(Some(9), 9));
        check(&f);
        // Observed below its reservation of 10, in the sidecar only.
        f.wall(&f.store, &second, "second", usage(Some(3), 3));
        // A retry reserved and never started: the recovery releases it.
        let _released = f.prepare("write");
        f.take_over_and_recover();
        check(&f);
        // A resumed late observation of the settled Attempt.
        f.observe(&first, usage(Some(50), 50));
        check(&f);
        (first, second, abandoned)
    });
    recording.finish();
    let ledger = read_ledger(&root, &run_id);
    let charge = |id: &str| {
        ledger
            .accounting
            .attempts
            .iter()
            .find(|attempt| attempt.attempt_id == id)
            .and_then(|attempt| attempt.charge.clone())
            .unwrap()
    };
    if level != Level::Off {
        assert_eq!(
            charge(&first).counters.chargeable_tokens.as_deref(),
            Some("50")
        );
        assert_eq!(charge(&first).source, "usage_observed");
        // Recovery charges the observation, never the reservation (ADR-0209).
        assert_eq!(
            charge(&second).counters.chargeable_tokens.as_deref(),
            Some("3")
        );
        assert_eq!(charge(&second).source, "settled");
        // Abandoned with nothing reported: zero, unknown, lease expired (ADR-0143).
        let abandoned = charge(&abandoned);
        assert_eq!(abandoned.counters.chargeable_tokens.as_deref(), Some("0"));
        assert_eq!(abandoned.observation.as_deref(), Some("unknown"));
        assert_eq!(abandoned.unknown_cause.as_deref(), Some("lease_expired"));
        assert!(
            ledger.accounting.attempts.iter().any(|attempt| attempt
                .charge
                .as_ref()
                .is_some_and(|c| c.source == "released")),
            "the released reservation"
        );
    }
    (ledger, f, temp)
}

/// Design §7.5, §9: an Attempt's figures are its charge snapshot with the highest
/// `af.usage.sequence`, taken from the Task projection after each committed accounting
/// transition, so they are the kernel's figures after every step — and the run's totals and
/// unknown count equal what `af task show --json` prints.
#[test]
fn telemetry_accounting_snapshot_is_the_projection() {
    let (ledger, fixture, _temp) = accounting_scenario(Level::Spans, true);
    assert_figures(&ledger, &fixture);
    let shown = task_show(&fixture);
    assert_eq!(
        ledger
            .accounting
            .totals
            .counters
            .chargeable_tokens
            .as_deref(),
        shown["chargeable_tokens"].as_str()
    );
    assert_eq!(
        ledger.accounting.totals.unknown_usage_attempts,
        shown["unknown_usage_attempts"].as_u64().unwrap_or(0)
    );
}

/// Design §9: the same scenario yields the same Attempt figures and run totals at `metrics`,
/// `spans` and `content`.
#[test]
fn telemetry_aggregates_identical_across_levels() {
    let mut seen = Vec::new();
    for level in [Level::Metrics, Level::Spans, Level::Content] {
        let (ledger, fixture, _temp) = accounting_scenario(level, false);
        assert_figures(&ledger, &fixture);
        let figures: Vec<(String, Option<String>, Option<String>)> = ledger
            .accounting
            .attempts
            .iter()
            .map(|attempt| {
                let charge = attempt.charge.as_ref().unwrap();
                (
                    charge.source.clone(),
                    charge.counters.chargeable_tokens.clone(),
                    charge.observation.clone(),
                )
            })
            .collect();
        seen.push((figures, ledger.accounting.totals.clone()));
    }
    assert_eq!(seen[0], seen[1]);
    assert_eq!(seen[1], seen[2]);
}

/// Design §7.5: the wall series never changes a charge figure — a sidecar row far above the
/// charge stays beside it — and a reindex answers the same.
#[test]
fn telemetry_usage_series_separate() {
    let (_temp, root, store_dir) = directories();
    let run_id = af_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
    let recording = Recording::start(&root, Level::Spans, store_dir.path(), &run_id, 1);
    let mirror = recording.mirror();
    let mut f = recording
        .within(|| TaskFixture::new(store_dir, |store| store.observe_commits(mirror.clone())));
    let attempt = recording.within(|| {
        let attempt = f.start("write");
        f.observe(&attempt, usage(Some(30), 30));
        f.wall(&f.store, &attempt, "write", usage(Some(900), 900));
        f.wall(&f.store, &attempt, "write", usage(Some(950), 950));
        attempt
    });
    recording.finish();
    let before = ledger_json(&root, &run_id);
    let accounting = &before["accounting"];
    let row = accounting["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["attempt_id"] == attempt.as_str())
        .unwrap();
    assert_eq!(row["charge"]["counters"]["chargeable_tokens"], "30");
    assert_eq!(row["walls"].as_array().unwrap().len(), 1);
    assert_eq!(row["walls"][0]["counters"]["chargeable_tokens"], "950");
    assert_eq!(row["wall_order"], "one execution");
    assert_eq!(accounting["totals"]["counters"]["chargeable_tokens"], "30");
    let runs = af(&root, &["telemetry", "runs", "--json"]);
    let runs: Value = serde_json::from_slice(&runs.stdout).unwrap();
    assert_eq!(runs["runs"][0]["charge"], "30", "{runs}");
    let reindex = af(&root, &["telemetry", "reindex", "--json"]);
    assert!(reindex.status.success());
    assert_eq!(
        ledger_json(&root, &run_id),
        before,
        "identical after reindex"
    );
    let _ = f.store.task_attempt_wall(&run_id).unwrap();
}

/// Design §7.5: sidecar writes are ordered within one execution only. A predecessor execution
/// writes the sidecar after its successor has: each execution's latest observation is shown and
/// labelled, the order across executions is `unknown`, and a reindex answers the same.
#[test]
fn telemetry_wall_order_is_per_execution() {
    let (_temp, root, store_dir) = directories();
    let run_id = af_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
    let store_path = store_dir.path().to_path_buf();
    let predecessor = Recording::start(&root, Level::Spans, &store_path, &run_id, 1);
    let mirror = predecessor.mirror();
    let mut f = predecessor
        .within(|| TaskFixture::new(store_dir, |store| store.observe_commits(mirror.clone())));
    let attempt = predecessor.within(|| {
        let attempt = f.start("write");
        f.wall(&f.store, &attempt, "write", usage(Some(5), 5));
        attempt
    });
    // The successor, on its own connection, writes the sidecar.
    let successor = Recording::start(&root, Level::Spans, &store_path, &run_id, 2);
    let mut theirs = EventStore::open(store_path.join("events.sqlite")).unwrap();
    theirs.observe_commits(successor.mirror());
    successor.within(|| f.wall(&theirs, &attempt, "write", usage(Some(7), 7)));
    // Then the predecessor, still running, writes again.
    predecessor.within(|| f.wall(&f.store, &attempt, "write", usage(Some(6), 6)));
    let predecessor_segment = predecessor.writer.segment_id().unwrap();
    let successor_segment = successor.writer.segment_id().unwrap();
    successor.finish();
    predecessor.finish();
    let read = || {
        let ledger = read_ledger(&root, &run_id);
        ledger
            .accounting
            .attempts
            .into_iter()
            .find(|row| row.attempt_id == attempt)
            .unwrap()
    };
    let row = read();
    assert_eq!(row.wall_order, "unknown");
    let walls: BTreeMap<String, (u64, Option<String>)> = row
        .walls
        .iter()
        .map(|wall| {
            (
                wall.execution_id.clone(),
                (wall.wall_sequence, wall.counters.chargeable_tokens.clone()),
            )
        })
        .collect();
    assert_eq!(walls.len(), 2, "{walls:?}");
    // The predecessor's latest write is its second; the sidecar merged it with the successor's.
    assert_eq!(walls[&predecessor_segment].1.as_deref(), Some("7"));
    assert_eq!(walls[&successor_segment].1.as_deref(), Some("7"));
    assert!(row.charge.is_none(), "no accounting transition committed");
    let before = ledger_json(&root, &run_id);
    assert!(af(&root, &["telemetry", "reindex"]).status.success());
    assert_eq!(read(), row);
    assert_eq!(ledger_json(&root, &run_id), before);
}

/// ADR-0078: counters above 2^53 and above `u64` round-trip as decimal strings and aggregate
/// exactly, in the reader and in `af telemetry runs`.
#[test]
fn telemetry_usage_exact_above_u64() {
    let (_temp, root, store_dir) = directories();
    let run_id = af_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
    let recording = Recording::start(&root, Level::Spans, store_dir.path(), &run_id, 1);
    let mirror = recording.mirror();
    let mut f = recording
        .within(|| TaskFixture::new(store_dir, |store| store.observe_commits(mirror.clone())));
    let wide = u128::from(u64::MAX) + 17;
    let above_double = (1u128 << 53) + 1;
    recording.within(|| {
        let first = f.start("write");
        let second = f.start("second");
        f.observe(&first, usage(Some(above_double), wide));
        f.observe(&second, usage(Some(above_double), wide));
    });
    let (_, committed, _) = f.kernel();
    recording.finish();
    let document = ledger_json(&root, &run_id);
    let totals = &document["accounting"]["totals"]["counters"];
    assert_eq!(totals["chargeable_tokens"], (wide * 2).to_string());
    assert_eq!(totals["chargeable_tokens"], committed.to_string());
    assert_eq!(totals["input_tokens"], (above_double * 2).to_string());
    for row in document["accounting"]["attempts"].as_array().unwrap() {
        assert_eq!(
            row["charge"]["counters"]["chargeable_tokens"],
            wide.to_string()
        );
        assert_eq!(
            row["charge"]["counters"]["input_tokens"],
            above_double.to_string()
        );
    }
    let runs = af(&root, &["telemetry", "runs", "--json"]);
    let runs: Value = serde_json::from_slice(&runs.stdout).unwrap();
    assert_eq!(runs["runs"][0]["charge"], (wide * 2).to_string());
    assert_eq!(
        runs["runs"][0]["tokens"]["input_tokens"],
        (above_double * 2).to_string()
    );
}

#[test]
fn slow_accounting_setup_renews_without_extending_the_lease_policy() {
    let started = std::time::Instant::now();
    let mut f = TaskFixture::with_setup_step(
        tempfile::tempdir().unwrap(),
        |_| {},
        || {
            std::thread::sleep(std::time::Duration::from_millis(600));
        },
    );
    assert!(started.elapsed() > std::time::Duration::from_secs(3));
    let old = f.lease.clone();
    let attempt = f.start("write");
    f.observe(&attempt, usage(Some(7), 7));
    assert_eq!(f.kernel().1, 7);
    f.take_over_and_recover();
    assert_ne!(f.lease.epoch(), old.epoch());
    assert!(f.store.renew_task_lease(&f.cas, &old, 3_000).is_err());
    assert!(
        f.store
            .task_projection(&f.cas, &f.task_id)
            .unwrap()
            .unwrap()
            .execution
            .unwrap()
            .pending_attempts()
            .is_empty()
    );
    assert_eq!(
        f.kernel().1,
        7,
        "recovery retains exactly the observed usage (ADR-0209)"
    );
    let cold = EventStore::open_read_only(f.directory.path().join("events.sqlite")).unwrap();
    let execution = cold
        .task_projection(&f.cas, &f.task_id)
        .unwrap()
        .unwrap()
        .execution
        .unwrap();
    let accounting = execution.attempt_accounting();
    let recovered = accounting
        .iter()
        .find(|row| row.attempt_id == attempt)
        .unwrap();
    assert_eq!(recovered.charged_tokens, 7);
    assert_eq!(recovered.reservation.tokens, 10);
    assert_eq!(recovered.unknown_usage, None);
    assert_eq!(
        recovered.abandoned,
        Some(af_store::store::task::execution::TaskAbandonCause::WriterLost)
    );
    assert_eq!(execution.budget.committed_tokens(), 7);
    assert_eq!(execution.budget.reserved_tokens(), 0);
    assert_eq!(execution.budget.begun_attempts(), 1);
    let recoveries = execution.recoveries();
    assert_eq!(recoveries.len(), 1);
    assert_eq!(recoveries[0].chargeable, 0);
    assert_eq!(recoveries[0].writer_lost, 1);
}

/// A delayed post-commit callback can arrive after a later committed Task transition.
struct Late {
    mirror: Arc<StoreMirror>,
    committed: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}
impl CommitObserver for Late {
    fn committed(&self, commit: &Commit<'_>) {
        self.committed.send(()).unwrap();
        self.release.lock().unwrap().recv().unwrap();
        self.mirror.committed(commit);
    }
}

fn assert_native_commit_order(root: &Path, run_id: &str, events: &[af_contracts::RunEvent]) {
    let ledger = read_ledger(root, run_id);
    assert_eq!(ledger.coverage, "complete", "{:?}", ledger.coverage_reasons);
    let order: Vec<_> = ledger
        .entries
        .iter()
        .filter(|e| e.entry == "event")
        .map(|e| e.sequence.unwrap())
        .collect();
    assert_eq!(order, events.iter().map(|e| e.sequence).collect::<Vec<_>>());
    let before = ledger_json(root, run_id);
    assert!(af(root, &["telemetry", "reindex"]).status.success());
    assert_eq!(ledger_json(root, run_id), before);
}

#[test]
fn telemetry_ledger_orders_reordered_native_post_commit_callbacks() {
    let (_temp, root, store_dir) = directories();
    let run_id = af_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
    let recording = Recording::start(&root, Level::Spans, store_dir.path(), &run_id, 1);
    let mirror = recording.mirror();
    let mut fixture = recording
        .within(|| TaskFixture::new(store_dir, |store| store.observe_commits(mirror.clone())));
    let mut second = EventStore::open(fixture.store.database_path().unwrap()).unwrap();
    second.observe_commits(mirror.clone());
    let (committed, entered) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    fixture.store.observe_commits(Arc::new(Late {
        mirror,
        committed,
        release: std::sync::Mutex::new(released),
    }));
    let first_sequence = fixture.store.replay(&run_id).unwrap().len() as u64;
    let cas = af_store::Cas::open(fixture.directory.path().join("cas")).unwrap();
    let lease = fixture.lease.clone();
    std::thread::scope(|scope| {
        let first = scope.spawn(|| recording.within(|| fixture.renew()));
        entered.recv().unwrap();
        recording
            .within(|| second.renew_task_lease(&cas, &lease, 3000))
            .unwrap();
        release.send(()).unwrap();
        first.join().unwrap();
    });
    let events = second.replay(&run_id).unwrap();
    let segment = recording.writer.segment_id().unwrap();
    recording.finish();
    let (index, _, _) = view::open(&root).unwrap();
    let recorded: Vec<i64> = index.connection().prepare(
        "SELECT json_extract(attributes, '$.\"af.store.event.sequence\"') FROM events WHERE segment_id=?1 AND name='af.store.event' ORDER BY seq"
    ).unwrap().query_map([segment], |r| r.get(0)).unwrap().collect::<Result<_,_>>().unwrap();
    assert!(
        recorded
            .iter()
            .position(|s| *s == (first_sequence + 1) as i64)
            .unwrap()
            < recorded
                .iter()
                .position(|s| *s == first_sequence as i64)
                .unwrap()
    );
    drop(index);
    assert_native_commit_order(&root, &run_id, &events);
}

#[test]
fn telemetry_ledger_orders_overlapping_native_executions_by_commit() {
    let (_temp, root, store_dir) = directories();
    let run_id = af_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
    let first = Recording::start(&root, Level::Spans, store_dir.path(), &run_id, 1);
    let mut fixture =
        first.within(|| TaskFixture::new(store_dir, |store| store.observe_commits(first.mirror())));
    let second = Recording::start(
        &root,
        Level::Spans,
        fixture.store.database_path().unwrap().parent().unwrap(),
        &run_id,
        2,
    );
    let mut theirs = EventStore::open(fixture.store.database_path().unwrap()).unwrap();
    theirs.observe_commits(second.mirror());
    second
        .within(|| theirs.renew_task_lease(&fixture.cas, &fixture.lease, 3000))
        .unwrap();
    first.within(|| fixture.renew());
    let events = theirs.replay(&run_id).unwrap();
    second.finish();
    first.finish();
    assert_native_commit_order(&root, &run_id, &events);
}
