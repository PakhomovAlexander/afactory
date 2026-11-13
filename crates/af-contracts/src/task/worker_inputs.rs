//! Pure projection from invocation provenance to rendered Worker ports.
use std::collections::{BTreeMap, BTreeSet};

/// Apply selections in invocation order. The caller supplies captured selection facts during
/// planning, or checked artifacts during rendering; this function grants no selection authority.
/// `T` can retain symbolic presence instead of inventing artifact payloads for planning.
pub fn project<T>(
    mut inputs: BTreeMap<String, T>,
    development_warm: Option<T>,
    knowledge: Option<T>,
    replaced: &BTreeSet<String>,
) -> BTreeMap<String, T> {
    if let Some(warm) = development_warm {
        inputs.insert(super::development::DEVELOPMENT_WARM_PORT.into(), warm);
    }
    if let Some(context) = knowledge {
        inputs.insert(super::knowledge::KNOWLEDGE_PORT.into(), context);
    }
    inputs.retain(|name, _| !replaced.contains(name));
    inputs
}
