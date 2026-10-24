//! Design §7.5 and §15 P4: the Store commit mirror, Finding and Demand summaries, the two
//! accounting series and `af telemetry ledger`, end to end over a Store on disk: the kernel
//! commits through its own API with the mirror observing, and the reader answers from the
//! telemetry directory alone.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use review_core::task::usage::{TaskTokenUsageV3, TaskUsageObservationV1};
use review_pipeline::telemetry::StoreMirror;
use review_store::store::observer::{Commit, CommitObserver, WallCommit};
use review_store::{Cas, EventStore};
use review_telemetry::ledger::Ledger;
use review_telemetry::level::Level;
use review_telemetry::record::SealExit;
use review_telemetry::segment::writer::{Writer, WriterOptions};
use review_telemetry::subscriber::{TelemetrySubscriber, Typed};
use review_telemetry::view::{self, Resolved};
use serde_json::Value;

#[path = "telemetry_ledger/campaign_fixture.rs"]
pub(crate) mod campaign_fixture;
#[path = "telemetry_ledger/task_fixture.rs"]
pub(crate) mod task_fixture;

use campaign_fixture as campaign;
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
        options.redactor = review_telemetry::redact::Redactor::new();
        let writer = Writer::new(options).unwrap();
        let dispatch = tracing::Dispatch::new(TelemetrySubscriber::new(writer.clone()));
        let store_id = review_telemetry::trace::store_id(store).unwrap();
        // A Task's log is a Task run; a Campaign's log is a review run.
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
    review_telemetry::ledger::ledger(
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

// ------------------------------------------------------------------------------------------
// Finding and Demand summaries

/// A recorded Campaign: its telemetry directory (in the first), the Findings' statuses as the
/// Ledger holds them, and its committed events as `(sequence, type, id)`.
type RecordedCampaign = (
    tempfile::TempDir,
    PathBuf,
    BTreeMap<String, String>,
    Vec<(u64, String, String)>,
);

/// The Campaign every summary test records: two reviewers report three claims and a standalone
/// Demand, a third disputes one claim (`contested`), and two are declined afterwards
/// (`rejected`, `wontfix`).
fn record_campaign(level: Level) -> RecordedCampaign {
    let (temp, root, store_dir) = directories();
    let recording = Recording::start(&root, level, store_dir.path(), campaign::RUN, 1);
    let cas = Cas::open(store_dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(store_dir.path().join("events.sqlite")).unwrap();
    store.observe_commits(recording.mirror());
    recording.within(|| {
        let round = campaign::opened_round(&mut store, &cas);
        let deep = campaign::stage(
            r#"{"reports":[{"severity":"major","file":"src/a.rs","line":7,
                 "title":"Retry loop can spin forever","body":"b","fix":"f","confidence":0.9}],
                "benchmark_demands":[],"dispositions":[]}"#,
        );
        let cross = campaign::stage(
            r#"{"reports":[
                  {"severity":"blocker","file":"src/queue.rs","line":3,
                   "title":"Queue grows without bound","body":"b","fix":"f","confidence":0.8},
                  {"severity":"minor","file":"src/lib.rs","line":1,
                   "title":"Misleading comment","body":"b","fix":"f","confidence":0.5}],
                "benchmark_demands":[{"claim":"Enqueue stays O(1)",
                   "why":"the queue is on the hot path","suggested_method":"time 10^6 enqueues"}],
                "dispositions":[]}"#,
        );
        campaign::admit(
            &mut store,
            &cas,
            &round,
            &[
                ("deep", "01jd8m4qz9k7v3n2p6r8t0w201", &deep),
                ("cross", "01jd8m4qz9k7v3n2p6r8t0w202", &cross),
            ],
        );
        let ledger = campaign::ledger(&store, &cas);
        let key = |title: &str| {
            ledger
                .findings()
                .into_iter()
                .find(|finding| finding.title == title)
                .unwrap()
                .key
                .clone()
        };
        let (retry, queue, comment) = (
            key("Retry loop can spin forever"),
            key("Queue grows without bound"),
            key("Misleading comment"),
        );
        let positions = campaign::stage(&format!(
            r#"{{"reports":[],"benchmark_demands":[],
                "dispositions":[{{"finding_id":"{queue}","position":"dispute",
                                  "reason":"the producer is bounded"}}]}}"#
        ));
        campaign::admit(
            &mut store,
            &cas,
            &round,
            &[("performance", "01jd8m4qz9k7v3n2p6r8t0w203", &positions)],
        );
        campaign::resolve(&mut store, &cas, &round, &retry, "rejected");
        campaign::resolve(&mut store, &cas, &round, &comment, "wontfix");
    });
    let statuses: BTreeMap<String, String> = campaign::ledger(&store, &cas)
        .findings()
        .into_iter()
        .map(|finding| (finding.key.clone(), finding.status.as_str().to_string()))
        .collect();
    let events = store
        .replay(campaign::RUN)
        .unwrap()
        .into_iter()
        .map(|event| {
            (
                event.sequence,
                event.event_type.as_str().to_string(),
                event.event_id,
            )
        })
        .collect();
    drop(store);
    recording.finish();
    // The Store is gone: every answer below comes from the telemetry directory alone.
    drop(store_dir);
    (temp, root, statuses, events)
}

