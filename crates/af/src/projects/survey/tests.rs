use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use review_core::project::{SurveyIdentityPolicyV1, SurveyReasonV1};
use serde_json::json;

use super::measure::{
    self, Change, ChecksFold, Clock, Commit, Ended, Entry, EntryKind, Found, Object, TreeFold,
    Walk, hash16, identity_of, read_batch, read_listing, read_log,
};
use super::policy::Policy;
use super::*;
use crate::project::glob_matches;

const A: &str = "1111111111111111111111111111111111111111";
const B: &str = "2222222222222222222222222222222222222222";
const C: &str = "3333333333333333333333333333333333333333";
const DAY: u64 = 86_400;

/// A clock whose deadline is a minute away.
fn clock() -> Clock {
    Clock::new(Instant::now(), 60_000)
}

/// `checks` over `declarations`, one per item of `clock`, as the measurement folds them.
fn checks_of(
    declarations: &[(&str, Option<&str>)],
    found: &Found,
    policy: &Policy,
    clock: &Clock,
) -> (review_core::project::SurveyChecksV1, bool) {
    let mut fold = ChecksFold::default();
    let deadline = declarations
        .iter()
        .any(|(path, content)| clock.item() || !fold.add(path, *content, clock));
    (fold.finish(found, policy), deadline)
}

/// A clock whose deadline passed.
fn passed() -> Clock {
    let clock = Clock::new(Instant::now(), 1);
    std::thread::sleep(std::time::Duration::from_millis(5));
    clock
}

#[test]
fn the_record_serializer_refuses_every_json_number_and_sorts_every_map() {
    // serde_json keeps insertion order in this crate, so the input is deliberately unsorted.
    let mut value = serde_json::Map::new();
    value.insert("zeta".into(), json!("1"));
    value.insert(
        "alpha".into(),
        json!({"y": "2", "b": [{"d": "4", "c": "3"}]}),
    );
    let bytes = canonical_bytes(&serde_json::Value::Object(value)).unwrap();
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        r#"{"alpha":{"b":[{"c":"3","d":"4"}],"y":"2"},"zeta":"1"}"#
    );
    for (refused, pointer) in [
        (json!({"tree": {"bytes": 1.5}}), "/tree/bytes"),
        (json!({"lines": [ "1", 2.0 ]}), "/lines/1"),
        (json!({"files": 3}), "/files"),
    ] {
        let error = canonical_bytes(&refused).unwrap_err();
        assert!(error.starts_with(pointer), "{error}");
        assert!(error.contains("not decimal text"), "{error}");
    }
    assert!(canonical_bytes(&json!({"removed": true, "title": null})).is_ok());
}

#[test]
fn every_pattern_is_the_af_toml_grammar_with_brackets_literal() {
    let policy = Policy::default();
    assert_eq!(policy.decisions, "docs/adr/*.md");
    assert!(measure::is_decision("docs/adr/0152-x.md", &policy));
    assert!(!measure::is_decision("docs/adr/README.md", &policy));
    assert!(!measure::is_decision("docs/adr/015-short.md", &policy));
    assert!(!measure::is_decision("docs/adr/01520-five.md", &policy));
    assert!(!measure::is_decision("docs/adr/old/0001-a.md", &policy));
    assert!(!measure::is_decision("docs/0001-elsewhere.md", &policy));
    assert_eq!(measure::decision_number("docs/adr/0152-x.md"), Some("0152"));
    // The agent patterns, the release tags and the paths share the one matcher.
    assert!(glob_matches("*[bot]", "dependabot[bot]"));
    assert!(!glob_matches("*[bot]", "robot"));
    assert!(
        !glob_matches("*[bot]", "dependabot"),
        "a name ending in `t`"
    );
    assert!(glob_matches("docs/adr/[0-9]*.md", "docs/adr/[0-9]x.md"));
    assert!(
        !glob_matches("docs/adr/[0-9]*.md", "docs/adr/0152-x.md"),
        "`[0-9]` is literal, not a class"
    );
    assert!(glob_matches("**/tests/**", "crates/af/tests/it/main.rs"));
    assert!(glob_matches("**/tests/**", "tests/a"));
    assert!(!glob_matches("**/tests/**", "crates/tests.rs"));
    assert!(glob_matches("**/*_test.*", "pkg/a_test.go"));
    assert!(glob_matches("**/test_*", "test_a.py"));
    assert!(glob_matches("vendor/**", "vendor/x/y.c"));
    assert!(glob_matches("*Claude*", "Claude Opus 5.5"));
    assert!(!glob_matches("*Claude*", "claude"));
    assert!(glob_matches("v*", "v1.0.0"));
    assert!(!glob_matches("v*", "release-1"));
    assert!(glob_matches("v?.*", "v1.2"));
}

#[test]
fn the_policy_has_the_documented_defaults_refuses_unknown_keys_and_extends_languages() {
    let defaults = Policy::from_effective(None).unwrap();
    assert_eq!(defaults, Policy::default());
    assert_eq!(defaults.max_wall_ms, 60_000);
    assert_eq!(defaults.max_commits, 100_000);
    assert_eq!(defaults.max_file_bytes, 1_048_576);
    assert_eq!(defaults.max_paths, 200_000);
    assert_eq!(defaults.max_entries, 1_000);
    assert_eq!(defaults.max_survey_bytes, 1_048_576);
    assert_eq!(defaults.hot_paths, 20);
    assert_eq!(defaults.identity, SurveyIdentityPolicyV1::Name);
    let document = defaults.document();
    assert_eq!(document["max_commits"], "100000", "decimal text");
    assert_eq!(document["max_entries"], "1000");
    let digest = defaults.digest().unwrap();
    assert!(review_core::is_digest(&digest), "{digest}");
    // The digest is SHA-256 of the canonical JSON, nothing else.
    use sha2::{Digest, Sha256};
    let bytes = canonical_bytes(&document).unwrap();
    assert_eq!(
        digest,
        format!(
            "sha256:{}",
            review_core::hex::encode(&Sha256::digest(&bytes))
        )
    );

    let table: toml::Value = toml::from_str(
        "max_commits = 1\nidentity = \"email_hash\"\nmax_survey_bytes = 1024\n\
         decisions = \"docs/[0-9\"\n[languages]\n\"*.pest\" = \"pest\"\n\"*.rs\" = \"rust-2024\"\n",
    )
    .unwrap();
    let policy = Policy::from_effective(Some(&table)).unwrap();
    assert_eq!(policy.max_commits, 1);
    assert_eq!(policy.max_survey_bytes, 1024);
    assert_eq!(policy.decisions, "docs/[0-9", "a bracket is a literal");
    assert_eq!(policy.identity, SurveyIdentityPolicyV1::EmailHash);
    assert_ne!(policy.digest().unwrap(), digest);
    assert_eq!(policy.language("grammar.pest"), "pest");
    assert_eq!(
        policy.language("lib.rs"),
        "rust-2024",
        "a key may be overridden"
    );
    assert_eq!(
        policy.language("README.md"),
        "markdown",
        "the built-in map stays"
    );
    assert_eq!(policy.language("Makefile"), "make");
    assert_eq!(policy.language("archive.tar.gz"), "other");

    for (broken, why) in [
        ("refresh = \"manual\"\n", "refresh is PJ4's"),
        ("max_commit = 1\n", "a typo is refused"),
        ("max_commits = 0\n", "at least one commit"),
        ("max_entries = 0\n", "at least one entry"),
        ("max_paths = 0\n", "at least one path"),
        ("max_wall_ms = -1\n", "not negative"),
        ("identity = \"email\"\n", "the identity policies are closed"),
        ("decisions = \"/abs/*.md\"\n", "anchored at the root"),
        ("exclude = [\"a//b\"]\n", "no empty segment"),
        ("vocabulary = \"/etc/passwd\"\n", "a repository path"),
        (
            "[languages]\n\"/src/*.rs\" = \"rust\"\n",
            "a key is a pattern anchored at the root",
        ),
        (
            "[languages]\n\"src//*.rs\" = \"rust\"\n",
            "no empty segment",
        ),
        (
            "[languages]\n\"*.x\" = \"Has Space\"\n",
            "a language is a short name",
        ),
        ("hot_paths = 100000\n", "bounded"),
    ] {
        let table: toml::Value = toml::from_str(broken).unwrap();
        let error = Policy::from_effective(Some(&table)).unwrap_err();
        assert!(
            error.starts_with("the [survey] policy does not load: "),
            "{why}: {error}"
        );
    }
}

