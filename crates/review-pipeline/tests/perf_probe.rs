//! The native performance probe (ADR-0163): its declared contracts equal the registry, every
//! request outside them is refused, and every native instance's real public-API execution
//! passes the trusted driver's golden oracle. A seeded instance reads a private copy of the seed
//! its writer contract wrote once (ADR-0170), and independent probe processes are shown to share
//! one immutable seed while each changes only its own copy. No test here asserts a timing
//! threshold; the probe's timings are recorded, never judged, in ordinary correctness CI.

#[path = "../examples/perf-probe/authority.rs"]
mod authority;
#[path = "../examples/perf-probe/cases.rs"]
mod cases;
#[path = "../examples/perf-probe/control.rs"]
mod control;
#[path = "../examples/perf-probe/fixtures.rs"]
mod fixtures;
#[path = "../examples/perf-probe/protocol.rs"]
mod protocol;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Instant;

use serde_json::{Value, json};

fn root() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn suite() -> toml::Value {
    toml::from_str(
        &std::fs::read_to_string(root().join("fixtures/performance/suite.toml")).unwrap(),
    )
    .unwrap()
}

fn golden() -> Value {
    serde_json::from_slice(
        &std::fs::read(root().join("fixtures/performance/golden/native.json")).unwrap(),
    )
    .unwrap()
}

/// A canonical, empty scratch directory the test owns.
fn empty_scratch() -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().canonicalize().unwrap();
    (directory, path)
}

fn request(entry: &Value, scratch: &Path) -> Value {
    seeded_request(entry, scratch, None)
}

fn seeded_request(entry: &Value, scratch: &Path, seed: Option<&Path>) -> Value {
    let repetitions = if entry["case_id"] == "sentinel_canonical" {
        fixtures::SENTINEL_REPETITIONS
    } else {
        1
    };
    json!({
        "schema": protocol::REQUEST_SCHEMA,
        "case_id": entry["case_id"],
        "instance_id": entry["instance_id"],
        "contract_id": entry["contract_id"],
        "parameters": entry["parameters"],
        "repetitions": repetitions,
        "scratch": scratch.to_str().unwrap(),
        "seed": seed.map(|seed| seed.to_str().unwrap()),
    })
}

