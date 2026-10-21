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
    // The entry a removal began is no path of a Workspace: the removal record holds it.
    let removing = "version = 1\n[[workspaces]]\nid = \"w-000000000000000a\"\nlabel = \"a\"\n\
                    path = \"/src/a\"\nstate = \"/state/a\"\nremoving_entry = \"/src/.git/worktrees/a\"\n";
    assert!(file::parse(removing).is_err());
}

const RECORD: &str = "version = 2\nworkspace_id = \"w-000000000000000a\"\npath = \"/src/a\"\n\n\
                      [checkout]\npath = \"/src/a\"\nidentity = \"16777230:42:1792000000000000042\"\n\
                      step = \"contained\"\ncontainer = \"/src/a.af-removing-7-0123456789abcdef\"\n\
                      container_identity = \"16777230:43:1792000000000000043\"\n\n[[entries]]\n\
                      path = \"/src/.git/worktrees/a\"\nidentity = \"16777230:44:1792000000000000044\"\n\
                      step = \"inspected\"\n";

#[test]
fn a_removal_record_round_trips_and_refuses_what_this_release_does_not_write() {
    let record = removal::parse(RECORD, "w-000000000000000a").unwrap();
    let checkout = record.checkout.clone().unwrap();
    assert_eq!(checkout.step, removal::Step::Contained);
    assert_eq!(
        removal::decode(&checkout.identity),
        Some(review_sandbox::removal::Identity {
            device: 16777230,
            inode: 42,
            born: Some(1_792_000_000_000_000_042),
        })
    );
    assert_eq!(record.entries.len(), 1);
    let store = tempfile::tempdir().unwrap();
    assert!(
        removal::read(store.path(), "w-000000000000000a")
            .unwrap()
            .is_none()
    );
    removal::write(store.path(), &record).unwrap();
    let file = removal::location(store.path(), "w-000000000000000a");
    assert!(
        file.ends_with("removals/w-000000000000000a.toml"),
        "{file:?}"
    );
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .starts_with("# An unfinished `af project workspace remove --delete-worktree`")
    );
    assert_eq!(
        removal::read(store.path(), "w-000000000000000a").unwrap(),
        Some(record)
    );
    // Nothing but the record is left: the temporary file was renamed into place.
    assert_eq!(
        std::fs::read_dir(file.parent().unwrap()).unwrap().count(),
        1
    );

    for (broken, why) in [
        (
            RECORD.replace("version = 2", "version = 3"),
            "another version",
        ),
        (
            RECORD.replace("version = 2", "version = 1"),
            "the superseded version",
        ),
        (format!("{RECORD}extra = true\n"), "an unknown key"),
        (
            RECORD.replace("\"w-000000000000000a\"", "\"w-000000000000000b\""),
            "another Workspace",
        ),
        (
            RECORD.replace("\"16777230:42:1792000000000000042\"", "\"16777230\""),
            "an identity without its inode",
        ),
        (
            RECORD.replace("\"16777230:42:1792000000000000042\"", "\"16777230:42\""),
            "an identity without its birth time",
        ),
        (
            RECORD.replace("\"16777230:42:1792000000000000042\"", "\"16777230:42:0\""),
            "an identity whose birth time is unknown",
        ),
        (
            RECORD.replace(
                "\"16777230:42:1792000000000000042\"",
                "\"-1:42:1792000000000000042\"",
            ),
            "a signed identity",
        ),
        (
            RECORD.replace("path = \"/src/a\"\n\n", "path = \"src/a\"\n\n"),
            "a relative checkout",
        ),
        (
            RECORD.replace(
                "container_identity = \"16777230:43:1792000000000000043\"\n",
                "",
            ),
            "a made container without its identity",
        ),
        (
            RECORD.replace(
                "\"/src/a.af-removing-7-0123456789abcdef\"",
                "\"/elsewhere/a.af-removing-7-0123456789abcdef\"",
            ),
            "a container that is not beside its directory",
        ),
        (
            RECORD.replace("step = \"inspected\"", "step = \"deleting\""),
            "a deletion without a container",
        ),
    ] {
        assert!(
            removal::parse(&broken, "w-000000000000000a").is_err(),
            "{why}: {broken}"
        );
    }
}

