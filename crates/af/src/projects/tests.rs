use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use review_core::project::WorkspaceOriginV1;
use review_core::{EventType, RunEvent};
use serde_json::json;

use super::*;

const ROOTS: &str =
    "1111111111111111111111111111111111111111,2222222222222222222222222222222222222222";

#[test]
fn the_project_id_is_the_domain_separated_digest_of_the_repository_id() {
    let mut bytes = b"af/project-id@1\0".to_vec();
    bytes.extend_from_slice(ROOTS.as_bytes());
    let expected = format!(
        "sha256:{}",
        review_core::hex::encode(&<Sha256 as Digest>::digest(&bytes))
    );
    assert_eq!(project_id(ROOTS), expected);
    assert!(review_core::project::is_project_id(&project_id(ROOTS)));
    assert_ne!(project_id(ROOTS), project_id(&ROOTS[..40]));
}

#[test]
fn a_workspace_id_is_opaque_and_depends_on_project_and_path() {
    let project = project_id(ROOTS);
    let id = workspace_id(&project, Path::new("/src/afactory"));
    assert!(review_core::project::is_project_workspace_id(&id), "{id}");
    assert!(!id.contains("afactory"));
    assert_ne!(id, workspace_id(&project, Path::new("/src/afactory-wt-x")));
    assert_ne!(
        id,
        workspace_id(&project_id(&ROOTS[..40]), Path::new("/src/afactory"))
    );
    let mut bytes = b"af/workspace-id@1\0".to_vec();
    bytes.extend_from_slice(project.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(b"/src/afactory");
    let hex = review_core::hex::encode(&<Sha256 as Digest>::digest(&bytes));
    assert_eq!(id, format!("w-{}", &hex[..16]));
}

#[test]
fn the_store_is_named_by_the_hex_of_the_project_id() {
    let project = project_id(ROOTS);
    let store = store_directory(Path::new("/state"), &project);
    assert_eq!(
        store,
        Path::new("/state/af/project").join(project.trim_start_matches("sha256:"))
    );
}

/// The automatic label of `name`, checked against the safe-name rule and its 128-byte limit.
fn automatic(name: &str) -> String {
    let label = label_from_name(OsStr::new(name));
    assert!(label.len() <= 128, "{name}: {label}");
    assert!(
        review_core::project::is_workspace_label(&label),
        "{name}: {label}"
    );
    label
}

#[test]
fn labels_from_directory_names_are_safe_names() {
    assert_eq!(automatic("afactory"), "afactory");
    assert_eq!(automatic("_hidden"), "hidden");
    assert_eq!(automatic("..."), "checkout");
    assert_eq!(automatic(""), "checkout");
}

#[test]
fn an_automatic_label_maps_a_dot_to_a_dash() {
    assert_eq!(automatic("repo.v2"), "repo-v2");
}

#[test]
fn an_automatic_label_maps_each_space_to_a_dash() {
    assert_eq!(automatic("my repo  copy"), "my-repo--copy");
}

#[test]
fn an_automatic_label_maps_every_byte_of_a_unicode_character_to_a_dash() {
    // `é` is two bytes, `日本` six: each byte becomes `-`.
    assert_eq!(automatic("café"), "caf--");
    assert_eq!(automatic("日本-repo"), "repo");
    assert_eq!(automatic("ünïcode"), "n--code");
}

#[test]
fn an_automatic_label_strips_leading_punctuation() {
    assert_eq!(automatic(".._-hidden.repo"), "hidden-repo");
    assert_eq!(automatic("-_x"), "x");
}

#[test]
fn an_automatic_label_of_a_200_byte_name_is_cut_to_119_bytes() {
    let name = "ab.".repeat(66) + "ab";
    assert_eq!(name.len(), 200);
    let label = automatic(&name);
    assert_eq!(label.len(), 119);
    assert_eq!(label, "ab-".repeat(66)[..119]);
    // Its collision suffix still fits within 128 bytes.
    let log = Log::fold(&[event(
        0,
        EventType::WorkspaceRegisteredV1,
        registered("w-0123456789abcdef", &label, "adopted"),
    )])
    .unwrap();
    let suffixed = free_label(&log, &label, "w-fedcba9876543210").unwrap();
    assert_eq!(suffixed, format!("{label}-fedcba98"));
    assert_eq!(suffixed.len(), 128);
    assert!(review_core::project::is_workspace_label(&suffixed));
}

#[test]
fn an_automatic_label_that_collides_with_an_explicit_one_gains_the_id_prefix() {
    // `fixture` was given with --label to another checkout's `workspace add`.
    let log = Log::fold(&[event(
        0,
        EventType::WorkspaceRegisteredV1,
        registered("w-0123456789abcdef", "fixture", "adopted"),
    )])
    .unwrap();
    let label = free_label(&log, &automatic("fixture"), "w-fedcba9876543210").unwrap();
    assert_eq!(label, "fixture-fedcba98");
    assert!(label.len() <= 128 && review_core::project::is_workspace_label(&label));
    // Both taken: an error naming --label.
    let both = Log::fold(&[
        event(
            0,
            EventType::WorkspaceRegisteredV1,
            registered("w-0123456789abcdef", "fixture", "adopted"),
        ),
        event(
            1,
            EventType::WorkspaceRegisteredV1,
            registered("w-1111111111111111", "fixture-fedcba98", "created"),
        ),
    ])
    .unwrap();
    let error = free_label(&both, "fixture", "w-fedcba9876543210").unwrap_err();
    assert!(error.contains("--label"), "{error}");
}

fn event(sequence: u64, event_type: EventType, payload: serde_json::Value) -> RunEvent {
    RunEvent {
        event_id: format!("{sequence:026}"),
        run_id: "project".into(),
        sequence,
        event_type,
        occurred_at: "1970-01-01T00:00:00Z".into(),
        node_id: None,
        attempt_id: None,
        causation_id: None,
        correlation_id: None,
        artifact_refs: Vec::new(),
        payload,
    }
}

fn registered(id: &str, label: &str, origin: &str) -> serde_json::Value {
    json!({"workspace_id": id, "label": label, "origin": origin, "registered_unix_ms": "5"})
}

#[test]
fn the_log_folds_registrations_and_removals_and_refuses_foreign_events() {
    let project = json!({
        "project_id": project_id(ROOTS), "repository_id": ROOTS, "registered_unix_ms": "1"
    });
    let a = "w-000000000000000a";
    let b = "w-000000000000000b";
    let events = [
        event(0, EventType::ProjectRegisteredV1, project.clone()),
        event(
            1,
            EventType::WorkspaceRegisteredV1,
            registered(a, "main", "adopted"),
        ),
        event(
            2,
            EventType::WorkspaceRegisteredV1,
            registered(b, "research", "created"),
        ),
        event(
            3,
            EventType::WorkspaceRemovedV1,
            json!({"workspace_id": a, "removed_unix_ms": "6"}),
        ),
        event(
            4,
            EventType::WorkspaceRegisteredV1,
            registered(a, "main-again", "adopted"),
        ),
    ];
    let log = Log::fold(&events).unwrap();
    assert_eq!(log.project.as_ref().unwrap().repository_id, ROOTS);
    assert_eq!(log.workspaces.len(), 2);
    assert_eq!(log.by_label("main-again").unwrap().workspace_id, a);
    assert!(log.by_label("main").is_none());
    assert_eq!(
        log.by_label("research").unwrap().origin,
        WorkspaceOriginV1::Created
    );
    let twice = [
        event(0, EventType::ProjectRegisteredV1, project.clone()),
        event(1, EventType::ProjectRegisteredV1, project),
    ];
    assert!(Log::fold(&twice).is_err());
    let foreign = [event(
        0,
        EventType::GenerationAdvancedV1,
        json!({"round": 1}),
    )];
    assert!(Log::fold(&foreign).is_err());
}

#[test]
fn a_taken_label_gains_the_workspace_id_prefix() {
    let a = "w-0123456789abcdef";
    let events = [event(
        0,
        EventType::WorkspaceRegisteredV1,
        registered(a, "afactory", "adopted"),
    )];
    let log = Log::fold(&events).unwrap();
    assert_eq!(
        free_label(&log, "research", "w-fedcba9876543210").unwrap(),
        "research"
    );
    assert_eq!(
        free_label(&log, "afactory", "w-fedcba9876543210").unwrap(),
        "afactory-fedcba98"
    );
}

fn paths(id: &str, label: &str) -> WorkspacePaths {
    WorkspacePaths {
        id: id.into(),
        label: label.into(),
        path: PathBuf::from(format!("/src/{label}")),
        state: PathBuf::from(format!("/state/{label}")),
        bindings: (label == "b").then(|| PathBuf::from("/state/b/bindings.toml")),
        tasks: None,
        removing_entry: None,
    }
}

#[test]
fn the_workspaces_file_is_written_whole_in_label_order_and_read_back() {
    let directory = tempfile::tempdir().unwrap();
    let location = file::location(directory.path());
    assert!(file::read(&location).unwrap().is_empty());
    file::write(
        &location,
        vec![
            paths("w-000000000000000b", "b"),
            paths("w-000000000000000a", "a"),
        ],
    )
    .unwrap();
    let read = file::read(&location).unwrap();
    assert_eq!(
        read,
        vec![
            paths("w-000000000000000a", "a"),
            paths("w-000000000000000b", "b")
        ]
    );
    let text = std::fs::read_to_string(&location).unwrap();
    assert!(text.starts_with("# This machine's paths"), "{text}");
    assert!(text.contains("[[workspaces]]"), "{text}");
    // Nothing but the file is left behind: the temporary file was renamed into place.
    let names: Vec<_> = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![std::ffi::OsString::from("workspaces.toml")]);
}

