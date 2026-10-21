//! The timed production operations of every declared case.
//!
//! Each sample generates its fixture (setup, timed separately), or opens the private copy of
//! the seed its side wrote once (ADR-0170), performs its production operation exactly once per
//! declared repetition inside the region, keeps the result alive through `black_box`, and only
//! afterwards reduces it to the digest and count the driver checks against golden entries.
//! Setup, CAS population, prewarming and every check stay outside the region. Freshness is per
//! sample: a fresh process, a fresh Store and CAS under the sample's own scratch or its own copy
//! of a seed, an application-cold or prewarmed projection as declared, and whatever the OS page
//! cache holds; nothing here evicts or claims to evict physical caches.

use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use review_core::task::plan::ExecutionPlanV1;
use review_core::{ArtifactEnvelope, Producer};
use review_graph::task::CompiledTask;
use review_pipeline::task::TaskRuntime;
use review_source_git::{Capture, Entry, EntryKind, Manifest, Repo};
use review_store::canonical::blob_content_id;
use review_store::{Cas, EventStore, StoreError};
use serde_json::{Value, json};

use review_core::perf_trace::{self, Phase};

use crate::authority::{self, AuthorityFixture};
use crate::control;
use crate::fixtures::{self, Envelope};
use crate::protocol::{Failure, FailureKind, Observation, Outcome, Request};

fn fixture_error(error: impl ToString) -> Failure {
    Failure::new(FailureKind::FixtureFailed, error.to_string())
}

fn operation_error(error: impl ToString) -> Failure {
    Failure::new(FailureKind::OperationFailed, error.to_string())
}

fn observed(digest: String, count: u64, detail: String) -> Observation {
    Observation {
        digest: Some(digest),
        count: Some(count),
        detail,
        extra_metrics: Default::default(),
    }
}

fn digest_of(value: &Value) -> Result<String, Failure> {
    review_store::content_id(value).map_err(operation_error)
}

/// A failure before the region: only setup was timed.
fn before_region(started: Instant, fixture_digest: Option<String>, failure: Failure) -> Outcome {
    Outcome {
        fixture_digest,
        setup_nanos: started.elapsed().as_nanos(),
        region_nanos: None,
        result: Err(failure),
    }
}

/// Run `setup`, then `region` once, then `observe`, timing setup and region separately.
fn sample<S, R>(
    started: Instant,
    setup: impl FnOnce() -> Result<(Option<String>, S), Failure>,
    region: impl FnOnce(&mut S) -> R,
    observe: impl FnOnce(S, R) -> Result<Observation, Failure>,
) -> Outcome {
    let setup_span = perf_trace::span(Phase::ProbeSetup);
    let (fixture_digest, mut state) = match setup() {
        Ok(value) => value,
        Err(failure) => return before_region(started, None, failure),
    };
    drop(setup_span);
    let setup_nanos = started.elapsed().as_nanos();
    // A profiling build brackets the region outside its clock (ADR-0174).
    let bracket = control::begin();
    let region_started = Instant::now();
    let region_span = perf_trace::span(Phase::ProbeRegion);
    let result = black_box(region(black_box(&mut state)));
    // A calibration trial's loop runs inside the region, after the operation (ADR-0181).
    review_core::perf_calibration::inject();
    drop(region_span);
    let region_nanos = region_started.elapsed().as_nanos();
    drop(bracket);
    Outcome {
        fixture_digest,
        setup_nanos,
        region_nanos: Some(region_nanos),
        result: observe(state, result),
    }
}

pub fn execute(request: &Request, work: &Path) -> Outcome {
    let started = Instant::now();
    match request.contract.contract_id {
        "canonical_serialize_v1" => canonical_serialize(request, work, started),
        "canonical_validate_v1" => canonical_validate(request, work, started),
        "cas_raw_get_verify_v1" => cas_raw_get(request, work, started),
        "cas_typed_get_validate_v1" => cas_typed_get(request, work, started),
        "source_capture_v2" => source_capture(request, work, started),
        "source_capture_seed_v1" => source_capture_seed(request, work, started),
        "state_projection_v2" => state_projection(request, work, started),
        "state_projection_seed_v1" => state_projection_seed(work, started),
        "authority_seed_v1" => authority_seed(request, work, started),
        "authority_plan_check_v2" | "authority_runtime_new_v2" => authority_sample_from(
            request.text("package"),
            request
                .seed
                .as_deref()
                .expect("an admitted seed reader has its seed"),
            if request.contract.contract_id == "authority_plan_check_v2" {
                AuthorityRegion::CurrentPlanCheck
            } else {
                AuthorityRegion::TaskRuntimeNew
            },
            PRODUCTION_LEASE,
            started,
            SetupHooks::default(),
            true,
        ),
        "persistence_fs_v1" => persistence(request, work, started),
        "cli_state_fixture_v1" => cli_state_fixture(work, started),
        "cli_campaign_fixture_v1" => cli_campaign_fixture(request, started),
        "sentinel_canonical_v1" => sentinel(request, started),
        other => before_region(
            started,
            None,
            Failure::new(
                FailureKind::FixtureFailed,
                format!("no workload for {other}"),
            ),
        ),
    }
}

// Typed envelopes -----------------------------------------------------------------------------

/// Write the named typed-envelope fixture through the product's writer into `cas` and check its
/// stored size is exactly the declared full serialized envelope size.
pub fn write_envelope(cas: &Cas, fixture_id: &str) -> Result<(String, ArtifactEnvelope), Failure> {
    let fixture = fixtures::envelope_fixture(fixture_id)
        .ok_or_else(|| fixture_error(format!("{fixture_id} is not a typed envelope fixture")))?;
    let (id, envelope) = match fixture.envelope {
        Envelope::Requirements { text_bytes } => cas
            .put_artifact(
                fixtures::REQUIREMENTS_V1,
                fixtures::producer(),
                vec![],
                None,
                fixtures::requirements_payload(text_bytes),
            )
            .map_err(fixture_error)?,
        Envelope::Package { digits, tens } => {
            let id = authority::capture_author_package(cas, Some(fixtures::padding(digits, tens)))
                .map_err(fixture_error)?;
            let envelope = cas.get_artifact(&id).map_err(fixture_error)?;
            (id, envelope)
        }
    };
    let stored = cas.stored_len(&id).map_err(fixture_error)?;
    if stored != fixture.serialized_bytes as u64 {
        return Err(fixture_error(format!(
            "{fixture_id} generated a {stored}-byte envelope; the fixture declares exactly {}",
            fixture.serialized_bytes
        )));
    }
    Ok((id, envelope))
}