#[test]
fn listings_batches_and_logs_are_read_as_they_stream() {
    let listing = format!(
        "100644 blob {A}      12\tREADME.md\0100755 blob {B}       3\tbin/run\0\
         120000 blob {C}       9\tlink\0160000 commit {A}       -\tvendor/sub\0"
    );
    let mut entries = Vec::new();
    let ended = read_listing(&mut listing.as_bytes(), &clock(), |entry| {
        entries.push(entry);
        true
    });
    assert_eq!(ended, Ended::Done);
    assert_eq!(
        entries,
        vec![
            Entry {
                kind: EntryKind::File,
                oid: A.into(),
                size: 12,
                path: "README.md".into()
            },
            Entry {
                kind: EntryKind::File,
                oid: B.into(),
                size: 3,
                path: "bin/run".into()
            },
            Entry {
                kind: EntryKind::Symlink,
                oid: C.into(),
                size: 9,
                path: "link".into()
            },
            Entry {
                kind: EntryKind::Submodule,
                oid: A.into(),
                size: 0,
                path: "vendor/sub".into()
            },
        ]
    );
    // A clock whose deadline passed stops a stream at its first item.
    let passed = Clock::new(Instant::now(), 1);
    std::thread::sleep(std::time::Duration::from_millis(5));
    let mut none = 0;
    assert_eq!(
        read_listing(&mut listing.as_bytes(), &passed, |_| {
            none += 1;
            true
        }),
        Ended::Deadline
    );
    assert_eq!(none, 0);

    let batch = format!("{A} blob 3\na\nb\n{B} missing\n{C} blob 4\nx\0y\n\n");
    let mut objects: Vec<(usize, Option<Object>)> = Vec::new();
    let ended = read_batch(&mut batch.as_bytes(), &clock(), Some(3), |index, object| {
        objects.push((index, object));
        true
    });
    assert_eq!(ended, Ended::Done);
    assert_eq!(objects.len(), 3);
    let first = objects[0].1.as_ref().unwrap();
    assert_eq!((first.size, first.lines), (3, Some(1)));
    assert_eq!(first.content.as_deref(), Some(&b"a\nb"[..]));
    assert!(objects[1].1.is_none(), "missing");
    let third = objects[2].1.as_ref().unwrap();
    assert_eq!(third.lines, None, "a NUL makes it binary");
    assert_eq!(third.content, None, "over the bytes kept");
    let short = format!("{A} blob 9\nshort\n");
    assert!(matches!(
        read_batch(&mut short.as_bytes(), &clock(), None, |_, _| true),
        Ended::Bad(_)
    ));

    // A merge and two commits as `log -z --numstat` prints them in the Surveyor's format.
    let log = format!(
        "\x1e{B}\x1f{A} {C}\x1f200\x1fAda\x1f\x1f\x1f\0\
         \x1e{A}\x1f\x1f100\x1frenovate[bot]\x1f\x1fBob <b@example.invalid>\x1f\0\n\
         2\t1\tsrc/lib.rs\0-\t-\tlogo.png\0\
         \x1e{C}\x1f\x1f50\x1fAda\x1f\x1fClaude <c@example.invalid>\x1dCodex <x@example.invalid>\x1f\0\n\
         1\t0\tREADME.md\0"
    );
    let policy = Policy::default();
    let mut commits: Vec<Commit> = Vec::new();
    let ended = read_log(&mut log.as_bytes(), &clock(), &policy, |commit| {
        commits.push(commit);
        Ok(())
    });
    assert_eq!(ended, Ended::Done);
    assert_eq!(commits.len(), 3);
    assert!(commits[0].merge && commits[0].changes.is_empty());
    assert_eq!(commits[0].name.as_deref(), Some("Ada"));
    assert_eq!(commits[0].key, measure_key(b"Ada"));
    assert!(!commits[0].agent);
    assert!(commits[1].agent, "the author matches `*[bot]`");
    assert_eq!(
        commits[1].changes,
        vec![
            Change {
                added: 2,
                removed: 1,
                path: "src/lib.rs".into()
            },
            Change {
                added: 0,
                removed: 0,
                path: "logo.png".into()
            }
        ]
    );
    assert!(commits[2].agent, "a Co-Authored-By name alone");
    // A trailer is matched by its name, never by its address.
    let address = format!("\x1e{A}\x1f\x1f1\x1fAda\x1f\x1fAda <claude@example.invalid>\x1f\0");
    let mut commits = Vec::new();
    read_log(&mut address.as_bytes(), &clock(), &policy, |commit| {
        commits.push(commit);
        Ok(())
    });
    assert!(!commits[0].agent);
    let excluded = Policy {
        exclude: vec!["*.png".into()],
        ..Policy::default()
    };
    let mut commits = Vec::new();
    read_log(&mut log.as_bytes(), &clock(), &excluded, |commit| {
        commits.push(commit);
        Ok(())
    });
    assert_eq!(commits[1].changes.len(), 1);
}

/// The 16-hex key of the exact author-name bytes, under `identity = "name"`.
fn measure_key(name: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    review_core::hex::encode(&Sha256::digest(name))[..16].to_string()
}

#[test]
fn no_record_of_a_stream_is_read_past_64_kib_and_no_commit_past_max_paths() {
    let long = "x".repeat(64 * 1024 + 1);
    // A tree entry, a numstat line and a ref over the bound stop their stream with the reason.
    let listing = format!("100644 blob {A}      12\tREADME.md\0100644 blob {B} 1\t{long}\0");
    let mut entries = Vec::new();
    assert_eq!(
        read_listing(&mut listing.as_bytes(), &clock(), |entry| {
            entries.push(entry);
            true
        }),
        Ended::Bound(SurveyReasonV1::MaxRecordBytes)
    );
    assert_eq!(entries.len(), 1, "the entry before it was read");
    let prefix = format!("100644 blob {A} 1\t");
    let exact = format!("{prefix}{}\0", "y".repeat(64 * 1024 - prefix.len()));
    assert_eq!(
        read_listing(&mut exact.as_bytes(), &clock(), |_| true),
        Ended::Done,
        "a record at the bound is read"
    );
    let log = format!("\x1e{A}\x1f\x1f100\x1fAda\x1f\x1f\x1f\0\n1\t0\t{long}\0");
    let mut commits = 0;
    assert_eq!(
        read_log(&mut log.as_bytes(), &clock(), &Policy::default(), |_| {
            commits += 1;
            Ok(())
        }),
        Ended::Bound(SurveyReasonV1::MaxRecordBytes)
    );
    assert_eq!(commits, 0, "the commit it stopped in is not passed on");

    // A commit touching more paths than max_paths stops the walk; the walk tracks at most
    // max_paths distinct paths, and the commit that would pass it is not folded.
    let few = Policy {
        max_paths: 2,
        ..Policy::default()
    };
    let wide = format!("\x1e{A}\x1f\x1f100\x1fAda\x1f\x1f\x1f\0\n1\t0\ta\01\t0\tb\01\t0\tc\0");
    assert_eq!(
        read_log(&mut wide.as_bytes(), &clock(), &few, |_| Ok(())),
        Ended::Bound(SurveyReasonV1::MaxPaths)
    );
    let cutoff = 1_000 * DAY;
    let mut walk = Walk::new(cutoff, 2);
    walk.add(commit(A, cutoff, "Ada", false, &[("a", 1), ("b", 1)]))
        .unwrap();
    walk.add(commit(B, cutoff, "Ada", false, &[("a", 1)]))
        .unwrap();
    assert_eq!(
        walk.add(commit(C, cutoff, "Bob", false, &[("c", 1)])),
        Err(SurveyReasonV1::MaxPaths)
    );
    assert_eq!(walk.commits, 2, "totals cover the observed commits");
    // An old commit's paths are not tracked.
    walk.add(commit(C, cutoff - 400 * DAY, "Bob", false, &[("c", 1)]))
        .unwrap();
    assert_eq!(walk.commits, 3);
}