/// Design §7.5: a Finding summary is recorded when the kernel admits a Report into the Ledger
/// and whenever its disposition changes, so `af telemetry ledger` answers with no Store: every
/// committed event in sequence, every Finding with its later dispositions, `complete`.
#[test]
fn telemetry_findings_from_summaries_only() {
    let (_temp, root, statuses, events) = record_campaign(Level::Spans);
    let ledger = read_ledger(&root, campaign::RUN);
    assert_eq!(ledger.coverage, "complete", "{:?}", ledger.coverage_reasons);
    let mirrored: Vec<(u64, String, String)> = ledger
        .entries
        .iter()
        .filter(|entry| entry.entry == "event")
        .map(|entry| {
            (
                entry.sequence.unwrap(),
                entry.name.clone().unwrap(),
                entry.event_id.clone().unwrap(),
            )
        })
        .collect();
    assert_eq!(mirrored, events, "every committed event, in sequence");
    assert_eq!(ledger.logs.len(), 1);
    assert!(ledger.logs[0].missing.is_empty());
    let summarized: BTreeMap<String, String> = ledger
        .findings
        .iter()
        .map(|finding| (finding.key.clone(), finding.status.clone()))
        .collect();
    assert_eq!(summarized, statuses, "the Ledger's own statuses");
    for finding in &ledger.findings {
        assert!(
            finding.report_artifact_id.starts_with("sha256:"),
            "{finding:?}"
        );
        assert!(finding.key.starts_with("sha256:"), "{finding:?}");
        assert!(!finding.worker.is_empty() && !finding.node_id.is_empty());
        assert_eq!((finding.round, finding.epoch), (Some(1), Some(1)));
    }
    let dispositions: BTreeMap<&str, Vec<(&str, &str)>> = ledger
        .findings
        .iter()
        .map(|finding| {
            (
                finding.title.as_str(),
                finding
                    .dispositions
                    .iter()
                    .map(|d| (d.previous_status.as_str(), d.status.as_str()))
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        dispositions["Retry loop can spin forever"],
        [("open", "rejected")]
    );
    assert_eq!(
        dispositions["Queue grows without bound"],
        [("open", "contested")]
    );
    assert_eq!(dispositions["Misleading comment"], [("open", "wontfix")]);
    // The command prints the same document, valid against its schema.
    let document = ledger_json(&root, campaign::RUN);
    assert_eq!(document["coverage"], "complete");
    assert_eq!(
        document["findings"].as_array().unwrap().len(),
        statuses.len()
    );
}

// ------------------------------------------------------------------------------------------
// the mirror's rules

/// Design §7.5: a transaction that rolls back is never mirrored — a Campaign batch refused
/// inside its transaction, and a generic append the Task's log refuses inside its transaction
/// — while the Store stays exactly as it was, and what did commit is mirrored once each.
#[test]
fn telemetry_mirror_skips_rolled_back_transactions() {
    let (_temp, root, store_dir) = directories();
    let run_id = review_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
    let recording = Recording::start(&root, Level::Spans, store_dir.path(), &run_id, 1);
    let mirror = recording.mirror();
    let mut fixture = recording
        .within(|| TaskFixture::new(store_dir, |store| store.observe_commits(mirror.clone())));
    let logs = |store: &EventStore| {
        [run_id.clone(), "campaign-accounting".to_string()].map(|log| store.replay(&log).unwrap())
    };
    let before = logs(&fixture.store);
    recording.within(|| {
        let captured = review_store::NewEvent::new(
            review_core::EventType::SourceCapturedV1,
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
                    review_store::NewEvent::new(
                        review_core::EventType::GenerationAdvancedV1,
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
    assert_eq!(ledger.logs.len(), 2, "{:?}", ledger.logs);
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
    let run_id = review_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
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
    let run_id = review_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
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
    let run_id = review_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
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
        // Recovery charges the reservation, never the lower observation.
        assert_eq!(
            charge(&second).counters.chargeable_tokens.as_deref(),
            Some("10")
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
    let run_id = review_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
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
    let run_id = review_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
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
    let run_id = review_store::store::task::task_run_id(task_fixture::TASK_ID).unwrap();
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

// ------------------------------------------------------------------------------------------
// the golden fixture

fn golden_path(name: &str) -> PathBuf {
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    workspace
        .join("crates/af/tests/it/telemetry_golden")
        .join(name)
}

/// Compare `actual` with the golden `name`; `AF_UPDATE_TELEMETRY_GOLDEN=1` rewrites it.
fn golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("AF_UPDATE_TELEMETRY_GOLDEN").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert_eq!(actual, expected, "{name} differs from its golden");
}

/// Design §10: `af telemetry ledger` over one recorded fixture Campaign — its committed events,
/// three Findings with their dispositions, a Demand, and two Attempts' charge snapshots and a
/// sidecar observation — in text and as `af/telemetry-ledger@1`, against goldens; the document
/// matches its schema and refuses a member its schema does not name. The Store's directory is
/// the machine's, so its id, the run's trace and the telemetry directory read as placeholders.
#[test]
fn telemetry_ledger_matches_its_golden() {
    use review_telemetry::record::Attrs;
    use std::sync::atomic::{AtomicU64, Ordering};
    let (_temp, root, store_dir) = directories();
    let clock = Arc::new(AtomicU64::new(1_700_000_200_000 * 1_000_000));
    let mut options = WriterOptions::new(&root, Level::Spans);
    options.argv = vec!["af".into(), "review".into(), "run".into()];
    options.cwd = PathBuf::from("/work");
    options.af_version = "0.0.0-golden".into();
    options.af_commit = "0123456789ab".into();
    options.segment_id = Some("1700000200000-4244-0c1d2e3f".into());
    options.clock = Some(Arc::new(move || {
        u128::from(clock.fetch_add(1_000_000, Ordering::SeqCst))
    }));
    let recording = Recording::with_options(options, store_dir.path(), campaign::RUN, 1);
    let store_id = review_telemetry::trace::store_id(store_dir.path()).unwrap();
    let cas = Cas::open(store_dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(store_dir.path().join("events.sqlite")).unwrap();
    store.observe_commits(recording.mirror());
    recording.within(|| {
        let round = campaign::opened_round(&mut store, &cas);
        let cross = campaign::stage(
            r#"{"reports":[
                  {"severity":"blocker","file":"src/queue.rs","line":3,
                   "title":"Queue grows without bound","body":"b","fix":"f","confidence":0.8}],
                "benchmark_demands":[{"claim":"Enqueue stays O(1)",
                   "why":"the queue is on the hot path","suggested_method":"time 10^6 enqueues"}],
                "dispositions":[]}"#,
        );
        campaign::admit(
            &mut store,
            &cas,
            &round,
            &[("cross", "01jd8m4qz9k7v3n2p6r8t0w202", &cross)],
        );
        let queue = campaign::ledger(&store, &cas).findings()[0].key.clone();
        campaign::resolve(&mut store, &cas, &round, &queue, "wontfix");
        // Two Attempts' accounting as the mirror records it: the charge series and, beside
        // it, one sidecar write.
        let span = review_telemetry::subscriber::current_recorded()
            .unwrap()
            .span_id;
        let state = |attempt: &str, sequence: i64, source: &'static str, charge: &str| {
            Attrs::new()
                .id("af.store_id", store_id.clone())
                .id("af.run_id", campaign::RUN)
                .id("af.attempt_id", attempt)
                .id("af.node_id", "root.nodes.cross")
                .enumerated("af.usage.source", source)
                .int("af.usage.sequence", sequence)
                .decimal("af.usage.chargeable_tokens", charge)
        };
        let writer = &recording.writer;
        writer.event(
            &span,
            "af.usage.state",
            state("attempt-1", 3, "usage_observed", "6")
                .decimal("af.usage.input_tokens", "6")
                .enumerated("af.usage.observation", "complete"),
        );
        writer.event_with_sequence(
            &span,
            "af.usage.wall",
            Attrs::new()
                .id("af.store_id", store_id.clone())
                .id("af.run_id", campaign::RUN)
                .id("af.attempt_id", "attempt-1")
                .id("af.node_id", "root.nodes.cross")
                .enumerated("af.usage.source", "sidecar")
                .int("af.usage.wall.started_unix_ms", 1_000)
                .int("af.usage.wall.elapsed_ms", 15)
                .decimal("af.usage.chargeable_tokens", "8")
                .decimal("af.usage.input_tokens", "8"),
            "af.usage.wall.sequence",
        );
        writer.event(
            &span,
            "af.usage.state",
            state("attempt-1", 5, "settled", "9")
                .decimal("af.usage.input_tokens", "9")
                .enumerated("af.usage.observation", "complete"),
        );
        writer.event(
            &span,
            "af.usage.state",
            state("attempt-2", 7, "settled", "0")
                .enumerated("af.usage.observation", "unknown")
                .enumerated("af.usage.unknown_cause", "lease_expired"),
        );
    });
    drop(store);
    recording.finish();
    let trace_id = review_telemetry::trace::trace_id(&store_id, campaign::RUN);
    let normalize = |text: String| {
        text.replace(&root.display().to_string(), "<root>")
            .replace(&store_id, "<store>")
            .replace(&store_id[..12], "<store>")
            .replace(&trace_id, "<trace>")
    };
    let document = ledger_json(&root, campaign::RUN);
    assert_eq!(
        document["coverage"], "complete",
        "{}",
        document["coverage_reasons"]
    );
    golden(
        "ledger.json",
        &normalize(serde_json::to_string_pretty(&document).unwrap() + "\n"),
    );
    let text = af(&root, &["telemetry", "ledger", campaign::RUN]);
    assert!(text.status.success());
    golden(
        "ledger.txt",
        &normalize(String::from_utf8(text.stdout).unwrap()),
    );
    let validator = crate::schemas::validator("telemetry-ledger-v1.json");
    for pointer in [
        "",
        "/run",
        "/entries/0",
        "/findings/0",
        "/demands/0",
        "/accounting/attempts/0",
    ] {
        let mut extended = document.clone();
        extended
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unnamed".into(), Value::from(1));
        assert!(
            !validator.is_valid(&extended),
            "{pointer} accepts an unnamed member"
        );
    }
}

// ------------------------------------------------------------------------------------------
// the ledger's order and the summaries' fields

/// A Campaign with its first Round open and two Findings admitted: the Store directory, its
/// CAS, the Round and the two Finding keys.
fn two_findings(store: &mut EventStore, cas: &Cas) -> (campaign::Round, String, String) {
    let round = campaign::opened_round(store, cas);
    let reports = campaign::stage(
        r#"{"reports":[
              {"severity":"major","file":"src/a.rs","line":7,
               "title":"Retry loop can spin forever","body":"b","fix":"f","confidence":0.9},
              {"severity":"minor","file":"src/lib.rs","line":1,
               "title":"Misleading comment","body":"b","fix":"f","confidence":0.5}],
            "benchmark_demands":[],"dispositions":[]}"#,
    );
    campaign::admit(
        store,
        cas,
        &round,
        &[("deep", "01jd8m4qz9k7v3n2p6r8t0w201", &reports)],
    );
    let ledger = campaign::ledger(store, cas);
    let key = |title: &str| {
        ledger
            .findings()
            .into_iter()
            .find(|finding| finding.title == title)
            .unwrap()
            .key
            .clone()
    };
    let (retry, comment) = (
        key("Retry loop can spin forever"),
        key("Misleading comment"),
    );
    (round, retry, comment)
}

/// The committed events of the ledger's entries, as `(log, sequence)` in entry order.
fn event_order(ledger: &Ledger) -> Vec<(String, u64)> {
    ledger
        .entries
        .iter()
        .filter(|entry| entry.entry == "event")
        .map(|entry| (entry.log.clone().unwrap(), entry.sequence.unwrap()))
        .collect()
}

/// Every entry's `(log, sequence)` is in commit order: never decreasing within a log.
fn assert_commit_order(ledger: &Ledger) {
    let mut last: BTreeMap<String, u64> = BTreeMap::new();
    for entry in &ledger.entries {
        let (Some(log), Some(sequence)) = (&entry.log, entry.sequence) else {
            panic!("an entry without its log and sequence: {entry:?}");
        };
        let previous = last.insert(log.clone(), sequence);
        assert!(
            previous.is_none_or(|previous| previous <= sequence),
            "{entry:?} after sequence {previous:?}: {:?}",
            ledger.entries
        );
    }
}

/// A commit observer whose callbacks wait at a gate before the mirror sees them, as a Store
/// connection whose post-commit observer runs late.
struct Late {
    mirror: Arc<StoreMirror>,
    committed: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl review_store::store::observer::CommitObserver for Late {
    fn committed(&self, commit: &review_store::store::observer::Commit<'_>) {
        let _ = self.committed.send(());
        let _ = self.release.lock().unwrap().recv();
        self.mirror.committed(commit);
    }
}

/// Design §7.5 and §10 (P4): `af telemetry ledger` orders one Store log by its committed
/// sequence, never by record order. Two connections of one execution share its mirror; the
/// first commits sequence N and its post-commit observer runs only after the second has
/// committed N+1 and been mirrored. The ledger displays N before N+1, folds the summaries in
/// that order, and answers identically after a reindex.
#[test]
fn telemetry_ledger_orders_reordered_post_commit_callbacks() {
    let (_temp, root, store_dir) = directories();
    let recording = Recording::start(&root, Level::Spans, store_dir.path(), campaign::RUN, 1);
    let cas = Cas::open(store_dir.path().join("cas")).unwrap();
    let mirror = recording.mirror();
    let mut prompt = EventStore::open(store_dir.path().join("events.sqlite")).unwrap();
    prompt.observe_commits(mirror.clone());
    let (round, retry, comment) = recording.within(|| two_findings(&mut prompt, &cas));
    let (committed, entered) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let mut late = EventStore::open(store_dir.path().join("events.sqlite")).unwrap();
    late.observe_commits(Arc::new(Late {
        mirror: mirror.clone(),
        committed,
        release: std::sync::Mutex::new(released),
    }));
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            recording.within(|| campaign::resolve(&mut late, &cas, &round, &retry, "rejected"));
        });
        entered.recv().unwrap();
        recording.within(|| campaign::resolve(&mut prompt, &cas, &round, &comment, "wontfix"));
        release.send(()).unwrap();
        first.join().unwrap();
    });
    let events = prompt.replay(campaign::RUN).unwrap();
    let rejected = events
        .iter()
        .find(|event| {
            event.correlation_id.as_deref() == Some(retry.as_str())
                && event.event_type == review_core::EventType::FindingResolvedV1
        })
        .unwrap()
        .sequence;
    drop((prompt, late));
    let segment = recording.writer.segment_id().unwrap();
    recording.finish();
    // The record order is the callbacks': the later commit was mirrored first.
    let (index, _, _) = view::open(&root).unwrap();
    let recorded: Vec<i64> = index
        .connection()
        .prepare(
            "SELECT json_extract(attributes, '$.\"af.store.event.sequence\"') FROM events
             WHERE segment_id = ?1 AND name = 'af.store.event' ORDER BY seq",
        )
        .unwrap()
        .query_map([&segment], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(index);
    let at = |sequence: u64| {
        recorded
            .iter()
            .position(|recorded| *recorded == sequence as i64)
            .unwrap()
    };
    assert!(at(rejected + 1) < at(rejected), "{recorded:?}");
    let ledger = read_ledger(&root, campaign::RUN);
    assert_eq!(ledger.coverage, "complete", "{:?}", ledger.coverage_reasons);
    let order = event_order(&ledger);
    let sequences: Vec<u64> = order.iter().map(|(_, sequence)| *sequence).collect();
    let mut sorted = sequences.clone();
    sorted.sort_unstable();
    assert_eq!(sequences, sorted, "in commit order");
    assert_eq!(sequences.len() as u64, rejected + 2);
    assert_commit_order(&ledger);
    let status = |key: &str| {
        ledger
            .findings
            .iter()
            .find(|finding| finding.key == key)
            .unwrap()
            .status
            .clone()
    };
    assert_eq!(
        (status(&retry), status(&comment)),
        ("rejected".into(), "wontfix".into())
    );
    let before = ledger_json(&root, campaign::RUN);
    assert!(af(&root, &["telemetry", "reindex"]).status.success());
    assert_eq!(
        ledger_json(&root, campaign::RUN),
        before,
        "identical after reindex"
    );
}

/// Design §5 and §7.5 (P4): executions overlap (ADR-0089), and the later one may commit first.
/// Execution 1 opens the Campaign and admits a Finding; execution 2, started later on its own
/// connection, commits the Finding's `rejected` at sequence N; then execution 1 commits
/// `wontfix` at N+1 (catching up N on its way). The ledger reads N before N+1 whatever the
/// segments' starts, the Finding's latest summary is the one of N+1 (`wontfix`), its
/// dispositions are in commit order, and a reindex answers the same.
#[test]
fn telemetry_ledger_orders_overlapping_executions_by_commit() {
    let (_temp, root, store_dir) = directories();
    let first = Recording::start(&root, Level::Spans, store_dir.path(), campaign::RUN, 1);
    let cas = Cas::open(store_dir.path().join("cas")).unwrap();
    let mut ours = EventStore::open(store_dir.path().join("events.sqlite")).unwrap();
    ours.observe_commits(first.mirror());
    let (round, retry, _) = first.within(|| two_findings(&mut ours, &cas));
    std::thread::sleep(std::time::Duration::from_millis(5));
    let second = Recording::start(&root, Level::Spans, store_dir.path(), campaign::RUN, 2);
    let mut theirs = EventStore::open(store_dir.path().join("events.sqlite")).unwrap();
    theirs.observe_commits(second.mirror());
    second.within(|| campaign::resolve(&mut theirs, &cas, &round, &retry, "rejected"));
    first.within(|| campaign::resolve(&mut ours, &cas, &round, &retry, "wontfix"));
    drop((ours, theirs));
    second.finish();
    first.finish();
    let ledger = read_ledger(&root, campaign::RUN);
    let sequences: Vec<u64> = event_order(&ledger)
        .into_iter()
        .map(|(_, sequence)| sequence)
        .collect();
    let mut sorted = sequences.clone();
    sorted.sort_unstable();
    assert_eq!(sequences, sorted, "in commit order");
    assert_commit_order(&ledger);
    let finding = ledger
        .findings
        .iter()
        .find(|finding| finding.key == retry)
        .unwrap();
    assert_eq!(finding.status, "wontfix", "the latest committed summary");
    let dispositions: Vec<(&str, &str)> = finding
        .dispositions
        .iter()
        .map(|d| (d.previous_status.as_str(), d.status.as_str()))
        .collect();
    assert_eq!(
        dispositions,
        [("open", "rejected"), ("rejected", "wontfix")]
    );
    let before = ledger_json(&root, campaign::RUN);
    assert!(af(&root, &["telemetry", "reindex"]).status.success());
    assert_eq!(read_ledger(&root, campaign::RUN).findings, ledger.findings);
    assert_eq!(
        ledger_json(&root, campaign::RUN),
        before,
        "identical after reindex"
    );
}

/// Design §7.5 (P4): a Finding summary carries the admitting Worker's bound model when the
/// kernel recorded that binding, and none when it did not: a model-backed reviewer's Report
/// names the model its node is bound to, a command reviewer's names none.
#[test]
fn telemetry_finding_carries_the_bound_model() {
    let (_temp, root, store_dir) = directories();
    let recording = Recording::start(&root, Level::Spans, store_dir.path(), campaign::RUN, 1);
    let cas = Cas::open(store_dir.path().join("cas")).unwrap();
    let mut store = EventStore::open(store_dir.path().join("events.sqlite")).unwrap();
    store.observe_commits(recording.mirror());
    // The kernel's binding of the model-backed reviewer, as the Task runtime records it.
    review_pipeline::telemetry::record_binding("cross", None, "claude-opus-5-5");
    recording.within(|| {
        let round = campaign::opened_round(&mut store, &cas);
        let bound = campaign::stage(
            r#"{"reports":[{"severity":"major","file":"src/a.rs","line":7,
                 "title":"Retry loop can spin forever","body":"b","fix":"f","confidence":0.9}],
                "benchmark_demands":[],"dispositions":[]}"#,
        );
        let command = campaign::stage(
            r#"{"reports":[{"severity":"minor","file":"src/lib.rs","line":1,
                 "title":"Misleading comment","body":"b","fix":"f","confidence":0.5}],
                "benchmark_demands":[],"dispositions":[]}"#,
        );
        campaign::admit(
            &mut store,
            &cas,
            &round,
            &[
                ("cross", "01jd8m4qz9k7v3n2p6r8t0w202", &bound),
                ("deep", "01jd8m4qz9k7v3n2p6r8t0w201", &command),
            ],
        );
    });
    drop(store);
    recording.finish();
    let ledger = read_ledger(&root, campaign::RUN);
    let model = |title: &str| {
        ledger
            .findings
            .iter()
            .find(|finding| finding.title == title)
            .unwrap()
            .model
            .clone()
    };
    assert_eq!(
        model("Retry loop can spin forever").as_deref(),
        Some("claude-opus-5-5")
    );
    assert_eq!(model("Misleading comment"), None);
}

/// Aggregate setup can take longer than the lease without holding any individual operation
/// beyond it. The fixture renews between operations; expiry/takeover remain real below.
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
        10,
        "recovery retains the original reservation floor"
    );
}
