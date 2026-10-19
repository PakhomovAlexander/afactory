//! The segment records of design §5: one JSON object per line, each carrying `seq` (its
//! sequence in its segment, 1 for the header and one more for every record the writer appends),
//! `t` (the record kind) and `schema` (its versioned type), and the record bound.
//!
//! **Record bound.** One encoded record, keys, escaping and arrays included, is at most
//! [`RECORD_MAX_BYTES`]. Attributes are a flat map from string to string, integer, float,
//! boolean, decimal string or array of strings: at most [`MAX_ATTRIBUTES`] per record, keys at
//! most [`MAX_KEY_BYTES`], string values at most [`MAX_STRING_BYTES`], arrays at most
//! [`MAX_ARRAY_ELEMENTS`] elements and [`MAX_ARRAY_BYTES`] together. A value over its bound
//! becomes, at `content`, a blob occurrence the attribute names; at every other level it is cut
//! at the bound with a trailing [`ELLIPSIS`]. A record that would still exceed the bound drops
//! attributes from the end of the writer's order until it fits; `af.truncated_fields` counts cut
//! values and `af.dropped_fields` dropped ones, and [`COUNTER_RESERVE_BYTES`] of every record
//! are reserved for the counters so they are never dropped themselves.
//!
//! Untrusted text is redacted whole before it is bounded (ADR-0153 clause 6); typed values
//! (identities, digests, decimals, numbers, booleans, enumerations) never pass the heuristic.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::level::Level;
use crate::redact::{self, Redactor};

/// One encoded record, newline excluded, is at most this many bytes.
pub const RECORD_MAX_BYTES: usize = 16 * 1024;
/// Bytes of every record reserved for `af.truncated_fields`, `af.dropped_fields` and
/// `af.redacted`.
pub const COUNTER_RESERVE_BYTES: usize = 256;
/// Attributes per record, the counters included.
pub const MAX_ATTRIBUTES: usize = 64;
/// Bytes of one attribute key.
pub const MAX_KEY_BYTES: usize = 128;
/// Bytes of one string value.
pub const MAX_STRING_BYTES: usize = 1024;
/// Elements of one array value.
pub const MAX_ARRAY_ELEMENTS: usize = 32;
/// Bytes of one array value's elements together.
pub const MAX_ARRAY_BYTES: usize = 4096;
/// The marker a cut value ends with.
pub const ELLIPSIS: &str = "…";
/// The prefix of an attribute value that names a blob occurrence instead of holding the value.
pub const OCCURRENCE_PREFIX: &str = "af.blob_ref:";

/// The counter attribute names, never dropped.
pub const TRUNCATED_FIELDS: &str = "af.truncated_fields";
pub const DROPPED_FIELDS: &str = "af.dropped_fields";
pub const REDACTED: &str = "af.redacted";
const COUNTERS: [&str; 3] = [TRUNCATED_FIELDS, DROPPED_FIELDS, REDACTED];

/// The versioned type of each record kind.
pub mod schema {
    pub const SEGMENT: &str = "af/TelemetrySegment@1";
    pub const SPAN: &str = "af/TelemetrySpan@1";
    pub const EVENT: &str = "af/TelemetryEvent@1";
    pub const CONFIG: &str = "af/TelemetryConfig@1";
    pub const NODE: &str = "af/TelemetryNode@1";
    pub const FINDING: &str = "af/TelemetryFinding@1";
    pub const DEMAND: &str = "af/TelemetryDemand@1";
    pub const BLOB: &str = "af/TelemetryBlob@1";
    pub const BLOB_REF: &str = "af/TelemetryBlobRef@1";
    pub const PROBLEM: &str = "af/TelemetryProblem@1";
    pub const SEAL: &str = "af/TelemetrySeal@1";
}

/// A record's attributes, in the writer's order.
pub type Attributes = Map<String, Value>;

/// One segment line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Record {
    Segment(SegmentHeader),
    SpanStart(SpanStart),
    SpanEnd(SpanEnd),
    Event(Event),
    Config(ConfigPart),
    Node(Node),
    Finding(Finding),
    Demand(Demand),
    Blob(BlobObject),
    BlobRef(BlobRef),
    Problem(Problem),
    Seal(Seal),
}

/// The host facts read once per segment, at open; a fact the platform did not deliver is
/// absent, never zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub os: String,
    pub arch: String,
    pub cpus: u32,
    pub memory_bytes: Option<u64>,
    pub load1: Option<f64>,
    pub free_bytes: Option<u64>,
}