#[test]
fn an_entry_names_a_gone_checkout_by_its_gitdir_resolved_as_git_reads_it() {
    let root = tempfile::tempdir().unwrap();
    let canonical = std::fs::canonicalize(root.path()).unwrap();
    std::fs::create_dir_all(canonical.join("repo/.git/worktrees/wt")).unwrap();
    std::fs::create_dir_all(canonical.join("real")).unwrap();
    std::os::unix::fs::symlink(canonical.join("real"), canonical.join("alias")).unwrap();
    let entry = canonical.join("repo/.git/worktrees/wt");
    // The checkout `real/wt` is gone; its directory above stays.
    let own = canonical.join("real/wt/.git");
    let gitdir = |path: &Path| path.as_os_str().as_encoded_bytes().to_vec();
    assert!(removal::names_checkout(&entry, &gitdir(&own), &own));
    // Spelled through a link above the worktree, as an adopted path may be.
    assert!(removal::names_checkout(
        &entry,
        &gitdir(&canonical.join("alias/wt/.git")),
        &own
    ));
    // Relative to the entry, as `worktree.useRelativePaths` writes it.
    assert!(removal::names_checkout(
        &entry,
        b"../../../../real/wt/.git",
        &own
    ));
    assert!(!removal::names_checkout(
        &entry,
        &gitdir(&canonical.join("real/other/.git")),
        &own
    ));
    assert!(!removal::names_checkout(
        &entry,
        &gitdir(&canonical.join("elsewhere/wt/.git")),
        &own
    ));
}

#[test]
fn an_entry_is_claimed_only_while_what_it_holds_now_names_the_checkout() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let canonical = std::fs::canonicalize(root.path()).unwrap();
    let entry = canonical.join("repo/.git/worktrees/wt");
    let moved = canonical.join("repo/.git/worktrees/wt.af-removing-7-0123456789abcdef/wt");
    std::fs::create_dir_all(&entry).unwrap();
    std::fs::create_dir_all(&moved).unwrap();
    let own = canonical.join("wt/.git");
    let identity = |at: &Path| review_sandbox::removal::Identity::born_of(at).unwrap();
    let unproven =
        |at: &Path, deleting| removal::entry_unproven(at, identity(at), &entry, &own, deleting);
    let condition = |at: &Path, deleting| unproven(at, deleting).unwrap_or_default();

    // A missing `gitdir` proves nothing, until the entry's own deletion may have removed it.
    for at in [&entry, &moved] {
        assert!(
            condition(at, false).ends_with(
                "is kept: its gitdir is missing, so it is not known to name this checkout"
            ),
            "{}",
            condition(at, false)
        );
    }
    assert_eq!(unproven(&moved, true), None);

    // It names the checkout as git reads it from where it was inspected, wherever it is now.
    for at in [&entry, &moved] {
        std::fs::write(at.join("gitdir"), format!("{}\n", own.display())).unwrap();
        assert_eq!(unproven(at, false), None);
        assert_eq!(unproven(at, true), None);
    }
    std::fs::write(moved.join("gitdir"), "../../../../wt/.git\n").unwrap();
    assert_eq!(
        unproven(&moved, false),
        None,
        "relative to the inspected path"
    );

    // Another checkout's, even while its deletion has begun.
    let other = canonical.join("other/.git");
    std::fs::write(entry.join("gitdir"), format!("{}\n", other.display())).unwrap();
    for deleting in [false, true] {
        assert_eq!(
            unproven(&entry, deleting),
            Some(format!(
                "its administrative entry {} is kept: its gitdir names {} now, not this worktree",
                entry.display(),
                other.display()
            ))
        );
    }
    std::fs::write(entry.join("gitdir"), format!("{}\n", own.display())).unwrap();

    // Locked, at its path or in its container, whatever the step.
    for at in [&entry, &moved] {
        std::fs::write(at.join("locked"), "").unwrap();
        for deleting in [false, true] {
            assert!(
                condition(at, deleting).contains("is locked, so it is kept"),
                "{}",
                condition(at, deleting)
            );
        }
        std::fs::remove_file(at.join("locked")).unwrap();
    }
    assert!(condition(&entry, false).is_empty());

    // Unreadable is never "no gitdir": it may name the checkout.
    let gitdir = entry.join("gitdir");
    std::fs::set_permissions(&gitdir, std::fs::Permissions::from_mode(0o000)).unwrap();
    // A superuser reads it anyway; there is nothing to refuse then.
    if std::fs::read(&gitdir).is_err() {
        for deleting in [false, true] {
            assert!(
                condition(&entry, deleting).contains("could not be read"),
                "{}",
                condition(&entry, deleting)
            );
        }
    }
    std::fs::set_permissions(&gitdir, std::fs::Permissions::from_mode(0o644)).unwrap();
}

