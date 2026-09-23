//! Validate actual CLI outputs with the shipped schema closure.
use serde_json::Value;

#[path = "../support/schemas.rs"]
mod schemas;

pub(super) fn valid(name: &str, value: &Value) {
    schemas::valid(&schemas::validator(name), value);
}