#[test]
fn the_workspaces_file_refuses_another_version_unknown_keys_and_relative_paths() {
    assert!(file::parse("version = 2\n").is_err());
    assert!(file::parse("version = 1\n").unwrap().is_empty());
    assert!(file::parse("version = 1\nextra = true\n").is_err());
    let relative = "version = 1\n[[workspaces]]\nid = \"w-000000000000000a\"\nlabel = \"a\"\n\
                    path = \"src/a\"\nstate = \"/state/a\"\n";
    assert!(file::parse(relative).is_err());
    let removing = "version = 1\n[[workspaces]]\nid = \"w-000000000000000a\"\nlabel = \"a\"\n\
                    path = \"/src/a\"\nstate = \"/state/a\"\nremoving_entry = \"worktrees/a\"\n";
    assert!(file::parse(removing).is_err());
    let absolute = removing.replace("\"worktrees/a\"", "\"/src/.git/worktrees/a\"");
    assert_eq!(
        file::parse(&absolute).unwrap()[0].removing_entry,
        Some(PathBuf::from("/src/.git/worktrees/a"))
    );
}

#[test]
fn an_entry_renamed_for_removal_is_known_by_its_name_and_pid() {
    for (entry, name) in [
        ("/repo/.git/worktrees/wt.af-removing-4242", Some("wt")),
        ("/repo/.git/worktrees/wt.v2.af-removing-1", Some("wt.v2")),
        ("/repo/.git/worktrees/wt", None),
        ("/repo/.git/worktrees/wt.af-removing-", None),
        ("/repo/.git/worktrees/wt.af-removing-12x", None),
        ("/repo/.git/worktrees/.af-removing-12", None),
    ] {
        assert_eq!(
            commands::renamed_name(Path::new(entry)),
            name.map(str::as_bytes),
            "{entry}"
        );
    }
}

