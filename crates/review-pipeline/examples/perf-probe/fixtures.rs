//! Deterministic workload fixtures, generated through the product's own writers.
//!
//! Every constant here is fixture *input*: the bytes the generator feeds the product. The
//! expected outputs (sizes, digests, semantic projections) are not computed here; the
//! independent reference generator `scripts/performance/perflab/reference.py` constructs them
//! from these same inputs and commits them under `fixtures/performance/golden/`. A change to
//! any constant below therefore needs that generator and its golden entries changed with it.

use std::collections::BTreeMap;

use review_core::Producer;
use serde_json::{Value, json};

/// The production Task lease (`crates/review-pipeline/src/task/lease.rs`); never extended.
pub const TASK_LEASE_MS: u64 = 15_000;

pub const REQUIREMENTS_V1: &str = "af/Requirements@1";

/// The trusted policy document every authority fixture is bound to, and its content ID.
pub fn policy_document() -> Value {
    json!({"perf_probe": "trusted fixture policy"})
}
pub const POLICY_ID: &str =
    "sha256:c18398d24826d0cbe9c3794f765d453ea7aa6e1bb9c186554b356a0079323940";

/// The producer of every fixture envelope the probe itself writes.
pub fn producer() -> Producer {
    Producer::KernelOperation {
        run_id: "perf-probe".into(),
        node_id: None,
        operation_id: "fixture@1".into(),
    }
}

/// How a typed-envelope fixture reaches its declared serialized size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Envelope {
    /// An `af/Requirements@1` envelope whose text is `text_bytes` generated ASCII letters.
    Requirements { text_bytes: usize },
    /// The `perf/author` Worker package (`af/TaskPackage@1`) with a padding file of
    /// `digits` single-digit bytes followed by `tens` bytes of value 10.
    Package { digits: usize, tens: usize },
}

/// One registry typed-envelope fixture: its ID, exact serialized size and generation rule.
#[derive(Debug, Clone, Copy)]
pub struct EnvelopeFixture {
    pub id: &'static str,
    pub serialized_bytes: usize,
    pub envelope: Envelope,
}

/// The registry's typed envelopes (`fixtures/performance/suite.toml`). Each size is the full
/// canonical envelope, not its payload. The generation parameters are solved by the reference
/// generator and checked against these sizes by it and by the probe tests.
pub const ENVELOPES: [EnvelopeFixture; 3] = [
    EnvelopeFixture {
        id: "small_envelope",
        serialized_bytes: 4096,
        envelope: Envelope::Requirements { text_bytes: 3762 },
    },
    EnvelopeFixture {
        id: "issue_255_medium",
        serialized_bytes: 1_900_000,
        envelope: Envelope::Package {
            digits: 947_165,
            tens: 1,
        },
    },
    EnvelopeFixture {
        id: "issue_255_large",
        serialized_bytes: 5_605_230,
        envelope: Envelope::Package {
            digits: 2_799_780,
            tens: 1,
        },
    },
];

pub fn envelope_fixture(id: &str) -> Option<EnvelopeFixture> {
    ENVELOPES.iter().copied().find(|fixture| fixture.id == id)
}

/// `count` ASCII letters `a..z` repeated; never a byte JSON escapes.
pub fn letters(count: usize) -> String {
    (0..count)
        .map(|index| char::from(b'a' + (index % 26) as u8))
        .collect()
}

pub fn requirements_payload(text_bytes: usize) -> Value {
    json!({"text": letters(text_bytes)})
}

pub const AUTHOR_PACKAGE: &str = "perf/author";
pub const CHECKER_PACKAGE: &str = "perf/checker";
pub const PIPELINE_PACKAGE: &str = "perf/two-node";
pub const PACKAGE_VERSION: &str = "1.0.0";
pub const PADDING_PATH: &str = "data/padding.bin";

const AUTHOR_WORKER: &str = r#"schema = "af.worker/1"
name = "perf/author"
version = "1.0.0"

[signature]
effects = []
roles = ["author"]
worker_input_type = "af/Requirements@1"
worker_output_type = "af/CheckedDocument@1"
outcome_port = "output"

[signature.contract.inputs.input]
artifact_type = "af/Requirements@1"
cardinality = "one"
optional = false
covers = []

