//! Fixture mutation must work when the Gate supplies a read-only source tree.

use std::os::unix::fs::PermissionsExt;

#[path = "support/task_cli.rs"]
#[allow(dead_code)] // This test exercises copying without initializing a fixture repository.
mod task_cli;

#[test]
fn copied_read_only_files_allow_owner_mutation_without_changing_source_or_execution_bits() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let copy = root.path().join("copy");
    std::fs::create_dir(&source).unwrap();
    let files = [
        ("worker.py", 0o444),
        ("executable", 0o555),
        ("private", 0o440),
    ];
    for (name, mode) in files {
        let path = source.join(name);
        std::fs::write(&path, b"original fixture\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    task_cli::copy_tree(&source, &copy);

    for (name, mode) in files {
        let path = copy.join(name);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            mode | 0o200,
            "only the disposable copy's owner-write bit may be added"
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"original fixture\n");
        std::fs::write(path, b"changed fixture\n").unwrap();
        let original = source.join(name);
        assert_eq!(std::fs::read(&original).unwrap(), b"original fixture\n");
        assert_eq!(
            std::fs::metadata(original).unwrap().permissions().mode() & 0o777,
            mode
        );
    }
}
