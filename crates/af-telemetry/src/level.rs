//! The four levels and the `[telemetry]` table (design §8), typed and bounded.
//!
//! `[telemetry]` is machine-only like `[self]` and `[storage]`: af reads it from the built-in,
//! system, user and environment (`AF_TELEMETRY__<KEY>`) layers only. This module owns every key,
//! its default and its bound; the configuration ladder hands it the merged values, and a value
//! outside its bound refuses the load naming the key.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const KIB: u64 = 1 << 10;
const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// The largest `segment_max_bytes` accepts: no segment this release writes is longer, so a
/// reader refuses a longer one before it reads or decompresses past it.
pub const SEGMENT_MAX_BYTES_BOUND: u64 = 4 * GIB;
/// The largest `content_max_blob_bytes` accepts: no blob this release retains is longer.
pub const CONTENT_MAX_BLOB_BYTES_BOUND: u64 = 256 * MIB;

/// What Telemetry records.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Nothing: no directory, no subscriber.
    Off,
    /// Command, run, gate, check, node, Attempt, harness and sweep spans with identity and
    /// numeric attributes; failures; resource use.
    Metrics,
    /// Every span and event, the configuration record, the Store mirror and Finding summaries;
    /// sizes and digests of content; no blobs.
    #[default]
    Spans,
    /// `spans` plus content blobs.
    Content,
}

impl Level {
    pub const ALL: [Level; 4] = [Level::Off, Level::Metrics, Level::Spans, Level::Content];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Metrics => "metrics",
            Self::Spans => "spans",
            Self::Content => "content",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.as_str() == text)
    }

    /// Whether content becomes blobs at this level.
    pub fn keeps_content(self) -> bool {
        self == Self::Content
    }

    /// Whether anything is recorded at this level.
    pub fn records(self) -> bool {
        self != Self::Off
    }
}

/// Where a run's telemetry would be pushed after the result is printed. This release implements
/// no sink: only `none` loads, and `otlp`, `sink_endpoint` and `sink_headers_file` are refused as
/// not implemented, so telemetry never leaves the machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sink {
    #[default]
    None,
    Otlp,
}

impl Sink {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Otlp => "otlp",
        }
    }
}

/// The `[telemetry]` table, typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub level: Level,
    /// `None`: the state root's `telemetry/`.
    pub dir: Option<PathBuf>,
    pub max_bytes: u64,
    pub flush_bytes: u64,
    pub flush_latency_ms: u64,
    pub observer_queue_bytes: u64,
    pub segment_max_bytes: u64,
    pub content_max_blob_bytes: u64,
    pub content_attempt_max_bytes: u64,
    pub content_process_max_bytes: u64,
    pub content_native_body_max_chars: u64,
    pub content_reasoning: bool,
    pub content_tool_io: bool,
    pub content_model_bodies: bool,
    pub content_index: bool,
    pub harness_native: bool,
    pub sink: Sink,
    pub sink_endpoint: String,
    pub sink_headers_file: String,
    pub sink_timeout_ms: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            level: Level::Spans,
            dir: None,
            max_bytes: 4 * GIB,
            flush_bytes: 64 * KIB,
            flush_latency_ms: 1000,
            observer_queue_bytes: 4 * MIB,
            segment_max_bytes: 256 * MIB,
            content_max_blob_bytes: 16 * MIB,
            content_attempt_max_bytes: 512 * MIB,
            content_process_max_bytes: 64 * MIB,
            content_native_body_max_chars: 1_048_576,
            content_reasoning: true,
            content_tool_io: true,
            content_model_bodies: true,
            content_index: true,
            harness_native: false,
            sink: Sink::None,
            sink_endpoint: String::new(),
            sink_headers_file: String::new(),
            sink_timeout_ms: 5000,
        }
    }
}

/// The refusal of a sink key this release does not implement.
pub const SINK_NOT_IMPLEMENTED: &str =
    "is not implemented in this release: no telemetry sink exists and nothing leaves the machine";

/// What a key's value is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Level,
    /// An absolute path, or empty for the default.
    Dir,
    /// A byte count, `B`/`KiB`/`MiB`/`GiB`/`TiB` or an integer; `max = 0` means the
    /// `[storage] max_bytes` of the same load.
    Bytes {
        min: u64,
        max: u64,
    },
    /// An integer.
    Integer {
        min: u64,
        max: u64,
    },
    Bool,
    /// `none`; `otlp` is refused, not implemented in this release.
    Sink,
    /// Empty; an endpoint URL is refused, not implemented in this release.
    Url,
    /// Empty; a headers file is refused, not implemented in this release.
    Path,
}

