//! A Store another af release wrote names its writer.
//!
//! A release still refuses to replay another release's Store (ADR-0145). Every Store keeps a
//! marker per release that opened it for writing (`af/store-writer@1`), so the refusal names
//! that release and the commands that run it, a Store from before markers is named as an
//! older release's, and a listing that meets several says so in one row instead of one error
//! with serde text each.

use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn af(args: &[&str]) -> Output {
    crate::common::af()
        .args(args)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A Store whose log holds `event` with `payload`, which this release does not replay, and
/// whose writer marker names `release`, or none when `None`.
fn foreign_store(dir: &Path, release: Option<&str>, run_id: &str, event: &str, payload: &str) {
    std::fs::create_dir_all(dir.join("cas/objects")).unwrap();
    let path = dir.join("events.sqlite");
    drop(review_store::EventStore::open(&path).unwrap());
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("DELETE FROM store_writers;").unwrap();
    // The event's id is the one the Store derives, so replay reaches its type and payload.
    let event_id = {
        let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
        sha2::Digest::update(&mut hasher, b"review.kernel/event-id/v1\0");
        sha2::Digest::update(&mut hasher, run_id.as_bytes());
        sha2::Digest::update(&mut hasher, b"\0");
        sha2::Digest::update(&mut hasher, b"0");
        review_core::hex::encode(&sha2::Digest::finalize(hasher))[..26].to_string()
    };
    conn.execute(
        "INSERT INTO events (run_id, sequence, event_id, type, occurred_at, artifact_refs,
                             payload)
         VALUES (?1, 0, ?2, ?3, '1970-01-01T00:00:00Z', '[]', ?4)",
        [run_id, &event_id, event, payload],
    )
    .unwrap();
    if let Some(release) = release {
        conn.execute(
            "INSERT INTO store_writers (af_version, record) VALUES (?1, '{}')",
            [release],
        )
        .unwrap();
    }
}

fn task_store(dir: &Path, release: Option<&str>) {
    foreign_store(dir, release, "task:t-1", "TaskTransition@9", "{}");
}

fn state_home() -> PathBuf {
    crate::common::private_home_env()
        .into_iter()
        .find(|(name, _)| *name == "XDG_STATE_HOME")
        .unwrap()
        .1
}

#[test]
fn a_store_another_release_wrote_is_refused_by_name_with_the_fix() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let newer = root.join("newer");
    task_store(&newer, Some("99.1.0"));
    let listed = af(&["task", "list", "--state", newer.to_str().unwrap()]);
    assert_eq!(listed.status.code(), Some(1), "{}", err(&listed));
    let expected = format!(
        "Store {} was written by af 99.1.0 (this is af {VERSION}); fix: AF_VERSION=99.1.0 af \
         task list --state {}, or af self install 99.1.0, or af onboard --refresh-lock --af \
         99.1.0",
        newer.display(),
        newer.display()
    );
    assert!(err(&listed).contains(&expected), "{}", err(&listed));
    assert!(
        !err(&listed).contains("TaskTransition@9"),
        "{}",
        err(&listed)
    );

    // The same refusal is the one document `--json` prints, with the same exit code; its fix
    // reruns the command as it was typed.
    let json = af(&["task", "list", "--state", newer.to_str().unwrap(), "--json"]);
    assert_eq!(json.status.code(), Some(1));
    let document: Value = serde_json::from_slice(&json.stdout).unwrap();
    assert_eq!(document["schema"], "af/error@1");
    assert!(
        document["error"].as_str().unwrap().contains(&format!(
            "Store {} was written by af 99.1.0 (this is af {VERSION}); fix: AF_VERSION=99.1.0 \
             af task list --state {} --json, or af self install 99.1.0",
            newer.display(),
            newer.display()
        )),
        "{document}"
    );

    // A Store from before markers is an older release's.
    let older = root.join("older");
    task_store(&older, None);
    let listed = af(&["task", "list", "--state", older.to_str().unwrap()]);
    assert_eq!(listed.status.code(), Some(1));
    assert!(
        err(&listed).contains(&format!(
            "Store {} was written by an older af release (no version marker; this is af \
             {VERSION})",
            older.display()
        )),
        "{}",
        err(&listed)
    );
}