/// `segment`, `af/TelemetrySegment@1`: the first line of every segment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentHeader {
    pub seq: u64,
    pub schema: String,
    pub segment_id: String,
    pub af_version: String,
    pub af_commit: String,
    pub pid: u32,
    pub parent_pid: u32,
    /// Redacted, each argument judged whole.
    pub argv: Vec<String>,
    /// Home-relative.
    pub cwd: String,
    pub host: Host,
    pub started_unix_ns: String,
    /// The unix time of the monotonic clock's origin: a monotonic reading plus this offset is
    /// a unix time.
    pub mono_origin_unix_ns: String,
    pub level: Level,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    Internal,
    Process,
    Client,
}

impl SpanKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Process => "process",
            Self::Client => "client",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatus {
    Ok,
    Error,
    Unset,
}

impl SpanStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Unset => "unset",
        }
    }
}

/// `span_start`, `af/TelemetrySpan@1`. `span_id` is derived from the segment id and this
/// record's `seq` ([`crate::trace::span_id`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpanStart {
    pub seq: u64,
    pub schema: String,
    /// The run's trace; absent until a span of the run names its Store and run.
    pub trace_id: Option<String>,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: SpanKind,
    pub start_unix_ns: String,
    pub attributes: Attributes,
}

/// `span_end`, `af/TelemetrySpan@1`. A recovering reader writes one with `end_unix_ns = null`
/// for every span the segment never ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpanEnd {
    pub seq: u64,
    pub schema: String,
    pub span_id: String,
    pub end_unix_ns: Option<String>,
    pub status: SpanStatus,
    pub attributes: Attributes,
}

/// `event`, `af/TelemetryEvent@1`. Its content, when it has one, is named by occurrence id
/// (`blob`), never by digest alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub seq: u64,
    pub schema: String,
    pub span_id: String,
    pub time_unix_ns: String,
    pub name: String,
    pub attributes: Attributes,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_media_type: Option<String>,
}

/// One row of the metadata-only configuration snapshot (§7.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigRow {
    pub key: String,
    pub value: String,
    pub layer: String,
    pub origin: Option<String>,
}

/// `config`, `af/TelemetryConfig@1`: one part of an execution's configuration snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigPart {
    pub seq: u64,
    pub schema: String,
    pub execution_id: String,
    pub part: u32,
    pub parts: u32,
    pub complete: bool,
    pub rows: Vec<ConfigRow>,
}

/// `node`, `af/TelemetryNode@1`: one plan node of an execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub seq: u64,
    pub schema: String,
    pub execution_id: String,
    pub plan_digest: String,
    pub node_id: String,
    pub kind: String,
    pub package: Option<String>,
    /// Up to 64 node ids, outside the attribute bounds.
    pub depends_on: Vec<String>,
    pub expected_nodes: u32,
}

/// `finding`, `af/TelemetryFinding@1`: the bounded summary of an admitted Finding (§7.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub seq: u64,
    pub schema: String,
    /// The run span the summary belongs to.
    pub span_id: String,
    pub key: String,
    pub severity: String,
    pub effective_severity: String,
    pub status: String,
    pub scope: String,
    pub file: Option<String>,
    pub line: Option<u64>,
    pub title: String,
    pub report_artifact_id: String,
    pub node_id: String,
    pub worker: String,
    pub model: Option<String>,
    pub round: Option<u64>,
    pub epoch: Option<u64>,
}

/// `demand`, `af/TelemetryDemand@1`: the bounded summary of a Demand, keyed by its id (§7.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Demand {
    pub seq: u64,
    pub schema: String,
    pub span_id: String,
    pub demand_id: String,
    pub kind: String,
    pub required: bool,
    pub subject_id: String,
    pub snapshot_id: String,
    pub status: String,
}

/// `blob`, `af/TelemetryBlob@1`: the shared object, written once per digest per segment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobObject {
    pub seq: u64,
    pub schema: String,
    /// `sha256:` and 64 hex over the retained, redacted bytes.
    pub digest: String,
    /// Retained bytes.
    pub bytes: u64,
    /// Compressed bytes on disk.
    pub stored_bytes: u64,
    pub media_type: String,
    /// The redaction version the retained bytes passed.
    pub redaction: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobState {
    Stored,
    Dropped,
}

impl BlobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stored => "stored",
            Self::Dropped => "dropped",
        }
    }
}