[signature.contract.inputs.input.affinity]
kind = "unbound"

[signature.contract.outputs.output]
artifact_type = "af/CheckedDocument@1"
cardinality = "one"
optional = false
covers = []

[signature.contract.outputs.output.affinity]
kind = "unbound"

[signature.evidence]
output = ["sha256:c18398d24826d0cbe9c3794f765d453ea7aa6e1bb9c186554b356a0079323940"]

[signature.retains]

[signature.attempt]
tokens = 0
wall_ms = 5000

[runner]
kind = "command"

[runner.command]
program = "/usr/bin/false"
args = []
"#;

const CHECKER_WORKER: &str = r#"schema = "af.worker/1"
name = "perf/checker"
version = "1.0.0"

[signature]
effects = []
roles = ["check"]
worker_input_type = "af/CheckedDocument@1"
worker_output_type = "af/CheckedDocument@1"
outcome_port = "output"

[signature.contract.inputs.input]
artifact_type = "af/CheckedDocument@1"
cardinality = "one"
optional = false
covers = []

[signature.contract.inputs.input.affinity]
kind = "unbound"

[signature.contract.outputs.output]
artifact_type = "af/CheckedDocument@1"
cardinality = "one"
optional = false
covers = []

[signature.contract.outputs.output.affinity]
kind = "unbound"

[signature.evidence]
output = ["sha256:c18398d24826d0cbe9c3794f765d453ea7aa6e1bb9c186554b356a0079323940"]

[signature.retains]

[signature.attempt]
tokens = 0
wall_ms = 5000

[runner]
kind = "command"

[runner.command]
program = "/usr/bin/false"
args = []
"#;

const INPUT_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["input"],"properties":{"input":{"type":"array","minItems":1,"maxItems":1,"items":{"type":"object","additionalProperties":false,"required":["artifact_id","artifact_type","payload"],"properties":{"artifact_id":{"type":"string"},"artifact_type":{"type":"string"},"payload":{"type":"object"}}}}}}"#;

const OUTPUT_SCHEMA: &str = r#"{"type":"object","additionalProperties":false,"required":["outcome","text"],"properties":{"outcome":{"enum":["passed","failed","inconclusive"]},"text":{"type":"string"}}}"#;

const INSTRUCTIONS: &str = "Performance fixture Worker. It is never executed.\n";

const PIPELINE: &str = r#"schema = "af.pipeline/1"
name = "perf/two-node"
version = "1.0.0"
max_attempts = 3
max_parallel = 1

[contract.inputs.requirements]
artifact_type = "af/Requirements@1"
cardinality = "one"
optional = false
covers = []

[contract.inputs.requirements.affinity]
kind = "unbound"

[contract.outputs.document]
artifact_type = "af/CheckedDocument@1"
cardinality = "one"
optional = false
covers = ["checked"]

[contract.outputs.document.affinity]
kind = "unbound"

[accepts]
kinds = ["document"]

[accepts.required_facts]

[slots.author]
worker = "perf/author"
role = "author"
input_type = "af/Requirements@1"
output_type = "af/CheckedDocument@1"
min_attempts = 1
max_attempts = 1
allow_local_replacement = false
independent_from = []

[slots.checker]
worker = "perf/checker"
role = "check"
input_type = "af/CheckedDocument@1"
output_type = "af/CheckedDocument@1"
min_attempts = 1
max_attempts = 1
allow_local_replacement = false
independent_from = []

[[nodes]]
id = "draft"

[nodes.operator]
op = "worker"
slot = "author"

[nodes.inputs.input]
kind = "input"
port = "requirements"

[[nodes]]
id = "check"

[nodes.operator]
op = "worker"
slot = "checker"

[nodes.inputs.input]
kind = "node"
node = "draft"
port = "output"

[outputs.document]
kind = "node"
node = "check"
port = "output"

[coverage.checked]
kind = "node"
node = "check"
port = "output"
"#;