#[test]
fn an_entry_is_read_only_while_it_is_the_directory_the_record_holds() {
    let root = tempfile::tempdir().unwrap();
    let canonical = std::fs::canonicalize(root.path()).unwrap();
    let worktrees = canonical.join("repo/.git/worktrees");
    let entry = worktrees.join("wt");
    let aside = worktrees.join("wt.aside");
    std::fs::create_dir_all(&entry).unwrap();
    let own = canonical.join("wt/.git");
    std::fs::write(entry.join("gitdir"), format!("{}\n", own.display())).unwrap();
    let recorded = review_sandbox::removal::Identity::born_of(&entry).unwrap();
    let unproven = || removal::entry_unproven(&entry, recorded, &entry, &own, false);
    let replaced = format!(
        "its administrative entry was replaced since inspection: {} is not the directory this \
         removal inspected, so it is left alone",
        entry.display()
    );
    assert_eq!(unproven(), None);

    // Moved aside and replaced by another directory naming the checkout, unlocked: a
    // replacement, whatever it holds.
    std::fs::rename(&entry, &aside).unwrap();
    std::fs::create_dir(&entry).unwrap();
    std::fs::write(entry.join("gitdir"), format!("{}\n", own.display())).unwrap();
    assert_eq!(unproven(), Some(replaced.clone()));
    // Its lock and its `gitdir` are never read: an unreadable one changes nothing.
    std::fs::write(entry.join("locked"), "").unwrap();
    std::fs::write(entry.join("gitdir"), "\0").unwrap();
    assert_eq!(unproven(), Some(replaced.clone()));
    std::fs::remove_dir_all(&entry).unwrap();

    // A link to the recorded entry is never followed, and nothing there is no proof either.
    std::os::unix::fs::symlink(&aside, &entry).unwrap();
    assert_eq!(unproven(), Some(replaced));
    std::fs::remove_file(&entry).unwrap();
    assert_eq!(
        unproven(),
        Some(format!(
            "its administrative entry {} is gone since inspection, so it is not known to name \
             this checkout",
            entry.display()
        ))
    );

    // Back at its path, the recorded entry is read again; a `locked` there keeps it.
    std::fs::rename(&aside, &entry).unwrap();
    assert_eq!(unproven(), None);
    std::fs::write(entry.join("locked"), "").unwrap();
    assert!(
        unproven().is_some_and(|reason| reason.contains("is locked, so it is kept")),
        "{:?}",
        unproven()
    );
}

#[test]
fn a_container_name_is_fresh_every_time_and_proves_nothing() {
    let first = removal::container_name(OsStr::new("wt"));
    let second = removal::container_name(OsStr::new("wt"));
    assert_ne!(first, second);
    let first = first.to_string_lossy().into_owned();
    let random = first
        .strip_prefix(&format!("wt.af-removing-{}-", std::process::id()))
        .unwrap();
    assert_eq!(random.len(), 16);
    assert!(random.bytes().all(|byte| byte.is_ascii_hexdigit()));
}