/// The semantic projection of a decoded, validated envelope; both identities are verified
/// before it exists.
pub fn envelope_projection(envelope: &ArtifactEnvelope, envelope_bytes: u64) -> Value {
    let producer = match envelope.producer {
        Producer::Attempt { .. } => "attempt",
        Producer::KernelOperation { .. } => "kernel_operation",
    };
    json!({
        "artifact_id": envelope.artifact_id,
        "artifact_type": envelope.artifact_type,
        "content_id": envelope.content_id,
        "envelope_bytes": envelope_bytes,
        "input_artifacts": envelope.input_artifacts.len(),
        "producer_kind": producer,
    })
}

fn canonical_serialize(request: &Request, work: &Path, started: Instant) -> Outcome {
    let package = request.text("package");
    sample(
        started,
        || {
            let cas = Cas::open(work.join("cas")).map_err(fixture_error)?;
            // Decoding the fixture into the typed value is setup.
            let (id, envelope) = write_envelope(&cas, package)?;
            Ok((Some(id), envelope))
        },
        |envelope| {
            serde_json::to_value(&*envelope)
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    review_store::canonicalize(&value).map_err(|error| error.to_string())
                })
        },
        |_, bytes| {
            let bytes = bytes.map_err(operation_error)?;
            Ok(observed(
                blob_content_id(&bytes),
                bytes.len() as u64,
                format!(
                    "fixture={package} serialized_envelope_bytes={}",
                    bytes.len()
                ),
            ))
        },
    )
}

fn canonical_validate(request: &Request, work: &Path, started: Instant) -> Outcome {
    let package = request.text("package");
    sample(
        started,
        || {
            let cas = Cas::open(work.join("cas")).map_err(fixture_error)?;
            let (id, _) = write_envelope(&cas, package)?;
            // Reading the fixture bytes into memory is setup.
            let bytes = cas.get(&id).map_err(fixture_error)?;
            Ok((Some(id), bytes))
        },
        |bytes| {
            serde_json::from_slice::<ArtifactEnvelope>(bytes)
                .map_err(|error| error.to_string())
                .and_then(|envelope| review_store::validate_envelope(&envelope).map(|()| envelope))
        },
        |bytes, envelope| {
            let envelope = envelope.map_err(operation_error)?;
            let projection = envelope_projection(&envelope, bytes.len() as u64);
            Ok(observed(
                digest_of(&projection)?,
                bytes.len() as u64,
                format!("fixture={package} validated_envelope_bytes={}", bytes.len()),
            ))
        },
    )
}

/// A fresh CAS holding the fixture envelope, flushed and reopened so no in-process state of
/// the writer survives into the region.
fn populated_cas(work: &Path, package: &str) -> Result<(String, Cas), Failure> {
    let root = work.join("cas");
    let cas = Cas::open(&root).map_err(fixture_error)?;
    let (id, _) = write_envelope(&cas, package)?;
    cas.flush().map_err(fixture_error)?;
    drop(cas);
    Ok((id, Cas::open_existing(&root).map_err(fixture_error)?))
}

fn cas_raw_get(request: &Request, work: &Path, started: Instant) -> Outcome {
    let package = request.text("package");
    sample(
        started,
        || {
            let (id, cas) = populated_cas(work, package)?;
            // The envelope's exact bytes, filed as a raw object: get/verify covers its full size.
            let bytes = cas.get(&id).map_err(fixture_error)?;
            let raw = cas.put(&bytes).map_err(fixture_error)?;
            cas.flush().map_err(fixture_error)?;
            drop(cas);
            let cas = Cas::open_existing(work.join("cas")).map_err(fixture_error)?;
            Ok((Some(id), (cas, raw)))
        },
        |(cas, raw)| cas.get(raw),
        |(_, raw), bytes| {
            let bytes = bytes.map_err(operation_error)?;
            let digest = blob_content_id(&bytes);
            if digest != raw {
                return Err(operation_error("raw get returned bytes of another object"));
            }
            Ok(observed(
                digest,
                bytes.len() as u64,
                format!("fixture={package} raw_object_bytes={}", bytes.len()),
            ))
        },
    )
}

fn cas_typed_get(request: &Request, work: &Path, started: Instant) -> Outcome {
    let package = request.text("package");
    sample(
        started,
        || {
            let (id, cas) = populated_cas(work, package)?;
            Ok((Some(id.clone()), (cas, id)))
        },
        |(cas, id)| cas.get_artifact(id),
        |(cas, id), envelope| {
            let envelope = envelope.map_err(operation_error)?;
            let bytes = cas.stored_len(&id).map_err(operation_error)?;
            let projection = envelope_projection(&envelope, bytes);
            Ok(observed(
                digest_of(&projection)?,
                bytes,
                format!("fixture={package} typed_envelope_bytes={bytes}"),
            ))
        },
    )
}

// Source capture ------------------------------------------------------------------------------