/// Judge one recorded execution with the trusted Python driver, as a paired run will.
fn judge(request: &Value, reply: &protocol::Reply) -> Value {
    let records = tempfile::tempdir().unwrap();
    let request_path = records.path().join("request.json");
    let stdout_path = records.path().join("stdout");
    std::fs::write(&request_path, serde_json::to_vec(request).unwrap()).unwrap();
    std::fs::write(&stdout_path, &reply.stdout).unwrap();
    let output = Command::new("python3")
        .arg("-I")
        .arg(root().join("scripts/performance/cli.py"))
        .args(["judge", "--suite"])
        .arg(root().join("fixtures/performance/suite.toml"))
        .arg("--request")
        .arg(&request_path)
        .arg("--stdout")
        .arg(&stdout_path)
        .args(["--exit-code", &reply.code.to_string()])
        .output()
        .expect("python3 runs the trusted driver");
    assert!(
        output.status.code() == Some(0) || output.status.code() == Some(3),
        "the driver refused its input: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The golden seed entry of `seed_id`, which its writer's check must match (ADR-0170).
fn seed_entry(seed_id: &str) -> Value {
    golden()["seeds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["instance_id"] == seed_id)
        .unwrap_or_else(|| panic!("no golden seed entry {seed_id}"))
        .clone()
}

/// The seed a golden entry's instance reads, by its seed ID, or None.
fn seed_of(entry: &Value) -> Option<String> {
    let contract = protocol::contract(entry["case_id"].as_str().unwrap()).unwrap();
    let protocol::Seed::Reads {
        case_id,
        parameters,
        ..
    } = contract.seed
    else {
        return None;
    };
    let assignment: BTreeMap<String, protocol::Param> = parameters
        .iter()
        .map(|name| {
            let value = &entry["parameters"][*name];
            let param = match value.as_u64() {
                Some(number) => protocol::Param::Integer(number),
                None => protocol::Param::Text(value.as_str().unwrap().into()),
            };
            ((*name).to_string(), param)
        })
        .collect();
    Some(protocol::instance_id(case_id, &assignment))
}

/// Write the seed `seed_id` through the probe's own writer contract into a fresh scratch, as a
/// shard does once per side, and require the trusted driver to pass the writer's check against
/// the golden seed entry. Returns the scratch's owner and the seed's root.
fn write_seed(seed_id: &str) -> (tempfile::TempDir, PathBuf) {
    let entry = seed_entry(seed_id);
    let (owner, scratch) = empty_scratch();
    let request = request(&entry, &scratch);
    let reply = protocol::run(&serde_json::to_vec(&request).unwrap());
    assert_eq!(
        reply.code,
        0,
        "{seed_id}: {}",
        String::from_utf8_lossy(&reply.stdout)
    );
    let verdict = judge(&request, &reply);
    assert_eq!(verdict["status"], "passed", "{verdict}");
    (owner, scratch)
}

/// A private, writable copy of the seed rooted at `seed` below a fresh sample directory, as the
/// laboratory's `native_seed_copy` setup makes one: new directories and files, never links.
/// Returns the sample directory's owner, its probe scratch and the copy's work directory.
fn private_copy(seed: &Path) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let (owner, sample) = empty_scratch();
    copy_tree(seed, &sample.join("seed"));
    let scratch = sample.join("probe");
    std::fs::create_dir(&scratch).unwrap();
    (owner, scratch, sample.join("seed").join(protocol::WORK_DIR))
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir(to).unwrap();
    for item in std::fs::read_dir(from).unwrap() {
        let item = item.unwrap();
        let kind = item.file_type().unwrap();
        if kind.is_dir() {
            copy_tree(&item.path(), &to.join(item.file_name()));
        } else {
            assert!(kind.is_file(), "{} is not a file", item.path().display());
            std::fs::write(
                to.join(item.file_name()),
                std::fs::read(item.path()).unwrap(),
            )
            .unwrap();
        }
    }
}

/// Run every golden instance of `case_ids` through the real probe and the trusted oracle; a
/// seeded instance reads a private copy of the seed its writer wrote first.
fn passes_the_oracle(case_ids: &[&str]) {
    let golden = golden();
    let entries: Vec<&Value> = golden["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| case_ids.contains(&entry["case_id"].as_str().unwrap()))
        .collect();
    assert!(!entries.is_empty(), "no golden entries for {case_ids:?}");
    for entry in entries {
        let seeded = seed_of(entry).map(|seed_id| write_seed(&seed_id));
        let copy = seeded.as_ref().map(|(_, seed)| private_copy(seed));
        let (_owner, scratch) = match &copy {
            Some((_, scratch, _)) => (None, scratch.clone()),
            None => {
                let (owner, scratch) = empty_scratch();
                (Some(owner), scratch)
            }
        };
        let request = seeded_request(
            entry,
            &scratch,
            copy.as_ref().map(|(_, _, seed)| seed.as_path()),
        );
        let reply = protocol::run(&serde_json::to_vec(&request).unwrap());
        let message: Value = serde_json::from_slice(&reply.stdout).unwrap_or(Value::Null);
        assert_eq!(
            reply.code,
            0,
            "{}: {} {message}",
            entry["instance_id"],
            String::from_utf8_lossy(&reply.stderr)
        );
        let verdict = judge(&request, &reply);
        assert_eq!(verdict["status"], "passed", "{verdict} from {message}");
        // The region and its setup are both reported, as exact decimals.
        for key in ["wall_ms", "setup_ms"] {
            assert_eq!(message["metrics"][key]["unit"], "ms", "{message}");
        }
        // The probe wrote only below its work directory and removed nothing: the scratch is
        // the caller's to remove, here the test's temporary directory.
        let names: Vec<_> = std::fs::read_dir(&scratch)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, [protocol::WORK_DIR], "{}", entry["instance_id"]);
    }
}

#[test]
fn the_declared_contracts_are_the_registry_cases() {
    let suite = suite();
    let cases = suite["cases"].as_table().unwrap();
    let mut declared = 0;
    for (case_id, case) in cases {
        if case["kind"].as_str() != Some("library") {
            assert!(
                protocol::contract(case_id).is_none(),
                "{case_id} is not a library case"
            );
            continue;
        }
        declared += 1;
        let contract = protocol::contract(case_id).unwrap_or_else(|| panic!("{case_id}"));
        assert_eq!(
            case["probe"]["contract_id"].as_str(),
            Some(contract.contract_id)
        );
        assert_eq!(
            case["probe"]["repetitions"].as_integer(),
            Some(contract.repetitions as i64)
        );
        assert_eq!(case["timing"]["region"].as_str(), Some(contract.region));
        let cache = &case["cache"];
        assert_eq!(
            (
                cache["process"].as_str(),
                cache["store"].as_str(),
                cache["projection"].as_str(),
                cache["os_page_cache"].as_str()
            ),
            (
                Some(contract.cache.process),
                Some(contract.cache.store),
                Some(contract.cache.projection),
                Some(contract.cache.os_page_cache)
            ),
            "{case_id}"
        );
        let parameters = case["parameters"].as_table().unwrap();
        assert_eq!(parameters.len(), contract.parameters.len(), "{case_id}");
        for (name, domain) in contract.parameters {
            let declared = &parameters[*name];
            let values = declared["values"].as_array().unwrap();
            match domain {
                protocol::Domain::Fixture(expected) | protocol::Domain::Enum(expected) => {
                    let kind = if matches!(domain, protocol::Domain::Fixture(_)) {
                        "fixture"
                    } else {
                        "enum"
                    };
                    assert_eq!(declared["kind"].as_str(), Some(kind), "{case_id}.{name}");
                    let values: Vec<&str> = values.iter().map(|v| v.as_str().unwrap()).collect();
                    assert_eq!(values, *expected, "{case_id}.{name}");
                }
                protocol::Domain::Integer(expected) => {
                    assert_eq!(declared["kind"].as_str(), Some("integer"));
                    let values: Vec<u64> = values
                        .iter()
                        .map(|v| v.as_integer().unwrap() as u64)
                        .collect();
                    assert_eq!(values, *expected, "{case_id}.{name}");
                }
            }
        }
    }
    assert_eq!(
        declared + 6,
        protocol::CONTRACTS.len(),
        "plus the sentinel, the two CLI seed writers and the three native seed writers"
    );
    // A seeded case's probe reads exactly the seed the registry declares, which its writer
    // contract writes over exactly the declared parameters (ADR-0170).
    let mut writers = 0;
    for (case_id, case) in cases {
        let Some(contract) = protocol::contract(case_id) else {
            continue;
        };
        match (contract.seed, case["probe"].get("seed")) {
            (
                protocol::Seed::Reads {
                    case_id: writer_case,
                    contract_id,
                    parameters,
                },
                Some(declared),
            ) => {
                writers += 1;
                assert_eq!(declared["case_id"].as_str(), Some(writer_case));
                assert_eq!(declared["contract_id"].as_str(), Some(contract_id));
                let names: Vec<&str> = declared["parameters"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|name| name.as_str().unwrap())
                    .collect();
                assert_eq!(names, parameters, "{case_id}");
                let writer = protocol::contract(writer_case).unwrap();
                assert_eq!(writer.contract_id, contract_id);
                assert_eq!(writer.seed, protocol::Seed::Writes);
                assert!(suite["cases"].get(writer_case).is_none());
                for name in parameters {
                    let (_, domain) = contract
                        .parameters
                        .iter()
                        .find(|(declared, _)| declared == name)
                        .unwrap();
                    let (_, written) = writer
                        .parameters
                        .iter()
                        .find(|(declared, _)| declared == name)
                        .unwrap();
                    assert_eq!(format!("{domain:?}"), format!("{written:?}"), "{name}");
                }
            }
            (protocol::Seed::None, None) => {}
            (seed, declared) => panic!("{case_id}: probe {seed:?}, registry {declared:?}"),
        }
    }
    assert_eq!(
        writers, 4,
        "source capture, state projection and both authority cases read seeds"
    );
    // The CLI seed writers are the CLI registry's, not timed cases: their identities and exact
    // event counts are what `fixtures/performance/cli.toml` declares (ADR-0169).
    let cli: toml::Value = toml::from_str(
        &std::fs::read_to_string(root().join("fixtures/performance/cli.toml")).unwrap(),
    )
    .unwrap();
    for (seed, case_id, events) in [
        (
            "task_state",
            "cli_state_fixture",
            fixtures::CLI_STATE_EVENTS,
        ),
        (
            "campaign_state",
            "cli_campaign_fixture",
            fixtures::CLI_CAMPAIGN_EVENTS,
        ),
    ] {
        let writer = protocol::contract(case_id).unwrap();
        let declared_writer = &cli["seeds"][seed];
        assert_eq!(
            declared_writer["case_id"].as_str(),
            Some(writer.case_id),
            "{declared_writer}"
        );
        assert_eq!(
            declared_writer["contract_id"].as_str(),
            Some(writer.contract_id)
        );
        assert_eq!(declared_writer["events"].as_integer(), Some(events as i64));
        assert_eq!(
            suite["fixtures"]["cli_state_10000"]["events"].as_integer(),
            Some(events as i64)
        );
        assert!(suite["cases"].get(writer.case_id).is_none());
    }
    // The fixture Campaign's reviewer reports one Finding per line of its fixture file.
    let findings = std::fs::read_to_string(
        root().join("fixtures/performance/cli/campaign-roots/repo/src/fixture.txt"),
    )
    .unwrap();
    assert_eq!(findings.lines().count(), fixtures::CLI_CAMPAIGN_FINDINGS);
    let sentinel = protocol::contract("sentinel_canonical").unwrap();
    assert_eq!(
        suite["probe"]["sentinel_contract"].as_str(),
        Some(sentinel.contract_id)
    );
    assert_eq!(
        suite["health"]["batch_repetitions"].as_integer(),
        Some(sentinel.repetitions as i64)
    );
    assert_eq!(
        suite["product_constraints"]["task_lease_ms"].as_integer(),
        Some(fixtures::TASK_LEASE_MS as i64)
    );
    for fixture in fixtures::ENVELOPES {
        let declared = &suite["fixtures"][fixture.id];
        assert_eq!(
            declared["serialized_envelope_bytes"].as_integer(),
            Some(fixture.serialized_bytes as i64),
            "{}",
            fixture.id
        );
    }
    assert_eq!(
        suite["fixtures"]["source_files_128b"]["file_bytes"].as_integer(),
        Some(fixtures::SOURCE_FILE_BYTES as i64)
    );
    assert_eq!(
        suite["fixtures"]["events_10000"]["events"].as_integer(),
        Some(fixtures::PROJECTION_EVENTS as i64)
    );
}

fn refused(input: &[u8], reason: &str) {
    let reply = protocol::run(input);
    let stderr = String::from_utf8_lossy(&reply.stderr);
    assert_eq!(reply.code, 2, "{stderr}");
    assert!(
        reply.stdout.is_empty(),
        "a refused request writes no message"
    );
    assert!(stderr.contains(reason), "{stderr} lacks {reason}");
}

#[test]
fn requests_outside_the_declared_contracts_are_refused() {
    let (_owner, scratch) = empty_scratch();
    let (_seed_owner, seed_dir) = empty_scratch();
    let base = json!({
        "schema": protocol::REQUEST_SCHEMA, "case_id": "source_capture",
        "instance_id": "source_capture__files_100_tree_source_files_128b",
        "contract_id": "source_capture_v2",
        "parameters": {"files": 100, "tree": "source_files_128b"},
        "repetitions": 1, "scratch": scratch.to_str().unwrap(), "seed": seed_dir.to_str().unwrap(),
    });
    let with = |key: &str, value: Value| {
        let mut request = base.clone();
        request[key] = value;
        serde_json::to_vec(&request).unwrap()
    };
    refused(
        &with("case_id", json!("cli_discovery")),
        "is not declared by this probe",
    );
    refused(
        &with("contract_id", json!("source_capture_v1")),
        "is not case source_capture's contract",
    );
    refused(&with("extra", json!(1)), "unknown field `extra`");
    refused(&with("schema", json!("af.perf-probe-request/1")), "schema");
    // A seed reader needs its seed; a seed is never given to a contract that reads none, and
    // it lies outside the scratch (ADR-0170).
    refused(&with("seed", Value::Null), "reads a private copy");
    refused(
        &with("seed", json!("relative/seed")),
        "seed: must be absolute",
    );
    refused(
        &with("seed", json!(scratch.join("absent").to_str().unwrap())),
        "seed:",
    );
    let mut nested = base.clone();
    nested["scratch"] = json!(seed_dir.to_str().unwrap());
    refused(
        &serde_json::to_vec(&nested).unwrap(),
        "seed: must lie outside the scratch",
    );
    refused(
        &with(
            "parameters",
            json!({"files": 101, "tree": "source_files_128b"}),
        ),
        "outside the declared domain",
    );
    refused(
        &with(
            "parameters",
            json!({"files": "100", "tree": "source_files_128b"}),
        ),
        "outside the declared domain",
    );
    refused(
        &with("parameters", json!({"files": 100})),
        "parameter tree is missing",
    );
    refused(
        &with(
            "parameters",
            json!({"files": 100, "tree": "source_files_128b", "size": 1}),
        ),
        "parameter size is not declared",
    );
    refused(
        &with(
            "instance_id",
            json!("source_capture__files_10000_tree_source_files_128b"),
        ),
        "does not name this assignment",
    );
    refused(&with("repetitions", json!(2)), "repetitions 2 differ");
    refused(
        &with("scratch", json!("relative/scratch")),
        "must be absolute",
    );
    refused(
        &with("scratch", json!(format!("{}/../x", scratch.display()))),
        "scratch",
    );
    refused(
        &with("scratch", json!(scratch.join("absent").to_str().unwrap())),
        "scratch",
    );
    let duplicate = format!(
        r#"{{"schema":"{}","case_id":"source_capture","instance_id":"source_capture__files_100_tree_source_files_128b","contract_id":"source_capture_v2","parameters":{{"files":100,"files":100,"tree":"source_files_128b"}},"repetitions":1,"scratch":"{}","seed":"{}"}}"#,
        protocol::REQUEST_SCHEMA,
        scratch.display(),
        seed_dir.display()
    );
    refused(duplicate.as_bytes(), "duplicate key files");
    let duplicate_field = String::from_utf8(with("repetitions", json!(1)))
        .unwrap()
        .replacen(
            "\"repetitions\":1",
            "\"repetitions\":1,\"repetitions\":1",
            1,
        );
    refused(duplicate_field.as_bytes(), "duplicate field `repetitions`");
    let mut oversized = with("case_id", json!("source_capture"));
    oversized.splice(1..1, vec![b' '; protocol::RECORD_BYTES_MAX]);
    refused(&oversized, "exceeds 65536 bytes");
    // A symlinked or non-empty scratch is refused before anything is written.
    let link = scratch.join("link");
    let target = scratch.join("target");
    std::fs::create_dir(&target).unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    refused(
        &with("scratch", json!(link.to_str().unwrap())),
        "not canonical",
    );
    refused(
        &with("scratch", json!(scratch.to_str().unwrap())),
        "must be empty",
    );
    let mut sentinel = json!({
        "schema": protocol::REQUEST_SCHEMA, "case_id": "sentinel_canonical",
        "instance_id": "sentinel_canonical", "contract_id": "sentinel_canonical_v1",
        "parameters": {}, "repetitions": 63, "scratch": target.to_str().unwrap(), "seed": null,
    });
    refused(
        &serde_json::to_vec(&sentinel).unwrap(),
        "differ from the contract's declared 64",
    );
    sentinel["repetitions"] = json!(64);
    sentinel["seed"] = json!(seed_dir.to_str().unwrap());
    refused(&serde_json::to_vec(&sentinel).unwrap(), "reads no seed");
}

#[test]
fn timings_are_exact_canonical_decimals() {
    for (nanos, spelled) in [
        (0, "0"),
        (1, "0.000001"),
        (1_000_000, "1"),
        (1_500_000, "1.5"),
        (12_345_678_901, "12345.678901"),
        (100_000_000_000, "100000"),
    ] {
        assert_eq!(protocol::decimal_ms(nanos), spelled);
    }
}

#[test]
fn canonical_cas_and_sentinel_instances_pass_the_oracle() {
    passes_the_oracle(&[
        "canonical_serialize_small",
        "canonical_serialize",
        "canonical_validate_small",
        "canonical_validate",
        "cas_raw_get_small",
        "cas_raw_get",
        "cas_typed_get_small",
        "cas_typed_get",
        "sentinel_canonical",
    ]);
}

#[test]
fn source_capture_of_100_and_10000_files_passes_the_oracle() {
    passes_the_oracle(&["source_capture"]);
}

#[test]
fn authority_instances_at_both_package_sizes_pass_the_oracle() {
    passes_the_oracle(&["authority_plan_check", "authority_runtime_new"]);
}

#[test]
fn persistence_diagnostics_pass_the_oracle() {
    passes_the_oracle(&["persistence_fs"]);
}

fn state_instance(projection: &str) {
    let golden = golden();
    let entry = golden["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["parameters"]["projection"] == projection)
        .unwrap();
    let (_seed_owner, seed) = write_seed(&seed_of(entry).unwrap());
    let (_owner, scratch, copy) = private_copy(&seed);
    let request = seeded_request(entry, &scratch, Some(&copy));
    let reply = protocol::run(&serde_json::to_vec(&request).unwrap());
    let message: Value = serde_json::from_slice(&reply.stdout).unwrap();
    assert_eq!(reply.code, 0, "{message}");
    assert_eq!(judge(&request, &reply)["status"], "passed", "{message}");
    let detail = message["correctness"]["detail"].as_str().unwrap();
    assert!(
        detail.contains(&format!("events=10000 projection={projection}")),
        "{detail}"
    );
    assert!(detail.contains("projection:by_parameter"), "{detail}");
}

#[test]
fn an_application_cold_projection_of_10000_events_passes_the_oracle() {
    state_instance("application_cold");
}

#[test]
fn a_prewarmed_projection_of_10000_events_passes_the_oracle() {
    state_instance("prewarmed");
}

// Seeds as independent processes (ADR-0170) ---------------------------------------------------

#[path = "../../review-process/tests/support/executables.rs"]
mod executables;

/// This process's own link to a verified snapshot of the `perf-probe` example as built,
/// `<target>/<profile>/examples/perf-probe` beside this test.
///
/// Selecting this test alone builds no example, and one left by an earlier build may be stale,
/// so the same cargo builds it into this test's own target and profile with the workspace's
/// feature resolution (ADR-0167, ADR-0168). Every build replaces cargo's uplifted path, which
/// `review-process`'s and `af`'s tests start too, so each build and snapshot happens under the
/// one boundary every test builder and launcher of a workspace output takes (ADR-0174), and
/// this test starts only its own link. A test binary started without cargo uses the example
/// beside it.
fn probe_binary() -> PathBuf {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| built("--example", "perf-probe", "review-pipeline"))
        .clone()
}

/// The authority `perf-supervisor` example and the actual `af` binary, built and owned the same
/// way, for the whole-CLI oracle over actual AF output (ADR-0171).
fn supervisor_binary() -> PathBuf {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT
        .get_or_init(|| built("--example", "perf-supervisor", "review-process"))
        .clone()
}

fn af_binary() -> PathBuf {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(|| built("--bin", "af", "af")).clone()
}

fn built(kind: &str, name: &str, package: &str) -> PathBuf {
    executables::built(
        kind,
        name,
        package,
        env!("REVIEW_PIPELINE_TARGET_TRIPLE_DIR"),
    )
}

#[test]
fn every_executable_this_test_builds_is_its_own_link_in_the_boundary_other_crates_share() {
    // ADR-0174: this crate rebuilds the example `review-process`'s tests start and the binary
    // `af`'s tests start, so its builds and its launches take the same boundary they take, one
    // directory per profile, and it starts only its own links to verified snapshots, never
    // cargo's uplifted paths.
    let profile = executables::profile();
    let shared = executables::directory(&profile);
    for (path, kind, name) in [
        (supervisor_binary(), "--example", "perf-supervisor"),
        (probe_binary(), "--example", "perf-probe"),
        (af_binary(), "--bin", "af"),
    ] {
        assert_eq!(path.parent(), Some(shared.as_path()), "{}", path.display());
        let uplifted = executables::uplifted(&profile, kind, name);
        assert_ne!(path, uplifted);
        let file = path.file_name().unwrap().to_str().unwrap();
        let (snapshot, pid) = file.rsplit_once('.').unwrap();
        assert_eq!(pid, std::process::id().to_string());
        assert!(snapshot.starts_with(&format!("{name}-")), "{file}");
        // A rebuild by any process replaces the uplifted path, never this link.
        let before = executables::identity(&path);
        let again = built(
            kind,
            name,
            if name == "af" {
                "af"
            } else if name == "perf-probe" {
                "review-pipeline"
            } else {
                "review-process"
            },
        );
        assert_eq!(executables::identity(&path), before);
        assert!(again == path || std::fs::read(&again).unwrap() != std::fs::read(&path).unwrap());
    }
    let version = Command::new(af_binary()).arg("--version").output().unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains(env!("CARGO_PKG_VERSION")));
}

/// One independent probe process: the request on stdin, the reply from its exit and stdout.
fn probe_process(request: &Value) -> protocol::Reply {
    probe_process_with(request, &[])
}

/// [`probe_process`] with extra environment variables.
fn probe_process_with(request: &Value, env: &[(&str, &str)]) -> protocol::Reply {
    use std::io::Write;
    let mut child = Command::new(probe_binary())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("LC_ALL", "C")
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(request).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    protocol::Reply {
        code: output.status.code().map_or(255, |code| code as u8),
        stdout: output.stdout,
        stderr: output.stderr,
    }
}

#[test]
fn a_verdict_build_of_the_probe_refuses_the_calibration_loop_control() {
    // The probe this test builds is a verdict build, without `perf-calibration` (ADR-0181): a
    // request whose environment names the loop's control is refused before it is read, never
    // measured without the loop; the same request without it passes its oracle and reports
    // no injection.
    let entry = golden()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["case_id"] == "canonical_serialize_small")
        .cloned()
        .unwrap();
    for value in ["0", "250000"] {
        let (_owner, scratch) = empty_scratch();
        let request = request(&entry, &scratch);
        let refused = probe_process_with(&request, &[("AF_PERF_CALIBRATION_ITERATIONS", value)]);
        assert_eq!(refused.code, 2, "{value}");
        assert!(refused.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("no perf-calibration feature"),
            "{}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
    let (_owner, scratch) = empty_scratch();
    let request = request(&entry, &scratch);
    let reply = probe_process(&request);
    assert_eq!(reply.code, 0);
    assert_eq!(judge(&request, &reply)["status"], "passed");
    assert!(
        !String::from_utf8_lossy(&reply.stderr).contains("af.perf-calibration-injection/1"),
        "a verdict build injects nothing"
    );
}

/// Seal a published seed as the shard does: directories 0555 and files 0444.
fn seal(root: &Path) {
    for item in std::fs::read_dir(root).unwrap() {
        let item = item.unwrap();
        if item.file_type().unwrap().is_dir() {
            seal(&item.path());
        } else {
            std::fs::set_permissions(item.path(), std::fs::Permissions::from_mode(0o444)).unwrap();
        }
    }
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o555)).unwrap();
}