#[test]
fn no_author_email_is_kept_and_an_email_shaped_name_is_hashed_under_both_policies() {
    use SurveyIdentityPolicyV1::{EmailHash, Name};
    use sha2::{Digest, Sha256};
    let rule = |text: &str| {
        review_core::hex::encode(&Sha256::digest(text.trim().to_lowercase().as_bytes()))[..16]
            .to_string()
    };
    let expected = rule("ada@example.invalid");
    assert_eq!(hash16("  Ada@Example.Invalid \n"), expected);
    assert_eq!(
        identity_of(b"Ada", " Ada@Example.Invalid", EmailHash),
        (expected.clone(), None, false)
    );
    // Under `name` the key is the hash of the exact name bytes; the name is its display text.
    assert_eq!(
        identity_of(b" Ada ", "ada@example.invalid", Name),
        (measure_key(b" Ada "), Some("Ada".to_string()), false)
    );
    // A name that is an address, or that carries one beside other text, is `h:` and the hash
    // of the name, under both policies, and the email is not used for it.
    for name in ["Ada@Example.invalid", "Alice alice@example.org", "a@b"] {
        let hashed = format!("h:{}", rule(name));
        assert_eq!(
            identity_of(name.as_bytes(), "", Name),
            (hashed.clone(), Some(hashed.clone()), false),
            "{name}"
        );
        assert_eq!(
            identity_of(name.as_bytes(), "bob@x.invalid", EmailHash),
            (hashed, None, false),
            "{name}"
        );
    }
    assert_eq!(
        identity_of(b"ada", "bob@x.invalid", EmailHash).0,
        hash16("bob@x.invalid")
    );
    // An `@` that is not an address is a name; a name is cut and marked, never hashed for it.
    assert_eq!(
        identity_of(b"Ada @ home", "", Name).1.as_deref(),
        Some("Ada @ home")
    );
    let long = format!("Ada {}", "é".repeat(148));
    assert_eq!(long.len(), 300);
    let (key, cut, truncated) = identity_of(long.as_bytes(), "", Name);
    let cut = cut.unwrap();
    assert!(truncated);
    assert!(cut.len() <= 256 && long.starts_with(&cut), "{}", cut.len());
    // Two names sharing their first 256 bytes are two keys with equal display text.
    let other = format!("{}x", &long[..long.len() - 2]);
    let (other_key, other_cut, _) = identity_of(other.as_bytes(), "", Name);
    assert_eq!(other_cut.as_deref(), Some(cut.as_str()));
    assert_ne!(other_key, key);
    // The cut is decided after the address: an address past 256 bytes still hashes the name.
    let late = format!("{} carol@example.invalid", "y".repeat(300));
    assert!(identity_of(late.as_bytes(), "", Name).0.starts_with("h:"));
    let policy = Policy {
        identity: EmailHash,
        ..Policy::default()
    };
    assert!(measure::log_format(&policy).contains("%ae"));
    assert!(!measure::log_format(&Policy::default()).contains("%ae"));
    // Repository text is recorded as committed, `@` included.
    assert_eq!(
        measure::display("Contact ada@fixture.invalid"),
        Some(("Contact ada@fixture.invalid".to_string(), false))
    );
}

fn commit(id: &str, time: u64, identity: &str, agent: bool, changes: &[(&str, u64)]) -> Commit {
    Commit {
        id: id.into(),
        merge: false,
        time,
        key: measure_key(identity.as_bytes()),
        name: Some(identity.into()),
        truncated: false,
        agent,
        changes: changes
            .iter()
            .map(|(path, added)| Change {
                added: *added,
                removed: 1,
                path: path.to_string(),
            })
            .collect(),
    }
}

#[test]
fn a_window_holds_commits_after_cutoff_minus_the_window_up_to_the_cutoff() {
    let cutoff = 1_000 * DAY;
    let mut walk = Walk::new(cutoff, 200_000);
    let mut merge = commit(C, cutoff - DAY, "Bob", false, &[("src/a.rs", 50)]);
    merge.merge = true;
    for commit in [
        commit(A, cutoff + 1, "Ada", false, &[]),
        commit(
            A,
            cutoff,
            "Ada",
            false,
            &[("src/a.rs", 3), ("README.md", 1)],
        ),
        merge,
        commit(
            B,
            cutoff - 30 * DAY,
            "Bot",
            true,
            &[("src/a.rs", 1), ("gone.rs", 1)],
        ),
        commit(C, cutoff - 400 * DAY, "Ada", false, &[("src/b.rs", 9)]),
    ] {
        walk.add(commit).unwrap();
    }
    let policy = Policy {
        hot_paths: 2,
        ..Policy::default()
    };
    let (hot, distinct) = walk.hot_paths(policy.hot_paths);
    let absent = BTreeSet::from(["gone.rs".to_string()]);
    let history = measure::history_section(&walk, 7, hot, distinct, &absent, &policy);
    assert_eq!(
        history.windows.days_30.commits.get(),
        2,
        "the cutoff itself and the merge; not after the cutoff, not at cutoff - 30 days"
    );
    assert_eq!(history.windows.days_30.merges.get(), 1);
    assert_eq!(history.windows.days_90.commits.get(), 3);
    assert_eq!(history.windows.days_365.commits.get(), 3);
    assert_eq!(history.commits.get(), 5);
    assert_eq!(history.merges.get(), 1);
    assert_eq!(
        history.oldest_reached.as_deref(),
        Some(C),
        "7 reachable, 5 reached"
    );
    assert_eq!(history.first_commit_unix_s.get(), cutoff - 400 * DAY);
    assert_eq!(history.last_commit_unix_s.get(), cutoff + 1);
    // The merge contributes no churn.
    assert_eq!(history.churn_90d["src"].commits.get(), 2);
    assert_eq!(history.churn_90d["src"].added.get(), 4);
    assert_eq!(history.churn_90d["."].commits.get(), 2);
    let hot: Vec<(&str, u64, Option<bool>)> = history
        .hot_paths
        .iter()
        .map(|hot| (hot.path.as_str(), hot.commits.get(), hot.removed))
        .collect();
    assert_eq!(
        hot,
        vec![("src/a.rs", 2, None), ("README.md", 1, None)],
        "most commits first, ties by path, two of them"
    );
    assert!(history.seen.is_empty());

    let (contributors, deadline) =
        measure::contributors_section(&walk, &Policy::default(), &clock());
    assert!(!deadline);
    assert_eq!(contributors.identities.get(), 3);
    assert_eq!(contributors.agent_commits.get(), 1);
    assert_eq!(contributors.human_commits.get(), 4);
    assert_eq!(contributors.agent_share_bp.get(), 2000);
    assert_eq!(contributors.top_share_bp.get(), 6000);
    let named = |name: &str| {
        contributors
            .by_identity
            .iter()
            .find(|entry| entry.name.as_deref() == Some(name))
            .unwrap()
    };
    let ada = named("Ada");
    assert_eq!(ada.key, measure_key(b"Ada"));
    assert_eq!(
        (ada.commits.get(), ada.added.get(), ada.removed.get()),
        (3, 13, 3)
    );
    assert_eq!(named("Bob").added.get(), 0, "a merge");
    assert!(ada.active_90d);

    // An empty window and an empty walk make every share 0.
    let (empty, _) =
        measure::contributors_section(&Walk::new(cutoff, 200_000), &Policy::default(), &clock());
    assert_eq!(empty.human_share_bp.get(), 0);
    assert_eq!(empty.agent_share_bp.get(), 0);
    assert_eq!(empty.top_share_bp.get(), 0);
}

