//! The probe's control protocol: one bounded `af.perf-probe-request/2` on stdin, one
//! `af.perf-probe/1` line on stdout.
//!
//! The request names exactly one declared case contract, its parameter assignment, the
//! repetitions the contract declares, an empty scratch directory the probe may write and, for a
//! contract that reads a seed (ADR-0170), the sample's private copy of that seed. Anything else
//! is refused before a fixture exists: an unknown case, contract or field, a value outside the
//! declared domain, a duplicate key, a wrong instance ID or repetition count, a scratch path
//! that is relative, missing, non-empty, or reached through a symlink, a seed a contract does
//! not read, and a missing seed one does.
//!
//! The caller owns the scratch: the probe creates `probe-work` in it, writes only below that,
//! and removes nothing, so no path another process can replace is ever deleted by the probe.
//!
//! Exit status: 0 the operation ran and the probe's own checks held; 1 it ran or was set up and
//! failed (the message says how); 2 the request was refused and no message was written. The
//! probe's `passed` is never the verdict: the trusted driver checks every message against the
//! authority's golden entries.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::{MapAccess, Visitor};
use serde_json::{Value, json};

use crate::cases;

pub const REQUEST_SCHEMA: &str = "af.perf-probe-request/2";
pub const MESSAGE_SCHEMA: &str = "af.perf-probe/1";
/// Every control message and record is at most 64 KiB.
pub const RECORD_BYTES_MAX: usize = 64 * 1024;
const SCRATCH_PATH_MAX: usize = 1024;
/// The probe creates exactly this directory below the scratch, exclusively, and writes only
/// below it. It removes nothing: the scratch and everything in it are the caller's.
pub const WORK_DIR: &str = "probe-work";

/// A finite parameter domain, as the registry declares it.
#[derive(Debug, Clone, Copy)]
pub enum Domain {
    Fixture(&'static [&'static str]),
    Enum(&'static [&'static str]),
    Integer(&'static [u64]),
}

/// One case's cache treatment: what is fresh in every sample, declared separately.
#[derive(Debug, Clone, Copy)]
pub struct Cache {
    pub process: &'static str,
    pub store: &'static str,
    pub projection: &'static str,
    pub os_page_cache: &'static str,
}

/// How a contract relates to a seed (ADR-0170): a seed is a fixture one side writes once
/// through its own writers, which the shard seals and every sample copies privately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seed {
    /// The contract generates its own fixture in every sample.
    None,
    /// The contract writes a seed: its whole work directory is the seed, published once every
    /// handle is closed and the probe has exited.
    Writes,
    /// The contract reads the sample's private copy of the seed this writer contract wrote,
    /// over the named parameters of its assignment.
    Reads {
        case_id: &'static str,
        contract_id: &'static str,
        parameters: &'static [&'static str],
    },
}

/// One declared case contract. Its region, cache treatment, parameter domains, repetitions and
/// seed equal the registry's case (`fixtures/performance/suite.toml`); a test pins them.
#[derive(Debug, Clone, Copy)]
pub struct Contract {
    pub case_id: &'static str,
    pub contract_id: &'static str,
    pub region: &'static str,
    pub parameters: &'static [(&'static str, Domain)],
    pub repetitions: u64,
    pub cache: Cache,
    pub seed: Seed,
}

const ENVELOPES_SMALL: &[&str] = &["small_envelope"];
const ENVELOPES_LARGE: &[&str] = &["issue_255_medium", "issue_255_large"];

const fn cache(store: &'static str, projection: &'static str) -> Cache {
    Cache {
        process: "fresh",
        store,
        projection,
        os_page_cache: "warm",
    }
}

const NO_STORE: Cache = cache("not_applicable", "not_applicable");
const FRESH_STORE: Cache = cache("fresh", "not_applicable");

const fn library(
    case_id: &'static str,
    contract_id: &'static str,
    region: &'static str,
    parameters: &'static [(&'static str, Domain)],
    cache: Cache,
) -> Contract {
    Contract {
        case_id,
        contract_id,
        region,
        parameters,
        repetitions: 1,
        cache,
        seed: Seed::None,
    }
}