/// `blob_ref`, `af/TelemetryBlobRef@1`: one occurrence of content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlobRef {
    pub seq: u64,
    pub schema: String,
    /// `segment_id:seq` of this record.
    pub occurrence_id: String,
    /// The retained bytes' digest; absent when the content was dropped before it was digested.
    pub digest: Option<String>,
    pub span_id: String,
    /// The `seq` of the event that owns the content.
    pub event_seq: u64,
    /// `sha256:` over the original bytes, before redaction and truncation.
    pub source_digest: String,
    pub original_bytes: u64,
    pub truncated: bool,
    pub state: BlobState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProblemWhat {
    Write,
    Blob,
    Receiver,
    Budget,
    Redaction,
    Sink,
    Observer,
}

impl ProblemWhat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Blob => "blob",
            Self::Receiver => "receiver",
            Self::Budget => "budget",
            Self::Redaction => "redaction",
            Self::Sink => "sink",
            Self::Observer => "observer",
        }
    }
}

/// `problem`, `af/TelemetryProblem@1`: recorded once per distinct `what` and `reason` per
/// process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Problem {
    pub seq: u64,
    pub schema: String,
    pub time_unix_ns: String,
    pub what: ProblemWhat,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SealExit {
    Ok,
    Error,
    Signal,
    Recovered,
}

impl SealExit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Signal => "signal",
            Self::Recovered => "recovered",
        }
    }
}

/// `seal`, `af/TelemetrySeal@1`: the last line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Seal {
    pub seq: u64,
    pub schema: String,
    pub ended_unix_ns: String,
    pub exit: SealExit,
    pub spans: u64,
    pub events: u64,
    pub blobs: u64,
    /// Records and blobs the writer did not keep.
    pub dropped: u64,
    pub dropped_chunks: u64,
    /// Bytes a recovering reader discarded after the last complete valid line.
    pub discarded_bytes: u64,
    /// The file name of the segment's `.refs` manifest.
    pub refs: String,
}

impl Record {
    pub fn seq(&self) -> u64 {
        match self {
            Self::Segment(r) => r.seq,
            Self::SpanStart(r) => r.seq,
            Self::SpanEnd(r) => r.seq,
            Self::Event(r) => r.seq,
            Self::Config(r) => r.seq,
            Self::Node(r) => r.seq,
            Self::Finding(r) => r.seq,
            Self::Demand(r) => r.seq,
            Self::Blob(r) => r.seq,
            Self::BlobRef(r) => r.seq,
            Self::Problem(r) => r.seq,
            Self::Seal(r) => r.seq,
        }
    }

    pub fn set_seq(&mut self, seq: u64) {
        match self {
            Self::Segment(r) => r.seq = seq,
            Self::SpanStart(r) => r.seq = seq,
            Self::SpanEnd(r) => r.seq = seq,
            Self::Event(r) => r.seq = seq,
            Self::Config(r) => r.seq = seq,
            Self::Node(r) => r.seq = seq,
            Self::Finding(r) => r.seq = seq,
            Self::Demand(r) => r.seq = seq,
            Self::Blob(r) => r.seq = seq,
            Self::BlobRef(r) => r.seq = seq,
            Self::Problem(r) => r.seq = seq,
            Self::Seal(r) => r.seq = seq,
        }
    }