#[test]
fn every_list_is_cut_at_max_entries_and_says_how_many_it_saw() {
    let cutoff = 1_000 * DAY;
    let policy = Policy {
        max_entries: 2,
        ..Policy::default()
    };
    // One commit touching more paths, in more directories, than max_entries.
    let mut walk = Walk::new(cutoff, 200_000);
    walk.add(commit(
        A,
        cutoff,
        "Ada",
        false,
        &[("a/1", 1), ("b/2", 1), ("c/3", 1), ("d/4", 1), ("e/5", 1)],
    ))
    .unwrap();
    walk.add(commit(B, cutoff, "Bob", false, &[])).unwrap();
    walk.add(commit(C, cutoff, "Cy", false, &[])).unwrap();
    let (hot, distinct) = walk.hot_paths(policy.hot_paths.min(policy.max_entries));
    assert_eq!((hot.len(), distinct), (2, 5));
    let history = measure::history_section(&walk, 3, hot, distinct, &BTreeSet::new(), &policy);
    assert_eq!(history.churn_90d.len(), 2);
    assert_eq!(history.seen["churn_90d"].get(), 5);
    assert_eq!(history.seen["hot_paths"].get(), 5);
    let (contributors, _) = measure::contributors_section(&walk, &policy, &clock());
    assert_eq!(contributors.by_identity.len(), 2);
    assert_eq!(
        contributors.by_identity[0].name.as_deref(),
        Some("Ada"),
        "most lines added among equal commits"
    );
    assert_eq!(contributors.identities.get(), 3);
    assert_eq!(contributors.seen["by_identity"].get(), 3);

    let records = (1..=3)
        .map(|n| measure::decision(&format!("docs/adr/000{n}-x.md"), Some("# T\n")))
        .collect();
    let decisions = measure::decisions_section(records, &policy);
    assert_eq!(decisions.count.get(), 3);
    assert_eq!(decisions.records.len(), 2);
    assert_eq!(decisions.seen["records"].get(), 3);
    let (vocabulary, _) = measure::vocabulary_section(
        "CONTEXT.md",
        true,
        Some("**C**:\n**A**:\n**B**:\n"),
        &policy,
        &clock(),
    );
    assert_eq!(vocabulary.count.unwrap().get(), 3);
    let names: Vec<&str> = vocabulary
        .terms
        .as_ref()
        .unwrap()
        .iter()
        .map(|term| term.name.as_str())
        .collect();
    assert_eq!(names, ["A", "B"], "ranked by name before the cut");
    assert_eq!(vocabulary.terms.as_ref().unwrap().len(), 2);
    assert_eq!(vocabulary.seen["terms"].get(), 3);
}

#[test]
fn trees_count_lines_of_text_blobs_only_and_split_tests_and_directories() {
    let entry = |kind, oid: &str, size, path: &str| Entry {
        kind,
        oid: oid.into(),
        size,
        path: path.into(),
    };
    let entries = vec![
        entry(EntryKind::File, A, 4, "README.md"),
        entry(EntryKind::File, B, 6, "src/lib.rs"),
        entry(EntryKind::File, C, 3, "src/tests/t.rs"),
        entry(EntryKind::File, "4".repeat(40).as_str(), 99, "big.txt"),
        entry(EntryKind::Symlink, "5".repeat(40).as_str(), 7, "src/link"),
        entry(EntryKind::Submodule, A, 0, "vendor"),
    ];
    let policy = Policy {
        max_file_bytes: 10,
        ..Policy::default()
    };
    let lines = BTreeMap::from([
        (A.to_string(), Some(2)),
        (B.to_string(), None),
        (C.to_string(), Some(2)),
    ]);
    // The fold of the listing, then of the blobs `cat-file` yields, as the streams arrive.
    let fold = |entries: &[Entry], lines: &BTreeMap<String, Option<u64>>, policy: &Policy| {
        let mut fold = TreeFold::default();
        for entry in entries {
            assert!(fold.add(entry, policy, &clock()));
        }
        let blobs = fold.blobs();
        for oid in blobs {
            if let Some(counted) = lines.get(&oid) {
                fold.lines(&oid, *counted);
            }
        }
        fold
    };
    assert!(
        fold(&entries, &lines, &policy).blobs().is_empty(),
        "every blob read"
    );
    let tree = fold(&entries, &lines, &policy).finish(6, &policy);
    assert_eq!(
        (tree.files.get(), tree.lines.get(), tree.bytes.get()),
        (6, 4, 119)
    );
    assert_eq!(
        tree.files_without_lines.get(),
        4,
        "binary, big, symlink, submodule"
    );
    // A root-level entry, the submodule `vendor` too, groups under `.`.
    assert_eq!(tree.directories["."].files.get(), 3);
    assert_eq!(tree.directories["src"].lines.get(), 2);
    assert_eq!(tree.directories["src"].bytes.get(), 16);
    assert_eq!(tree.languages["other"].files.get(), 2);
    assert_eq!(tree.languages["rust"].files.get(), 2);
    assert_eq!((tree.tests.files.get(), tree.tests.lines.get()), (1, 2));
    assert!(tree.seen.is_empty());
    let unread = fold(&entries, &BTreeMap::new(), &policy).finish(6, &policy);
    assert_eq!(unread.lines.get(), 0);
    assert_eq!(unread.files_without_lines.get(), 6);
    // Entries beyond max_paths are seen, not counted; lists beyond max_entries are cut.
    let cut = Policy {
        max_entries: 1,
        ..policy
    };
    let tree = fold(&entries[..2], &lines, &cut).finish(250_000, &cut);
    assert_eq!(tree.files.get(), 2);
    assert_eq!(tree.seen["paths"].get(), 250_000);
    assert_eq!(tree.seen["directories"].get(), 2);
    assert_eq!(tree.directories.len(), 1);
}