const fn seeded(contract: Contract, seed: Seed) -> Contract {
    Contract { seed, ..contract }
}

const SOURCE_PARAMETERS: &[(&str, Domain)] = &[
    ("files", Domain::Integer(&[100, 10000])),
    ("tree", Domain::Fixture(&["source_files_128b"])),
];
const LOG_PARAMETER: (&str, Domain) = ("log", Domain::Fixture(&["events_10000"]));

pub const CONTRACTS: &[Contract] = &[
    library(
        "canonical_serialize_small",
        "canonical_serialize_v1",
        "canonical_serialize",
        &[("package", Domain::Fixture(ENVELOPES_SMALL))],
        NO_STORE,
    ),
    library(
        "canonical_serialize",
        "canonical_serialize_v1",
        "canonical_serialize",
        &[("package", Domain::Fixture(ENVELOPES_LARGE))],
        NO_STORE,
    ),
    library(
        "canonical_validate_small",
        "canonical_validate_v1",
        "canonical_validate",
        &[("package", Domain::Fixture(ENVELOPES_SMALL))],
        NO_STORE,
    ),
    library(
        "canonical_validate",
        "canonical_validate_v1",
        "canonical_validate",
        &[("package", Domain::Fixture(ENVELOPES_LARGE))],
        NO_STORE,
    ),
    library(
        "cas_raw_get_small",
        "cas_raw_get_verify_v1",
        "cas_raw_get_verify",
        &[("package", Domain::Fixture(ENVELOPES_SMALL))],
        FRESH_STORE,
    ),
    library(
        "cas_raw_get",
        "cas_raw_get_verify_v1",
        "cas_raw_get_verify",
        &[("package", Domain::Fixture(ENVELOPES_LARGE))],
        FRESH_STORE,
    ),
    library(
        "cas_typed_get_small",
        "cas_typed_get_validate_v1",
        "cas_typed_get_validate",
        &[("package", Domain::Fixture(ENVELOPES_SMALL))],
        FRESH_STORE,
    ),
    library(
        "cas_typed_get",
        "cas_typed_get_validate_v1",
        "cas_typed_get_validate",
        &[("package", Domain::Fixture(ENVELOPES_LARGE))],
        FRESH_STORE,
    ),
    // Both read the sample's private copy of a seed their side wrote once (ADR-0170): the
    // committed repository, and the Task log of exactly 10,000 events.
    seeded(
        library(
            "source_capture",
            "source_capture_v2",
            "source_capture",
            SOURCE_PARAMETERS,
            FRESH_STORE,
        ),
        Seed::Reads {
            case_id: "source_capture_seed",
            contract_id: "source_capture_seed_v1",
            parameters: &["files", "tree"],
        },
    ),
    seeded(
        library(
            "state_projection",
            "state_projection_v2",
            "application_projection",
            &[
                LOG_PARAMETER,
                (
                    "projection",
                    Domain::Enum(&["application_cold", "prewarmed"]),
                ),
            ],
            cache("fresh", "by_parameter"),
        ),
        Seed::Reads {
            case_id: "state_projection_seed",
            contract_id: "state_projection_seed_v1",
            parameters: &["log"],
        },
    ),
    // Not timed cases: the seed writers of the two above. Each writes, through the side's own
    // public writers, the fixture its readers copy, and checks it against the golden seed entry.
    seeded(
        library(
            "source_capture_seed",
            "source_capture_seed_v1",
            "source_capture_seed",
            SOURCE_PARAMETERS,
            FRESH_STORE,
        ),
        Seed::Writes,
    ),
    seeded(
        library(
            "state_projection_seed",
            "state_projection_seed_v1",
            "state_projection_seed",
            &[LOG_PARAMETER],
            FRESH_STORE,
        ),
        Seed::Writes,
    ),
    seeded(
        library(
            "authority_seed",
            "authority_seed_v1",
            "authority_seed",
            &[("package", Domain::Fixture(ENVELOPES_LARGE))],
            cache("fresh", "application_cold"),
        ),
        Seed::Writes,
    ),
    seeded(
        library(
            "authority_plan_check",
            "authority_plan_check_v2",
            "current_plan_check",
            &[("package", Domain::Fixture(ENVELOPES_LARGE))],
            cache("fresh", "application_cold"),
        ),
        Seed::Reads {
            case_id: "authority_seed",
            contract_id: "authority_seed_v1",
            parameters: &["package"],
        },
    ),
    seeded(
        library(
            "authority_runtime_new",
            "authority_runtime_new_v2",
            "task_runtime_new",
            &[("package", Domain::Fixture(ENVELOPES_LARGE))],
            cache("fresh", "application_cold"),
        ),
        Seed::Reads {
            case_id: "authority_seed",
            contract_id: "authority_seed_v1",
            parameters: &["package"],
        },
    ),
    library(
        "persistence_fs",
        "persistence_fs_v1",
        "persistence_round_trip",
        &[(
            "operation",
            Domain::Enum(&["cas_write_read", "state_append_read"]),
        )],
        FRESH_STORE,
    ),
    // Not a timed case: the fixture writer of the CLI state cases (ADR-0168). Each side's own
    // probe writes, through that side's public writers, the Task state whose 10,000 events the
    // side's `af task show` then reads; the laboratory's CLI registry declares it.
    library(
        "cli_state_fixture",
        "cli_state_fixture_v1",
        "cli_state_fixture",
        &[("state", Domain::Fixture(&["cli_state_10000"]))],
        FRESH_STORE,
    ),
    // Not a timed case either: it tops the Campaign the side's own `af review run` wrote up to
    // exactly 10,000 events through that side's public Findings Ledger writer, for the side's
    // `af review ledger` and `af review report` (ADR-0169).
    library(
        "cli_campaign_fixture",
        "cli_campaign_fixture_v1",
        "cli_campaign_fixture",
        &[("state", Domain::Fixture(&["cli_state_10000"]))],
        cache("existing", "application_cold"),
    ),
    Contract {
        case_id: "sentinel_canonical",
        contract_id: "sentinel_canonical_v1",
        region: "sentinel_canonical_batch",
        parameters: &[],
        repetitions: crate::fixtures::SENTINEL_REPETITIONS,
        cache: NO_STORE,
        seed: Seed::None,
    },
];

