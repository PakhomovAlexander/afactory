use super::*;

/// Bounds no fixture below comes near, for walks that are not about bounds.
const BOUNDS: PackageBounds = PackageBounds {
    files: 4096,
    bytes: 16 * 1024 * 1024,
};

/// A package `pkg` beside an `outside` directory holding bytes no package may contain.
fn package() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("pkg");
    std::fs::create_dir_all(root.join("prompts")).unwrap();
    std::fs::write(root.join("reviewer.toml"), "name = \"pkg\"\n").unwrap();
    std::fs::write(root.join("prompts/review.md"), "review\n").unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("reviewer.toml"), "name = \"outside\"\n").unwrap();
    std::fs::write(outside.join("review.md"), "outside\n").unwrap();
    (dir, root, outside)
}

/// Walk `root`, swapping the entry at `target` for whatever `swap` makes of it after the entry
/// was classified and before it is opened: the window a check-then-open walk leaves.
fn race(
    root: &Path,
    target: &Path,
    swap: impl Fn(&Path),
) -> Result<BTreeMap<String, Vec<u8>>, LockError> {
    let mut swapped = false;
    let result = collect_no_follow("pkg", root, RootLink::Refuse, None, &mut |path| {
        if path == target && !swapped {
            swapped = true;
            swap(path);
        }
    });
    assert!(swapped, "the walk never reached {}", target.display());
    result
}

