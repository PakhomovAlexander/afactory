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

#[test]
fn labels_from_directory_names_are_safe_names() {
    assert_eq!(label_from_name("afactory"), "afactory");
    assert_eq!(label_from_name("my.repo"), "my-repo");
    assert_eq!(label_from_name("_hidden"), "hidden");
    assert_eq!(label_from_name("..."), "checkout");
    assert_eq!(label_from_name(&"a".repeat(300)).len(), 119);
    for name in ["afactory", "my.repo", "_hidden", "...", "ünïcode dir"] {
        assert!(
            review_core::project::is_workspace_label(&label_from_name(name)),
            "{name}"
        );
    }
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

#[test]
fn undo_removes_what_workspace_new_made_newest_first_and_names_what_remains() {
    use commands::Made;
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let (git, commit) = committed_repository(&root);
    let administrative = root.join("repo/.git/worktrees");
    let worktree_made = |path: PathBuf| Made::Worktree {
        path,
        administrative: administrative.clone(),
        before: BTreeSet::new(),
    };
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
            Made::Ancestors {
                from: root.join("deep"),
                to: root.join("deep/er"),
            },
            Made::Worktree {
                path: worktree.clone(),
                administrative: administrative.clone(),
                before: BTreeSet::new(),
            },
            Made::Registration(id.clone()),
        ],
    );
    assert_eq!(
        report,
        format!(
            "undone, newest first: unregistered Workspace {id}, removed the worktree {}, removed \
             the administrative entry worktrees/wt, removed the directory {}, deleted the branch \
             {reference}, removed {}; nothing was left behind",
            worktree.display(),
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
            vec![Made::Ancestors {
                from: root.join("absent"),
                to: root.join("absent/below"),
            }],
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

    // A branch that moved is not deleted, and the report says it remains.
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
    assert!(
        report.starts_with(&format!("left behind: the branch {reference} (")),
        "{report}"
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
        ("fixture-wt-spike", None, true),
        ("fixture-wt-spike", Some(own), true),
        ("fixture-wt-spike1", None, true),
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
        (None, true),
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
fn undo_deletes_a_locked_entry_git_neither_lists_nor_prunes_and_only_one_of_this_add() {
    use commands::Made;
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let (git, _) = committed_repository(&root);
    let mut store = ProjectStore::open_unrecorded(&root.join("state"), &project_id(ROOTS)).unwrap();
    let administrative = root.join("repo/.git/worktrees");
    let worktree = root.join("wt");
    // Older than the add, and new but of another add: neither is touched nor reported.
    for name in ["wt", "other"] {
        std::fs::create_dir_all(administrative.join(name)).unwrap();
        std::fs::write(administrative.join(name).join("locked"), "initializing\n").unwrap();
    }
    let before = BTreeSet::from([OsStr::new("wt").to_os_string()]);
    // What a stopped add leaves: a locked entry with neither a path nor a `gitdir`.
    std::fs::create_dir_all(administrative.join("wt1")).unwrap();
    std::fs::write(administrative.join("wt1/locked"), "initializing\n").unwrap();
    assert!(
        !git.require(["worktree", "list", "--porcelain"], None)
            .unwrap()
            .contains("wt1")
    );
    let report = commands::undo(
        &git,
        &mut store,
        vec![Made::Worktree {
            path: worktree.clone(),
            administrative: administrative.clone(),
            before,
        }],
    );
    assert_eq!(
        report,
        "undone, newest first: removed the administrative entry worktrees/wt1; nothing was left \
         behind"
    );
    assert!(!administrative.join("wt1").exists());
    assert!(administrative.join("wt/locked").is_file());
    assert!(administrative.join("other/locked").is_file());
}
