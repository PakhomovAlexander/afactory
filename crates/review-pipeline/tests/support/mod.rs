use review_config::Definition;
use review_pipeline::{Kernel, RoundAuthority};
use review_source_git::Manifest;
use review_store::{Cas, EventStore};

/// Test composition follows the same validated Subject path as production.
pub fn whole_tree_kernel<'a>(
    cas: &'a Cas,
    store: &'a mut EventStore,
    run_id: impl Into<String>,
    snapshot: Manifest,
) -> Kernel<'a> {
    let loaded = Definition::from_toml(
        r#"
version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "reviewer"
kind = "reviewer"
runner = { program = "/bin/true" }
"#,
    )
    .unwrap()
    .load()
    .unwrap();

    let context = cas.put(b"test round authority").unwrap();
    let authority =
        RoundAuthority::new("round-started-test", &context, &context, &context).unwrap();
    Kernel::from_loaded(cas, store, run_id, snapshot, &loaded, authority).unwrap()
}