/// One key of the table: its name, its built-in default as TOML and its bound.
#[derive(Debug, Clone, Copy)]
pub struct Key {
    pub name: &'static str,
    pub default: &'static str,
    pub kind: KeyKind,
}

/// Every key of `[telemetry]`, in the documented order.
pub const KEYS: [Key; 20] = [
    Key {
        name: "level",
        default: "\"spans\"",
        kind: KeyKind::Level,
    },
    Key {
        name: "dir",
        default: "\"\"",
        kind: KeyKind::Dir,
    },
    Key {
        name: "max_bytes",
        default: "\"4GiB\"",
        kind: KeyKind::Bytes {
            min: 256 * MIB,
            max: 0,
        },
    },
    Key {
        name: "flush_bytes",
        default: "\"64KiB\"",
        kind: KeyKind::Bytes {
            min: 4 * KIB,
            max: 16 * MIB,
        },
    },
    Key {
        name: "flush_latency_ms",
        default: "1000",
        kind: KeyKind::Integer {
            min: 50,
            max: 10_000,
        },
    },
    Key {
        name: "observer_queue_bytes",
        default: "\"4MiB\"",
        kind: KeyKind::Bytes {
            min: 256 * KIB,
            max: 64 * MIB,
        },
    },
    Key {
        name: "segment_max_bytes",
        default: "\"256MiB\"",
        kind: KeyKind::Bytes {
            min: 16 * MIB,
            max: SEGMENT_MAX_BYTES_BOUND,
        },
    },
    Key {
        name: "content_max_blob_bytes",
        default: "\"16MiB\"",
        kind: KeyKind::Bytes {
            min: 64 * KIB,
            max: CONTENT_MAX_BLOB_BYTES_BOUND,
        },
    },
    Key {
        name: "content_attempt_max_bytes",
        default: "\"512MiB\"",
        kind: KeyKind::Bytes {
            min: 16 * MIB,
            max: 8 * GIB,
        },
    },
    Key {
        name: "content_process_max_bytes",
        default: "\"64MiB\"",
        kind: KeyKind::Bytes {
            min: 4 * MIB,
            max: GIB,
        },
    },
    Key {
        name: "content_native_body_max_chars",
        default: "1048576",
        kind: KeyKind::Integer {
            min: 4096,
            max: 8_388_608,
        },
    },
    Key {
        name: "content_reasoning",
        default: "true",
        kind: KeyKind::Bool,
    },
    Key {
        name: "content_tool_io",
        default: "true",
        kind: KeyKind::Bool,
    },
    Key {
        name: "content_model_bodies",
        default: "true",
        kind: KeyKind::Bool,
    },
    Key {
        name: "content_index",
        default: "true",
        kind: KeyKind::Bool,
    },
    Key {
        name: "harness_native",
        default: "false",
        kind: KeyKind::Bool,
    },
    Key {
        name: "sink",
        default: "\"none\"",
        kind: KeyKind::Sink,
    },
    Key {
        name: "sink_endpoint",
        default: "\"\"",
        kind: KeyKind::Url,
    },
    Key {
        name: "sink_headers_file",
        default: "\"\"",
        kind: KeyKind::Path,
    },
    Key {
        name: "sink_timeout_ms",
        default: "5000",
        kind: KeyKind::Integer {
            min: 100,
            max: 60_000,
        },
    },
];

/// The `[telemetry]` table with every default, as the configuration ladder's built-in layer
/// spells it.
pub fn built_in_table() -> String {
    let mut table = String::from("[telemetry]\n");
    for key in KEYS {
        table.push_str(&format!("{} = {}\n", key.name, key.default));
    }
    table
}

/// One value as the configuration ladder holds it.
#[derive(Debug, Clone, PartialEq)]
pub enum Raw {
    String(String),
    Integer(i64),
    Bool(bool),
    /// Any other TOML value (a float, an array, a table, a date).
    Other(String),
}

/// A byte count as `[storage]` and `[telemetry]` spell one: digits, optionally followed by `B`,
/// `KiB`, `MiB`, `GiB` or `TiB` (binary units), with optional spaces between.
pub fn parse_bytes(text: &str) -> Option<u64> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: u64 = number.parse().ok()?;
    let factor: u64 = match unit.trim() {
        "" | "B" => 1,
        "KiB" => 1 << 10,
        "MiB" => 1 << 20,
        "GiB" => 1 << 30,
        "TiB" => 1 << 40,
        _ => return None,
    };
    number.checked_mul(factor)
}