#[test]
fn decisions_vocabulary_and_checks_read_names_never_definitions_or_arguments() {
    let adr = "# ADR-0007: Pick one\n\n**Status:** Superseded by 0008 (2026-01-02, was \
               accepted 2025-12-01).\n";
    let other = "# ADR-0010: Accepted words in a title\n\nNo status here.\n";
    let contact = "# Contact ada@fixture.invalid\n\nStatus: accepted\n";
    let records = vec![
        measure::decision("docs/adr/0010-words.md", Some(other)),
        measure::decision("docs/adr/0007-pick.md", Some(adr)),
        measure::decision("docs/adr/0002-big.md", None),
        measure::decision("docs/adr/0003-contact.md", Some(contact)),
    ];
    let decisions = measure::decisions_section(records, &Policy::default());
    assert_eq!(decisions.count.get(), 4);
    assert_eq!(decisions.by_status["superseded"].get(), 1);
    assert_eq!(decisions.by_status["unknown"].get(), 1);
    assert_eq!(decisions.by_status["unreadable"].get(), 1);
    let numbers: Vec<&str> = decisions
        .records
        .iter()
        .map(|d| d.number.as_str())
        .collect();
    assert_eq!(numbers, ["0002", "0003", "0007", "0010"], "by number");
    let big = &decisions.records[0];
    assert_eq!(
        (big.title.as_deref(), big.status.as_deref()),
        (None, Some("unreadable"))
    );
    assert_eq!(
        decisions.records[1].title.as_deref(),
        Some("Contact ada@fixture.invalid"),
        "repository text as committed"
    );
    let seven = &decisions.records[2];
    assert_eq!(seven.title.as_deref(), Some("ADR-0007: Pick one"));
    assert_eq!(seven.status.as_deref(), Some("superseded"));
    assert_eq!(seven.date.as_deref(), Some("2026-01-02"));
    assert_eq!(
        decisions.records[3].status, None,
        "a title is not a status line"
    );
    let long = format!("# {}\n", "t".repeat(300));
    let cut = measure::decision("docs/adr/0001-long.md", Some(&long));
    assert_eq!(cut.title.as_deref().map(str::len), Some(256));
    assert!(cut.truncated);

    let context = "# Terms\n\n**Survey**:  \nOne record.\n**Project**:\nThe identity.\n\n\
                   **Project**:\nagain\n**Not a term** here:\n**Bold**: inline definition\n";
    let (terms, deadline) = measure::terms(context, &clock());
    let names: Vec<String> = terms.into_iter().map(|term| term.name).collect();
    assert_eq!(names, ["Project", "Survey"], "by name, each once");
    assert!(!deadline);
    let (unread, _) =
        measure::vocabulary_section("CONTEXT.md", true, None, &Policy::default(), &clock());
    assert!(unread.present && unread.terms.is_none() && unread.count.is_none());
    let (absent, _) =
        measure::vocabulary_section("CONTEXT.md", false, None, &Policy::default(), &clock());
    assert_eq!(absent.terms, Some(Vec::new()));

    let code_policy = r#"
schema = "af.code-task-policy/1"
[checks.kernel]
name = "kernel"
[checks.kernel.command]
program = "/usr/bin/make"
[[checks.kernel.command.args]]
value = "--secret-argument"
[checks.lint]
[checks.lint.command]
program = "npx"
"#;
    let pipeline = r#"
[[checks]]
name = "verify"
program = "bash"
args = [{ value = "scripts/verify.sh" }]
[[checks]]
name = "lint"
program = "cargo"
[[checks]]
name = "verify"
program = "second"
"#;
    let mut found = Found::default();
    for path in [
        "Cargo.toml",
        ".github/workflows/ci.yml",
        "sub/.nvmrc",
        ".node-version",
        "rust-toolchain",
    ] {
        found.add(path);
    }
    let (checks, deadline) = checks_of(
        &[
            (".af/code-policy.toml", Some(code_policy)),
            (".af/pipelines/review.toml", Some(pipeline)),
            (".af/pipelines/huge.toml", None),
        ],
        &found,
        &Policy::default(),
        &clock(),
    );
    assert!(!deadline);
    let declared: Vec<(String, &str)> = checks
        .declared
        .iter()
        .map(|c| (c.identity(), c.program.as_str()))
        .collect();
    assert_eq!(
        declared,
        [
            ("code-policy:kernel".to_string(), "make"),
            ("code-policy:lint".to_string(), "npx"),
            ("review.toml:lint".to_string(), "cargo"),
            ("review.toml:verify".to_string(), "bash"),
        ],
        "equal names in two sources are two checks; one per identity, the first declared"
    );
    assert_eq!(checks.unreadable, [".af/pipelines/huge.toml"]);
    assert_eq!(
        checks.toolchain,
        [".node-version", "rust-toolchain"],
        "the listed toolchain files, only at the root"
    );
    assert_eq!(checks.ci_workflows.get(), 1);
    let rendered = serde_json::to_string(&checks).unwrap();
    assert!(!rendered.contains("secret-argument") && !rendered.contains("verify.sh"));

    // Every list of the section is cut at max_entries, with what it saw.
    let one = Policy {
        max_entries: 1,
        ..Policy::default()
    };
    let (cut, _) = checks_of(
        &[
            (".af/pipelines/a.toml", None),
            (".af/pipelines/b.toml", None),
        ],
        &found,
        &one,
        &clock(),
    );
    assert_eq!(cut.unreadable, [".af/pipelines/a.toml"]);
    assert_eq!(cut.seen["unreadable"].get(), 2);
    assert_eq!(cut.toolchain, [".node-version"]);
    assert_eq!(cut.seen["toolchain"].get(), 2);
}

#[test]
fn a_status_line_names_the_first_status_its_first_date_and_whether_it_was_amended() {
    let status = |text: &str| {
        let decision = measure::decision("docs/adr/0001-x.md", Some(text));
        (decision.status, decision.date, decision.amended)
    };
    let owned = |status: &str, date: &str| (Some(status.to_string()), Some(date.to_string()));
    let two_dates = status("# T\n\n**Status:** accepted 2026-01-02, reviewed 2026-03-04\n");
    assert_eq!((two_dates.0, two_dates.1), owned("accepted", "2026-01-02"));
    let partial = status("# T\n\nStatus: accepted; superseded in part by 0009 (2026-05-06)\n");
    assert_eq!((partial.0, partial.1), owned("accepted", "2026-05-06"));
    assert!(!partial.2);
    let amended = status("# T\n\n**Status:** Accepted (2026-10-20), amended by ADR-0153.\n");
    assert_eq!(
        amended,
        (Some("accepted".into()), Some("2026-10-20".into()), true)
    );
    assert_eq!(
        status("# T\n\nStatus: proposed\n"),
        (Some("proposed".into()), None, false),
        "a missing date is unknown"
    );
    assert_eq!(
        status("# T\n\nStatus: proposed 2026-13-01\n"),
        (Some("proposed".into()), None, false),
        "a malformed date is unknown"
    );
    assert_eq!(
        status("# T\n\nThis was accepted on 2026-01-02.\n"),
        (None, None, false),
        "only a status line names a status"
    );
    assert_eq!(
        status("# T\n\nStatus: under discussion 2026-01-02\nStatus: accepted\n"),
        (None, Some("2026-01-02".into()), false),
        "the first status line decides; a status it does not name is unknown"
    );
    let unknown = measure::decisions_section(
        vec![measure::decision("docs/adr/0001-x.md", Some("# T\n"))],
        &Policy::default(),
    );
    assert_eq!(unknown.by_status["unknown"].get(), 1);
}

#[test]
fn shares_are_basis_points_of_their_population_rounded_down() {
    let cutoff = 1_000 * DAY;
    let mut walk = Walk::new(cutoff, 200_000);
    walk.add(commit(A, cutoff, "Ada", true, &[])).unwrap();
    walk.add(commit(B, cutoff, "Bob", false, &[])).unwrap();
    walk.add(commit(C, cutoff, "Cy", false, &[])).unwrap();
    let (contributors, _) = measure::contributors_section(&walk, &Policy::default(), &clock());
    assert_eq!(
        contributors.agent_share_bp.get(),
        3333,
        "one third of three"
    );
    assert_eq!(contributors.human_share_bp.get(), 6666);
    assert_eq!(contributors.top_share_bp.get(), 3333);
}

