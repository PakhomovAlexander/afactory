//! Hand-written labels for serde enums stay pinned to the serde spelling, so a diagnostic that
//! uses `as_str()` can never drift from the wire form — and never needs `Debug`.

use review_core::{PortCardinality, SnapshotAffinity};

#[test]
fn port_contract_labels_match_their_serde_names() {
    for cardinality in [PortCardinality::One, PortCardinality::Many] {
        assert_eq!(
            serde_json::to_value(cardinality).unwrap(),
            serde_json::Value::String(cardinality.as_str().to_string())
        );
    }
    for affinity in [
        SnapshotAffinity::SameSubject,
        SnapshotAffinity::Unbound,
        SnapshotAffinity::Any,
    ] {
        assert_eq!(
            serde_json::to_value(affinity).unwrap(),
            serde_json::Value::String(affinity.as_str().to_string())
        );
    }
}
