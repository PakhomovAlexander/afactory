//! The telemetry crate's integration fixtures (design §15 P1, §16 items 3, 5 and 7).

mod blobs;
mod budget;
mod config;
mod consumer;
mod content;
mod dirs;
mod history;
mod identity;
mod index;
mod redaction;
mod schemas;
mod segments;
mod sources;
mod streams;
mod subscriber;

use std::path::{Path, PathBuf};

use review_telemetry::level::Level;
use review_telemetry::record::Record;
use review_telemetry::redact::Redactor;
use review_telemetry::segment::writer::{Writer, WriterOptions};

/// A writer below `root` with a fixed argv and working directory and a small buffer.
pub fn options(root: &Path, level: Level) -> WriterOptions {
    let mut options = WriterOptions::new(root, level);
    options.argv = vec!["af".into(), "task".into(), "run".into()];
    options.cwd = PathBuf::from("/work");
    options.flush_bytes = 4096;
    options.af_commit = "0123456789abcdef".into();
    options.redactor = Redactor::new();
    options
}

/// The owner of a fixture's configuration parts: the execution `execution_id`, of a run the
/// fixture does not otherwise name.
pub fn config_owner(execution_id: &str) -> review_telemetry::segment::writer::ConfigOwner<'_> {
    review_telemetry::segment::writer::ConfigOwner {
        execution_id,
        run_id: "run-1",
        run_span_id: "0000000000000001",
    }
}

pub fn writer(root: &Path, level: Level) -> Writer {
    Writer::new(options(root, level)).expect("a level that records")
}

/// Every record of a sealed segment's file.
pub fn sealed_records(path: &Path) -> Vec<Record> {
    review_telemetry::segment::reader::read_sealed(path).unwrap()
}

/// The decompressed bytes of a sealed segment.
pub fn sealed_bytes(path: &Path) -> Vec<u8> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap())
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

/// Every line of a sealed segment, as JSON.
pub fn sealed_lines(path: &Path) -> Vec<serde_json::Value> {
    use std::io::Read as _;
    let mut text = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap())
        .read_to_string(&mut text)
        .unwrap();
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
