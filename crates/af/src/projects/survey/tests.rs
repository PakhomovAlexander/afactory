use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use review_core::project::{SurveyIdentityPolicyV1, SurveyReasonV1};
use serde_json::json;

use super::measure::{
    self, Change, Clock, Commit, Ended, Entry, EntryKind, Found, Object, Walk, hash16, identity_of,
    read_batch, read_listing, read_log,
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
            "[languages]\n\"src/*.rs\" = \"rust\"\n",
            "a key is a file name or extension",
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
        entries.push(entry)
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
        read_listing(&mut listing.as_bytes(), &passed, |_| none += 1),
        Ended::Deadline
    );
    assert_eq!(none, 0);

    let batch = format!("{A} blob 3\na\nb\n{B} missing\n{C} blob 4\nx\0y\n\n");
    let mut objects: Vec<(usize, Option<Object>)> = Vec::new();
    let ended = read_batch(&mut batch.as_bytes(), &clock(), Some(3), |index, object| {
        objects.push((index, object))
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
        read_batch(&mut short.as_bytes(), &clock(), None, |_, _| {}),
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
        commits.push(commit)
    });
    assert_eq!(ended, Ended::Done);
    assert_eq!(commits.len(), 3);
    assert!(commits[0].merge && commits[0].changes.is_empty());
    assert_eq!(commits[0].identity, "Ada");
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
        commits.push(commit)
    });
    assert!(!commits[0].agent);
    let excluded = Policy {
        exclude: vec!["*.png".into()],
        ..Policy::default()
    };
    let mut commits = Vec::new();
    read_log(&mut log.as_bytes(), &clock(), &excluded, |commit| {
        commits.push(commit)
    });
    assert_eq!(commits[1].changes.len(), 1);
}