/// A directory below a fresh root, opened as a parent, and the identity of its child `name`.
fn claimed(root: &Path, name: &str) -> (std::os::fd::OwnedFd, review_sandbox::removal::Identity) {
    let parent = review_sandbox::removal::open_anchor(root).unwrap();
    let identity = review_sandbox::removal::Identity::born_of(&root.join(name)).unwrap();
    (parent, identity)
}

#[test]
fn a_claimed_tree_is_deleted_through_descriptors_and_a_link_is_never_followed() {
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let outside = root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("sentinel"), "keep\n").unwrap();
    let target = root.join("container/wt");
    std::fs::create_dir_all(target.join("deep/er")).unwrap();
    std::fs::write(target.join("deep/er/file"), "x").unwrap();
    std::os::unix::fs::symlink(&outside, target.join("deep/link")).unwrap();
    let (container, identity) = claimed(&root.join("container"), "wt");

    // Another directory than the one claimed is never deleted.
    let other = review_sandbox::removal::Identity {
        inode: identity.inode + 1,
        ..identity
    };
    assert!(removal::remove_tree(&container, OsStr::new("wt"), other).is_err());
    // Nor one at the same device and inode born later, as a file system that reuses an inode
    // hands it to the next directory: the birth time tells them apart.
    let later = review_sandbox::removal::Identity {
        born: identity.born.map(|born| born + 1),
        ..identity
    };
    assert!(removal::remove_tree(&container, OsStr::new("wt"), later).is_err());
    assert!(target.join("deep/er/file").is_file());

    removal::remove_tree(&container, OsStr::new("wt"), identity).unwrap();
    assert!(!target.exists());
    assert_eq!(
        std::fs::read_to_string(outside.join("sentinel")).unwrap(),
        "keep\n"
    );
    assert_eq!(
        std::fs::read_dir(root.join("container")).unwrap().count(),
        0
    );
}