    /// The record kind, as `t` spells it.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Segment(_) => "segment",
            Self::SpanStart(_) => "span_start",
            Self::SpanEnd(_) => "span_end",
            Self::Event(_) => "event",
            Self::Config(_) => "config",
            Self::Node(_) => "node",
            Self::Finding(_) => "finding",
            Self::Demand(_) => "demand",
            Self::Blob(_) => "blob",
            Self::BlobRef(_) => "blob_ref",
            Self::Problem(_) => "problem",
            Self::Seal(_) => "seal",
        }
    }

    /// The versioned type a record of this kind carries.
    pub fn expected_schema(&self) -> &'static str {
        match self {
            Self::Segment(_) => schema::SEGMENT,
            Self::SpanStart(_) | Self::SpanEnd(_) => schema::SPAN,
            Self::Event(_) => schema::EVENT,
            Self::Config(_) => schema::CONFIG,
            Self::Node(_) => schema::NODE,
            Self::Finding(_) => schema::FINDING,
            Self::Demand(_) => schema::DEMAND,
            Self::Blob(_) => schema::BLOB,
            Self::BlobRef(_) => schema::BLOB_REF,
            Self::Problem(_) => schema::PROBLEM,
            Self::Seal(_) => schema::SEAL,
        }
    }

    fn schema_field(&self) -> &str {
        match self {
            Self::Segment(r) => &r.schema,
            Self::SpanStart(r) => &r.schema,
            Self::SpanEnd(r) => &r.schema,
            Self::Event(r) => &r.schema,
            Self::Config(r) => &r.schema,
            Self::Node(r) => &r.schema,
            Self::Finding(r) => &r.schema,
            Self::Demand(r) => &r.schema,
            Self::Blob(r) => &r.schema,
            Self::BlobRef(r) => &r.schema,
            Self::Problem(r) => &r.schema,
            Self::Seal(r) => &r.schema,
        }
    }

    fn attributes_mut(&mut self) -> Option<&mut Attributes> {
        match self {
            Self::SpanStart(r) => Some(&mut r.attributes),
            Self::SpanEnd(r) => Some(&mut r.attributes),
            Self::Event(r) => Some(&mut r.attributes),
            _ => None,
        }
    }

    /// A record this release writes: its `schema` is the one its kind carries, every
    /// timestamp a canonical decimal, every digest a digest. A record under another schema
    /// version is refused, never migrated (ADR-0145).
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_field() != self.expected_schema() {
            return Err(format!(
                "a `{}` record of schema `{}`, not `{}`",
                self.kind(),
                self.schema_field(),
                self.expected_schema()
            ));
        }
        if self.seq() == 0 {
            return Err("seq 0".into());
        }
        let decimal = |name: &str, value: &str| {
            if redact::is_canonical_decimal(value) {
                Ok(())
            } else {
                Err(format!("`{name}` is not a decimal timestamp"))
            }
        };
        let digest = |name: &str, value: &str| {
            if redact::is_digest(value) {
                Ok(())
            } else {
                Err(format!("`{name}` is not a digest"))
            }
        };
        match self {
            Self::Segment(r) => {
                decimal("started_unix_ns", &r.started_unix_ns)?;
                decimal("mono_origin_unix_ns", &r.mono_origin_unix_ns)?;
            }
            Self::SpanStart(r) => decimal("start_unix_ns", &r.start_unix_ns)?,
            Self::SpanEnd(r) => {
                if let Some(end) = &r.end_unix_ns {
                    decimal("end_unix_ns", end)?;
                }
            }
            Self::Event(r) => decimal("time_unix_ns", &r.time_unix_ns)?,
            Self::Blob(r) => digest("digest", &r.digest)?,
            Self::BlobRef(r) => {
                if let Some(value) = &r.digest {
                    digest("digest", value)?;
                }
                digest("source_digest", &r.source_digest)?;
                if r.state == BlobState::Stored && r.digest.is_none() {
                    return Err("a stored occurrence without a digest".into());
                }
            }
            Self::Problem(r) => decimal("time_unix_ns", &r.time_unix_ns)?,
            Self::Seal(r) => decimal("ended_unix_ns", &r.ended_unix_ns)?,
            Self::Config(_) | Self::Node(_) | Self::Finding(_) | Self::Demand(_) => {}
        }
        Ok(())
    }

    /// Parse and [`Self::validate`] one line (without its newline).
    pub fn parse_line(line: &[u8]) -> Result<Self, String> {
        let record: Record = serde_json::from_slice(line).map_err(|error| error.to_string())?;
        record.validate()?;
        Ok(record)
    }
}

// ------------------------------------------------------------------------------------------
// attributes