pub fn contract(case_id: &str) -> Option<&'static Contract> {
    CONTRACTS
        .iter()
        .find(|contract| contract.case_id == case_id)
}

/// One parameter value: an ID-shaped string or a nonnegative integer, never anything else.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Param {
    Text(String),
    Integer(u64),
}

impl Param {
    pub fn text(&self) -> Option<&str> {
        match self {
            Param::Text(text) => Some(text),
            Param::Integer(_) => None,
        }
    }

    pub fn integer(&self) -> Option<u64> {
        match self {
            Param::Integer(value) => Some(*value),
            Param::Text(_) => None,
        }
    }

    fn json(&self) -> Value {
        match self {
            Param::Text(text) => json!(text),
            Param::Integer(value) => json!(value),
        }
    }

    fn spelled(&self) -> String {
        match self {
            Param::Text(text) => text.clone(),
            Param::Integer(value) => value.to_string(),
        }
    }
}

/// A JSON object whose keys are unique: a repeated key is refused, never silently overwritten.
#[derive(Debug, Default)]
pub struct UniqueMap(pub BTreeMap<String, Value>);

impl<'de> Deserialize<'de> for UniqueMap {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Unique;
        impl<'de> Visitor<'de> for Unique {
            type Value = UniqueMap;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("an object with unique keys")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<UniqueMap, A::Error> {
                let mut out = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, Value>()? {
                    if out.contains_key(&key) {
                        return Err(serde::de::Error::custom(format!("duplicate key {key}")));
                    }
                    out.insert(key, value);
                }
                Ok(UniqueMap(out))
            }
        }
        deserializer.deserialize_map(Unique)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRequest {
    schema: String,
    case_id: String,
    instance_id: String,
    contract_id: String,
    parameters: UniqueMap,
    repetitions: u64,
    scratch: String,
    seed: Option<String>,
}

/// A request admitted against its declared contract.
#[derive(Debug)]
pub struct Request {
    pub contract: &'static Contract,
    pub instance_id: String,
    pub parameters: BTreeMap<String, Param>,
    pub repetitions: u64,
    pub scratch: PathBuf,
    /// The sample's private copy of its seed, exactly for a contract that reads one.
    pub seed: Option<PathBuf>,
}

impl Request {
    pub fn text(&self, name: &str) -> &str {
        self.parameters[name]
            .text()
            .expect("admitted text parameter")
    }