fn git(repo: &Path, home: &Path, args: &[&str]) -> Result<(), Failure> {
    let output = Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .env("GIT_AUTHOR_NAME", "AF performance fixture")
        .env("GIT_AUTHOR_EMAIL", "perf-fixture@invalid")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_NAME", "AF performance fixture")
        .env("GIT_COMMITTER_EMAIL", "perf-fixture@invalid")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .current_dir(repo)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.autocrlf=false",
            // A commit of 10,000 loose objects would otherwise start a detached `gc --auto`
            // that repacks the repository after the writer exits: the seed must be complete
            // and unchanging once every process of its writer has exited (ADR-0170).
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
        ])
        .args(args)
        .output()
        .map_err(fixture_error)?;
    if !output.status.success() {
        return Err(fixture_error(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(())
}

/// A committed repository of `files` deterministic 128-byte files at `root/repo`, written
/// through Git with the private Git home `root/home`; nothing else is written.
pub fn source_repository(root: &Path, files: usize) -> Result<(), Failure> {
    let repo_root = root.join("repo");
    let home = root.join("home");
    for directory in [&repo_root, &home] {
        std::fs::create_dir_all(directory).map_err(fixture_error)?;
    }
    for index in 0..files {
        let (path, bytes) = fixtures::source_file(index);
        let target = repo_root.join(&path);
        std::fs::create_dir_all(target.parent().expect("file has a directory"))
            .map_err(fixture_error)?;
        std::fs::write(&target, bytes).map_err(fixture_error)?;
    }
    git(&repo_root, &home, &["init", "-q", "-b", "main"])?;
    git(&repo_root, &home, &["add", "-A"])?;
    git(
        &repo_root,
        &home,
        &["commit", "-q", "-m", "performance fixture"],
    )
}

/// The opened source repository a sample captures, below `root` as [`source_repository`] wrote
/// it, and a fresh CAS in the sample's own work directory.
fn opened_repository(root: &Path, work: &Path) -> Result<(Repo, Cas), Failure> {
    let repo_root = root.join("repo");
    let metadata = std::fs::symlink_metadata(repo_root.join(".git")).map_err(|error| {
        fixture_error(format!(
            "the seed holds no committed repository at {}: {error}",
            repo_root.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(fixture_error("the seed's repository has no .git directory"));
    }
    let cas = Cas::open(work.join("cas")).map_err(fixture_error)?;
    Ok((Repo::open(&repo_root, root.join("home")), cas))
}

/// The written working tree of `repo` as a source manifest: every regular file outside `.git`,
/// with the CAS content identity of its bytes as read back, in path-byte order.
fn written_manifest(repo: &Path) -> Result<Manifest, Failure> {
    let mut entries = Vec::new();
    let mut pending = vec![repo.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for item in std::fs::read_dir(&directory).map_err(operation_error)? {
            let item = item.map_err(operation_error)?;
            let path = item.path();
            let kind = item.file_type().map_err(operation_error)?;
            if kind.is_dir() {
                if path != repo.join(".git") {
                    pending.push(path);
                }
            } else if kind.is_file() {
                let bytes = std::fs::read(&path).map_err(operation_error)?;
                let relative = path
                    .strip_prefix(repo)
                    .map_err(operation_error)?
                    .to_str()
                    .ok_or_else(|| operation_error("a written path is not UTF-8"))?
                    .to_string();
                entries.push(Entry {
                    path: relative,
                    kind: EntryKind::File,
                    content: review_source_git::digest_bytes(&bytes),
                    size: bytes.len() as u64,
                });
            } else {
                return Err(operation_error(format!(
                    "{} is neither a directory nor a regular file",
                    path.display()
                )));
            }
        }
    }
    entries.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    let manifest = Manifest { entries };
    manifest.validate().map_err(operation_error)?;
    Ok(manifest)
}

/// The source capture seed (ADR-0170): the committed repository its side's readers copy. Its
/// check reads the written tree back and reports its source-manifest digest, which the driver
/// compares with the golden seed entry before any sample copies it.
fn source_capture_seed(request: &Request, work: &Path, started: Instant) -> Outcome {
    let files = request.integer("files") as usize;
    sample(
        started,
        || Ok((None, source_repository(work, files)?)),
        |()| written_manifest(&work.join("repo")),
        |(), manifest| {
            let manifest = manifest?;
            Ok(observed(
                manifest.content_digest(),
                manifest.entries.len() as u64,
                format!(
                    "files={} file_bytes={} seed=committed_repository",
                    manifest.entries.len(),
                    fixtures::SOURCE_FILE_BYTES
                ),
            ))
        },
    )
}

fn source_capture(request: &Request, work: &Path, started: Instant) -> Outcome {
    let files = request.integer("files") as usize;
    let seed = request
        .seed
        .clone()
        .expect("an admitted seed reader has its seed");
    sample(
        started,
        || Ok((None, opened_repository(&seed, work)?)),
        |(repo, cas)| Capture::new(repo, cas).committed("HEAD"),
        |(_, cas), snapshot| {
            let snapshot = snapshot.map_err(operation_error)?;
            let entries = &snapshot.manifest.entries;
            if entries.len() != files {
                return Err(operation_error(format!(
                    "captured {} entries of {files} files",
                    entries.len()
                )));
            }
            for entry in entries {
                let stored = cas.verify(&entry.content).map_err(operation_error)?;
                if entry.kind != EntryKind::File
                    || entry.size != fixtures::SOURCE_FILE_BYTES as u64
                    || stored != entry.size
                {
                    return Err(operation_error(format!(
                        "{} is not a stored 128-byte file",
                        entry.path
                    )));
                }
            }
            Ok(observed(
                snapshot.content_digest.clone(),
                entries.len() as u64,
                format!("files={files} file_bytes={}", fixtures::SOURCE_FILE_BYTES),
            ))
        },
    )
}

// State projection ----------------------------------------------------------------------------

/// The admitted fixture Task with exactly `events` relevant events in its log: open, propose,
/// admit and production lease renewals, each through the protected public writer.
pub fn event_log(work: &Path, events: u64) -> Result<(), Failure> {
    let mut fixture =
        authority::build(work, None, fixtures::TASK_LEASE_MS).map_err(fixture_error)?;
    let run = fixture.run_id().map_err(fixture_error)?;
    let mut length = fixture.store.len(&run).map_err(fixture_error)?;
    while length < events {
        fixture.renew().map_err(fixture_error)?;
        length += 1;
    }
    let recorded = fixture.store.len(&run).map_err(fixture_error)?;
    if recorded != events {
        return Err(fixture_error(format!(
            "the fixture Task log holds {recorded} events, not exactly {events}"
        )));
    }
    Ok(())
}

pub fn state_projection_value(
    state: &review_store::store::task::TaskProjection,
    events: u64,
) -> Result<Value, Failure> {
    Ok(json!({
        "task_id": state.task_id,
        "phase": serde_json::to_value(&state.phase).map_err(operation_error)?,
        "admitted": state.admitted,
        "plan_recorded": state.plan_id.is_some(),
        "next_sequence": state.next_sequence,
        "events": events,
    }))
}

/// The state projection seed (ADR-0170): exactly [`fixtures::PROJECTION_EVENTS`] events written
/// through the protected public writer into `work`, which its side's readers copy. Its check
/// projects them through a fresh Store, so the message's count is what was recorded.
fn state_projection_seed(work: &Path, started: Instant) -> Outcome {
    state_projection_sample(
        work,
        fixtures::PROJECTION_EVENTS,
        false,
        started,
        |_| Ok(()),
    )
}

fn state_projection(request: &Request, _work: &Path, started: Instant) -> Outcome {
    let prewarmed = request.text("projection") == "prewarmed";
    let seed = request
        .seed
        .clone()
        .expect("an admitted seed reader has its seed");
    sample(
        started,
        || {
            // An absent log is the seed's failure, never a new empty Store.
            let log = seed.join("events.sqlite");
            if !std::fs::symlink_metadata(&log).is_ok_and(|metadata| metadata.is_file()) {
                return Err(fixture_error(format!(
                    "the seed holds no event log at {}",
                    log.display()
                )));
            }
            Ok((None, opened_projection(&seed, prewarmed)?))
        },
        project,
        |state, projected| observe_projection(state, projected, prewarmed),
    )
}

/// A newly opened Store over the log below `root` has no projection memo: application-cold.
/// Prewarming is one full projection on that connection, still setup.
fn opened_projection(root: &Path, prewarmed: bool) -> Result<(EventStore, Cas), Failure> {
    let store = EventStore::open(root.join("events.sqlite")).map_err(fixture_error)?;
    let cas = Cas::open_existing(root.join("cas")).map_err(fixture_error)?;
    if prewarmed {
        store
            .task_projection(&cas, authority::TASK_ID)
            .map_err(fixture_error)?
            .ok_or_else(|| fixture_error("the fixture Task is unknown"))?;
    }
    Ok((store, cas))
}

type Projected = Result<Option<review_store::store::task::TaskProjection>, StoreError>;

fn project(state: &mut (EventStore, Cas)) -> Projected {
    let (store, cas) = state;
    store.task_projection(cas, authority::TASK_ID)
}

fn observe_projection(
    (store, _): (EventStore, Cas),
    state: Projected,
    prewarmed: bool,
) -> Result<Observation, Failure> {
    let state = state
        .map_err(operation_error)?
        .ok_or_else(|| operation_error("the projected Task is unknown"))?;
    let run =
        review_store::store::task::task_run_id(authority::TASK_ID).map_err(operation_error)?;
    let recorded = store.len(&run).map_err(operation_error)?;
    let projection = state_projection_value(&state, recorded)?;
    Ok(observed(
        digest_of(&projection)?,
        recorded,
        format!(
            "events={recorded} projection={}",
            if prewarmed {
                "prewarmed"
            } else {
                "application_cold"
            }
        ),
    ))
}

/// The state projection region over a fixture log of exactly `events` events written in this
/// sample's `work`. `after_log` runs once the log is written and before the Store is reopened;
/// only tests use it, to damage the fixture and prove the production projection's refusal is
/// reported, never hidden.
pub fn state_projection_sample(
    work: &Path,
    events: u64,
    prewarmed: bool,
    started: Instant,
    after_log: impl FnOnce(&Path) -> Result<(), Failure>,
) -> Outcome {
    sample(
        started,
        || {
            event_log(work, events)?;
            after_log(work)?;
            Ok((None, opened_projection(work, prewarmed)?))
        },
        project,
        |state, projected| observe_projection(state, projected, prewarmed),
    )
}

/// The CLI state fixture: the admitted fixture Task with exactly
/// [`fixtures::CLI_STATE_EVENTS`] events, written through the protected public writer into
/// `work`, the state directory (`events.sqlite`, `cas`) a side's `af task show --state` reads.
/// Its region re-reads that log through a fresh Store so the message's count is what was
/// recorded, never what was intended.
fn cli_state_fixture(work: &Path, started: Instant) -> Outcome {
    state_projection_sample(work, fixtures::CLI_STATE_EVENTS, false, started, |_| Ok(()))
}

/// The CLI Campaign fixture (ADR-0169). The side's own `af review run` wrote the Campaign
/// [`fixtures::CLI_CAMPAIGN`] with exactly [`fixtures::CLI_CAMPAIGN_FINDINGS`] Findings into the
/// state directory [`fixtures::CLI_CAMPAIGN_STATE`] beside this request's scratch. Its setup
/// tops that Campaign's log up to exactly [`fixtures::CLI_CAMPAIGN_EVENTS`] events through the
/// side's public Findings Ledger writer ([`Ingest`]), the one `af review group` and `af review
/// ungroup` use: the second Finding is grouped under the first and released again, pair after
/// pair, so the Ledger ends exactly as the review left it. An odd remainder first advances the
/// policy clock once, through the writer `af review policy-time advance` uses. Every event is
/// one the Ledger projection folds. Its region rebuilds that projection through a fresh Store,
/// so the message's count is what was recorded, never what was intended.
///
/// [`Ingest`]: review_store::Ingest
fn cli_campaign_fixture(request: &Request, started: Instant) -> Outcome {
    let run = format!("campaign-{}", fixtures::CLI_CAMPAIGN);
    let events = fixtures::CLI_CAMPAIGN_EVENTS;
    sample(
        started,
        || {
            let state = request
                .scratch
                .parent()
                .ok_or_else(|| fixture_error("the writer's scratch has no parent"))?
                .join(fixtures::CLI_CAMPAIGN_STATE);
            let metadata = std::fs::symlink_metadata(&state).map_err(|error| {
                fixture_error(format!("the Campaign state {}: {error}", state.display()))
            })?;
            if !metadata.is_dir() {
                return Err(fixture_error(format!(
                    "the Campaign state {} is not a directory",
                    state.display()
                )));
            }
            {
                let mut store =
                    EventStore::open(state.join("events.sqlite")).map_err(fixture_error)?;
                let cas = Cas::open_existing(state.join("cas")).map_err(fixture_error)?;
                let mut length = store.len(&run).map_err(fixture_error)?;
                if length == 0 || length > events {
                    return Err(fixture_error(format!(
                        "the reviewed Campaign log holds {length} events; it must hold between \
                         1 and {events} before the top-up"
                    )));
                }
                let mut ingest =
                    review_store::Ingest::new(&mut store, &cas, &run).map_err(fixture_error)?;
                let keys: Vec<String> = ingest
                    .ledger()
                    .findings()
                    .iter()
                    .map(|finding| finding.key.clone())
                    .collect();
                if keys.len() != fixtures::CLI_CAMPAIGN_FINDINGS {
                    return Err(fixture_error(format!(
                        "the reviewed Campaign holds {} Findings, not {}",
                        keys.len(),
                        fixtures::CLI_CAMPAIGN_FINDINGS
                    )));
                }
                if (events - length) % 2 == 1 {
                    ingest
                        .advance_policy_time(1, "perf-fixture", "CLI Campaign fixture parity")
                        .map_err(fixture_error)?;
                    length += 1;
                }
                while length < events {
                    ingest.group(&keys[1], &keys[0]).map_err(fixture_error)?;
                    ingest.ungroup(&keys[1], &keys[0]).map_err(fixture_error)?;
                    length += 2;
                }
            }
            // Every writer handle is closed: the region reads through a fresh Store.
            let store = EventStore::open(state.join("events.sqlite")).map_err(fixture_error)?;
            let cas = Cas::open_existing(state.join("cas")).map_err(fixture_error)?;
            Ok((None, (store, cas)))
        },
        |(store, cas)| review_store::LedgerProjection::rebuild(store, cas, &run),
        |(store, _), projection| {
            let projection = projection.map_err(operation_error)?;
            let recorded = store.len(&run).map_err(operation_error)?;
            let ledger = projection.ledger();
            let views = ledger.finding_views();
            let open = views
                .iter()
                .filter(|finding| finding.status == review_store::Status::Open)
                .count();
            let value = json!({
                "campaign": fixtures::CLI_CAMPAIGN,
                "events": recorded,
                "findings": views.len(),
                "open": open,
                "round": ledger.round,
            });
            Ok(observed(
                digest_of(&value)?,
                recorded,
                format!(
                    "events={recorded} findings={} open={open} projection=application_cold",
                    views.len()
                ),
            ))
        },
    )
}

// Authority -----------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityRegion {
    CurrentPlanCheck,
    TaskRuntimeNew,
}

/// How a sample holds its production lease. Immediately before the region every state-persistence
/// request renews it, and every authority request takes a fresh one after its setup writer
/// handed its lease over ([`PRODUCTION_LEASE`]); only the expiry tests keep the setup lease and
/// let it lapse at that boundary, waiting until the Store itself reports it lapsed, to prove
/// expiry is reported as the side's failure and never retried.
#[derive(Debug, Clone, Copy)]
pub struct LeasePolicy {
    pub renew_before_region: bool,
    pub wait_for_expiry: bool,
}

pub const PRODUCTION_LEASE: LeasePolicy = LeasePolicy {
    renew_before_region: true,
    wait_for_expiry: false,
};

/// Wait until the Store itself reports the lease lapsed; bounded by twice the lease.
fn wait_for_expiry(store: &EventStore, lease: &review_store::store::task::TaskLease) {
    let deadline = Instant::now() + Duration::from_millis(2 * fixtures::TASK_LEASE_MS);
    while Instant::now() < deadline {
        match (lease_state(store, lease), authority::now_ms()) {
            (LeaseState::Live(until), Ok(now)) if now < until => {
                std::thread::sleep(Duration::from_millis(until - now + 10));
            }
            // Lapsed, unreadable or a clock error: the classification that follows says which.
            _ => return,
        }
    }
}

/// The Store's own spelling of a lapsed lease: [`EventStore::task_lease_state`] returns a
/// [`StoreError::Conflict`] starting with exactly this when it observes the lease expired at its
/// clock or the stream written by another writer or epoch (fenced), and for nothing else.
const LEASE_LAPSED: &str = "Task writer lease is expired or fenced";

/// What the Store itself establishes about a lease, read once after a failure.
#[derive(Debug, PartialEq, Eq)]
pub enum LeaseState {
    /// The Store observed the lease held and live, until this Unix millisecond.
    Live(u64),
    /// The Store established expiry or fencing; its own diagnostic.
    Lapsed(String),
    /// The state could not be read: a SQLite, decoding, malformed-transition or clock error.
    /// Expiry is not established.
    Unreadable(String),
}

pub fn lease_state(store: &EventStore, lease: &review_store::store::task::TaskLease) -> LeaseState {
    match store.task_lease_state(lease) {
        Ok(until) => LeaseState::Live(until),
        Err(StoreError::Conflict(what)) if what.starts_with(LEASE_LAPSED) => {
            LeaseState::Lapsed(what)
        }
        Err(error) => LeaseState::Unreadable(error.to_string()),
    }
}

/// Classify a failed lease-bound operation by the lease state the Store establishes after it.
/// It is `lease_expired` only when the Store reports the lease lapsed or fenced; otherwise it is
/// `otherwise`, the operation's or fixture's own failure. The original diagnostic always leads,
/// and an unreadable lease state is appended to it, never mistaken for expiry. Nothing is
/// retried.
pub fn classify(
    store: &EventStore,
    lease: &review_store::store::task::TaskLease,
    error: String,
    otherwise: FailureKind,
) -> Failure {
    match lease_state(store, lease) {
        LeaseState::Lapsed(state) => Failure::new(
            FailureKind::LeaseExpired,
            format!("{error}; the Store reports: {state}"),
        ),
        LeaseState::Live(_) => Failure::new(otherwise, error),
        LeaseState::Unreadable(state) => Failure::new(
            otherwise,
            format!(
                "{error}; the lease state is unreadable, so expiry is not established: {state}"
            ),
        ),
    }
}

fn load_plan_and_graph(
    cas: &Cas,
    plan_id: &str,
) -> Result<(ExecutionPlanV1, CompiledTask), Failure> {
    let plan: ExecutionPlanV1 =
        serde_json::from_value(cas.get_artifact(plan_id).map_err(operation_error)?.payload)
            .map_err(operation_error)?;
    let graph = compiled_graph(cas, &plan)?;
    Ok((plan, graph))
}

fn compiled_graph(cas: &Cas, plan: &ExecutionPlanV1) -> Result<CompiledTask, Failure> {
    serde_json::from_value(
        cas.get_artifact(&plan.compiled_graph_id)
            .map_err(operation_error)?
            .payload,
    )
    .map_err(operation_error)
}

/// Write one admitted Task through this side's own public writers. Both setup leases are
/// released while live; the process exits and closes every handle before the driver seals it.
/// Its original revision deadline is preserved in every copy, never extended by a reader.
fn authority_seed(request: &Request, work: &Path, started: Instant) -> Outcome {
    let package = request.text("package");
    sample(
        started,
        || {
            let Some(Envelope::Package { digits, tens }) =
                fixtures::envelope_fixture(package).map(|fixture| fixture.envelope)
            else {
                return Err(fixture_error("an authority seed needs a package fixture"));
            };
            let (fixture, author) = authority::build_then(
                work,
                Some(fixtures::padding(digits, tens)),
                fixtures::TASK_LEASE_MS,
                authority::Setup {
                    hand_over: true,
                    ..authority::Setup::default()
                },
                |_, _, _, packages, _| packages[fixtures::AUTHOR_PACKAGE].clone(),
            )?;
            Ok((Some(author), fixture))
        },
        |fixture| {
            let state = fixture
                .store
                .task_projection(&fixture.cas, authority::TASK_ID)
                .map_err(operation_error)?
                .ok_or_else(|| operation_error("the seed Task is unknown"))?;
            let plan_id = state
                .plan_id
                .as_deref()
                .ok_or_else(|| operation_error("the seed Task has no plan"))?;
            let (plan, graph) = load_plan_and_graph(&fixture.cas, plan_id)?;
            authority::semantic_projection(&fixture.cas, &state, &plan, &graph)
                .map_err(operation_error)
        },
        |_, projection| {
            let projection = projection?;
            let bytes = projection["packages"][fixtures::AUTHOR_PACKAGE]["envelope_bytes"]
                .as_u64()
                .ok_or_else(|| operation_error("the seed lacks its author package"))?;
            Ok(observed(
                digest_of(&projection)?,
                bytes,
                format!("package={package} seed=admitted_task setup_leases=released"),
            ))
        },
    )
}

/// Measure the current-plan check or the first `TaskRuntime::new` of the two-node graph whose
/// author package envelope is the named fixture's exact size. The setup writes' authority is
/// captured before the first setup lease opens, and each setup write holds a production lease of
/// its own that ends after it ([`authority::build_then`]). After setup, outside every lease, the
/// region captures a host and authority no setup write used, whose plan validator is cold
/// ([`authority::require_cold`]); then a fresh production lease is taken and a fresh Store
/// connection opened immediately before the region, so the region's lease is whole and its
/// projection application-cold.
#[cfg(test)]
pub fn authority_sample(
    package: &str,
    work: &Path,
    region: AuthorityRegion,
    lease: LeasePolicy,
    started: Instant,
) -> Outcome {
    authority_sample_with(package, work, region, lease, started, SetupHooks::default())
}

/// Test hooks of [`authority_sample_with`], run where setup's own costs lie; only a test passes
/// ones that do anything.
pub struct SetupHooks<'a> {
    /// Between the setup capture and the first lease's opening.
    pub after_capture: Box<dyn FnOnce() -> Result<(), String> + 'a>,
    /// Between the proposal's released lease and the admission's fresh one.
    pub between_writes: Box<dyn FnOnce() -> Result<(), String> + 'a>,
    /// After the region's capture and before the region's lease is taken.
    pub after_region_capture: Box<dyn FnOnce() -> Result<(), String> + 'a>,
}

impl Default for SetupHooks<'_> {
    fn default() -> Self {
        SetupHooks {
            after_capture: Box::new(|| Ok(())),
            between_writes: Box::new(|| Ok(())),
            after_region_capture: Box::new(|| Ok(())),
        }
    }
}

/// [`authority_sample`] with test hooks where setup's costs lie.
#[cfg(test)]
pub fn authority_sample_with(
    package: &str,
    work: &Path,
    region: AuthorityRegion,
    lease: LeasePolicy,
    started: Instant,
    hooks: SetupHooks<'_>,
) -> Outcome {
    authority_sample_from(package, work, region, lease, started, hooks, false)
}

/// The seeded reader and the direct writer tests share exactly the same timed operation.
/// `restore` reads the private seed copy at `work`; it never reuses a setup lease.
pub fn authority_sample_from(
    package: &str,
    work: &Path,
    region: AuthorityRegion,
    lease: LeasePolicy,
    started: Instant,
    hooks: SetupHooks<'_>,
    restore: bool,
) -> Outcome {
    let setup_span = perf_trace::span(Phase::ProbeSetup);
    let padding = match fixtures::envelope_fixture(package).map(|fixture| fixture.envelope) {
        Some(Envelope::Package { digits, tens }) => fixtures::padding(digits, tens),
        _ => {
            return before_region(
                started,
                None,
                fixture_error(format!("{package} is not a package envelope fixture")),
            );
        }
    };
    let declared =
        fixtures::envelope_fixture(package).map(|fixture| fixture.serialized_bytes as u64);
    let store_path = work.join("events.sqlite");
    let cas_path = work.join("cas");
    let after_region_capture = hooks.after_region_capture;
    let measure = |store: &mut EventStore,
                   cas: &Cas,
                   task_lease: Option<&review_store::store::task::TaskLease>,
                   packages: &BTreeMap<String, String>,
                   inputs: &authority::Region<'_>| {
        let author = packages[fixtures::AUTHOR_PACKAGE].clone();
        match cas.stored_len(&author) {
            Ok(bytes) if Some(bytes) == declared => {}
            other => {
                return Err((
                    Some(author),
                    fixture_error(format!(
                        "the author package envelope is {other:?} bytes, not {declared:?}"
                    )),
                ));
            }
        }
        let measured = inputs.capture(|authority, host| {
            after_region_capture().map_err(|error| (Some(author.clone()), fixture_error(error)))?;
            // The setup writers' leases ended after their writes; the region's own
            // production lease is taken here, as the last Store write before it.
            let region_lease = if lease.renew_before_region {
                authority::take_fresh_lease(store, cas)
                    .map_err(|error| (Some(author.clone()), fixture_error(error)))?
            } else {
                task_lease
                    .expect("only direct writer tests keep a setup lease")
                    .clone()
            };
            if lease.wait_for_expiry {
                wait_for_expiry(store, &region_lease);
            }
            let task_lease = &region_lease;
            let fresh = EventStore::open(&store_path)
                .map_err(fixture_error)
                .and_then(|store| {
                    Cas::open_existing(&cas_path)
                        .map_err(fixture_error)
                        .map(|cas| (store, cas))
                })
                .and_then(|opened| {
                    authority::require_cold(authority)
                        .map_err(fixture_error)
                        .map(|()| opened)
                });
            let (mut fresh, fresh_cas) =
                fresh.map_err(|failure| (Some(author.clone()), failure))?;
            drop(setup_span);
            let setup_nanos = started.elapsed().as_nanos();
            let bracket = control::begin();
            let region_started = Instant::now();
            let region_span = perf_trace::span(Phase::ProbeRegion);
            let (region_nanos, result) = match region {
                AuthorityRegion::CurrentPlanCheck => {
                    let checked =
                        black_box(fresh.check_current_task_plan_for_recording(
                            &fresh_cas, task_lease, authority,
                        ));
                    review_core::perf_calibration::inject();
                    let nanos = region_started.elapsed().as_nanos();
                    drop(region_span);
                    drop(bracket);
                    let result = checked.map_err(|error| error.to_string()).and_then(|plan| {
                        fresh
                            .task_projection(&fresh_cas, authority::TASK_ID)
                            .map_err(|error| error.to_string())
                            .map(|state| (Some(plan), state))
                    });
                    (nanos, result)
                }
                AuthorityRegion::TaskRuntimeNew => {
                    let constructed = black_box(TaskRuntime::new(
                        &mut fresh,
                        &fresh_cas,
                        task_lease.clone(),
                        authority,
                        host,
                    ));
                    review_core::perf_calibration::inject();
                    let nanos = region_started.elapsed().as_nanos();
                    drop(region_span);
                    drop(bracket);
                    // The constructed runtime's own projection names the plan it loaded.
                    let result = constructed
                        .and_then(|runtime| runtime.projection())
                        .map(|state| (None, Some(state)));
                    (nanos, result)
                }
            };
            let result = result
                .map_err(|error| classify(&fresh, task_lease, error, FailureKind::OperationFailed));
            Ok((author.clone(), setup_nanos, region_nanos, result))
        });
        measured.map_err(|error| (Some(author.clone()), fixture_error(error)))?
    };
    let built = if restore {
        authority::restore_then(work, Some(padding), |store, cas, packages, inputs| {
            measure(store, cas, None, packages, inputs)
        })
    } else {
        authority::build_then(
            work,
            Some(padding),
            fixtures::TASK_LEASE_MS,
            authority::Setup {
                hand_over: lease.renew_before_region,
                after_capture: hooks.after_capture,
                between_writes: hooks.between_writes,
                ..authority::Setup::default()
            },
            |store, cas, lease, packages, inputs| {
                measure(store, cas, Some(lease), packages, inputs)
            },
        )
        .map(|(fixture, value)| (fixture.cas, value))
    };
    let (cas, author, setup_nanos, region_nanos, result) = match built {
        Ok((cas, Ok((author, setup, region, result)))) => (cas, author, setup, region, result),
        Ok((_, Err((author, failure)))) => return before_region(started, author, failure),
        Err(failure) => return before_region(started, None, failure),
    };
    let result = result.and_then(|(plan, state)| {
        let state = state.ok_or_else(|| operation_error("the fixture Task is unknown"))?;
        let plan_id = state
            .plan_id
            .clone()
            .ok_or_else(|| operation_error("the fixture Task has no plan"))?;
        let (plan, graph) = match plan {
            Some(plan) => {
                let graph = compiled_graph(&cas, &plan)?;
                (plan, graph)
            }
            None => load_plan_and_graph(&cas, &plan_id)?,
        };
        let projection =
            authority::semantic_projection(&cas, &state, &plan, &graph).map_err(operation_error)?;
        let bytes = projection["packages"][fixtures::AUTHOR_PACKAGE]["envelope_bytes"]
            .as_u64()
            .ok_or_else(|| operation_error("the plan does not depend on the author package"))?;
        Ok(observed(
            digest_of(&projection)?,
            bytes,
            format!(
                "package={package} author_envelope_bytes={bytes} nodes={}",
                graph.nodes.len()
            ),
        ))
    });
    Outcome {
        fixture_digest: Some(author),
        setup_nanos,
        region_nanos: Some(region_nanos),
        result,
    }
}

// Persistence diagnostic ----------------------------------------------------------------------

fn persistence(request: &Request, work: &Path, started: Instant) -> Outcome {
    match request.text("operation") {
        "cas_write_read" => sample(
            started,
            || {
                let blob = fixtures::persistence_blob();
                let cas = Cas::open(work.join("cas")).map_err(fixture_error)?;
                Ok((Some(blob_content_id(&blob)), (cas, blob)))
            },
            // Flush semantics: Cas::flush fsyncs the object, its shard directory, `objects`,
            // the CAS root and the root's parent (plain fsync, as the event store's SQLite).
            |(cas, blob)| {
                let digest = cas.put(blob)?;
                cas.flush()?;
                cas.get(&digest)
            },
            |(_, blob), read| {
                let read = read.map_err(operation_error)?;
                if read != blob {
                    return Err(operation_error("read back differs from the written object"));
                }
                let logical = (blob.len() + read.len()) as u64;
                let mut observation = observed(
                    blob_content_id(&read),
                    logical,
                    format!(
                        "flush=cas_fsync_object_and_directories written_bytes={} read_bytes={}",
                        blob.len(),
                        read.len()
                    ),
                );
                observation
                    .extra_metrics
                    .insert("logical_bytes", (logical.to_string(), "bytes"));
                Ok(observation)
            },
        ),
        _ => persistence_state_sample(work, PRODUCTION_LEASE, started),
    }
}

/// The advisory state round trip: [`fixtures::PERSISTENCE_EVENTS`] production lease renewals
/// appended and the log read back, with the production lease.
pub fn persistence_state_sample(work: &Path, lease: LeasePolicy, started: Instant) -> Outcome {
    persistence_state_sample_with(work, lease, started, |_| Ok(()))
}

/// [`persistence_state_sample`] with `before_refresh` run after every setup read of the Store
/// and before the lease refresh; only tests pass one that does anything.
///
/// Setup reads the run ID and the built log's length first, then refreshes the fixture's lease
/// as its last successful operation, so the region starts with a full production lease whatever
/// setup cost. The refresh appends exactly one event, counted into the prefix without reading
/// the Store again. A failed refresh, or a renewal that fails in the region, is classified by
/// the Store's own lease state, and the first failure ends the sample without retry. A
/// scheduler pause between the refresh and the region can still consume the lease; that is
/// reported as `lease_expired`, never hidden. Only the expiry test passes a policy other than
/// [`PRODUCTION_LEASE`].
pub fn persistence_state_sample_with(
    work: &Path,
    lease: LeasePolicy,
    started: Instant,
    before_refresh: impl FnOnce(&AuthorityFixture) -> Result<(), String>,
) -> Outcome {
    sample(
        started,
        || {
            let mut fixture =
                authority::build(work, None, fixtures::TASK_LEASE_MS).map_err(fixture_error)?;
            let run = fixture.run_id().map_err(fixture_error)?;
            let built = fixture.store.len(&run).map_err(fixture_error)?;
            before_refresh(&fixture).map_err(fixture_error)?;
            let prefix = if lease.renew_before_region {
                fixture.renew().map_err(|error| {
                    classify(
                        &fixture.store,
                        &fixture.lease,
                        error,
                        FailureKind::FixtureFailed,
                    )
                })?;
                built + 1
            } else {
                built
            };
            if lease.wait_for_expiry {
                wait_for_expiry(&fixture.store, &fixture.lease);
            }
            Ok((None, (fixture, run, prefix)))
        },
        // Flush semantics: each renewal is one SQLite transaction at synchronous=FULL in
        // WAL mode, committed before the next begins; the read is a full ordered replay.
        |(fixture, run, _): &mut (AuthorityFixture, String, u64)| {
            for _ in 0..fixtures::PERSISTENCE_EVENTS {
                if let Err(error) = fixture.renew() {
                    return Err(classify(
                        &fixture.store,
                        &fixture.lease,
                        error,
                        FailureKind::OperationFailed,
                    ));
                }
            }
            fixture.store.replay(run).map_err(operation_error)
        },
        |(_, _, prefix), events| {
            let events = events?;
            let appended = events.get(prefix as usize..).unwrap_or_default();
            let mut types = std::collections::BTreeSet::new();
            for event in appended {
                let spelled = serde_json::to_value(event.event_type).map_err(operation_error)?;
                types.insert(
                    spelled
                        .as_str()
                        .ok_or_else(|| operation_error("an event type is not a string"))?
                        .to_string(),
                );
            }
            let contiguous = appended
                .windows(2)
                .all(|pair| pair[1].sequence == pair[0].sequence + 1);
            if appended.len() as u64 != fixtures::PERSISTENCE_EVENTS || !contiguous {
                return Err(operation_error(format!(
                    "read back {} appended events (contiguous: {contiguous})",
                    appended.len()
                )));
            }
            // The canonical bytes of the appended events, metadata included: their timestamps
            // and identifiers vary, so this is reported, never compared with a golden value.
            let mut logical = 0_u64;
            for event in appended {
                let value = serde_json::to_value(event).map_err(operation_error)?;
                logical += review_store::canonicalize(&value)
                    .map_err(operation_error)?
                    .len() as u64;
            }
            let projection = json!({
                "appended": appended.len(),
                "event_types": types.into_iter().collect::<Vec<_>>(),
                "prefix_events": prefix,
            });
            let mut observation = observed(
                digest_of(&projection)?,
                appended.len() as u64,
                format!(
                    "flush=sqlite_wal_synchronous_full_per_event appended={} prefix={prefix} \
                     lease=refreshed_before_region",
                    appended.len()
                ),
            );
            observation
                .extra_metrics
                .insert("logical_bytes", (logical.to_string(), "bytes"));
            Ok(observation)
        },
    )
}

// Health sentinel -----------------------------------------------------------------------------

fn sentinel(request: &Request, started: Instant) -> Outcome {
    let repetitions = request.repetitions as usize;
    sample(
        started,
        || Ok((None, fixtures::sentinel_value())),
        |value| {
            let mut outputs = Vec::with_capacity(repetitions);
            for _ in 0..repetitions {
                outputs.push(review_store::canonicalize(black_box(&*value)));
            }
            outputs
        },
        |_, outputs| {
            let outputs = outputs
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .map_err(operation_error)?;
            // Every repetition is checked: all of them must be the same bytes, and the driver's
            // golden digest covers their concatenation.
            if let Some(first) = outputs.first() {
                if let Some(index) = outputs.iter().position(|output| output != first) {
                    return Err(operation_error(format!(
                        "repetition {index} differs from the first"
                    )));
                }
            }
            let joined = outputs.concat();
            Ok(observed(
                blob_content_id(&joined),
                outputs.len() as u64,
                format!(
                    "repetitions={} canonical_bytes={}",
                    outputs.len(),
                    joined.len() / outputs.len().max(1)
                ),
            ))
        },
    )
}