/// One attribute value as the writer hands it over.
#[derive(Debug, Clone, PartialEq)]
pub enum Attr {
    /// Untrusted text: judged whole by the credential rules, then bounded.
    Text(String),
    /// A kernel identity or digest the kernel validated; exempt from the heuristic. A value
    /// that does not validate as one ([`redact::is_identity`]) is treated as text.
    Id(String),
    /// A value from a closed enumeration.
    Enum(&'static str),
    /// An exact counter as a canonical decimal string (`DecimalU128`, ADR-0078). A value that is
    /// not one is dropped, never recorded as text.
    Decimal(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    /// Untrusted texts, each judged whole.
    Texts(Vec<String>),
}

/// Attributes in the writer's order: the bound drops from the end.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Attrs(pub Vec<(String, Attr)>);

impl Attrs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(mut self, key: &str, value: Attr) -> Self {
        self.0.push((key.to_string(), value));
        self
    }

    pub fn text(self, key: &str, value: impl Into<String>) -> Self {
        self.push(key, Attr::Text(value.into()))
    }

    pub fn id(self, key: &str, value: impl Into<String>) -> Self {
        self.push(key, Attr::Id(value.into()))
    }

    pub fn enumerated(self, key: &str, value: &'static str) -> Self {
        self.push(key, Attr::Enum(value))
    }

    pub fn decimal(self, key: &str, value: impl Into<String>) -> Self {
        self.push(key, Attr::Decimal(value.into()))
    }

    pub fn int(self, key: &str, value: i64) -> Self {
        self.push(key, Attr::Int(value))
    }

    pub fn float(self, key: &str, value: f64) -> Self {
        self.push(key, Attr::Float(value))
    }

    pub fn boolean(self, key: &str, value: bool) -> Self {
        self.push(key, Attr::Bool(value))
    }

    pub fn texts(self, key: &str, values: Vec<String>) -> Self {
        self.push(key, Attr::Texts(values))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// What bounding did to a record's attributes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub truncated: u64,
    pub dropped: u64,
    pub redacted: u64,
}

/// Cut `text` to at most `max` bytes on a character boundary, the marker included.
pub fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max.saturating_sub(ELLIPSIS.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ELLIPSIS}", &text[..end])
}

/// Bound one untrusted text value: redacted whole, then over [`MAX_STRING_BYTES`] either named
/// as a blob occurrence by `oversized` (at `content`) or cut.
fn bound_text(
    key: &str,
    text: &str,
    redactor: &Redactor,
    counters: &mut Counters,
    oversized: &mut dyn FnMut(&str, &str) -> Option<String>,
) -> String {
    let (value, withheld) = redactor.value(text);
    if withheld {
        counters.redacted += 1;
    }
    if value.len() <= MAX_STRING_BYTES {
        return value;
    }
    if let Some(occurrence) = oversized(key, &value) {
        return format!("{OCCURRENCE_PREFIX}{occurrence}");
    }
    counters.truncated += 1;
    cut(&value, MAX_STRING_BYTES)
}

/// Bound a writer's attributes: redact untrusted text whole, validate typed values, apply the
/// per-value bounds and the count bound. `oversized` is offered each string value over its bound
/// and returns the occurrence id of a blob that now holds it, or `None` to cut it; outside
/// `content` the writer passes a function that always returns `None`.
pub fn bound_attributes(
    attrs: Attrs,
    redactor: &Redactor,
    oversized: &mut dyn FnMut(&str, &str) -> Option<String>,
) -> (Attributes, Counters) {
    let mut counters = Counters::default();
    let mut out = Attributes::new();
    let room = MAX_ATTRIBUTES - COUNTERS.len();
    for (key, value) in attrs.0 {
        if key.len() > MAX_KEY_BYTES || COUNTERS.contains(&key.as_str()) || out.len() >= room {
            counters.dropped += 1;
            continue;
        }
        let value = match value {
            Attr::Text(text) => {
                Value::String(bound_text(&key, &text, redactor, &mut counters, oversized))
            }
            Attr::Id(id) if redact::is_identity(&id) => Value::String(id),
            Attr::Id(text) => {
                Value::String(bound_text(&key, &text, redactor, &mut counters, oversized))
            }
            Attr::Enum(value) => Value::String(value.to_string()),
            Attr::Decimal(value) if redact::is_canonical_decimal(&value) => Value::String(value),
            Attr::Decimal(_) => {
                counters.dropped += 1;
                continue;
            }
            Attr::Int(value) => Value::from(value),
            Attr::Float(value) => match serde_json::Number::from_f64(value) {
                Some(number) => Value::Number(number),
                None => {
                    counters.dropped += 1;
                    continue;
                }
            },
            Attr::Bool(value) => Value::Bool(value),
            Attr::Texts(values) => {
                let mut kept = Vec::new();
                let mut bytes = 0;
                let mut cut_any = false;
                for (index, text) in values.iter().enumerate() {
                    let (value, withheld) = redactor.value(text);
                    if withheld {
                        counters.redacted += 1;
                    }
                    let value = if value.len() > MAX_STRING_BYTES {
                        cut_any = true;
                        cut(&value, MAX_STRING_BYTES)
                    } else {
                        value
                    };
                    // Room for the marker element when more follow.
                    let last = index + 1 == values.len();
                    let reserve = if last { 0 } else { ELLIPSIS.len() };
                    if kept.len() + usize::from(!last) >= MAX_ARRAY_ELEMENTS
                        || bytes + value.len() + reserve > MAX_ARRAY_BYTES
                    {
                        kept.push(ELLIPSIS.to_string());
                        cut_any = true;
                        break;
                    }
                    bytes += value.len();
                    kept.push(value);
                }
                if cut_any {
                    counters.truncated += 1;
                }
                Value::Array(kept.into_iter().map(Value::String).collect())
            }
        };
        out.insert(key, value);
    }
    (out, counters)
}

/// Encode one record within [`RECORD_MAX_BYTES`]: attributes are dropped from the end of the
/// writer's order until it fits, and the counters (with `counters` added) are written last.
/// `None` when the record does not fit even without attributes; the writer then counts it
/// dropped.
pub fn encode(record: &mut Record, mut counters: Counters) -> Option<Vec<u8>> {
    let mut line = serde_json::to_vec(record).expect("a record serializes");
    if line.len() <= RECORD_MAX_BYTES && counters == Counters::default() {
        return Some(line);
    }
    if let Some(attributes) = record.attributes_mut() {
        for name in COUNTERS {
            attributes.shift_remove(name);
        }
        line = serde_json::to_vec(record).expect("a record serializes");
        while line.len() > RECORD_MAX_BYTES - COUNTER_RESERVE_BYTES {
            let attributes = record.attributes_mut().expect("checked above");
            let last = attributes.keys().next_back().cloned()?;
            attributes.shift_remove(&last);
            counters.dropped += 1;
            line = serde_json::to_vec(record).expect("a record serializes");
        }
        let attributes = record.attributes_mut().expect("checked above");
        for (name, count) in [
            (TRUNCATED_FIELDS, counters.truncated),
            (DROPPED_FIELDS, counters.dropped),
            (REDACTED, counters.redacted),
        ] {
            if count > 0 {
                attributes.insert(name.to_string(), Value::from(count));
            }
        }
        line = serde_json::to_vec(record).expect("a record serializes");
    }
    (line.len() <= RECORD_MAX_BYTES).then_some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span_start(attributes: Attributes) -> Record {
        Record::SpanStart(SpanStart {
            seq: 2,
            schema: schema::SPAN.into(),
            trace_id: None,
            span_id: "0011223344556677".into(),
            parent_span_id: None,
            name: "af.command".into(),
            kind: SpanKind::Internal,
            start_unix_ns: "1".into(),
            attributes,
        })
    }

    #[test]
    fn a_record_round_trips_with_its_kind_and_schema() {
        let record = span_start(Attributes::new());
        let line = serde_json::to_string(&record).unwrap();
        assert!(line.contains("\"t\":\"span_start\""), "{line}");
        assert!(line.contains("\"seq\":2"), "{line}");
        assert_eq!(Record::parse_line(line.as_bytes()).unwrap(), record);
        let foreign = line.replace(schema::SPAN, "af/TelemetrySpan@2");
        assert!(Record::parse_line(foreign.as_bytes()).is_err());
        let unknown = line.replace("\"name\"", "\"extra\":1,\"name\"");
        assert!(Record::parse_line(unknown.as_bytes()).is_err());
    }

    #[test]
    fn typed_values_skip_the_heuristic_and_text_does_not() {
        let digest = format!("sha256:{}", "ab".repeat(32));
        let (attributes, counters) = bound_attributes(
            Attrs::new()
                .id("af.context.manifest", digest.clone())
                .text("af.failure.reason", digest.clone())
                .decimal(
                    "af.usage.input_tokens",
                    "340282366920938463463374607431768211455",
                )
                .decimal("af.usage.output_tokens", "1.5"),
            &Redactor::new(),
            &mut |_, _| None,
        );
        assert_eq!(attributes["af.context.manifest"], digest.as_str());
        assert_eq!(attributes["af.failure.reason"], redact::WITHHELD);
        assert_eq!(
            attributes["af.usage.input_tokens"],
            "340282366920938463463374607431768211455"
        );
        assert!(!attributes.contains_key("af.usage.output_tokens"));
        assert_eq!(counters.redacted, 1);
        assert_eq!(counters.dropped, 1);
    }

    #[test]
    fn cutting_respects_character_boundaries() {
        let text = "é".repeat(600);
        let cut = cut(&text, MAX_STRING_BYTES);
        assert!(cut.len() <= MAX_STRING_BYTES);
        assert!(cut.ends_with(ELLIPSIS));
    }
}