#[test]
fn a_store_registers_once_and_unregisters_without_touching_paths_elsewhere() {
    let home = tempfile::tempdir().unwrap();
    let checkout = Checkout {
        toplevel: PathBuf::from("/src/afactory"),
        repository_id: ROOTS.into(),
        project_id: project_id(ROOTS),
    };
    // Not `open`: the Store registry is the machine's own, and a unit test never writes it.
    let mut store = ProjectStore::open_unrecorded(home.path(), &checkout.project_id).unwrap();
    let log = store.ensure_project(&checkout).unwrap();
    let project = log.project.clone().unwrap();
    assert_eq!(project.project_id, checkout.project_id);
    assert_eq!((project.name, project.min_af), (None, None));
    // A second call records nothing new.
    assert_eq!(store.ensure_project(&checkout).unwrap(), log);
    let id = workspace_id(&checkout.project_id, &checkout.toplevel);
    let first = store
        .register(&log, WorkspaceOriginV1::Adopted, paths(&id, "afactory"))
        .unwrap();
    assert!(matches!(first, Registration::New(_)));
    let log = store.log().unwrap();
    let again = store
        .register(&log, WorkspaceOriginV1::Adopted, paths(&id, "other"))
        .unwrap();
    assert_eq!(again, Registration::Existing(first.workspace().clone()));
    // A lost paths file is repaired by the next registration of the same checkout.
    std::fs::remove_file(file::location(&store.directory)).unwrap();
    store
        .register(&log, WorkspaceOriginV1::Adopted, paths(&id, "other"))
        .unwrap();
    assert_eq!(store.paths().unwrap()[0].label, "afactory");
    store.unregister(&id).unwrap();
    assert!(store.log().unwrap().workspaces.is_empty());
    assert!(store.paths().unwrap().is_empty());
    let events = store.events.replay(RUN_ID).unwrap();
    let kinds: Vec<_> = events.iter().map(|event| event.event_type).collect();
    assert_eq!(
        kinds,
        [
            EventType::ProjectRegisteredV1,
            EventType::WorkspaceRegisteredV1,
            EventType::WorkspaceRemovedV1
        ]
    );
    // No record holds a path.
    for event in events {
        let text = event.payload.to_string();
        assert!(!text.contains("/src/"), "{text}");
        assert!(!text.contains(&home.path().display().to_string()), "{text}");
    }
}