/// Deterministic padding: `digits` bytes `i % 10`, then `tens` bytes of value 10. Each digit
/// byte adds two canonical characters (`,d`) and each ten three (`,10`).
pub fn padding(digits: usize, tens: usize) -> Vec<u8> {
    let mut bytes: Vec<u8> = (0..digits).map(|index| (index % 10) as u8).collect();
    bytes.extend(std::iter::repeat_n(10_u8, tens));
    bytes
}

fn worker_files(worker: &str) -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([
        ("worker.toml".into(), worker.as_bytes().to_vec()),
        ("input.schema.json".into(), INPUT_SCHEMA.as_bytes().to_vec()),
        (
            "outputs/output.schema.json".into(),
            OUTPUT_SCHEMA.as_bytes().to_vec(),
        ),
        ("instructions.md".into(), INSTRUCTIONS.as_bytes().to_vec()),
    ])
}

/// The `perf/author` package files; `padding` adds the size-setting file when present.
pub fn author_files(padding_bytes: Option<Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    let mut files = worker_files(AUTHOR_WORKER);
    if let Some(bytes) = padding_bytes {
        files.insert(PADDING_PATH.into(), bytes);
    }
    files
}

pub fn checker_files() -> BTreeMap<String, Vec<u8>> {
    worker_files(CHECKER_WORKER)
}

pub fn pipeline_files() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::from([("pipeline.toml".into(), PIPELINE.as_bytes().to_vec())])
}

/// The fixed sentinel value: known ASCII keys and admitted integers only, so its canonical
/// form has one independent spelling.
pub fn sentinel_value() -> Value {
    json!({
        "kind": "af-perf-sentinel",
        "items": (0..64).map(|index| index * 977).collect::<Vec<u64>>(),
        "label": "baseline versus baseline health workload",
        "nested": {"b": [true, false, null], "a": 1}
    })
}

/// The registry's `health.batch_repetitions`.
pub const SENTINEL_REPETITIONS: u64 = 64;

/// File `index` of a deterministic source tree: its repository path and exactly 128 bytes.
pub fn source_file(index: usize) -> (String, Vec<u8>) {
    let path = format!("src/d{:03}/f{:05}.txt", index / 100, index);
    let head = format!("af performance source fixture file {index:05}\n");
    let mut bytes = head.into_bytes();
    let mut fill = 0_usize;
    while bytes.len() < 127 {
        bytes.push(b'a' + ((index + fill) % 26) as u8);
        fill += 1;
    }
    bytes.push(b'\n');
    (path, bytes)
}

pub const SOURCE_FILE_BYTES: usize = 128;

/// The persistence diagnostic's object: 1 MiB of `(i * 31 + 7) mod 251`.
pub const PERSISTENCE_BLOB_BYTES: usize = 1024 * 1024;

pub fn persistence_blob() -> Vec<u8> {
    (0..PERSISTENCE_BLOB_BYTES)
        .map(|index| ((index * 31 + 7) % 251) as u8)
        .collect()
}

/// Lease renewals the state-append diagnostic writes and reads back.
pub const PERSISTENCE_EVENTS: u64 = 100;

/// Exactly this many valid relevant events in the state projection fixture's Task log.
pub const PROJECTION_EVENTS: u64 = 10_000;

/// Exactly this many valid relevant events in the CLI state fixture's Task log
/// (`fixtures/performance/cli.toml`, registry fixture `cli_state_10000`).
pub const CLI_STATE_EVENTS: u64 = 10_000;

/// Exactly this many valid relevant events in the CLI Campaign fixture's Campaign log
/// (`fixtures/performance/cli.toml`, seed `campaign_state`).
pub const CLI_CAMPAIGN_EVENTS: u64 = 10_000;

/// The CLI Campaign fixture's label: the side's own `af review run --campaign` wrote it, and
/// its log is the Campaign run `campaign-<label>` beside the review's own Task log.
pub const CLI_CAMPAIGN: &str = "perf-campaign";

/// The Findings the fixture's reviewer reports in its one Round, one per line of
/// `src/fixture.txt` in `fixtures/performance/cli/campaign-roots/repo`.
pub const CLI_CAMPAIGN_FINDINGS: usize = 100;

/// The Campaign state directory the side's `af review run` wrote, relative to the parent of the
/// writer's scratch: the setup creates the scratch beside it.
pub const CLI_CAMPAIGN_STATE: &str = "campaign";