#[test]
fn every_list_is_ranked_before_its_cut_and_its_totals_cover_the_population() {
    let cutoff = 1_000 * DAY;
    let mut walk = Walk::new(cutoff, 200_000);
    // Ada has the most commits and the fewest lines; Bob more lines than Cy.
    walk.add(commit(A, cutoff, "Ada", false, &[("a", 1)]))
        .unwrap();
    walk.add(commit(B, cutoff, "Ada", false, &[("a", 1)]))
        .unwrap();
    walk.add(commit(C, cutoff, "Cy", false, &[("c", 5)]))
        .unwrap();
    walk.add(commit(A, cutoff, "Bob", false, &[("b", 50)]))
        .unwrap();
    let policy = Policy {
        max_entries: 2,
        ..Policy::default()
    };
    let (contributors, _) = measure::contributors_section(&walk, &policy, &clock());
    let kept: Vec<&str> = contributors
        .by_identity
        .iter()
        .map(|entry| entry.name.as_deref().unwrap())
        .collect();
    assert_eq!(kept, ["Ada", "Bob"], "commits first, then lines added");
    assert_eq!(contributors.seen["by_identity"].get(), 3);
    assert_eq!(contributors.identities.get(), 3);
    assert_eq!(
        contributors.human_commits.get(),
        4,
        "the totals cover all of them"
    );

    // Release tags: by date, newest first, undated last, then name.
    let mut refs = measure::Refs::default();
    let all = Policy::default();
    refs.add("refs/tags/v1", Some(100), &all);
    refs.add("refs/tags/v0", None, &all);
    refs.add("refs/tags/v3", Some(300), &all);
    refs.add("refs/tags/v2b", Some(200), &all);
    refs.add("refs/tags/v2a", Some(200), &all);
    refs.add("refs/tags/nightly", Some(900), &all);
    refs.add("refs/heads/main", Some(900), &all);
    let mut history = measure::history_section(
        &walk,
        4,
        Vec::new(),
        0,
        &BTreeSet::new(),
        &Policy::default(),
    );
    refs.fill(&mut history, &policy);
    let tags = history.tags.unwrap();
    assert_eq!(tags.count.get(), 6);
    let release: Vec<&str> = tags.release.iter().map(|tag| tag.name.as_str()).collect();
    assert_eq!(release, ["v3", "v2a"], "ranked, then cut at max_entries");
    assert_eq!(history.seen["tags"].get(), 5);
    assert_eq!(history.local_branches.unwrap().get(), 1);
    let mut refs = measure::Refs::default();
    for (name, date) in [("v1", Some(1)), ("v0", None), ("v9", Some(9))] {
        refs.add(&format!("refs/tags/{name}"), date, &all);
    }
    let mut history = measure::history_section(&walk, 4, Vec::new(), 0, &BTreeSet::new(), &all);
    refs.fill(&mut history, &all);
    let release: Vec<String> = history
        .tags
        .unwrap()
        .release
        .into_iter()
        .map(|tag| tag.name)
        .collect();
    assert_eq!(release, ["v9", "v1", "v0"], "undated last");
}

#[test]
fn each_file_counts_for_one_language_the_first_configured_pattern_bytewise() {
    let table: toml::Value = toml::from_str(
        "[languages]\n\"**/*.rs\" = \"rust-any\"\n\"generated/**\" = \"generated\"\n\
         \"*.md\" = \"docs\"\n",
    )
    .unwrap();
    let policy = Policy::from_effective(Some(&table)).unwrap();
    assert_eq!(
        policy.language("generated/x.rs"),
        "rust-any",
        "`**/*.rs` < `generated/**`"
    );
    assert_eq!(policy.language("generated/x.txt"), "generated");
    assert_eq!(policy.language("src/lib.rs"), "rust-any");
    assert_eq!(
        policy.language("lib.rs"),
        "rust-any",
        "`**` spans no segment too"
    );
    assert_eq!(
        policy.language("docs/README.md"),
        "docs",
        "a pattern without `/` matches the file name, over the built-in map"
    );
    assert_eq!(
        policy.language("Makefile"),
        "make",
        "the built-in map stays"
    );
    assert_eq!(policy.language("data.xyz"), "other", "nothing matches");
    let mut fold = TreeFold::default();
    for path in ["generated/x.rs", "generated/x.txt", "README.md", "data.xyz"] {
        assert!(fold.add(
            &Entry {
                kind: EntryKind::File,
                oid: A.into(),
                size: 1,
                path: path.into(),
            },
            &policy,
            &clock(),
        ));
    }
    let tree = fold.finish(4, &policy);
    let languages: Vec<(&str, u64)> = tree
        .languages
        .iter()
        .map(|(name, language)| (name.as_str(), language.files.get()))
        .collect();
    assert_eq!(
        languages,
        [("docs", 1), ("generated", 1), ("other", 1), ("rust-any", 1)],
        "each file once"
    );
}

#[test]
fn a_passed_deadline_stops_every_loop_after_the_streams() {
    // The ranking of identities.
    let cutoff = 1_000 * DAY;
    let mut walk = Walk::new(cutoff, 200_000);
    walk.add(commit(A, cutoff, "Ada", false, &[])).unwrap();
    walk.add(commit(B, cutoff, "Bob", false, &[])).unwrap();
    let (contributors, deadline) =
        measure::contributors_section(&walk, &Policy::default(), &passed());
    assert!(deadline);
    assert!(contributors.by_identity.is_empty());
    assert_eq!(
        contributors.seen["by_identity"].get(),
        2,
        "it says how many there were"
    );
    assert_eq!(contributors.identities.get(), 2);
    // The reading of terms and of declarations.
    let (terms, deadline) = measure::terms("**A**:\n**B**:\n", &passed());
    assert!(deadline && terms.is_empty());
    let (vocabulary, deadline) = measure::vocabulary_section(
        "CONTEXT.md",
        true,
        Some("**A**:\n"),
        &Policy::default(),
        &passed(),
    );
    assert!(deadline && vocabulary.terms == Some(Vec::new()));
    let (checks, deadline) = checks_of(
        &[(".af/pipelines/a.toml", None)],
        &Found::default(),
        &Policy::default(),
        &passed(),
    );
    assert!(deadline && checks.unreadable.is_empty());
}

#[test]
fn a_failure_reason_is_one_line_without_an_email() {
    assert_eq!(public_reason("git log\nfailed"), "git log failed");
    assert!(!public_reason("ada@example.invalid broke it").contains('@'));
    assert!(public_reason(&"x".repeat(2000)).len() <= 512);
}

#[test]
fn a_section_cut_by_a_bound_names_it_and_the_record_folds_partial() {
    use review_core::project::{SurveyCompletenessV1 as C, SurveySectionV1};
    let cut: SurveySectionV1<()> = SurveySectionV1::unavailable(SurveyReasonV1::MaxWallMs);
    assert_eq!(cut.completeness, C::Unavailable);
    assert_eq!(C::fold([C::Complete, C::Unavailable]), C::Partial);
    assert_eq!(C::fold([C::Unavailable, C::Unavailable]), C::Unavailable);
    assert_eq!(C::fold([C::Complete, C::Partial]), C::Partial);
    assert_eq!(C::fold([C::Complete, C::Complete]), C::Complete);
}

#[test]
fn the_surveyor_starts_no_process_outside_the_supervised_git() {
    let sources = [
        ("survey.rs", include_str!("../survey.rs")),
        ("survey/measure.rs", include_str!("measure.rs")),
        ("survey/policy.rs", include_str!("policy.rs")),
    ];
    // Spelled in pieces, so this test does not find itself.
    let unsupervised = [
        concat!(".out", "put()"),
        concat!(".sta", "tus()"),
        concat!(".spa", "wn()"),
        concat!("Command::", "new"),
        concat!("std::", "process"),
    ];
    for (name, source) in sources {
        for needle in unsupervised {
            assert!(!source.contains(needle), "{name} contains `{needle}`");
        }
    }
    let measure = include_str!("measure.rs");
    assert!(measure.contains("run_within") && measure.contains("stream_within"));
}

#[test]
fn the_ceilings_are_only_lowered_and_every_other_value_is_refused_naming_the_key() {
    for (key, ceiling) in [
        ("max_paths", 200_000_i64),
        ("max_entries", 1_000),
        ("max_survey_bytes", 1_048_576),
    ] {
        let load = |value: &str| {
            let table: toml::Value = toml::from_str(&format!("{key} = {value}\n")).unwrap();
            Policy::from_effective(Some(&table))
        };
        assert!(load(&ceiling.to_string()).is_ok(), "{key} at its ceiling");
        assert!(load("1").is_ok(), "{key} lowered to one");
        for refused in [
            "0".to_string(),
            "-1".into(),
            (ceiling + 1).to_string(),
            i64::MAX.to_string(),
            "\"99999999999999999999\"".into(),
            "1.5".into(),
        ] {
            let error = load(&refused).unwrap_err();
            assert!(
                error.contains(&format!(
                    "{key} is a positive integer at most its ceiling {ceiling}"
                )),
                "{key} = {refused}: {error}"
            );
        }
    }
}