#[test]
fn a_debug_fixture_can_only_lower_the_git_deadline() {
    let production = std::time::Duration::from_secs(30);
    assert_eq!(git::lowered(production, None), production);
    assert_eq!(
        git::lowered(production, Some("500")),
        std::time::Duration::from_millis(500)
    );
    for ignored in ["0", "60000", "-1", "soon"] {
        assert_eq!(
            git::lowered(production, Some(ignored)),
            production,
            "{ignored}"
        );
    }
}

#[test]
fn every_process_of_the_projects_modules_is_supervised() {
    let sources = [
        ("projects.rs", include_str!("../projects.rs")),
        ("projects/commands.rs", include_str!("commands.rs")),
        ("projects/file.rs", include_str!("file.rs")),
        ("projects/git.rs", include_str!("git.rs")),
        ("projects/tests.rs", include_str!("tests.rs")),
    ];
    // Spelled in pieces, so this test does not find itself.
    let unsupervised = [
        concat!(".out", "put()"),
        concat!(".sta", "tus()"),
        concat!(".spa", "wn()"),
        concat!("Command::", "new"),
    ];
    for (name, source) in sources {
        for needle in unsupervised {
            assert!(!source.contains(needle), "{name} contains `{needle}`");
        }
    }
    assert!(include_str!("git.rs").contains("run_supervised_with_policy"));
}

/// A repository with one commit, made through the supervised git alone.
fn committed_repository(root: &Path) -> (Git, String) {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = Git::open(&repo).unwrap();
    git.require(["init", "-q", "-b", "main"], None).unwrap();
    std::fs::write(repo.join("README.md"), "one\n").unwrap();
    git.require(["add", "README.md"], None).unwrap();
    git.require(
        [
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "one",
        ],
        None,
    )
    .unwrap();
    let commit = git.require(["rev-parse", "HEAD"], None).unwrap();
    (git, commit)
}

/// What `workspace new` records of the worktree it adds at `path`: the identity the directory has
/// now, or none when it does not exist, and an empty manifest, as before materialization.
fn adding(
    path: &Path,
    administrative: &Path,
    before: BTreeSet<std::ffi::OsString>,
    commit: &str,
) -> commands::Adding {
    commands::Adding {
        path: path.to_path_buf(),
        identity: std::fs::symlink_metadata(path)
            .map(|metadata| commands::identity(&metadata))
            .unwrap_or_default(),
        administrative: administrative.to_path_buf(),
        before,
        commit: commit.to_string(),
        manifest: review_source_git::Manifest::default(),
    }
}