    pub fn integer(&self, name: &str) -> u64 {
        self.parameters[name]
            .integer()
            .expect("admitted integer parameter")
    }
}

/// The instance ID the registry expands an assignment to: the case ID, then `name_value`
/// pairs in parameter-name order after a double underscore.
pub fn instance_id(case_id: &str, parameters: &BTreeMap<String, Param>) -> String {
    if parameters.is_empty() {
        return case_id.into();
    }
    let parts: Vec<String> = parameters
        .iter()
        .map(|(name, value)| format!("{name}_{}", value.spelled()))
        .collect();
    format!("{case_id}__{}", parts.join("_"))
}

fn admit_parameter(name: &str, domain: Domain, value: &Value) -> Result<Param, String> {
    let refused = || format!("parameter {name}: {value} is outside the declared domain");
    match domain {
        Domain::Fixture(values) | Domain::Enum(values) => match value {
            Value::String(text) if values.contains(&text.as_str()) => Ok(Param::Text(text.clone())),
            _ => Err(refused()),
        },
        Domain::Integer(values) => match value.as_u64() {
            Some(number) if value.is_u64() && values.contains(&number) => {
                Ok(Param::Integer(number))
            }
            _ => Err(refused()),
        },
    }
}

/// An absolute, canonical (no symlink, `.` or `..`), existing directory.
fn admit_directory(what: &str, raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty() || raw.len() > SCRATCH_PATH_MAX {
        return Err(format!("{what}: an absolute path of at most 1024 bytes"));
    }
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(format!("{what}: must be absolute"));
    }
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("{what}: {error}"))?;
    if canonical != path {
        return Err(format!(
            "{what}: {raw} is not canonical; it resolves to {}",
            canonical.display()
        ));
    }
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| format!("{what}: {error}"))?;
    if !metadata.is_dir() {
        return Err(format!("{what}: not a directory"));
    }
    Ok(path)
}

/// The scratch must be an absolute, canonical, existing, empty directory: everything the probe
/// writes stays below it.
fn admit_scratch(raw: &str) -> Result<PathBuf, String> {
    let path = admit_directory("scratch", raw)?;
    if std::fs::read_dir(&path)
        .map_err(|error| format!("scratch: {error}"))?
        .next()
        .is_some()
    {
        return Err("scratch: must be empty".into());
    }
    Ok(path)
}

/// A seed is given exactly to a contract that reads one: the sample's own copy, an absolute,
/// canonical directory outside the scratch, whose files the sample may change.
fn admit_seed(
    contract: &Contract,
    raw: Option<&str>,
    scratch: &Path,
) -> Result<Option<PathBuf>, String> {
    match (contract.seed, raw) {
        (Seed::Reads { .. }, Some(raw)) => {
            let path = admit_directory("seed", raw)?;
            if path.starts_with(scratch) || scratch.starts_with(&path) {
                return Err("seed: must lie outside the scratch".into());
            }
            Ok(Some(path))
        }
        (Seed::Reads { contract_id, .. }, None) => Err(format!(
            "seed: {} reads a private copy of a {contract_id} seed; none was given",
            contract.contract_id
        )),
        (_, Some(_)) => Err(format!("seed: {} reads no seed", contract.contract_id)),
        (_, None) => Ok(None),
    }
}