#[test]
fn no_author_email_is_kept_and_an_email_shaped_name_is_hashed_under_both_policies() {
    use sha2::{Digest, Sha256};
    let expected = &review_core::hex::encode(&Sha256::digest(b"ada@example.invalid"))[..16];
    assert_eq!(hash16("  Ada@Example.Invalid \n"), expected);
    assert_eq!(
        identity_of(
            "Ada",
            " Ada@Example.Invalid",
            SurveyIdentityPolicyV1::EmailHash
        ),
        (expected.to_string(), false)
    );
    assert_eq!(
        identity_of(" Ada ", "ada@example.invalid", SurveyIdentityPolicyV1::Name),
        ("Ada".to_string(), false)
    );
    // An address written as the name is `h:` and the hash of the name, never text.
    let named = identity_of("Ada@Example.invalid", "", SurveyIdentityPolicyV1::Name);
    assert_eq!(named, (format!("h:{expected}"), false));
    assert_eq!(
        identity_of(
            "ada@example.invalid",
            "bob@x.invalid",
            SurveyIdentityPolicyV1::EmailHash
        )
        .0,
        hash16("bob@x.invalid")
    );
    // An `@` that is not an address is a name; a name is cut and marked, never hashed for it.
    assert_eq!(
        identity_of("Ada @ home", "", SurveyIdentityPolicyV1::Name),
        ("Ada @ home".to_string(), false)
    );
    let long = format!("Ada {}", "é".repeat(148));
    assert_eq!(long.len(), 300);
    let (cut, truncated) = identity_of(&long, "", SurveyIdentityPolicyV1::Name);
    assert!(truncated);
    assert!(cut.len() <= 256 && long.starts_with(&cut), "{}", cut.len());
    assert!(cut.is_char_boundary(cut.len()));
    let policy = Policy {
        identity: SurveyIdentityPolicyV1::EmailHash,
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
        identity: identity.into(),
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
    let mut walk = Walk::new(cutoff);
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
        walk.add(commit);
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

    let contributors = measure::contributors_section(&walk, &Policy::default());
    assert_eq!(contributors.identities.get(), 3);
    assert_eq!(contributors.agent_commits.get(), 1);
    assert_eq!(contributors.human_commits.get(), 4);
    assert_eq!(contributors.agent_share_bp.get(), 2000);
    assert_eq!(contributors.top_share_bp.get(), 6000);
    let ada = &contributors.by_identity["Ada"];
    assert_eq!(
        (ada.commits.get(), ada.added.get(), ada.removed.get()),
        (3, 13, 3)
    );
    assert_eq!(contributors.by_identity["Bob"].added.get(), 0, "a merge");
    assert!(ada.active_90d);

    // An empty window and an empty walk make every share 0.
    let empty = measure::contributors_section(&Walk::new(cutoff), &Policy::default());
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
    let mut walk = Walk::new(cutoff);
    walk.add(commit(
        A,
        cutoff,
        "Ada",
        false,
        &[("a/1", 1), ("b/2", 1), ("c/3", 1), ("d/4", 1), ("e/5", 1)],
    ));
    walk.add(commit(B, cutoff, "Bob", false, &[]));
    walk.add(commit(C, cutoff, "Cy", false, &[]));
    let (hot, distinct) = walk.hot_paths(policy.hot_paths.min(policy.max_entries));
    assert_eq!((hot.len(), distinct), (2, 5));
    let history = measure::history_section(&walk, 3, hot, distinct, &BTreeSet::new(), &policy);
    assert_eq!(history.churn_90d.len(), 2);
    assert_eq!(history.seen["churn_90d"].get(), 5);
    assert_eq!(history.seen["hot_paths"].get(), 5);
    let contributors = measure::contributors_section(&walk, &policy);
    assert_eq!(contributors.by_identity.len(), 2);
    assert!(contributors.by_identity.contains_key("Ada"), "most commits");
    assert_eq!(contributors.identities.get(), 3);
    assert_eq!(contributors.seen["by_identity"].get(), 3);

    let records = (1..=3)
        .map(|n| measure::decision(&format!("docs/adr/000{n}-x.md"), Some(&b"# T\n"[..])))
        .collect();
    let decisions = measure::decisions_section(records, &policy);
    assert_eq!(decisions.count.get(), 3);
    assert_eq!(decisions.records.len(), 2);
    assert_eq!(decisions.seen["records"].get(), 3);
    let vocabulary = measure::vocabulary_section(
        "CONTEXT.md",
        true,
        Some(&b"**A**:\n**B**:\n**C**:\n"[..]),
        &policy,
    );
    assert_eq!(vocabulary.count.unwrap().get(), 3);
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
    let tree = measure::tree_section(&entries, &lines, 6, &policy);
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
    let unread = measure::tree_section(&entries, &BTreeMap::new(), 6, &policy);
    assert_eq!(unread.lines.get(), 0);
    assert_eq!(unread.files_without_lines.get(), 6);
    // Entries beyond max_paths are seen, not counted; lists beyond max_entries are cut.
    let cut = Policy {
        max_entries: 1,
        ..policy
    };
    let tree = measure::tree_section(&entries[..2], &lines, 250_000, &cut);
    assert_eq!(tree.files.get(), 2);
    assert_eq!(tree.seen["paths"].get(), 250_000);
    assert_eq!(tree.seen["directories"].get(), 2);
    assert_eq!(tree.directories.len(), 1);
}

#[test]
fn decisions_vocabulary_and_checks_read_names_never_definitions_or_arguments() {
    let adr = b"# ADR-0007: Pick one\n\n**Status:** Superseded by 0008 (2026-01-02, was \
                accepted 2025-12-01).\n";
    let other = b"# ADR-0010: Accepted words in a title\n\nNo status here.\n";
    let contact = b"# Contact ada@fixture.invalid\n\nStatus: accepted\n";
    let records = vec![
        measure::decision("docs/adr/0010-words.md", Some(&other[..])),
        measure::decision("docs/adr/0007-pick.md", Some(&adr[..])),
        measure::decision("docs/adr/0002-big.md", None),
        measure::decision("docs/adr/0003-contact.md", Some(&contact[..])),
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
    let cut = measure::decision("docs/adr/0001-long.md", Some(long.as_bytes()));
    assert_eq!(cut.title.as_deref().map(str::len), Some(256));
    assert!(cut.truncated);

    let context = b"# Terms\n\n**Project**:\nThe identity.\n\n**Survey**:  \nOne record.\n\
                    **Project**:\nagain\n**Not a term** here:\n**Bold**: inline definition\n";
    let names: Vec<String> = measure::terms(context)
        .into_iter()
        .map(|term| term.name)
        .collect();
    assert_eq!(names, ["Project", "Survey"]);
    let unread = measure::vocabulary_section("CONTEXT.md", true, None, &Policy::default());
    assert!(unread.present && unread.terms.is_none() && unread.count.is_none());
    let absent = measure::vocabulary_section("CONTEXT.md", false, None, &Policy::default());
    assert_eq!(absent.terms, Some(Vec::new()));

    let code_policy = br#"
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
    let pipeline = br#"
[[checks]]
name = "verify"
program = "bash"
args = [{ value = "scripts/verify.sh" }]
"#;
    let mut found = Found::default();
    for path in ["Cargo.toml", ".github/workflows/ci.yml", "sub/go.mod"] {
        found.add(path);
    }
    let checks = measure::checks_section(
        &[
            (".af/code-policy.toml", Some(&code_policy[..])),
            (".af/pipelines/review.toml", Some(&pipeline[..])),
            (".af/pipelines/huge.toml", None),
        ],
        &found,
        &Policy::default(),
    );
    let declared: Vec<(&str, &str, &str)> = checks
        .declared
        .iter()
        .map(|c| (c.source.as_str(), c.name.as_str(), c.program.as_str()))
        .collect();
    assert_eq!(
        declared,
        [
            (".af/code-policy.toml", "kernel", "make"),
            (".af/code-policy.toml", "lint", "npx"),
            (".af/pipelines/review.toml", "verify", "bash"),
        ]
    );
    assert_eq!(checks.unreadable, [".af/pipelines/huge.toml"]);
    assert_eq!(checks.toolchain, ["Cargo.toml"], "only at the root");
    assert_eq!(checks.ci_workflows.get(), 1);
    let rendered = serde_json::to_string(&checks).unwrap();
    assert!(!rendered.contains("secret-argument") && !rendered.contains("verify.sh"));
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