#[test]
fn undo_removes_what_workspace_new_made_newest_first_and_names_what_remains() {
    use commands::Made;
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let (git, commit) = committed_repository(&root);
    let administrative = root.join("repo/.git/worktrees");
    let worktree_made =
        |path: PathBuf| Made::Worktree(adding(&path, &administrative, BTreeSet::new(), &commit));
    let directory = root.join("store/workspaces/spike");
    std::fs::create_dir_all(directory.join("state")).unwrap();
    let reference = "refs/heads/af/spike".to_string();
    git.require(["update-ref", &reference, &commit], None)
        .unwrap();
    let worktree = root.join("deep/er/wt");
    git.require(
        [
            "worktree",
            "add",
            "--no-checkout",
            "--",
            worktree.to_str().unwrap(),
            "af/spike",
        ],
        None,
    )
    .unwrap();
    let checkout = Checkout {
        toplevel: root.join("repo"),
        repository_id: ROOTS.into(),
        project_id: project_id(ROOTS),
    };
    let mut store =
        ProjectStore::open_unrecorded(&root.join("state"), &checkout.project_id).unwrap();
    let log = store.ensure_project(&checkout).unwrap();
    let id = workspace_id(&checkout.project_id, &worktree);
    store
        .register(&log, WorkspaceOriginV1::Created, paths(&id, "spike"))
        .unwrap();

    let report = commands::undo(
        &git,
        &mut store,
        vec![
            Made::Directory(directory.clone()),
            Made::Branch {
                reference: reference.clone(),
                commit: commit.clone(),
            },
            Made::Ancestors(vec![root.join("deep"), root.join("deep/er")]),
            Made::Worktree(adding(&worktree, &administrative, BTreeSet::new(), &commit)),
            Made::Registration(id.clone()),
        ],
    );
    assert_eq!(
        report,
        format!(
            "undone, newest first: unregistered Workspace {id}, removed the worktree {}, removed \
             the administrative entry worktrees/wt, removed the directory {}, removed the \
             directory {}, deleted the branch {reference}, removed {}; nothing was left behind",
            worktree.display(),
            root.join("deep/er").display(),
            root.join("deep").display(),
            directory.display()
        )
    );
    assert!(!root.join("deep").exists());
    assert!(!directory.exists());
    assert!(store.log().unwrap().workspaces.is_empty());
    let branch = git
        .run(["rev-parse", "--verify", "--quiet", &reference], None)
        .unwrap();
    assert!(!branch.status.success());
    let listed = git
        .require(["worktree", "list", "--porcelain"], None)
        .unwrap();
    assert_eq!(listed.matches("worktree ").count(), 1, "{listed}");

    // Directories git never created need no undo and are not reported.
    assert_eq!(
        commands::undo(
            &git,
            &mut store,
            vec![Made::Ancestors(vec![
                root.join("absent"),
                root.join("absent/below"),
            ])],
        ),
        "nothing was created"
    );

    // A worktree git never began is not reported either; a partial one, its administrative entry
    // still locked the way a stopped `worktree add` leaves it, is removed with that entry.
    assert_eq!(
        commands::undo(&git, &mut store, vec![worktree_made(root.join("never"))]),
        "nothing was created"
    );
    let partial = root.join("partial");
    let entry = root.join("repo/.git/worktrees/partial");
    std::fs::create_dir_all(&partial).unwrap();
    std::fs::create_dir_all(&entry).unwrap();
    std::fs::write(
        partial.join(".git"),
        format!("gitdir: {}\n", entry.display()),
    )
    .unwrap();
    std::fs::write(
        entry.join("gitdir"),
        format!("{}\n", partial.join(".git").display()),
    )
    .unwrap();
    std::fs::write(entry.join("locked"), "initializing\n").unwrap();
    assert_eq!(
        commands::undo(&git, &mut store, vec![worktree_made(partial.clone())]),
        format!(
            "undone, newest first: removed the worktree {}, removed the administrative entry \
             worktrees/partial; nothing was left behind",
            partial.display()
        )
    );
    assert!(!partial.exists());
    assert!(!entry.exists());
    let listed = git
        .require(["worktree", "list", "--porcelain"], None)
        .unwrap();
    assert_eq!(listed.matches("worktree ").count(), 1, "{listed}");

    // A branch that moved since it was created at `1111…` is not deleted, and the report says so.
    git.require(["update-ref", &reference, &commit], None)
        .unwrap();
    let report = commands::undo(
        &git,
        &mut store,
        vec![Made::Branch {
            reference: reference.clone(),
            commit: "1111111111111111111111111111111111111111".into(),
        }],
    );
    assert_eq!(
        report,
        format!("left behind: the branch {reference} moved since this add created it and is kept")
    );
    assert!(
        git.run(["rev-parse", "--verify", "--quiet", &reference], None)
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn an_administrative_entry_belongs_to_the_add_whose_basename_and_gitdir_it_bears() {
    let path = Path::new("/work/fixture-wt-spike");
    let own = "/work/fixture-wt-spike/.git\n";
    for (name, gitdir, owned) in [
        // Without a `gitdir` an entry may be another add's of the same basename: never this one's.
        ("fixture-wt-spike", None, false),
        ("fixture-wt-spike", Some(own), true),
        ("fixture-wt-spike1", None, false),
        ("fixture-wt-spike27", Some(own), true),
        ("fixture-wt-spike-2", None, false),
        ("fixture-wt-spikes", None, false),
        ("fixture-wt-other", None, false),
        ("fixture-wt-other", Some(own), false),
        (
            "fixture-wt-spike",
            Some("/elsewhere/fixture-wt-spike/.git\n"),
            false,
        ),
        (
            "fixture-wt-spike2",
            Some("/work/fixture-wt-spike2/.git\n"),
            false,
        ),
    ] {
        assert_eq!(
            commands::owns_entry(path, OsStr::new(name), gitdir.map(str::as_bytes)),
            owned,
            "{name} {gitdir:?}"
        );
    }
}

// A unit test only: APFS refuses non-UTF-8 file names, so no integration test creates such a path.
#[cfg(unix)]
#[test]
fn an_administrative_entry_of_a_non_utf8_path_belongs_to_its_add() {
    use std::os::unix::ffi::OsStrExt;
    let path = Path::new(OsStr::from_bytes(b"/work/fixture-wt-\xffspike"));
    let name = OsStr::from_bytes(b"fixture-wt-\xffspike");
    let own = b"/work/fixture-wt-\xffspike/.git".to_vec();
    let own_newline = [own.as_slice(), b"\n"].concat();
    for (gitdir, owned) in [
        (None, false),
        (Some(own.as_slice()), true),
        (Some(own_newline.as_slice()), true),
        (Some(b"/work/fixture-wt-\xfeother/.git\n".as_slice()), false),
    ] {
        assert_eq!(
            commands::owns_entry(path, name, gitdir),
            owned,
            "{gitdir:?}"
        );
    }
}

#[test]
fn undo_deletes_only_an_entry_whose_gitdir_names_the_path_and_reports_a_locked_one_without() {
    use commands::Made;
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let (git, commit) = committed_repository(&root);
    let mut store = ProjectStore::open_unrecorded(&root.join("state"), &project_id(ROOTS)).unwrap();
    let administrative = root.join("repo/.git/worktrees");
    let worktree = root.join("wt");
    let lock = |name: &str| {
        std::fs::create_dir_all(administrative.join(name)).unwrap();
        std::fs::write(administrative.join(name).join("locked"), "initializing\n").unwrap();
    };
    // Older than the add, and new but of another basename: neither is touched nor reported.
    for name in ["wt", "other"] {
        lock(name);
    }
    // A stale entry of another worktree, unlocked and without a `gitdir`, which `git worktree
    // prune` would remove: the undo never prunes, so it stays.
    std::fs::create_dir_all(administrative.join("stale")).unwrap();
    let before = BTreeSet::from([
        OsStr::new("wt").to_os_string(),
        OsStr::new("stale").to_os_string(),
    ]);
    // What a stopped add leaves, or a concurrent add of another path with the same basename: a
    // locked entry with neither a path nor a `gitdir`. Git neither lists nor prunes it.
    lock("wt1");
    // The same, unlocked: `git worktree prune` removes it once no add is running.
    std::fs::create_dir_all(administrative.join("wt3")).unwrap();
    // This add's own entry, its `gitdir` naming `<path>/.git`.
    lock("wt2");
    std::fs::write(
        administrative.join("wt2/gitdir"),
        format!("{}\n", worktree.join(".git").display()),
    )
    .unwrap();
    assert!(
        !git.require(["worktree", "list", "--porcelain"], None)
            .unwrap()
            .contains("wt1")
    );
    let report = commands::undo(
        &git,
        &mut store,
        vec![Made::Worktree(adding(
            &worktree,
            &administrative,
            before,
            &commit,
        ))],
    );
    assert_eq!(
        report,
        format!(
            "undone, newest first: removed the administrative entry worktrees/wt2; left behind: \
             {}: a locked entry without a gitdir, created during this add and possibly another add's; delete that directory once no add is running (git worktree prune keeps a locked entry); \
             {}: an entry without a gitdir, created during this add and possibly another add's; run git worktree prune once no add is running",
            administrative.join("wt1").display(),
            administrative.join("wt3").display()
        )
    );
    assert!(!report.contains("nothing was left behind"), "{report}");
    assert!(!administrative.join("wt2").exists());
    assert!(administrative.join("wt1/locked").is_file());
    assert!(administrative.join("wt3").is_dir());
    assert!(administrative.join("wt/locked").is_file());
    assert!(administrative.join("other/locked").is_file());
    assert!(administrative.join("stale").is_dir(), "the undo pruned");
}