#[test]
fn a_directory_that_may_not_be_emptied_stays_and_is_the_error() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let held = root.join("container/wt/held");
    std::fs::create_dir_all(&held).unwrap();
    std::fs::write(held.join("file"), "held\n").unwrap();
    std::fs::set_permissions(&held, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (container, identity) = claimed(&root.join("container"), "wt");
    let error = removal::remove_tree(&container, OsStr::new("wt"), identity).unwrap_err();
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied,
        "{error}"
    );
    // Its permissions are never changed to make it go.
    assert_eq!(
        std::fs::metadata(&held).unwrap().permissions().mode() & 0o777,
        0o555
    );
    assert!(held.join("file").is_file());
    std::fs::set_permissions(&held, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_claimed_worktree_is_compared_with_its_commit_through_descriptors() {
    use review_source_git::{Entry, EntryKind, Manifest, digest_bytes};
    use std::os::unix::fs::PermissionsExt;
    let entry = |path: &str, kind, bytes: &[u8]| Entry {
        path: path.into(),
        kind,
        content: digest_bytes(bytes),
        size: bytes.len() as u64,
    };
    let manifest = Manifest::new(vec![
        entry("README.md", EntryKind::File, b"one\n"),
        entry("bin/run", EntryKind::Executable, b"#!/bin/sh\n"),
        entry("link", EntryKind::Symlink, b"README.md"),
    ])
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let worktree = root.join("container/wt");
    std::fs::create_dir_all(worktree.join("bin")).unwrap();
    std::fs::write(worktree.join("README.md"), "one\n").unwrap();
    std::fs::write(worktree.join("bin/run"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(
        worktree.join("bin/run"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::os::unix::fs::symlink("README.md", worktree.join("link")).unwrap();
    // Git's own `.git` file at the top is no file of the worktree; an empty directory is none.
    std::fs::write(worktree.join(".git"), "gitdir: /repo/.git/worktrees/wt\n").unwrap();
    std::fs::create_dir(worktree.join("empty")).unwrap();
    let (container, identity) = claimed(&root.join("container"), "wt");
    let first =
        || removal::first_difference_at(&container, OsStr::new("wt"), identity, &manifest).unwrap();
    assert_eq!(first(), None);

    // A re-moded file, an untracked file and a link where a directory was each differ, and the
    // first in manifest order is named.
    std::fs::set_permissions(
        worktree.join("bin/run"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert_eq!(first().as_deref(), Some("bin/run"));
    std::fs::write(worktree.join("a-scratch.txt"), "work\n").unwrap();
    assert_eq!(first().as_deref(), Some("a-scratch.txt"));
    std::fs::remove_file(worktree.join("a-scratch.txt")).unwrap();
    std::fs::remove_dir_all(worktree.join("bin")).unwrap();
    std::os::unix::fs::symlink(&root, worktree.join("bin")).unwrap();
    assert_eq!(first().as_deref(), Some("bin"));
    std::fs::remove_file(worktree.join("bin")).unwrap();
    assert_eq!(first().as_deref(), Some("bin/run"));
    // Changed content of the same size differs too.
    std::fs::create_dir(worktree.join("bin")).unwrap();
    std::fs::write(worktree.join("bin/run"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(
        worktree.join("bin/run"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(worktree.join("README.md"), "two\n").unwrap();
    assert_eq!(first().as_deref(), Some("README.md"));
    // A `.git` directory is not git's file: what it holds is compared like any other path.
    std::fs::write(worktree.join("README.md"), "one\n").unwrap();
    std::fs::remove_file(worktree.join(".git")).unwrap();
    std::fs::create_dir(worktree.join(".git")).unwrap();
    std::fs::write(worktree.join(".git/config"), "").unwrap();
    assert_eq!(first().as_deref(), Some(".git/config"));
    // Another directory than the one claimed is refused.
    let other = review_sandbox::removal::Identity {
        inode: identity.inode + 1,
        ..identity
    };
    assert!(removal::first_difference_at(&container, OsStr::new("wt"), other, &manifest).is_err());
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
        ("projects/removal.rs", include_str!("removal.rs")),
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

/// The identity of nothing: what `workspace new` would hold for a directory that does not exist.
const NONE: review_sandbox::Identity = review_sandbox::Identity {
    device: 0,
    inode: 0,
    born: None,
};

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
            .unwrap_or(NONE),
        administrative: administrative.to_path_buf(),
        before,
        commit: commit.to_string(),
        manifest: review_source_git::Manifest::default(),
        submodules: Vec::new(),
    }
}

/// What `workspace new` records of a directory above the worktree it made at `path`: the identity
/// that directory has now, or none when it does not exist.
fn ancestor(path: PathBuf) -> commands::Ancestor {
    commands::Ancestor {
        identity: std::fs::symlink_metadata(&path)
            .map(|metadata| commands::identity(&metadata))
            .unwrap_or(NONE),
        path,
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
            Made::Ancestors(vec![
                ancestor(root.join("deep")),
                ancestor(root.join("deep/er")),
            ]),
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
                ancestor(root.join("absent")),
                ancestor(root.join("absent/below")),
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

#[test]
fn foreign_content_keeps_a_destination_holding_any_entry_it_did_not_write() {
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let written = root.join("written");
    std::fs::create_dir_all(written.join("src/deep")).unwrap();
    std::fs::write(written.join("README.md"), "one\n").unwrap();
    std::fs::write(written.join("src/deep/lib.rs"), "two\n").unwrap();
    let manifest = review_source_git::scan_tree(&written).unwrap();
    let submodules = [PathBuf::from("vendor/module")];
    std::fs::create_dir_all(written.join("vendor/module")).unwrap();
    std::fs::write(written.join(".git"), "gitdir: elsewhere\n").unwrap();
    // The directories above the manifest's paths and a submodule's own are what this add wrote.
    assert_eq!(
        commands::foreign_content(&written, &manifest, &submodules),
        None
    );
    // A partial materialization, a directory made before its file, is still only this add's.
    std::fs::remove_file(written.join("src/deep/lib.rs")).unwrap();
    assert_eq!(
        commands::foreign_content(&written, &manifest, &submodules),
        None
    );
    for (entry, reason) in [
        (
            "sentinel",
            "it holds `sentinel`, which this add did not write",
        ),
        (
            "src/empty",
            "it holds `src/empty`, which this add did not write",
        ),
        (
            "vendor/module/inner",
            "it holds `vendor/module/inner`, which this add did not write",
        ),
        ("README.md", "`README.md` differs from what this add wrote"),
    ] {
        let path = written.join(entry);
        let was_file = path.is_file();
        if was_file {
            std::fs::remove_file(&path).unwrap();
        }
        std::fs::create_dir(&path).unwrap();
        assert_eq!(
            commands::foreign_content(&written, &manifest, &submodules).as_deref(),
            Some(reason),
            "{entry}"
        );
        std::fs::remove_dir(&path).unwrap();
        if was_file {
            std::fs::write(&path, "one\n").unwrap();
        }
    }
    assert_eq!(
        commands::foreign_content(&written, &manifest, &submodules),
        None
    );
}

#[test]
fn undo_removes_an_ancestor_only_while_it_is_still_the_directory_it_created() {
    use commands::Made;
    let root = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(root.path()).unwrap();
    let (git, _) = committed_repository(&root);
    let mut store = ProjectStore::open_unrecorded(&root.join("state"), &project_id(ROOTS)).unwrap();
    let high = root.join("high");
    let low = high.join("low");
    std::fs::create_dir_all(&low).unwrap();
    let recorded = vec![ancestor(high.clone()), ancestor(low.clone())];
    // Removed and recreated by another: the same path, another directory. The replacement is made
    // before the original goes, so the two cannot share an inode.
    std::fs::create_dir(root.join("replacement")).unwrap();
    std::fs::remove_dir(&low).unwrap();
    std::fs::rename(root.join("replacement"), &low).unwrap();
    let report = commands::undo(&git, &mut store, vec![Made::Ancestors(recorded)]);
    assert_eq!(
        report,
        format!(
            "left behind: the directory {}, kept because it was replaced since this add created \
             it; the directory {}, kept because it is not empty",
            low.display(),
            high.display()
        )
    );
    assert!(low.is_dir());
}

/// Every file beneath `directory` with its bytes.
fn bytes_beneath(directory: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
            } else {
                found.push((entry.path(), std::fs::read(entry.path()).unwrap()));
            }
        }
    }
    found.sort();
    found
}

/// A read-only opening creates nothing where there is no Store, changes no byte of one that
/// exists, and holds its lock shared: readers share it, a writer waits for them.
#[test]
fn a_read_only_store_creates_nothing_changes_no_byte_and_excludes_writers() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let id = project_id(ROOTS);
    assert!(ProjectStore::open_read_only(&state, &id).is_err());
    assert!(
        !state.exists(),
        "a read-only opening created the state root"
    );
    drop(ProjectStore::open_unrecorded(&state, &id).unwrap());
    let directory = store_directory(&state, &id);
    let before = bytes_beneath(&directory);
    {
        let reader = ProjectStore::open_read_only(&state, &id).unwrap();
        let other = ProjectStore::open_read_only(&state, &id).unwrap();
        assert_eq!(reader.log().unwrap(), Log::default());
        assert_eq!(other.log().unwrap(), Log::default());
        let soon = std::time::Instant::now() + std::time::Duration::from_millis(50);
        assert!(matches!(
            ProjectStore::open_unrecorded_waiting(&state, &id, Wait::Until(soon)),
            Err(Unopened::Locked)
        ));
    }
    assert_eq!(bytes_beneath(&directory), before);
    std::fs::remove_file(directory.join("project.lock")).unwrap();
    assert!(ProjectStore::open_read_only(&state, &id).is_err());
    assert!(!directory.join("project.lock").exists());
}