#[test]
fn storage_and_campaigns_list_other_releases_stores_in_one_row() {
    let tasks = state_home().join("af/task/local");
    task_store(&tasks.join("aaaaaaaaaaaaaaaa"), Some("99.1.0"));
    task_store(&tasks.join("bbbbbbbbbbbbbbbb"), Some("99.1.0"));
    task_store(&tasks.join("cccccccccccccccc"), None);
    let shown = af(&["storage"]);
    assert!(shown.status.success(), "{}", err(&shown));
    let text = out(&shown);
    // Two kinds of writer: a command for the release's Stores, and none for the older one.
    assert!(
        text.contains(
            "3 Stores written by other releases; run each under the release that wrote it:\n  \
             AF_VERSION=99.1.0 af task list --state DIR (2 Stores)\n  1 Store by an older af \
             release with no version marker: finish it under its own release, or\n    remove it \
             with af storage prune --apply once idle; af storage --verbose names each"
        ),
        "{text}"
    );
    assert!(
        !text.contains("run them with AF_VERSION=99.1.0"),
        "no one release is recommended for every Store: {text}"
    );
    let summary = text
        .lines()
        .skip_while(|line| !line.starts_with("3 Stores written"))
        .take(4);
    for line in summary {
        assert!(line.chars().count() <= 100, "{line}");
    }
    let per_store = |text: &str| {
        text.lines()
            .filter(|line| line.starts_with("unreadable Store /"))
            .count()
    };
    assert_eq!(per_store(&text), 0, "{text}");
    assert!(!text.contains("TaskTransition@9"), "{text}");

    // `--verbose` names each, with its own refusal.
    let verbose = out(&af(&["storage", "--verbose"]));
    assert_eq!(per_store(&verbose), 3, "{verbose}");
    assert!(
        verbose.contains("was written by af 99.1.0 (this is af"),
        "{verbose}"
    );

    // The document keeps its shape: one entry each, its reason naming the writer.
    let document: Value = serde_json::from_slice(&af(&["storage", "--json"]).stdout).unwrap();
    assert_eq!(document["schema"], "af/storage@5");
    let reasons: Vec<&str> = document["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "unreadable_store")
        .filter_map(|entry| entry["reason"].as_str())
        .collect();
    assert_eq!(reasons.len(), 3, "{document}");
    assert!(
        reasons
            .iter()
            .all(|reason| reason.contains("TaskTransition@9") && reason.contains("fix:")),
        "{reasons:?}"
    );
    // The writer and the next command are fields of their own, from the Store's provenance.
    crate::schemas::valid(&crate::schemas::validator("storage-v5.json"), &document);
    let mut fixes: Vec<(Value, Value)> = document["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["kind"] == "unreadable_store")
        .map(|entry| {
            (
                entry["writer_release"].clone(),
                entry["next_action"].clone(),
            )
        })
        .collect();
    fixes.sort_by_key(|(release, _)| release.to_string());
    let rerun = |name: &str| {
        Value::from(format!(
            "AF_VERSION=99.1.0 af task list --state {}",
            tasks.join(name).display()
        ))
    };
    assert_eq!(
        fixes,
        [
            (Value::from("99.1.0"), rerun("aaaaaaaaaaaaaaaa")),
            (Value::from("99.1.0"), rerun("bbbbbbbbbbbbbbbb")),
            (Value::Null, Value::from("af storage prune --apply")),
        ],
        "{document}"
    );

    // A Campaign another release wrote: one row in `af review campaigns` as well.
    let campaigns = state_home().join("af/review/campaigns");
    foreign_store(
        &campaigns.join("old-pr"),
        Some("99.1.0"),
        "campaign-old-pr",
        "CampaignOpened@1",
        "{\"opened\":true}",
    );
    let listed = af(&["review", "campaigns"]);
    assert!(listed.status.success(), "{}", err(&listed));
    assert!(
        out(&listed).contains(
            "1 Store written by another release: af 99.1.0 (1); run it with AF_VERSION=99.1.0 \
             af review campaigns"
        ),
        "{}",
        out(&listed)
    );
    assert!(
        !out(&listed).contains("invalid replayed event payload"),
        "{}",
        out(&listed)
    );
}

/// The releases a Store's writer markers name, oldest first.
fn markers(dir: &Path) -> Vec<String> {
    let conn = rusqlite::Connection::open(dir.join("events.sqlite")).unwrap();
    let mut statement = conn
        .prepare("SELECT af_version FROM store_writers ORDER BY rowid")
        .unwrap();
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// A Store another release wrote stays named by that release alone: a command of this release
/// that opens it and is refused leaves no marker of this release behind.
#[test]
fn a_refused_store_gets_no_marker_of_this_release() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let newer = root.join("newer");
    task_store(&newer, Some("99.1.0"));
    for args in [
        vec!["task", "refresh", "t-1", "--state", newer.to_str().unwrap()],
        vec!["task", "list", "--state", newer.to_str().unwrap()],
        vec!["task", "show", "t-1", "--state", newer.to_str().unwrap()],
    ] {
        let refused = af(&args);
        assert_ne!(
            refused.status.code(),
            Some(0),
            "{args:?}: {}",
            err(&refused)
        );
        assert_eq!(markers(&newer), ["99.1.0"], "{args:?}");
    }
}