#[test]
fn a_blank_or_control_character_name_is_hashed_by_name_under_both_policies() {
    use SurveyIdentityPolicyV1::{EmailHash, Name};
    for name in ["", "   ", "Bell\u{7}Name", "Tab\tName"] {
        let hashed = format!("h:{}", hash16(name));
        assert_eq!(
            identity_of(name.as_bytes(), "ada@example.invalid", Name),
            (hashed.clone(), Some(hashed.clone()), false),
            "{name:?}"
        );
        // Never the email's hash: grouped by the name, as under `name`.
        assert_eq!(
            identity_of(name.as_bytes(), "ada@example.invalid", EmailHash),
            (hashed, None, false),
            "{name:?}"
        );
    }
    assert_eq!(hash16(""), hash16("   "), "blank is blank");
    assert_eq!(
        identity_of(b"Ada", "ada@example.invalid", EmailHash).0,
        hash16("ada@example.invalid"),
        "a name a record can hold is keyed by the email under email_hash"
    );
}

#[test]
fn a_decision_date_must_be_a_day_of_the_calendar() {
    let date = |line: &str| {
        measure::decision("docs/adr/0001-x.md", Some(&format!("# T\n\n{line}\n"))).date
    };
    assert_eq!(date("Status: accepted 2026-02-31"), None);
    assert_eq!(date("Status: accepted 2025-02-29"), None);
    assert_eq!(
        date("Status: accepted 2024-02-29"),
        Some("2024-02-29".into())
    );
    assert_eq!(date("Status: accepted 2026-04-31"), None);
    assert_eq!(
        date("Status: accepted 2026-04-30"),
        Some("2026-04-30".into())
    );
}

#[test]
fn a_deadline_during_language_matching_stops_within_one_match() {
    let languages: String = (0..64)
        .map(|n| format!("\"nothing{n:02}/**\" = \"l{n}\"\n"))
        .collect();
    let table: toml::Value = toml::from_str(&format!("[languages]\n{languages}")).unwrap();
    let policy = Policy::from_effective(Some(&table)).unwrap();
    // The deadline passes during the third pattern's match: nothing after it is matched.
    let mut asked = 0;
    let language = policy.language_within("src/lib.rs", || {
        asked += 1;
        asked > 3
    });
    assert_eq!(language, None, "no language past the deadline");
    assert_eq!(asked, 4, "three patterns matched, the fourth never begun");
    let mut asked = 0;
    assert_eq!(
        policy
            .language_within("src/lib.rs", || {
                asked += 1;
                false
            })
            .as_deref(),
        Some("rust"),
        "the built-in fallback is a lookup, after every configured pattern"
    );
    assert_eq!(asked, 64);
    // The tree folds no entry whose language the deadline cut.
    let mut fold = TreeFold::default();
    let entry = Entry {
        kind: EntryKind::File,
        oid: A.into(),
        size: 1,
        path: "src/lib.rs".into(),
    };
    assert!(!fold.add(&entry, &policy, &passed()));
    assert_eq!(fold.files(), 0);
}

#[test]
fn a_language_map_is_a_pattern_list_of_at_most_sixty_four() {
    let load = |count: usize| {
        let languages: String = (0..count)
            .map(|n| format!("\"*.x{n}\" = \"l{n}\"\n"))
            .collect();
        let table: toml::Value = toml::from_str(&format!("[languages]\n{languages}")).unwrap();
        Policy::from_effective(Some(&table)).and_then(within_pattern_limits)
    };
    assert!(load(64).is_ok());
    let error = load(65).unwrap_err();
    assert!(
        error.contains("[survey.languages] holds 65 patterns, more than 64"),
        "{error}"
    );
}

#[test]
fn the_ref_observation_yields_the_key_and_the_statistics_from_the_same_lines() {
    let policy = Policy::default();
    let mut refs = measure::Refs::default();
    for line in [
        format!("refs/heads/main\t{A}\tcommit\t100"),
        format!("refs/tags/v1\t{B}\ttag\t300"),
        format!("refs/tags/v0\t{C}\tcommit\t200"),
        format!("refs/tags/vtree\t{C}\ttree\t"),
    ] {
        refs.line(line.as_bytes(), &policy);
    }
    let walk = Walk::new(1_000 * DAY, 200_000);
    let mut history = measure::history_section(&walk, 0, Vec::new(), 0, &BTreeSet::new(), &policy);
    refs.fill(&mut history, &policy);
    assert_eq!(history.local_branches.unwrap().get(), 1);
    let tags = history.tags.unwrap();
    assert_eq!(tags.count.get(), 3);
    let release: Vec<&str> = tags.release.iter().map(|tag| tag.name.as_str()).collect();
    assert_eq!(
        release,
        ["v1", "v0", "vtree"],
        "the tagger time of the annotated tag, the commit's time, an undated tree last"
    );
    assert!(measure::REF_FORMAT.contains("%(objecttype)"));
}

#[test]
fn every_section_checks_the_clock_after_its_finishing_work() {
    assert!(!clock().section("tree").finished());
    assert!(passed().section("tree").finished());
}

#[test]
fn every_git_the_surveyor_starts_ends_by_the_lesser_of_its_limit_and_the_survey() {
    use std::time::Duration;
    let limit = Duration::from_secs(review_source_git::DEFAULT_GIT_TIMEOUT_SECONDS);
    assert_eq!(
        crate::projects::Git::within(Duration::from_millis(1500)),
        Duration::from_millis(1500)
    );
    assert!(crate::projects::Git::within(Duration::from_secs(86_400)) <= limit);
}

#[test]
fn a_replay_refusal_names_the_sequence_the_field_and_what_it_differs_from() {
    let unbound = Unbound {
        sequence: 7,
        field: "refs_digest",
        against: "tombstone SurveyCollected@1 at sequence 9".into(),
    };
    assert_eq!(
        unbound.to_string(),
        "SurveyRecorded@1 at sequence 7 names a refs_digest its tombstone SurveyCollected@1 at \
         sequence 9 does not record"
    );
}

#[test]
fn a_control_character_past_the_display_bound_hashes_the_whole_name_under_both_policies() {
    use SurveyIdentityPolicyV1::{EmailHash, Name};
    // 256 printable bytes, then a control character: the cut would keep only the printable part.
    for tail in ["\u{7}", "\u{7}more", "\n"] {
        let name = format!("{}{tail}", "n".repeat(256));
        let hashed = format!("h:{}", hash16(&name));
        assert_eq!(
            identity_of(name.as_bytes(), "", Name),
            (hashed.clone(), Some(hashed.clone()), false),
            "{tail:?}"
        );
        assert_eq!(
            identity_of(name.as_bytes(), "n@x.invalid", EmailHash),
            (hashed, None, false),
            "{tail:?}"
        );
    }
    // Without the control character the same name is its own and is cut, never hashed.
    let printable = "n".repeat(257);
    let (key, cut, truncated) = identity_of(printable.as_bytes(), "", Name);
    assert!(!key.starts_with("h:") && truncated);
    assert_eq!(cut.as_deref(), Some(&printable[..256]));
}

/// A recorded Survey's record as the fixture's expected record holds it, with `previous`.
fn fixture_survey(previous: Option<&str>) -> ProjectSurveyV1 {
    let text = include_str!("../../../tests/fixtures/project-survey/expected.json")
        .replace("GIT_VERSION", "git version 2.0.0");
    let mut survey: ProjectSurveyV1 = serde_json::from_str(&text).unwrap();
    survey.previous_survey_id = previous.map(str::to_string);
    survey.validate().unwrap();
    survey
}

/// A Project Store below `home` holding two recorded Surveys, oldest first.
fn store_with_two_surveys(home: &std::path::Path) -> (ProjectStore, [Recorded; 2]) {
    let mut store =
        ProjectStore::open_unrecorded(home, &format!("sha256:{}", "e".repeat(64))).unwrap();
    let first = fixture_survey(None);
    let first = store
        .record_survey(&first, record_bytes(&first).unwrap())
        .unwrap();
    let second = fixture_survey(Some(&first.event.survey_id));
    let second = store
        .record_survey(&second, record_bytes(&second).unwrap())
        .unwrap();
    (store, [first, second])
}