/// Parse and admit one request; every refusal names its reason.
pub fn admit(input: &[u8]) -> Result<Request, String> {
    if input.len() > RECORD_BYTES_MAX {
        return Err(format!(
            "request exceeds {RECORD_BYTES_MAX} bytes; control messages are bounded"
        ));
    }
    let raw: RawRequest = serde_json::from_slice(input).map_err(|error| error.to_string())?;
    if raw.schema != REQUEST_SCHEMA {
        return Err(format!("schema: expected {REQUEST_SCHEMA}"));
    }
    let contract = contract(&raw.case_id)
        .ok_or_else(|| format!("case {} is not declared by this probe", raw.case_id))?;
    if raw.contract_id != contract.contract_id {
        return Err(format!(
            "contract {} is not case {}'s contract {}",
            raw.contract_id, contract.case_id, contract.contract_id
        ));
    }
    let mut parameters = BTreeMap::new();
    for (name, value) in &raw.parameters.0 {
        let (_, domain) = contract
            .parameters
            .iter()
            .find(|(declared, _)| declared == name)
            .ok_or_else(|| format!("parameter {name} is not declared for {}", contract.case_id))?;
        parameters.insert(name.clone(), admit_parameter(name, *domain, value)?);
    }
    for (name, _) in contract.parameters {
        if !parameters.contains_key(*name) {
            return Err(format!("parameter {name} is missing"));
        }
    }
    let expected = instance_id(contract.case_id, &parameters);
    if raw.instance_id != expected {
        return Err(format!(
            "instance {} does not name this assignment; expected {expected}",
            raw.instance_id
        ));
    }
    if raw.repetitions != contract.repetitions {
        return Err(format!(
            "repetitions {} differ from the contract's declared {}",
            raw.repetitions, contract.repetitions
        ));
    }
    let scratch = admit_scratch(&raw.scratch)?;
    let seed = admit_seed(contract, raw.seed.as_deref(), &scratch)?;
    Ok(Request {
        contract,
        instance_id: raw.instance_id,
        parameters,
        repetitions: raw.repetitions,
        scratch,
        seed,
    })
}

/// Exact milliseconds of a monotonic nanosecond count, as a canonical decimal string.
pub fn decimal_ms(nanos: u128) -> String {
    let whole = nanos / 1_000_000;
    let fraction = nanos % 1_000_000;
    if fraction == 0 {
        return whole.to_string();
    }
    let digits = format!("{fraction:06}");
    format!("{whole}.{}", digits.trim_end_matches('0'))
}

/// What the probe observed; the driver compares it with golden entries.
#[derive(Debug)]
pub struct Observation {
    pub digest: Option<String>,
    pub count: Option<u64>,
    pub detail: String,
    pub extra_metrics: BTreeMap<&'static str, (String, &'static str)>,
}

/// A side-specific functional failure. The kind leads the message detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// The production lease lapsed before or during the timed region.
    LeaseExpired,
    /// The timed production operation returned an error or a self-check failed.
    OperationFailed,
    /// A fixture could not be generated through the product's writers.
    FixtureFailed,
}

impl FailureKind {
    pub fn spelled(self) -> &'static str {
        match self {
            FailureKind::LeaseExpired => "lease_expired",
            FailureKind::OperationFailed => "operation_failed",
            FailureKind::FixtureFailed => "fixture_failed",
        }
    }
}

#[derive(Debug)]
pub struct Failure {
    pub kind: FailureKind,
    pub detail: String,
}