fn shown(raw: &Raw) -> String {
    match raw {
        Raw::String(text) => format!("{text:?}"),
        Raw::Integer(value) => value.to_string(),
        Raw::Bool(value) => value.to_string(),
        Raw::Other(text) => text.clone(),
    }
}

fn human(bytes: u64) -> String {
    for (unit, size) in [("GiB", GIB), ("MiB", MIB), ("KiB", KIB)] {
        if bytes >= size && bytes % size == 0 {
            return format!("{}{unit}", bytes / size);
        }
    }
    format!("{bytes}B")
}

impl Settings {
    /// The table from the merged values: `value(name)` is the key's value, or `None` when no
    /// layer set it (the built-in default applies). `names` are every key the table carries; an
    /// unknown one is refused. `storage_max_bytes` bounds `max_bytes`. Every refusal names its
    /// key.
    pub fn from_values(
        names: &[String],
        value: impl Fn(&str) -> Option<Raw>,
        storage_max_bytes: u64,
    ) -> Result<Self, String> {
        if let Some(unknown) = names
            .iter()
            .find(|name| !KEYS.iter().any(|key| key.name == name.as_str()))
        {
            return Err(format!(
                "[telemetry] {unknown} is not a [telemetry] key — see af help telemetry"
            ));
        }
        let mut settings = Settings::default();
        for key in KEYS {
            let Some(raw) = value(key.name) else {
                continue;
            };
            let refuse = |why: &str| format!("[telemetry] {} = {} {why}", key.name, shown(&raw));
            match key.kind {
                KeyKind::Level => {
                    settings.level = match &raw {
                        Raw::String(text) => Level::parse(text),
                        _ => None,
                    }
                    .ok_or_else(|| refuse("is not one of off, metrics, spans, content"))?;
                }
                KeyKind::Dir => match &raw {
                    Raw::String(text) if text.is_empty() => settings.dir = None,
                    Raw::String(text) if PathBuf::from(text).is_absolute() => {
                        settings.dir = Some(PathBuf::from(text));
                    }
                    _ => return Err(refuse("is not an absolute path")),
                },
                KeyKind::Bytes { min, max } => {
                    let max = if max == 0 { storage_max_bytes } else { max };
                    let bytes = match &raw {
                        Raw::Integer(value) => u64::try_from(*value).ok(),
                        Raw::String(text) => parse_bytes(text),
                        _ => None,
                    }
                    .ok_or_else(|| {
                        refuse(
                            "is not a byte count: an integer, or a number with B, KiB, MiB, GiB \
                             or TiB",
                        )
                    })?;
                    if bytes < min || bytes > max {
                        let upper = if key.name == "max_bytes" {
                            format!("[storage] max_bytes ({})", human(max))
                        } else {
                            human(max)
                        };
                        return Err(refuse(&format!(
                            "is outside its bound, {} to {upper}",
                            human(min)
                        )));
                    }
                    match key.name {
                        "max_bytes" => settings.max_bytes = bytes,
                        "flush_bytes" => settings.flush_bytes = bytes,
                        "observer_queue_bytes" => settings.observer_queue_bytes = bytes,
                        "segment_max_bytes" => settings.segment_max_bytes = bytes,
                        "content_max_blob_bytes" => settings.content_max_blob_bytes = bytes,
                        "content_attempt_max_bytes" => settings.content_attempt_max_bytes = bytes,
                        "content_process_max_bytes" => settings.content_process_max_bytes = bytes,
                        other => unreachable!("{other} is not a byte key"),
                    }
                }
                KeyKind::Integer { min, max } => {
                    let number = match &raw {
                        Raw::Integer(value) => u64::try_from(*value).ok(),
                        _ => None,
                    }
                    .ok_or_else(|| refuse("is not a non-negative integer"))?;
                    if number < min || number > max {
                        return Err(refuse(&format!("is outside its bound, {min} to {max}")));
                    }
                    match key.name {
                        "flush_latency_ms" => settings.flush_latency_ms = number,
                        "content_native_body_max_chars" => {
                            settings.content_native_body_max_chars = number;
                        }
                        "sink_timeout_ms" => settings.sink_timeout_ms = number,
                        other => unreachable!("{other} is not an integer key"),
                    }
                }
                KeyKind::Bool => {
                    let Raw::Bool(flag) = raw else {
                        return Err(refuse("is not true or false"));
                    };
                    match key.name {
                        "content_reasoning" => settings.content_reasoning = flag,
                        "content_tool_io" => settings.content_tool_io = flag,
                        "content_model_bodies" => settings.content_model_bodies = flag,
                        "content_index" => settings.content_index = flag,
                        "harness_native" => settings.harness_native = flag,
                        other => unreachable!("{other} is not a boolean key"),
                    }
                }
                // The sink keys are reserved: a configured collector that silently received
                // nothing would be worse than a refusal.
                KeyKind::Sink => {
                    settings.sink = match &raw {
                        Raw::String(text) if text == "none" => Sink::None,
                        Raw::String(text) if text == "otlp" => {
                            return Err(refuse(SINK_NOT_IMPLEMENTED));
                        }
                        _ => return Err(refuse("is not none, the only sink this release has")),
                    };
                }
                KeyKind::Url | KeyKind::Path => match &raw {
                    Raw::String(text) if text.is_empty() => {}
                    Raw::String(_) => return Err(refuse(SINK_NOT_IMPLEMENTED)),
                    _ => return Err(refuse("is not a string")),
                },
            }
        }
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn load(values: &[(&str, Raw)]) -> Result<Settings, String> {
        let map: BTreeMap<String, Raw> = values
            .iter()
            .map(|(name, raw)| ((*name).to_string(), raw.clone()))
            .collect();
        let names: Vec<String> = map.keys().cloned().collect();
        Settings::from_values(&names, |name| map.get(name).cloned(), 20 * GIB)
    }

    #[test]
    fn the_built_in_table_is_the_default() {
        let table = built_in_table();
        assert!(
            table.starts_with("[telemetry]\nlevel = \"spans\"\n"),
            "{table}"
        );
        assert_eq!(table.lines().count(), 21);
        assert_eq!(load(&[]).unwrap(), Settings::default());
    }

    #[test]
    fn every_bound_refuses_naming_its_key() {
        for (name, raw) in [
            ("level", Raw::String("verbose".into())),
            ("dir", Raw::String("relative/dir".into())),
            ("max_bytes", Raw::String("21GiB".into())),
            ("max_bytes", Raw::String("1MiB".into())),
            ("flush_bytes", Raw::String("1KiB".into())),
            ("flush_latency_ms", Raw::Integer(10)),
            ("observer_queue_bytes", Raw::String("128MiB".into())),
            ("segment_max_bytes", Raw::String("8MiB".into())),
            ("content_max_blob_bytes", Raw::String("1GiB".into())),
            ("content_attempt_max_bytes", Raw::String("9GiB".into())),
            ("content_process_max_bytes", Raw::String("2GiB".into())),
            ("content_native_body_max_chars", Raw::Integer(1)),
            ("content_reasoning", Raw::String("yes".into())),
            ("sink", Raw::String("prometheus".into())),
            ("sink_timeout_ms", Raw::Integer(60_001)),
            ("sink_endpoint", Raw::String("ftp://collector".into())),
            ("sink_headers_file", Raw::Integer(1)),
        ] {
            let error = load(&[(name, raw.clone())]).unwrap_err();
            assert!(
                error.starts_with(&format!("[telemetry] {name} = ")),
                "{name}: {error}"
            );
        }
        let error = load(&[("verbosity", Raw::Integer(1))]).unwrap_err();
        assert!(error.contains("verbosity"), "{error}");
        let settings = load(&[
            ("sink", Raw::String("none".into())),
            ("sink_endpoint", Raw::String(String::new())),
            ("sink_headers_file", Raw::String(String::new())),
            ("level", Raw::String("content".into())),
            ("segment_max_bytes", Raw::Integer(16 << 20)),
        ])
        .unwrap();
        assert_eq!(settings.level, Level::Content);
        assert_eq!(settings.sink, Sink::None);
        assert_eq!(settings.segment_max_bytes, 16 << 20);
    }

    /// No sink is implemented: `otlp`, an endpoint and a headers file are refused at load, each
    /// naming its key, alone or together, rather than accepted and silently never pushed.
    #[test]
    fn every_sink_key_is_refused_as_not_implemented() {
        for values in [
            vec![("sink", Raw::String("otlp".into()))],
            vec![(
                "sink_endpoint",
                Raw::String("https://collector:4318".into()),
            )],
            vec![(
                "sink_headers_file",
                Raw::String("/etc/af/otlp-headers".into()),
            )],
            vec![
                ("sink", Raw::String("otlp".into())),
                (
                    "sink_endpoint",
                    Raw::String("https://collector:4318".into()),
                ),
            ],
        ] {
            let error = load(&values).unwrap_err();
            assert!(error.starts_with("[telemetry] sink"), "{error}");
            assert!(error.contains("not implemented in this release"), "{error}");
        }
    }
}
