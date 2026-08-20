//! Subject support is enforced at the composition boundary, not only by reviewctl.

use review_core::SubjectKind;
use review_pipeline::Kernel;
use review_source_git::Manifest;
use review_store::{Cas, EventStore};

#[test]
fn a_diff_subject_is_refused_before_execution() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();

    let result = Kernel::for_subject(
        &cas,
        &mut store,
        "run",
        Manifest::new(vec![]),
        SubjectKind::Diff,
    );

    assert!(matches!(result, Err(error) if error.contains("pinned Base and Change Set")));
}