impl Failure {
    pub fn new(kind: FailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

/// One executed sample: setup and region durations, fixture identity and the result.
#[derive(Debug)]
pub struct Outcome {
    pub fixture_digest: Option<String>,
    pub setup_nanos: u128,
    /// None when a failure happened before the timed region began.
    pub region_nanos: Option<u128>,
    pub result: Result<Observation, Failure>,
}

const DETAIL_MAX: usize = 4096;

fn bounded(text: String) -> String {
    if text.len() <= DETAIL_MAX {
        return text;
    }
    let mut end = DETAIL_MAX - 3;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

/// The `af.perf-probe/1` message of one outcome.
pub fn message(request: &Request, outcome: &Outcome) -> Value {
    let contract = request.contract;
    let mut metrics = serde_json::Map::new();
    if let Some(nanos) = outcome.region_nanos {
        metrics.insert(
            "wall_ms".into(),
            json!({"value": decimal_ms(nanos), "unit": "ms"}),
        );
    }
    metrics.insert(
        "setup_ms".into(),
        json!({"value": decimal_ms(outcome.setup_nanos), "unit": "ms"}),
    );
    let scope = format!(
        "region={} repetitions={} cache=process:{},store:{},projection:{},os_page_cache:{}",
        contract.region,
        request.repetitions,
        contract.cache.process,
        contract.cache.store,
        contract.cache.projection,
        contract.cache.os_page_cache
    );
    let correctness = match &outcome.result {
        Ok(observation) => {
            for (key, (value, unit)) in &observation.extra_metrics {
                metrics.insert((*key).into(), json!({"value": value, "unit": unit}));
            }
            json!({"status": "passed", "digest": observation.digest, "count": observation.count,
                   "detail": bounded(format!("ok: {scope}; {}", observation.detail))})
        }
        // af.perf-probe/1 names a digest or a count: a failed sample checked nothing.
        Err(failure) => json!({"status": "failed", "digest": null, "count": 0,
            "detail": bounded(format!("{}: {scope}; {}", failure.kind.spelled(), failure.detail))}),
    };
    let parameters: serde_json::Map<String, Value> = request
        .parameters
        .iter()
        .map(|(name, value)| (name.clone(), value.json()))
        .collect();
    json!({
        "schema": MESSAGE_SCHEMA,
        "case_id": contract.case_id,
        "instance_id": request.instance_id,
        "contract_id": contract.contract_id,
        "fixture_digest": outcome.fixture_digest,
        "parameters": parameters,
        "metrics": metrics,
        "correctness": correctness,
    })
}

/// The probe's whole effect on its process: exit status, stdout and stderr bytes.
#[derive(Debug)]
pub struct Reply {
    pub code: u8,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Admit one request, run it in the work directory it creates below the scratch, and render
/// the reply. Nothing is removed; see [`run_admitted`].
pub fn run(input: &[u8]) -> Reply {
    match admit(input) {
        Ok(request) => run_admitted(&request, cases::execute),
        Err(reason) => Reply {
            code: 2,
            stdout: vec![],
            stderr: format!("perf-probe: refused: {reason}\n").into_bytes(),
        },
    }
}

/// Run an admitted request through `execute` in the work directory this invocation creates.
///
/// Lifecycle: the caller owns the scratch and everything below it, including the work
/// directory and whatever the sample wrote there; the probe never removes anything. A work
/// directory it could not create (an entry of any kind already at that path) fails the sample
/// as `fixture_failed` before anything runs. Removing the scratch afterwards is the caller's:
/// the trusted driver removes its private per-invocation namespace through a directory handle
/// (`perflab.oracle.Namespace`), and a test removes its own temporary directory.
pub fn run_admitted(request: &Request, execute: impl FnOnce(&Request, &Path) -> Outcome) -> Reply {
    let work = request.scratch.join(WORK_DIR);
    let outcome = match std::fs::create_dir(&work) {
        Ok(()) => execute(request, &work),
        Err(error) => Outcome {
            fixture_digest: None,
            setup_nanos: 0,
            region_nanos: None,
            result: Err(Failure::new(
                FailureKind::FixtureFailed,
                format!(
                    "cannot create {}: {error}; nothing ran and nothing was removed",
                    work.display()
                ),
            )),
        },
    };
    let mut line = serde_json::to_vec(&message(request, &outcome)).expect("message serializes");
    line.push(b'\n');
    if line.len() > RECORD_BYTES_MAX {
        return Reply {
            code: 1,
            stdout: vec![],
            stderr: b"perf-probe: the message exceeds 64 KiB\n".to_vec(),
        };
    }
    Reply {
        code: if outcome.result.is_ok() { 0 } else { 1 },
        stdout: line,
        stderr: vec![],
    }
}