/// The CAS file of `survey_id`, made writable.
fn object_of(store: &ProjectStore, survey_id: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let object = collect::object_path(&store.directory, survey_id);
    std::fs::set_permissions(&object, std::fs::Permissions::from_mode(0o644)).unwrap();
    object
}

#[test]
fn an_unreadable_record_refuses_replay_naming_the_sequence_and_the_error_and_no_field() {
    let home = tempfile::tempdir().unwrap();
    let (store, [first, second]) = store_with_two_surveys(home.path());
    let sequence_of = |recorded: &Recorded| store.recorded_sequence(&recorded.event).unwrap();
    let (first_at, second_at) = (sequence_of(&first), sequence_of(&second));
    assert!(first_at < second_at);

    // The newest record corrupt and the older one gone, neither collected.
    let newest_object = object_of(&store, &second.event.survey_id);
    std::fs::write(&newest_object, b"{\"schema\":\"not a survey\"}").unwrap();
    std::fs::remove_file(object_of(&store, &first.event.survey_id)).unwrap();
    let corrupt = format!(
        "SurveyRecorded@1 at sequence {second_at} names Survey {}, whose record cannot be read: \
         its bytes do not match its digest",
        second.event.survey_id
    );
    let log = store.log().unwrap();
    for refused in [
        newest(&store, &log).map(|_| ()).unwrap_err(),
        store.verify_chain().unwrap_err(),
    ] {
        assert!(refused.contains(&corrupt), "{refused}");
        assert!(!refused.contains(&second.event.head[..12]), "{refused}");
        assert!(!refused.contains("not a survey"), "{refused}");
    }

    // The newest readable again: the history still refuses the older one, gone from the CAS,
    // while the newest Survey is read.
    std::fs::write(&newest_object, &second.bytes).unwrap();
    let refused = store.verify_chain().unwrap_err();
    assert!(
        refused.contains(&format!(
            "SurveyRecorded@1 at sequence {first_at} names Survey {}, whose record cannot be \
             read: it is not in the CAS",
            first.event.survey_id
        )),
        "{refused}"
    );
    assert_eq!(
        newest(&store, &log).unwrap().unwrap().event,
        second.event,
        "the newest Survey is read"
    );
}

#[test]
fn a_parse_error_names_its_place_and_never_the_bytes() {
    let said = parse_record(b"{\"schema\": \"carol@example.invalid\"}").unwrap_err();
    assert!(
        said.starts_with("its bytes are not a Survey record: a data error at line 1"),
        "{said}"
    );
    assert!(!said.contains("carol"), "{said}");
    let said = parse_record(b"not json").unwrap_err();
    assert!(
        said.contains("a syntax error at line 1, column 2"),
        "{said}"
    );
}

#[test]
fn a_tombstone_binds_the_records_identity_and_refuses_an_event_that_disagrees() {
    let home = tempfile::tempdir().unwrap();
    let (mut store, [first, second]) = store_with_two_surveys(home.path());
    // Collection copies the identity from the record once the event is verified against it.
    let identity = TombstoneIdentity::of_record(&second.event, &second.bytes)
        .unwrap()
        .unwrap();
    let recorded = second.survey.identity.body.as_ref().unwrap();
    assert_eq!(
        identity,
        TombstoneIdentity {
            head: recorded.head.clone(),
            cutoff_unix_s: recorded.cutoff_unix_s,
            completeness: second.survey.completeness,
            refs_digest: recorded.refs_digest.clone(),
            policy_digest: recorded.policy_digest.clone(),
            surveyor_version: recorded.surveyor_version,
            previous_survey_id: Some(first.event.survey_id.clone()),
        }
    );
    // An event that disagrees with its record is refused at collection, naming the field.
    let digest = format!("sha256:{}", "0".repeat(64));
    let other = SurveyRecordedPayloadV1 {
        refs_digest: digest.clone(),
        ..second.event.clone()
    };
    assert_eq!(
        TombstoneIdentity::of_record(&other, &second.bytes).unwrap(),
        Err("refs_digest")
    );
    // Bytes that are no Survey vouch for nothing.
    assert!(TombstoneIdentity::of_record(&second.event, b"[]").is_err());

    // Every identity field the tombstone binds is compared with the event.
    let tombstone = SurveyCollectedPayloadV1 {
        survey_id: second.event.survey_id.clone(),
        head: identity.head.clone(),
        cutoff_unix_s: identity.cutoff_unix_s,
        completeness: identity.completeness,
        refs_digest: identity.refs_digest.clone(),
        policy_digest: identity.policy_digest.clone(),
        surveyor_version: identity.surveyor_version,
        previous_survey_id: identity.previous_survey_id.clone(),
        tree: None,
        history: None,
        unreadable: None,
        collected_unix_ms: 1.into(),
    };
    tombstone.validate().unwrap();
    assert_eq!(untombstoned_field(&second.event, &tombstone), None);
    let event = &second.event;
    for (changed, field) in [
        (
            SurveyRecordedPayloadV1 {
                head: A.into(),
                ..event.clone()
            },
            "head",
        ),
        (
            SurveyRecordedPayloadV1 {
                refs_digest: digest.clone(),
                ..event.clone()
            },
            "refs_digest",
        ),
        (
            SurveyRecordedPayloadV1 {
                cutoff_unix_s: 1.into(),
                ..event.clone()
            },
            "cutoff_unix_s",
        ),
        (
            SurveyRecordedPayloadV1 {
                completeness: SurveyCompletenessV1::Partial,
                ..event.clone()
            },
            "completeness",
        ),
        (
            SurveyRecordedPayloadV1 {
                policy_digest: digest.clone(),
                ..event.clone()
            },
            "policy_digest",
        ),
        (
            SurveyRecordedPayloadV1 {
                surveyor_version: 99.into(),
                ..event.clone()
            },
            "surveyor_version",
        ),
        (
            SurveyRecordedPayloadV1 {
                previous_survey_id: None,
                ..event.clone()
            },
            "previous_survey_id",
        ),
    ] {
        assert_eq!(untombstoned_field(&changed, &tombstone), Some(field));
    }

    // Collected, the newest Survey is none once its event is bound to its tombstone; an event
    // whose head differs from its tombstone is refused before the head is used.
    store
        .events
        .append(
            collect::COLLECTION_RUN,
            &store.cas,
            review_store::NewEvent::new(
                EventType::SurveyCollectedV1,
                serde_json::to_value(&tombstone).unwrap(),
            )
            .at(review_core::task::collection::collected_time(1)),
        )
        .unwrap();
    let mut log = store.log().unwrap();
    assert!(newest(&store, &log).unwrap().is_none());
    assert_eq!(store.collected(&second.event).unwrap(), Some(tombstone));
    store.verify_chain().unwrap();
    let Some(Outcome::Recorded(newest_event)) = log.surveys.last_mut() else {
        panic!("the newest Survey");
    };
    newest_event.head = A.into();
    let refused = newest(&store, &log).map(|_| ()).unwrap_err();
    assert!(
        refused.contains("names a head its tombstone SurveyCollected@1 at sequence"),
        "{refused}"
    );
    assert_eq!(store.collected(&first.event).unwrap(), None);
}

#[test]
fn an_unreadable_record_refusal_names_the_sequence_the_survey_and_the_error() {
    let unreadable = Unreadable {
        sequence: 4,
        survey_id: format!("sha256:{}", "a".repeat(64)),
        error: "its bytes do not match its digest".into(),
    };
    assert_eq!(
        unreadable.to_string(),
        format!(
            "SurveyRecorded@1 at sequence 4 names Survey sha256:{}, whose record cannot be read: \
             its bytes do not match its digest",
            "a".repeat(64)
        )
    );
}
