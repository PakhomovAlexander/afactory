//! Subject support is enforced at the composition boundary, not only by reviewctl.

use review_config::Definition;
use review_config::lock::{Lockfile, Registry};
use review_pipeline::Kernel;
use review_source_git::Manifest;
use review_store::{Cas, EventStore};

#[test]
fn a_diff_subject_is_refused_before_execution() {
    let directory = tempfile::tempdir().unwrap();
    let cas = Cas::open(directory.path().join("cas")).unwrap();
    let mut store = EventStore::open(directory.path().join("events.sqlite")).unwrap();
    let reviewers = directory.path().join("reviewers");
    let package = reviewers.join("tester");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("reviewer.toml"),
        "name = \"tester\"\nversion = \"1.0.0\"\nsubjects = [\"diff\"]\n\n\
         [runner]\nprogram = \"codex\"\nargs = []\n",
    )
    .unwrap();
    let registry = Registry::new([&reviewers]);
    let mut lockfile = Lockfile::empty();
    lockfile
        .reviewers
        .insert("tester".into(), Lockfile::pin("tester", &registry).unwrap());
    let loaded = Definition::from_toml(
        r#"
version = 2
[subject]
kind = "diff"
[[nodes]]
id = "reviewer"
kind = "reviewer"
package = "tester"
"#,
    )
    .unwrap()
    .load_with(&lockfile, &registry)
    .unwrap();

    let result = Kernel::from_loaded(&cas, &mut store, "run", Manifest::new(vec![]), &loaded);

    assert!(matches!(result, Err(error) if error.contains("pinned Base and Change Set")));
}