/// The test's own identity of a tree: every entry's relative path, kind, mode and bytes.
fn tree_identity(root: &Path) -> Vec<(String, u32, Option<Vec<u8>>)> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for item in std::fs::read_dir(&directory).unwrap() {
            let item = item.unwrap();
            let metadata = std::fs::symlink_metadata(item.path()).unwrap();
            let relative = item
                .path()
                .strip_prefix(root)
                .unwrap()
                .display()
                .to_string();
            let mode = metadata.permissions().mode() & 0o7777;
            if metadata.is_dir() {
                pending.push(item.path());
                entries.push((relative, mode, None));
            } else {
                assert!(metadata.is_file(), "{relative} is not a regular file");
                entries.push((relative, mode, Some(std::fs::read(item.path()).unwrap())));
            }
        }
    }
    entries.sort();
    entries
}

fn unseal(root: &Path) {
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755)).unwrap();
    for item in std::fs::read_dir(root).unwrap() {
        let item = item.unwrap();
        if item.file_type().unwrap().is_dir() {
            unseal(&item.path());
        } else {
            std::fs::set_permissions(item.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        }
    }
}

/// Every golden instance reading the seed `seed_id`, judged as independent processes that each
/// copy the one sealed seed: the writer runs once in its own process, every sample in its own,
/// and the seed's identity never changes. A copy one sample damages fails only that sample,
/// and the next sample's fresh copy passes again.
fn independent_processes_share_one_sealed_seed(seed_id: &str) {
    let (_seed_owner, seed_scratch) = empty_scratch();
    let writer = request(&seed_entry(seed_id), &seed_scratch);
    let reply = probe_process(&writer);
    assert_eq!(reply.code, 0, "{}", String::from_utf8_lossy(&reply.stderr));
    assert_eq!(judge(&writer, &reply)["status"], "passed");
    // The writer's process has exited, so every handle it held is closed; only then is the
    // seed sealed and identified.
    seal(&seed_scratch);
    let identity = tree_identity(&seed_scratch);
    assert!(
        identity.iter().all(|(_, mode, bytes)| match bytes {
            Some(_) => *mode == 0o444,
            None => *mode == 0o555,
        }),
        "a sealed seed is read-only"
    );
    let golden = golden();
    let readers: Vec<&Value> = golden["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| seed_of(entry).as_deref() == Some(seed_id))
        .collect();
    assert!(!readers.is_empty(), "no instance reads {seed_id}");
    for entry in readers {
        for round in 0..2 {
            let (_owner, scratch, copy) = private_copy(&seed_scratch);
            let request = seeded_request(entry, &scratch, Some(&copy));
            let reply = probe_process(&request);
            assert_eq!(
                reply.code,
                0,
                "{} round {round}: {}",
                entry["instance_id"],
                String::from_utf8_lossy(&reply.stdout)
            );
            let verdict = judge(&request, &reply);
            assert_eq!(verdict["status"], "passed", "{verdict}");
            assert_eq!(tree_identity(&seed_scratch), identity, "the seed changed");
        }
        // A copy damaged before its sample fails that sample alone, as the side's failure;
        // the sealed seed is untouched and the next fresh copy passes.
        let (_owner, scratch, copy) = private_copy(&seed_scratch);
        let damaged = if copy.join("events.sqlite").is_file() {
            copy.join("events.sqlite")
        } else {
            copy.join("repo/.git/HEAD")
        };
        std::fs::write(&damaged, b"damaged by this sample\n").unwrap();
        let request = seeded_request(entry, &scratch, Some(&copy));
        let reply = probe_process(&request);
        assert_ne!(reply.code, 0, "a damaged copy never passes");
        assert_eq!(judge(&request, &reply)["status"], "failed");
        assert_eq!(tree_identity(&seed_scratch), identity, "the seed changed");
        let (_owner, scratch, copy) = private_copy(&seed_scratch);
        let request = seeded_request(entry, &scratch, Some(&copy));
        let reply = probe_process(&request);
        assert_eq!(judge(&request, &reply)["status"], "passed");
    }
    // Changed bytes change the seed's identity, which is what the shard verifies around every
    // sample (`seed_changed`, tests.test_runner).
    unseal(&seed_scratch);
    let first = identity
        .iter()
        .find(|(_, _, bytes)| bytes.is_some())
        .unwrap();
    std::fs::write(seed_scratch.join(&first.0), b"tampered").unwrap();
    seal(&seed_scratch);
    assert_ne!(tree_identity(&seed_scratch), identity);
    unseal(&seed_scratch);
}

#[test]
fn authority_operations_share_one_sealed_seed_across_fresh_processes() {
    independent_processes_share_one_sealed_seed("authority_seed__package_issue_255_medium");
}

#[test]
fn authority_seed_restoration_rejects_an_unreleased_setup_lease() {
    let (_owner, scratch) = empty_scratch();
    // A valid admitted Task is insufficient: build keeps its setup lease instead of releasing
    // it. Restoration must refuse even if that lease could subsequently expire.
    let fixture = authority::build(&scratch, None, fixtures::TASK_LEASE_MS).unwrap();
    drop(fixture);
    let restored = authority::restore_then(&scratch, None, |_, _, _, _| {
        panic!("an unreleased seed never reaches the region capture")
    });
    let failure = restored.err().unwrap();
    assert_eq!(failure.kind, protocol::FailureKind::FixtureFailed);
    assert!(failure.detail.contains("both setup leases released"));
}

#[test]
fn seeded_authority_keeps_the_original_task_and_takes_its_lease_after_capture() {
    let (_seed_owner, seed) = write_seed("authority_seed__package_issue_255_medium");
    let seed_root = seed.join(protocol::WORK_DIR);
    let seed_store = review_store::EventStore::open(seed_root.join("events.sqlite")).unwrap();
    let run = review_store::store::task::task_run_id(authority::TASK_ID).unwrap();
    let prefix = seed_store.replay(&run).unwrap();
    assert_eq!(prefix.len(), 6);
    for region in [
        cases::AuthorityRegion::CurrentPlanCheck,
        cases::AuthorityRegion::TaskRuntimeNew,
    ] {
        let (_owner, _scratch, copy) = private_copy(&seed);
        let mut captured_ms = 0;
        let outcome = cases::authority_sample_from(
            "issue_255_medium",
            &copy,
            region,
            cases::PRODUCTION_LEASE,
            Instant::now(),
            cases::SetupHooks {
                after_region_capture: Box::new(|| {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    captured_ms = authority::now_ms()?;
                    Ok(())
                }),
                ..cases::SetupHooks::default()
            },
            true,
        );
        assert!(outcome.result.is_ok(), "{:?}", outcome.result);
        let store = review_store::EventStore::open(copy.join("events.sqlite")).unwrap();
        let events = store.replay(&run).unwrap();
        assert_eq!(events.len(), 7, "the reader only takes its own lease");
        assert_eq!(
            serde_json::to_value(&events[..6]).unwrap(),
            serde_json::to_value(&prefix).unwrap(),
            "restoration preserves every original event and revision reference"
        );
        let (at, change) = transition(&events[6]);
        assert_eq!(change["kind"], "lease_taken");
        assert!(
            at >= captured_ms,
            "capture and its delay are outside the new lease"
        );
        assert_eq!(
            change["lease_until_unix_ms"].as_u64().unwrap() - at,
            fixtures::TASK_LEASE_MS
        );
        assert_eq!(
            seed_store.replay(&run).unwrap().len(),
            6,
            "the seed never changes"
        );
    }
}

#[test]
fn cold_and_prewarmed_projections_share_one_sealed_seed_across_processes() {
    independent_processes_share_one_sealed_seed("state_projection_seed__log_events_10000");
}

#[test]
fn source_captures_of_100_files_share_one_sealed_seed_across_processes() {
    independent_processes_share_one_sealed_seed(
        "source_capture_seed__files_100_tree_source_files_128b",
    );
}

#[test]
fn source_captures_of_10000_files_share_one_sealed_seed_across_processes() {
    independent_processes_share_one_sealed_seed(
        "source_capture_seed__files_10000_tree_source_files_128b",
    );
}

#[test]
fn a_seed_writer_s_check_must_match_its_golden_seed_entry() {
    // The writer's message is judged against the golden seed entry, never accepted on the
    // probe's word: a changed digest or count is the writer's failure.
    let (_owner, scratch) = empty_scratch();
    let entry = seed_entry("source_capture_seed__files_100_tree_source_files_128b");
    let request = request(&entry, &scratch);
    let reply = protocol::run(&serde_json::to_vec(&request).unwrap());
    assert_eq!(reply.code, 0);
    let mut message: Value = serde_json::from_slice(&reply.stdout).unwrap();
    message["correctness"]["count"] = json!(99);
    let mut stdout = serde_json::to_vec(&message).unwrap();
    stdout.push(b'\n');
    let changed = protocol::Reply {
        code: 0,
        stdout,
        stderr: vec![],
    };
    let verdict = judge(&request, &changed);
    assert_eq!(verdict["status"], "failed", "{verdict}");
    assert_eq!(verdict["failure"]["kind"], "oracle_mismatch");
    // An absent seed fails the reader before its region, as the seed's failure.
    let entry = golden()["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["case_id"] == "state_projection")
        .unwrap()
        .clone();
    let (_owner, scratch) = empty_scratch();
    let (_seed_owner, empty_seed) = empty_scratch();
    let request = seeded_request(&entry, &scratch, Some(&empty_seed));
    let reply = protocol::run(&serde_json::to_vec(&request).unwrap());
    let message: Value = serde_json::from_slice(&reply.stdout).unwrap();
    assert_eq!(reply.code, 1);
    assert!(
        message["correctness"]["detail"]
            .as_str()
            .unwrap()
            .contains("fixture_failed: "),
        "{message}"
    );
    assert!(
        message["metrics"].get("wall_ms").is_none(),
        "nothing was measured"
    );
}

#[test]
fn an_event_fixture_must_be_exact_and_its_damage_is_reported() {
    // Open, propose and admit are three events: fewer cannot be the fixture.
    let (_owner, scratch) = empty_scratch();
    let failure = cases::event_log(&scratch, 2).unwrap_err();
    assert_eq!(failure.kind, protocol::FailureKind::FixtureFailed);
    assert!(
        failure.detail.contains("not exactly 2"),
        "{}",
        failure.detail
    );
    // A log whose referenced artifacts are gone is refused by the production projection; the
    // probe reports the side's failure and never a fast success.
    let (_owner, scratch) = empty_scratch();
    let outcome = cases::state_projection_sample(&scratch, 20, false, Instant::now(), |work| {
        let objects = work.join("cas/objects");
        std::fs::remove_dir_all(&objects).unwrap();
        std::fs::create_dir(&objects).unwrap();
        Ok(())
    });
    let failure = outcome.result.unwrap_err();
    assert_eq!(failure.kind, protocol::FailureKind::OperationFailed);
    assert!(
        outcome.region_nanos.is_some(),
        "the projection itself refused"
    );
}

#[test]
fn the_campaign_writer_refuses_without_the_side_s_reviewed_campaign() {
    // The writer only tops up a Campaign the side's own `af review run` wrote beside its
    // scratch; it never creates one by itself.
    let (_owner, parent) = empty_scratch();
    let scratch = parent.join("campaign-probe");
    std::fs::create_dir(&scratch).unwrap();
    let entry = json!({"case_id": "cli_campaign_fixture",
                       "instance_id": "cli_campaign_fixture__state_cli_state_10000",
                       "contract_id": "cli_campaign_fixture_v1",
                       "parameters": {"state": "cli_state_10000"}});
    let request =
        protocol::admit(&serde_json::to_vec(&request(&entry, &scratch)).unwrap()).unwrap();
    let outcome = cases::execute(&request, &scratch.join(protocol::WORK_DIR));
    let failure = outcome.result.unwrap_err();
    assert_eq!(failure.kind, protocol::FailureKind::FixtureFailed);
    assert!(
        failure.detail.contains("the Campaign state"),
        "{}",
        failure.detail
    );
    assert!(outcome.region_nanos.is_none(), "nothing was measured");
    // A Campaign state directory that is not a directory is refused the same way.
    std::fs::write(parent.join(fixtures::CLI_CAMPAIGN_STATE), b"not a Campaign").unwrap();
    let failure = cases::execute(&request, &scratch.join(protocol::WORK_DIR))
        .result
        .unwrap_err();
    assert!(
        failure.detail.contains("is not a directory"),
        "{}",
        failure.detail
    );
}

#[test]
fn the_cap_admits_exactly_its_size_and_refuses_one_byte_more() {
    let golden = golden();
    let cap = &golden["cap_boundary"];
    let text_bytes = cap["admitted"]["text_bytes"].as_u64().unwrap() as usize;
    let directory = tempfile::tempdir().unwrap();
    let cas = review_store::Cas::open(directory.path().join("cas")).unwrap();
    let (id, _) = cas
        .put_artifact(
            fixtures::REQUIREMENTS_V1,
            fixtures::producer(),
            vec![],
            None,
            fixtures::requirements_payload(text_bytes),
        )
        .unwrap();
    assert_eq!(id, cap["admitted"]["artifact_id"].as_str().unwrap());
    assert_eq!(cas.stored_len(&id).unwrap(), 8_388_608);
    assert_eq!(cas.get_artifact(&id).unwrap().artifact_id, id);
    let refused = cas
        .put_artifact(
            fixtures::REQUIREMENTS_V1,
            fixtures::producer(),
            vec![],
            None,
            fixtures::requirements_payload(text_bytes + 1),
        )
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("8388609 bytes; limit is 8388608"),
        "{refused}"
    );
    assert_eq!(cap["refused"]["serialized_envelope_bytes"], 8_388_609);
}

fn expires(region: cases::AuthorityRegion, instance: &str) {
    let (_seed_owner, seed) = write_seed("authority_seed__package_issue_255_medium");
    let (_owner, _scratch, copy) = private_copy(&seed);
    let lapsed = cases::LeasePolicy {
        renew_before_region: true,
        wait_for_expiry: true,
    };
    let outcome = cases::authority_sample_from(
        "issue_255_medium",
        &copy,
        region,
        lapsed,
        Instant::now(),
        cases::SetupHooks::default(),
        true,
    );
    assert!(
        outcome.region_nanos.is_some(),
        "the production check ran and refused"
    );
    let failure = outcome.result.as_ref().unwrap_err();
    assert_eq!(
        failure.kind,
        protocol::FailureKind::LeaseExpired,
        "{failure:?}"
    );
    assert!(
        failure
            .detail
            .contains("the Store reports: Task writer lease is expired or fenced"),
        "{}",
        failure.detail
    );
    // Rendered as the request would have been: the driver attributes it to this side.
    let golden = golden();
    let entry = golden["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["instance_id"] == instance)
        .unwrap();
    let (_rendering, empty) = empty_scratch();
    let request_json = seeded_request(entry, &empty, Some(&copy));
    let admitted = protocol::admit(&serde_json::to_vec(&request_json).unwrap()).unwrap();
    let mut line = serde_json::to_vec(&protocol::message(&admitted, &outcome)).unwrap();
    line.push(b'\n');
    let reply = protocol::Reply {
        code: 1,
        stdout: line,
        stderr: vec![],
    };
    let verdict = judge(&request_json, &reply);
    assert_eq!(verdict["failure"]["kind"], "lease_expired", "{verdict}");
}

#[test]
fn an_expired_production_lease_fails_the_plan_check_without_retry() {
    expires(
        cases::AuthorityRegion::CurrentPlanCheck,
        "authority_plan_check__package_issue_255_medium",
    );
}

#[test]
fn an_expired_production_lease_fails_the_runtime_constructor_without_retry() {
    expires(
        cases::AuthorityRegion::TaskRuntimeNew,
        "authority_runtime_new__package_issue_255_medium",
    );
}

// Scratch ownership ---------------------------------------------------------------------------

/// The admitted sentinel request over a fresh empty scratch.
fn admitted_sentinel() -> (tempfile::TempDir, PathBuf, protocol::Request) {
    let (owner, scratch) = empty_scratch();
    let entry = json!({"case_id": "sentinel_canonical", "instance_id": "sentinel_canonical",
                       "contract_id": "sentinel_canonical_v1", "parameters": {}});
    let request =
        protocol::admit(&serde_json::to_vec(&request(&entry, &scratch)).unwrap()).unwrap();
    (owner, scratch, request)
}

fn passed_outcome() -> protocol::Outcome {
    protocol::Outcome {
        fixture_digest: None,
        setup_nanos: 1,
        region_nanos: Some(1),
        result: Ok(protocol::Observation {
            digest: None,
            count: Some(1),
            detail: "test-constructed".into(),
            extra_metrics: Default::default(),
        }),
    }
}

fn failure_detail(reply: &protocol::Reply) -> String {
    let message: Value = serde_json::from_slice(&reply.stdout).unwrap();
    message["correctness"]["detail"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn a_work_directory_the_probe_could_not_create_is_never_removed() {
    // Another process creates probe-work between admission and creation: the probe fails its
    // sample and leaves that directory and everything in it alone.
    let (_owner, scratch, request) = admitted_sentinel();
    let foreign = scratch.join(protocol::WORK_DIR);
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("theirs"), b"not the probe's").unwrap();
    let mut ran = false;
    let reply = protocol::run_admitted(&request, |_, _| {
        ran = true;
        passed_outcome()
    });
    assert!(!ran, "nothing runs without an owned work directory");
    assert_eq!(reply.code, 1);
    assert!(
        reply.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&reply.stderr)
    );
    let detail = failure_detail(&reply);
    assert!(detail.starts_with("fixture_failed:"), "{detail}");
    assert!(detail.contains("nothing was removed"), "{detail}");
    assert_eq!(
        std::fs::read(foreign.join("theirs")).unwrap(),
        b"not the probe's"
    );
    // A symlink planted at the path is not followed or removed either.
    let (_owner, scratch, request) = admitted_sentinel();
    let (_outside_owner, outside) = empty_scratch();
    std::fs::write(outside.join("kept"), b"outside").unwrap();
    std::os::unix::fs::symlink(&outside, scratch.join(protocol::WORK_DIR)).unwrap();
    let reply = protocol::run_admitted(&request, |_, _| passed_outcome());
    assert_eq!(reply.code, 1);
    assert!(
        std::fs::symlink_metadata(scratch.join(protocol::WORK_DIR))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(outside.join("kept")).unwrap(), b"outside");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
}

/// Swap `work` for a foreign directory holding one file, keeping the original as `moved`.
fn replace_with_foreign(work: &Path) {
    std::fs::rename(work, work.with_file_name("moved")).unwrap();
    std::fs::create_dir(work).unwrap();
    std::fs::write(work.join("theirs"), b"foreign").unwrap();
}

fn assert_untouched(scratch: &Path) {
    assert_eq!(
        std::fs::read(scratch.join(protocol::WORK_DIR).join("theirs")).unwrap(),
        b"foreign"
    );
    assert!(scratch.join("moved").is_dir(), "the probe's own work stays");
}

#[test]
fn a_directory_replacing_probe_work_right_after_its_creation_is_never_removed() {
    // `execute` runs immediately after `create_dir` returns, at the boundary where an identity
    // was once captured. The probe has no later step that reads or removes the path.
    let (_owner, scratch, request) = admitted_sentinel();
    let reply = protocol::run_admitted(&request, |_, work| {
        replace_with_foreign(work);
        passed_outcome()
    });
    assert_eq!(reply.code, 0, "{}", String::from_utf8_lossy(&reply.stderr));
    assert!(reply.stderr.is_empty());
    assert_untouched(&scratch);
}

#[test]
fn a_directory_replacing_probe_work_at_the_end_of_the_sample_is_never_removed() {
    // The sample has written its work; the swap happens as `execute` returns, at the boundary
    // where a cleanup once checked identity before a recursive removal by path.
    let (_owner, scratch, request) = admitted_sentinel();
    let (_outside_owner, outside) = empty_scratch();
    std::fs::write(outside.join("kept"), b"outside").unwrap();
    let reply = protocol::run_admitted(&request, |_, work| {
        std::fs::create_dir(work.join("nested")).unwrap();
        std::fs::write(work.join("nested/file"), b"probe-owned").unwrap();
        std::os::unix::fs::symlink(&outside, work.join("link")).unwrap();
        replace_with_foreign(work);
        passed_outcome()
    });
    assert_eq!(reply.code, 0, "{}", String::from_utf8_lossy(&reply.stderr));
    assert_untouched(&scratch);
    assert_eq!(
        std::fs::read(scratch.join("moved/nested/file")).unwrap(),
        b"probe-owned"
    );
    assert_eq!(std::fs::read(outside.join("kept")).unwrap(), b"outside");
    // Replaced by a symlink to a directory outside the scratch: never followed.
    let (_owner, scratch, request) = admitted_sentinel();
    let reply = protocol::run_admitted(&request, |_, work| {
        std::fs::remove_dir(work).unwrap();
        std::os::unix::fs::symlink(&outside, work).unwrap();
        passed_outcome()
    });
    assert_eq!(reply.code, 0);
    assert!(scratch.join(protocol::WORK_DIR).is_symlink());
    assert_eq!(std::fs::read(outside.join("kept")).unwrap(), b"outside");
}

#[test]
fn the_probe_leaves_its_work_for_the_caller_and_reports_only_the_sample() {
    // The caller owns the scratch: what the sample wrote stays, and a passed sample exits 0
    // without claiming any cleanup. Removed by someone else, nothing is reported either.
    let (_owner, scratch, request) = admitted_sentinel();
    let reply = protocol::run_admitted(&request, |_, work| {
        std::fs::write(work.join("file"), b"probe-owned").unwrap();
        passed_outcome()
    });
    assert_eq!(reply.code, 0);
    assert!(reply.stderr.is_empty());
    assert_eq!(
        std::fs::read(scratch.join(protocol::WORK_DIR).join("file")).unwrap(),
        b"probe-owned"
    );
    let (_owner, _scratch, request) = admitted_sentinel();
    let reply = protocol::run_admitted(&request, |_, work| {
        std::fs::remove_dir(work).unwrap();
        passed_outcome()
    });
    assert_eq!(reply.code, 0);
    assert!(reply.stderr.is_empty());
}

// State persistence lease ---------------------------------------------------------------------

#[test]
fn the_state_persistence_region_starts_with_a_refreshed_production_lease() {
    let (_owner, scratch) = empty_scratch();
    let outcome =
        cases::persistence_state_sample(&scratch, cases::PRODUCTION_LEASE, Instant::now());
    let observation = outcome.result.as_ref().unwrap();
    // Open, propose and admit, then the refresh: the region's 100 renewals follow it.
    assert!(
        observation
            .detail
            .contains("appended=100 prefix=4 lease=refreshed_before_region"),
        "{}",
        observation.detail
    );
    let (logical, unit) = &observation.extra_metrics["logical_bytes"];
    assert_eq!(*unit, "bytes");
    assert!(logical.parse::<u64>().unwrap() > 0, "{logical}");
}

#[test]
fn a_production_lease_lapsing_at_the_state_persistence_boundary_fails_without_retry() {
    let (_owner, scratch) = empty_scratch();
    // Refreshed, then held until the Store itself reports the 15-second lease lapsed.
    let lapsed = cases::LeasePolicy {
        renew_before_region: true,
        wait_for_expiry: true,
    };
    let outcome = cases::persistence_state_sample(&scratch, lapsed, Instant::now());
    assert!(
        outcome.region_nanos.is_some(),
        "the first renewal ran and failed"
    );
    let failure = outcome.result.as_ref().unwrap_err();
    assert_eq!(
        failure.kind,
        protocol::FailureKind::LeaseExpired,
        "{failure:?}"
    );
    assert!(
        failure
            .detail
            .contains("the Store reports: Task writer lease is expired or fenced"),
        "{}",
        failure.detail
    );
    // No renewal was retried or recorded after the lapse: the log ends at the refresh.
    let store = review_store::EventStore::open(scratch.join("events.sqlite")).unwrap();
    let run = review_store::store::task::task_run_id(authority::TASK_ID).unwrap();
    assert_eq!(store.len(&run).unwrap(), 4);
    let golden = golden();
    let entry = golden["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["instance_id"] == "persistence_fs__operation_state_append_read")
        .unwrap();
    let (_rendering, empty) = empty_scratch();
    let request_json = request(entry, &empty);
    let admitted = protocol::admit(&serde_json::to_vec(&request_json).unwrap()).unwrap();
    let mut line = serde_json::to_vec(&protocol::message(&admitted, &outcome)).unwrap();
    line.push(b'\n');
    let reply = protocol::Reply {
        code: 1,
        stdout: line,
        stderr: vec![],
    };
    let verdict = judge(&request_json, &reply);
    assert_eq!(verdict["failure"]["kind"], "lease_expired", "{verdict}");
}

/// The `TaskTransition@5` payload of one recorded event, with its observation time.
fn transition(event: &review_core::RunEvent) -> (u64, &Value) {
    let payload = &event.payload;
    (payload["now_unix_ms"].as_u64().unwrap(), &payload["change"])
}

/// The `kind` of every transition in the probe Task's log.
fn kinds(events: &[review_core::RunEvent]) -> Vec<String> {
    events
        .iter()
        .map(|event| transition(event).1["kind"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn each_authority_setup_write_owns_a_whole_lease_and_the_region_captures_outside_every_lease() {
    // Capturing the authority validates the plan, which can take long on a loaded host; on the
    // gate one admission alone took 14.5 s of a 15-second lease opened before the proposal. So
    // each setup write holds a production lease of its own: the opening lease covers the
    // proposal and is released while live, a lease taken at the next epoch covers the
    // admission and is released, and the region captures its own host and authority while no
    // lease is held, then takes a whole lease as its last write. Delays where those costs lie
    // never fall inside a lease: each lease opens after the delay before it and was released
    // before the delay after it.
    for region in [
        cases::AuthorityRegion::CurrentPlanCheck,
        cases::AuthorityRegion::TaskRuntimeNew,
    ] {
        let (_owner, scratch) = empty_scratch();
        let marks = std::cell::RefCell::new(Vec::new());
        let mark = |label: &'static str| -> Result<(), String> {
            std::thread::sleep(std::time::Duration::from_millis(300));
            marks.borrow_mut().push((label, authority::now_ms()?));
            Ok(())
        };
        let outcome = cases::authority_sample_with(
            "issue_255_medium",
            &scratch,
            region,
            cases::PRODUCTION_LEASE,
            Instant::now(),
            cases::SetupHooks {
                after_capture: Box::new(|| mark("capture")),
                between_writes: Box::new(|| mark("between")),
                after_region_capture: Box::new(|| mark("region_capture")),
            },
        );
        assert!(outcome.region_nanos.is_some(), "{region:?}");
        let observation = outcome.result.as_ref().unwrap();
        assert!(
            observation.detail.contains("package=issue_255_medium"),
            "{}",
            observation.detail
        );
        let marks = marks.into_inner();
        assert_eq!(
            marks.iter().map(|(label, _)| *label).collect::<Vec<_>>(),
            ["capture", "between", "region_capture"]
        );
        let store = review_store::EventStore::open(scratch.join("events.sqlite")).unwrap();
        let run = review_store::store::task::task_run_id(authority::TASK_ID).unwrap();
        let events = store.replay(&run).unwrap();
        assert_eq!(
            &kinds(&events)[..7],
            [
                "opened",
                "plan_proposed",
                "lease_released",
                "lease_taken",
                "plan_admitted",
                "lease_released",
                "lease_taken"
            ],
            "{region:?}"
        );
        let at = |index: usize| transition(&events[index]).0;
        assert!(
            at(0) >= marks[0].1,
            "the first lease opened after the capture"
        );
        assert!(
            at(2) <= marks[1].1,
            "the proposal's lease ended before the delay"
        );
        assert!(at(3) >= marks[1].1, "the admission's lease began after it");
        assert!(
            at(5) <= marks[2].1,
            "no lease was held while the region captured"
        );
        assert!(
            at(6) >= marks[2].1,
            "the region's lease was taken after its capture"
        );
        // Every lease taken is a whole production lease, at the next epoch.
        for (index, epoch) in [(3, 2), (6, 3)] {
            let (taken_ms, change) = transition(&events[index]);
            assert_eq!(
                change["lease_until_unix_ms"].as_u64().unwrap() - taken_ms,
                fixtures::TASK_LEASE_MS
            );
            assert_eq!(events[index].payload["epoch"], epoch);
        }
        assert_eq!(
            events[4].payload["epoch"], 2,
            "admitted under its own lease"
        );
    }
    // A failing capture-side step is the fixture's failure before any lease exists.
    let (_owner, scratch) = empty_scratch();
    let outcome = cases::authority_sample_with(
        "issue_255_medium",
        &scratch,
        cases::AuthorityRegion::CurrentPlanCheck,
        cases::PRODUCTION_LEASE,
        Instant::now(),
        cases::SetupHooks {
            after_capture: Box::new(|| Err("the capture failed".into())),
            ..cases::SetupHooks::default()
        },
    );
    assert!(outcome.region_nanos.is_none());
    let failure = outcome.result.unwrap_err();
    assert_eq!(failure.kind, protocol::FailureKind::FixtureFailed);
    let store = review_store::EventStore::open(scratch.join("events.sqlite")).unwrap();
    let run = review_store::store::task::task_run_id(authority::TASK_ID).unwrap();
    assert_eq!(store.len(&run).unwrap(), 0, "no lease was opened");
}

#[test]
fn a_lapsed_setup_lease_fails_the_sample_and_no_later_lease_replaces_it() {
    // A live setup lease is released through the public writer.
    let (_owner, scratch) = empty_scratch();
    let mut fixture = authority::build(&scratch, None, fixtures::TASK_LEASE_MS).unwrap();
    authority::hand_over(&mut fixture.store, &fixture.cas, &fixture.lease).unwrap();
    let run = review_store::store::task::task_run_id(authority::TASK_ID).unwrap();
    assert_eq!(
        &kinds(&fixture.store.replay(&run).unwrap())[3..],
        ["lease_released"]
    );

    // One the Store reports lapsed is not released, and that is the side's lease expiry.
    let (_owner, scratch) = empty_scratch();
    let mut fixture = authority::build(&scratch, None, 3_000).unwrap();
    let deadline = Instant::now() + std::time::Duration::from_secs(30);
    while matches!(
        cases::lease_state(&fixture.store, &fixture.lease),
        cases::LeaseState::Live(_)
    ) {
        assert!(Instant::now() < deadline, "the short lease never lapsed");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let failure =
        authority::hand_over(&mut fixture.store, &fixture.cas, &fixture.lease).unwrap_err();
    assert_eq!(
        failure.kind,
        protocol::FailureKind::LeaseExpired,
        "{failure:?}"
    );
    assert!(
        failure
            .detail
            .contains("the Store reports: Task writer lease is expired or fenced"),
        "{}",
        failure.detail
    );
    assert_eq!(
        fixture.store.len(&run).unwrap(),
        3,
        "nothing is written for a lapsed lease"
    );

    // A setup whose lease lapses before its write ends there: the write is refused, no
    // replacement lease is taken, and the region never runs. The proposal waits until the
    // Store itself reports the opening lease lapsed, so expiry precedes the write by the
    // Store's own account rather than by an assumed scheduling delay.
    let (_owner, scratch) = empty_scratch();
    let ran = std::cell::Cell::new(false);
    let observed = std::cell::Cell::new(false);
    let built = authority::build_then(
        &scratch,
        None,
        1,
        authority::Setup {
            hand_over: true,
            after_open: Box::new(
                |store: &review_store::EventStore, lease: &review_store::store::task::TaskLease| {
                    let deadline = Instant::now() + std::time::Duration::from_secs(30);
                    loop {
                        match cases::lease_state(store, lease) {
                            cases::LeaseState::Lapsed(_) => break,
                            cases::LeaseState::Live(_) => {}
                            cases::LeaseState::Unreadable(why) => return Err(why),
                        }
                        if Instant::now() >= deadline {
                            return Err("the 1 ms setup lease never lapsed".into());
                        }
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                    observed.set(true);
                    Ok(())
                },
            ),
            ..authority::Setup::default()
        },
        |_, _, _, _, _| ran.set(true),
    );
    let failure = built.err().expect("the lapsed setup lease fails");
    assert_eq!(
        failure.kind,
        protocol::FailureKind::LeaseExpired,
        "{failure:?}"
    );
    assert!(
        observed.get(),
        "the Store reported the lease lapsed before the proposal"
    );
    assert!(!ran.get(), "nothing ran after the failed setup");
    let store = review_store::EventStore::open(scratch.join("events.sqlite")).unwrap();
    assert_eq!(kinds(&store.replay(&run).unwrap()), ["opened"]);
}

#[test]
fn the_region_checks_through_a_cold_authority_that_no_setup_write_used() {
    // The setup writes validate the plan through their own authority, whose validator then
    // holds it; the region's authority is captured afresh after setup and holds nothing, so
    // its first check recompiles the plan. An authority that already checked is refused.
    let (_owner, scratch) = empty_scratch();
    let (_fixture, checked) = authority::build_then(
        &scratch,
        None,
        fixtures::TASK_LEASE_MS,
        authority::Setup {
            hand_over: true,
            ..authority::Setup::default()
        },
        |store, cas, _, _, region| {
            region
                .capture(|authority, _| {
                    assert!(
                        !authority.holds_validated_plan(),
                        "a fresh region authority"
                    );
                    authority::require_cold(authority).unwrap();
                    let lease = authority::take_fresh_lease(store, cas).unwrap();
                    store
                        .check_current_task_plan_for_recording(cas, &lease, authority)
                        .unwrap();
                    assert!(authority.holds_validated_plan());
                    authority::require_cold(authority).unwrap_err()
                })
                .unwrap()
        },
    )
    .unwrap();
    assert!(checked.contains("would not be cold"), "{checked}");
    // Each sample's own capture is cold again.
    let (_owner, scratch) = empty_scratch();
    let outcome = cases::authority_sample(
        "issue_255_medium",
        &scratch,
        cases::AuthorityRegion::CurrentPlanCheck,
        cases::PRODUCTION_LEASE,
        Instant::now(),
    );
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
}

#[test]
fn the_state_persistence_refresh_is_the_last_setup_step_after_a_delayed_store_read() {
    // A delayed Store read before the refresh is setup cost, not lease time: the refresh
    // follows it, the prefix counts the refresh without another read, and the region's
    // renewals follow the refresh directly.
    let (_owner, scratch) = empty_scratch();
    let mut finished_ms = 0;
    let outcome = cases::persistence_state_sample_with(
        &scratch,
        cases::PRODUCTION_LEASE,
        Instant::now(),
        |fixture| {
            let run = fixture.run_id()?;
            assert_eq!(fixture.store.len(&run).unwrap(), 3);
            std::thread::sleep(std::time::Duration::from_millis(300));
            finished_ms = authority::now_ms()?;
            Ok(())
        },
    );
    let observation = outcome.result.as_ref().unwrap();
    assert!(
        observation.detail.contains("appended=100 prefix=4"),
        "{}",
        observation.detail
    );
    let store = review_store::EventStore::open(scratch.join("events.sqlite")).unwrap();
    let run = review_store::store::task::task_run_id(authority::TASK_ID).unwrap();
    let events = store.replay(&run).unwrap();
    assert_eq!(events.len(), 104);
    let (refreshed_ms, change) = transition(&events[3]);
    assert_eq!(change["kind"], "lease_renewed", "{change}");
    assert!(
        refreshed_ms >= finished_ms,
        "the refresh followed the delay"
    );
    // The unchanged 15000-ms production lease.
    assert_eq!(fixtures::TASK_LEASE_MS, 15_000);
    assert_eq!(
        change["lease_until_unix_ms"].as_u64().unwrap() - refreshed_ms,
        15_000
    );
    let (first_renewal_ms, change) = transition(&events[4]);
    assert_eq!(change["kind"], "lease_renewed", "{change}");
    assert!(first_renewal_ms >= refreshed_ms);
    // A failing pre-refresh read is the fixture's failure; nothing is refreshed after it.
    let (_owner, scratch) = empty_scratch();
    let outcome = cases::persistence_state_sample_with(
        &scratch,
        cases::PRODUCTION_LEASE,
        Instant::now(),
        |_| Err("a pre-refresh Store read failed".into()),
    );
    let failure = outcome.result.unwrap_err();
    assert_eq!(failure.kind, protocol::FailureKind::FixtureFailed);
    assert!(outcome.region_nanos.is_none());
    let store = review_store::EventStore::open(scratch.join("events.sqlite")).unwrap();
    assert_eq!(store.len(&run).unwrap(), 3);
}

/// Append one event to the fixture's run with a separate connection, as damage would.
fn append_raw(scratch: &Path, kind: &str, payload: &str) {
    let connection = rusqlite::Connection::open(scratch.join("events.sqlite")).unwrap();
    let run = review_store::store::task::task_run_id(authority::TASK_ID).unwrap();
    connection
        .execute(
            "INSERT INTO events (run_id, sequence, event_id, type, occurred_at, artifact_refs, \
             payload) SELECT ?1, MAX(sequence) + 1, 'evt-damage', ?2, \
             '2026-10-19T00:00:00Z', '[]', ?3 FROM events WHERE run_id = ?1",
            rusqlite::params![run, kind, payload],
        )
        .unwrap();
}

#[test]
fn a_lease_state_the_store_cannot_read_is_never_classified_as_expiry() {
    let original = "the timed renewal failed: test-constructed diagnostic".to_string();
    // Held and live: the operation's own failure, with its diagnostic unchanged.
    let (_owner, scratch) = empty_scratch();
    let fixture = authority::build(&scratch, None, fixtures::TASK_LEASE_MS).unwrap();
    assert!(matches!(
        cases::lease_state(&fixture.store, &fixture.lease),
        cases::LeaseState::Live(_)
    ));
    let failure = cases::classify(
        &fixture.store,
        &fixture.lease,
        original.clone(),
        protocol::FailureKind::OperationFailed,
    );
    assert_eq!(failure.kind, protocol::FailureKind::OperationFailed);
    assert_eq!(failure.detail, original);
    // A malformed transition at the stream tail: the Store cannot decode the lease state.
    append_raw(&scratch, "TaskTransition@5", r#"{"malformed":true}"#);
    let state = cases::lease_state(&fixture.store, &fixture.lease);
    assert!(
        matches!(state, cases::LeaseState::Unreadable(_)),
        "{state:?}"
    );
    for otherwise in [
        protocol::FailureKind::OperationFailed,
        protocol::FailureKind::FixtureFailed,
    ] {
        let failure = cases::classify(&fixture.store, &fixture.lease, original.clone(), otherwise);
        assert_eq!(failure.kind, otherwise, "{failure:?}");
        assert!(failure.detail.starts_with(&original), "{}", failure.detail);
        assert!(
            failure
                .detail
                .contains("the lease state is unreadable, so expiry is not established"),
            "{}",
            failure.detail
        );
    }
    // A SQLite read error: the event table is gone below the open Store.
    let (_owner, scratch) = empty_scratch();
    let fixture = authority::build(&scratch, None, fixtures::TASK_LEASE_MS).unwrap();
    rusqlite::Connection::open(scratch.join("events.sqlite"))
        .unwrap()
        .execute_batch("DROP TABLE events")
        .unwrap();
    let state = cases::lease_state(&fixture.store, &fixture.lease);
    let cases::LeaseState::Unreadable(read) = &state else {
        panic!("{state:?}");
    };
    assert!(read.contains("events"), "{read}");
    let failure = cases::classify(
        &fixture.store,
        &fixture.lease,
        original.clone(),
        protocol::FailureKind::OperationFailed,
    );
    assert_eq!(failure.kind, protocol::FailureKind::OperationFailed);
    assert!(failure.detail.starts_with(&original), "{}", failure.detail);
    assert!(failure.detail.contains(read.as_str()), "{}", failure.detail);
}

#[test]
fn a_lease_fenced_by_another_writer_is_classified_as_expiry() {
    // The Store establishes fencing from the stream itself: a transition by another writer.
    let (_owner, scratch) = empty_scratch();
    let fixture = authority::build(&scratch, None, fixtures::TASK_LEASE_MS).unwrap();
    let store = review_store::EventStore::open(scratch.join("events.sqlite")).unwrap();
    let run = review_store::store::task::task_run_id(authority::TASK_ID).unwrap();
    let events = store.replay(&run).unwrap();
    let mut foreign = events.last().unwrap().payload.clone();
    foreign["writer"] = json!("another-writer");
    append_raw(
        &scratch,
        "TaskTransition@5",
        &serde_json::to_string(&foreign).unwrap(),
    );
    let state = cases::lease_state(&fixture.store, &fixture.lease);
    assert!(
        matches!(&state, cases::LeaseState::Lapsed(what) if what.contains("expired or fenced")),
        "{state:?}"
    );
    let failure = cases::classify(
        &fixture.store,
        &fixture.lease,
        "renewal refused".into(),
        protocol::FailureKind::OperationFailed,
    );
    assert_eq!(failure.kind, protocol::FailureKind::LeaseExpired);
    assert!(
        failure.detail.starts_with(
            "renewal refused; the Store reports: Task writer lease is expired or fenced"
        ),
        "{}",
        failure.detail
    );
}

/// One `task show` sample as a trusted local shard runs it (ADR-0171, ADR-0172): the request
/// `fixtures/performance/cli.toml` declares, with the shard's environment and log caps, run by
/// the actual supervisor around the actual `af`, and judged by the task_show oracle from the
/// retained prefix and the supervisor's whole-stream observation of its declared JSON fields.
const TASK_SHOW_SAMPLE: &str = r#"
import json, os, sys
sys.path.insert(0, sys.argv[1])
from perflab import clicases, runner, supervise
from perflab import suite as suite_module
supervisor, af, scratch = sys.argv[2:5]
root = os.path.join(sys.argv[1], "..", "..")
doc = clicases.load(os.path.join(root, "fixtures/performance/cli.toml"))
entry = [command for command in doc["adapters"]["af_cli_v1"]["commands"]
         if command["parameters"].get("command") == "task_show"][0]
case = suite_module.load(os.path.join(root, "fixtures/performance/suite.toml")).doc["cases"][
    entry["case_id"]]
values = {"af": af, "probe": "", "scratch": scratch, "package_version": ""}
environment = dict(runner.CLI_ENVIRONMENT)
environment.update(supervise.scratch_environment(scratch))
environment.update(case.get("af_environment", {}))
log_cap = 262144
order = supervise.request(
    [clicases.expand(text, values) for text in entry["argv"]], environment,
    clicases.expand(entry["cwd"], values), "", 120000, log_cap - log_cap // 4, log_cap,
    grace_ms=runner.GRACE_MS, patterns=clicases.patterns(entry),
    selectors=clicases.selectors(entry))
supervised = supervise.launch([supervisor], order)
result = supervised.result
kind, detail, _ = supervise.outcome(supervised, "process_group", control_record=False)
passed, why, _ = clicases.judge(entry, values, supervise.returncode(result), supervised.stdout,
                                result["stdout"]["truncated"], result["stdout_pattern_counts"],
                                result["stdout_json"])
print(json.dumps({"outcome": kind, "outcome_detail": detail, "passed": passed, "detail": why,
                  "retained": result["stdout"]["retained"],
                  "truncated": result["stdout"]["truncated"],
                  "json": result["stdout_json"],
                  "stderr": supervised.stderr.decode("utf-8", "replace")[-2048:]}))
"#;

#[test]
fn actual_af_task_show_over_a_side_written_seed_passes_the_whole_history_oracle() {
    // The side's own probe writes the 10,000-event Task state through its public writer, as a
    // shard's seed writer does; the actual `af task show` reads a private copy of it under the
    // actual supervisor. Its document is larger than any retained log, and the oracle still
    // judges it whole as one complete JSON document: the Task's identity, every history entry
    // numbered 0..9999 once and in order, and 9,997 lease renewals (ADR-0172).
    let (_writer_owner, written) = empty_scratch();
    let seed = written.join("seed");
    std::fs::create_dir(&seed).unwrap();
    let writer = json!({"case_id": "cli_state_fixture",
                        "instance_id": "cli_state_fixture__state_cli_state_10000",
                        "contract_id": "cli_state_fixture_v1",
                        "parameters": {"state": "cli_state_10000"}});
    let reply = protocol::run(&serde_json::to_vec(&request(&writer, &seed)).unwrap());
    assert_eq!(
        reply.code,
        0,
        "the seed writer failed: {}",
        String::from_utf8_lossy(&reply.stdout)
    );
    let (_owner, sample) = empty_scratch();
    copy_tree(&seed, &sample.join("seed"));
    let output = Command::new("python3")
        .args(["-I", "-B"])
        .arg("-c")
        .arg(TASK_SHOW_SAMPLE)
        .arg(root().join("scripts/performance"))
        .arg(supervisor_binary())
        .arg(af_binary())
        .arg(&sample)
        .output()
        .expect("python3 runs the trusted driver");
    assert!(
        output.status.success(),
        "the driver failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let judged: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(judged["outcome"].is_null(), "{judged}");
    assert_eq!(judged["passed"], true, "{judged}");
    assert!(
        judged["truncated"].as_u64().unwrap() > 0,
        "the document outgrows the retained log: {judged}"
    );
    let observed = &judged["json"];
    assert_eq!(observed["document"]["status"], "complete", "{judged}");
    let selected = |index: usize, holders: u64, count: u64, matches: Value| {
        let found = &observed["selectors"][index];
        assert_eq!(
            (&found["holders"], &found["count"], &found["matches"]),
            (&json!(holders), &json!(count), &matches),
            "{judged}"
        );
        assert_eq!(
            (&found["duplicates"], &found["invalid"], &found["breaks"]),
            (&json!(0), &json!(0), &json!(0)),
            "{judged}"
        );
        found.clone()
    };
    selected(0, 1, 1, json!([1]));
    let sequence = selected(1, 10000, 10000, json!([]));
    assert_eq!(
        (&sequence["first"], &sequence["last"]),
        (&json!(0), &json!(9999))
    );
    selected(2, 10000, 10000, json!([9997]));
}

/// The laboratory's calibration builds of the probe and of `af` (ADR-0181, ADR-0182), executed:
/// both compiled with `perf-calibration` from this source, as a preparation or slowdown member
/// builds both sides. This test binary carries the feature only when `make lint` runs its
/// `test-perf-calibration` step; every other build of it compiles none of this.
#[cfg(feature = "perf-calibration")]
mod calibration_build {
    use std::time::Duration;

    use super::*;

    const CONTROL: &str = "AF_PERF_CALIBRATION_ITERATIONS";
    const REPORT: &str = "af.perf-calibration-injection/1 ";
    /// Large enough that its measured duration is never zero, small enough for a debug build.
    const ITERATIONS: u64 = 20_000_000;

    fn calibrated(kind: &str, name: &str, package: &str, feature: &str) -> PathBuf {
        executables::built_with_features(
            kind,
            name,
            package,
            env!("REVIEW_PIPELINE_TARGET_TRIPLE_DIR"),
            &[feature],
        )
    }

    fn calibrated_probe() -> PathBuf {
        static BUILT: OnceLock<PathBuf> = OnceLock::new();
        BUILT
            .get_or_init(|| {
                calibrated(
                    "--example",
                    "perf-probe",
                    "review-pipeline",
                    "review-pipeline/perf-calibration",
                )
            })
            .clone()
    }

    fn calibrated_af() -> PathBuf {
        static BUILT: OnceLock<PathBuf> = OnceLock::new();
        BUILT
            .get_or_init(|| calibrated("--bin", "af", "af", "af/perf-calibration"))
            .clone()
    }

    /// One calibrated probe process, its environment only `PATH`, `LC_ALL` and `env`.
    fn probe(request: &Value, env: &[(&str, &str)]) -> (protocol::Reply, Duration) {
        use std::io::Write;
        let binary = calibrated_probe();
        let started = Instant::now();
        let mut child = Command::new(binary)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("LC_ALL", "C")
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // A refused control exits before it reads the request, which may close the pipe.
        let _ = child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(request).unwrap());
        let output = child.wait_with_output().unwrap();
        (
            protocol::Reply {
                code: output.status.code().map_or(255, |code| code as u8),
                stdout: output.stdout,
                stderr: output.stderr,
            },
            started.elapsed(),
        )
    }

    /// Every `af.perf-calibration-injection/1` line of `stderr`, as (iterations, nanoseconds).
    fn reports(stderr: &[u8]) -> Vec<(u64, u128)> {
        String::from_utf8_lossy(stderr)
            .lines()
            .filter_map(|line| line.strip_prefix(REPORT))
            .map(|rest| {
                let (iterations, nanos) = rest
                    .strip_prefix("iterations=")
                    .and_then(|rest| rest.split_once(" injected_ns="))
                    .unwrap_or_else(|| panic!("a malformed report: {rest}"));
                (iterations.parse().unwrap(), nanos.parse().unwrap())
            })
            .collect()
    }

    fn decimal_ms(value: &Value) -> f64 {
        value["value"].as_str().unwrap().parse().unwrap()
    }

    fn canonical_entry() -> Value {
        golden()["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["case_id"] == "canonical_serialize_small")
            .cloned()
            .unwrap()
    }

    #[test]
    fn calibration_build_probe_injects_once_inside_the_region_and_keeps_its_oracle() {
        let entry = canonical_entry();
        let mut digests = Vec::new();
        for value in [None, Some("0".to_string()), Some(ITERATIONS.to_string())] {
            let (_owner, scratch) = empty_scratch();
            let request = request(&entry, &scratch);
            let env: Vec<(&str, &str)> = value.iter().map(|value| (CONTROL, &value[..])).collect();
            let (reply, _) = probe(&request, &env);
            let message: Value = serde_json::from_slice(&reply.stdout).unwrap();
            assert_eq!(reply.code, 0, "{value:?}: {message}");
            // The loop touched no state: the trusted driver's oracle passes as without it.
            assert_eq!(judge(&request, &reply)["status"], "passed", "{value:?}");
            digests.push(message["correctness"]["digest"].clone());
            let found = reports(&reply.stderr);
            match value.as_deref() {
                // No control is no injection, and no report.
                None => assert!(found.is_empty(), "{found:?}"),
                Some(value) => {
                    // One region, one injection, reported once, for exactly the count named.
                    assert_eq!(found.len(), 1, "{found:?}");
                    let (iterations, nanos) = found[0];
                    assert_eq!(iterations.to_string(), value);
                    let last = String::from_utf8_lossy(&reply.stderr);
                    assert!(
                        last.trim_end().lines().last().unwrap().starts_with(REPORT),
                        "the report is the last stderr line: {last}"
                    );
                    if iterations > 0 {
                        // Measured inside the timed region: the region's own wall includes it.
                        let region_ms = decimal_ms(&message["metrics"]["wall_ms"]);
                        let injected_ms = nanos as f64 / 1_000_000.0;
                        assert!(nanos > 0);
                        assert!(
                            region_ms >= injected_ms,
                            "region {region_ms} ms, injected {injected_ms} ms"
                        );
                    }
                }
            }
        }
        assert!(
            digests.iter().all(|digest| digest == &digests[0]),
            "{digests:?}"
        );
    }

    #[test]
    fn calibration_build_probe_refuses_malformed_or_unbounded_counts_before_any_loop() {
        let entry = canonical_entry();
        let beyond = (review_core::perf_calibration::ITERATIONS_MAX + 1).to_string();
        for value in ["", "01", "-1", "1e6", " 5", &beyond[..]] {
            let (_owner, scratch) = empty_scratch();
            let (reply, elapsed) = probe(&request(&entry, &scratch), &[(CONTROL, value)]);
            assert_eq!(reply.code, 2, "{value:?}");
            assert!(reply.stdout.is_empty(), "{value:?}");
            let stderr = String::from_utf8_lossy(&reply.stderr);
            assert!(
                stderr.contains("is not a canonical iteration count"),
                "{value:?}: {stderr}"
            );
            assert!(reports(&reply.stderr).is_empty());
            // Refused before the request is read: a loop near 2^40 iterations would take hours.
            assert!(elapsed < Duration::from_secs(60), "{value:?}: {elapsed:?}");
        }
    }

    /// One calibrated `af` process with a private HOME and XDG directories below `home`.
    fn af(home: &Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        let mut command = Command::new(calibrated_af());
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("LC_ALL", "C")
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .envs(env.iter().copied())
            .args(args)
            .stdin(Stdio::null());
        command.output().unwrap()
    }

    #[test]
    fn calibration_build_af_injects_once_in_its_process_and_keeps_its_output() {
        let (_home_owner, home) = empty_scratch();
        for args in [&["--version"][..], &["--help"][..]] {
            let plain = af(&home, args, &[]);
            assert!(plain.status.success(), "{args:?}");
            assert!(reports(&plain.stderr).is_empty(), "{args:?}");
            for value in ["0".to_string(), ITERATIONS.to_string()] {
                let injected = af(&home, args, &[(CONTROL, &value)]);
                assert!(injected.status.success(), "{args:?} {value}");
                // The process's output is unchanged; only one report line is added to stderr.
                assert_eq!(injected.stdout, plain.stdout, "{args:?} {value}");
                let found = reports(&injected.stderr);
                assert_eq!(found.len(), 1, "{args:?} {value}: {found:?}");
                assert_eq!(found[0].0.to_string(), value);
                assert!(value == "0" || found[0].1 > 0);
            }
        }
        // A malformed count is refused before the command runs.
        let refused = af(&home, &["--version"], &[(CONTROL, "01")]);
        assert_eq!(refused.status.code(), Some(2));
        assert!(refused.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&refused.stderr).contains("is not a canonical iteration count")
        );
    }
}