fn to_symlink(target: PathBuf) -> impl Fn(&Path) {
    move |path: &Path| {
        if path.is_dir() {
            std::fs::remove_dir_all(path).unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
        std::os::unix::fs::symlink(&target, path).unwrap();
    }
}

#[test]
fn the_walk_reads_every_regular_file_by_relative_path() {
    let (_dir, root, _) = package();
    let files = package_files("pkg", &root, BOUNDS).unwrap();
    assert_eq!(
        files,
        BTreeMap::from([
            ("prompts/review.md".to_string(), b"review\n".to_vec()),
            ("reviewer.toml".to_string(), b"name = \"pkg\"\n".to_vec()),
        ])
    );
    assert_eq!(
        package_digest("pkg", &root).unwrap(),
        package_digest_from_files(&files)
    );
}

#[test]
fn a_file_swapped_for_a_symlink_after_its_check_is_refused_not_read() {
    let (_dir, root, outside) = package();
    let target = root.join("reviewer.toml");
    let error = race(&root, &target, to_symlink(outside.join("reviewer.toml"))).unwrap_err();
    assert!(
        matches!(&error, LockError::Symlink { path, .. } if *path == target),
        "{error}"
    );
}

#[test]
fn a_directory_swapped_for_a_symlink_after_its_check_is_refused_not_walked() {
    let (_dir, root, outside) = package();
    let target = root.join("prompts");
    let error = race(&root, &target, to_symlink(outside.clone())).unwrap_err();
    assert!(
        matches!(&error, LockError::Symlink { path, .. } if *path == target),
        "{error}"
    );
}

#[test]
fn an_entry_swapped_for_an_outside_object_is_refused_not_read() {
    let (_dir, root, outside) = package();
    let target = root.join("reviewer.toml");
    let moved = outside.join("reviewer.toml");
    let error = race(&root, &target, |path| {
        std::fs::rename(&moved, path).unwrap();
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("was replaced while the package was read"),
        "{error}"
    );

    let (_dir, root, outside) = package();
    let target = root.join("prompts");
    let error = race(&root, &target, |path| {
        std::fs::remove_dir_all(path).unwrap();
        std::fs::rename(&outside, path).unwrap();
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("was replaced while the package was read"),
        "{error}"
    );
}

#[test]
fn a_file_swapped_for_a_fifo_is_refused_without_blocking() {
    let (_dir, root, _) = package();
    let target = root.join("reviewer.toml");
    let error = race(&root, &target, |path| {
        std::fs::remove_file(path).unwrap();
        nix::unistd::mkfifo(path, nix::sys::stat::Mode::from_bits_truncate(0o600)).unwrap();
    })
    .unwrap_err();
    assert!(
        matches!(&error, LockError::UnsupportedFileType { path, .. } if *path == target),
        "{error}"
    );
}

#[test]
fn a_symlinked_root_is_followed_by_package_digest_and_refused_for_a_named_package() {
    let (dir, root, _) = package();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&root, &link).unwrap();
    assert_eq!(
        package_digest("pkg", &link).unwrap(),
        package_digest("pkg", &root).unwrap()
    );
    assert!(matches!(
        package_files("pkg", &link, BOUNDS),
        Err(LockError::Symlink { .. })
    ));
}

/// `link/`, `link/.` and their repeated spellings name the link, not its target: the trailing
/// separator or `.` must not turn the root's last component into the directory it points at.
#[test]
fn a_symlinked_root_spelled_with_a_trailing_separator_or_dot_is_still_refused() {
    let (dir, root, _) = package();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&root, &link).unwrap();
    let digest = package_digest("pkg", &root).unwrap();
    for suffix in ["/", "/.", "//", "/./", "/.//./.", "//.//"] {
        let mut spelled = link.clone().into_os_string();
        spelled.push(suffix);
        let error = package_files("pkg", Path::new(&spelled), BOUNDS).unwrap_err();
        assert!(
            matches!(error, LockError::Symlink { .. }),
            "link{suffix}: {error}"
        );
        let mut directory = root.clone().into_os_string();
        directory.push(suffix);
        let files = package_files("pkg", Path::new(&directory), BOUNDS).unwrap();
        assert_eq!(
            package_digest_from_files(&files),
            digest,
            "root{suffix} is the package itself"
        );
    }
}

/// Read `pkg` through a registry, running `swap` once on its root after `locate` checked it and
/// before the walk opens it.
fn registry_race(
    root: &Path,
    swap: impl Fn(&Path),
) -> Result<(PathBuf, BTreeMap<String, Vec<u8>>), LockError> {
    let registry = Registry::new(root.parent().unwrap());
    let mut swapped = false;
    let result = registry.read_with("pkg", &mut |path| {
        if path == root && !swapped {
            swapped = true;
            swap(path);
        }
    });
    assert!(swapped, "the read never reached {}", root.display());
    result
}

#[test]
fn a_registry_package_root_swapped_for_a_symlink_after_locate_is_refused_not_read() {
    let (dir, root, outside) = package();
    let moved = dir.path().join("moved");
    let error = registry_race(&root, |path| {
        std::fs::rename(path, &moved).unwrap();
        std::os::unix::fs::symlink(&outside, path).unwrap();
    })
    .unwrap_err();
    assert!(
        matches!(&error, LockError::Symlink { path, .. } if *path == root),
        "{error}"
    );
}

#[test]
fn a_registry_package_root_swapped_for_another_directory_after_locate_is_refused() {
    let (_dir, root, outside) = package();
    let error = registry_race(&root, |path| {
        std::fs::remove_dir_all(path).unwrap();
        std::fs::rename(&outside, path).unwrap();
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("was replaced while the package was read"),
        "{error}"
    );
}

#[test]
fn an_unswapped_registry_package_reads_as_the_walk_does() {
    let (dir, root, _) = package();
    let (found, files) = Registry::new(dir.path()).read("pkg").unwrap();
    assert_eq!(found, root);
    assert_eq!(files, package_files("pkg", &root, BOUNDS).unwrap());
}

#[test]
fn a_bounded_walk_refuses_an_oversized_sparse_file_before_reading_it() {
    let (_dir, root, _) = package();
    // Two GiB of holes: reading it whole would allocate two GiB before any check after the read.
    let sparse = std::fs::File::create(root.join("sparse.bin")).unwrap();
    sparse.set_len(2 * 1024 * 1024 * 1024).unwrap();
    drop(sparse);
    let error = package_files("pkg", &root, BOUNDS).unwrap_err();
    assert!(
        matches!(&error, LockError::TooLarge { files: 4096, bytes, .. } if *bytes == 16 << 20),
        "{error}"
    );
}

#[test]
fn a_bounded_walk_refuses_one_file_or_byte_past_its_bounds_and_admits_exactly_them() {
    let (_dir, root, _) = package();
    let files = package_files("pkg", &root, BOUNDS).unwrap();
    let bytes = files.values().map(Vec::len).sum::<usize>();
    let exact = PackageBounds {
        files: files.len(),
        bytes,
    };
    assert_eq!(package_files("pkg", &root, exact).unwrap(), files);
    for tight in [
        PackageBounds {
            files: files.len() - 1,
            ..exact
        },
        PackageBounds {
            bytes: bytes - 1,
            ..exact
        },
    ] {
        assert!(matches!(
            package_files("pkg", &root, tight),
            Err(LockError::TooLarge { .. })
        ));
    }
}

#[test]
fn a_file_that_grows_past_the_bounds_while_the_walk_runs_is_refused_not_truncated() {
    let (_dir, root, _) = package();
    let target = root.join("reviewer.toml");
    let mut grown = false;
    let error = collect_no_follow("pkg", &root, RootLink::Refuse, Some(BOUNDS), &mut |path| {
        if path == target && !grown {
            grown = true;
            let file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
            file.set_len(BOUNDS.bytes as u64 + 1).unwrap();
        }
    })
    .unwrap_err();
    assert!(grown);
    assert!(matches!(error, LockError::TooLarge { .. }), "{error}");
}
