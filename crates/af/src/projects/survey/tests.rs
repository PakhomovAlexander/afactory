use std::collections::{BTreeMap, BTreeSet};

use review_core::project::{SurveyIdentityPolicyV1, SurveyReasonV1};
use serde_json::json;

use super::glob::{name_matches, path_matches, validate_path_glob};
use super::measure::{
    self, Change, Commit, Entry, EntryKind, hash16, identity_of, parse_batch, parse_listing,
    parse_log,
};
use super::policy::Policy;
use super::*;

const A: &str = "1111111111111111111111111111111111111111";
const B: &str = "2222222222222222222222222222222222222222";
const C: &str = "3333333333333333333333333333333333333333";

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
fn path_globs_match_by_segment_with_classes_and_name_globs_take_brackets_literally() {
    let decisions = "docs/adr/[0-9][0-9][0-9][0-9]-*.md";
    assert!(path_matches(decisions, "docs/adr/0152-measure.md"));
    assert!(!path_matches(decisions, "docs/adr/README.md"));
    assert!(!path_matches(decisions, "docs/adr/015-short.md"));
    assert!(!path_matches(decisions, "docs/adr/old/0001-a.md"));
    assert!(path_matches("**/tests/**", "crates/af/tests/it/main.rs"));
    assert!(path_matches("**/tests/**", "tests/a"));
    assert!(!path_matches("**/tests/**", "crates/tests.rs"));
    assert!(path_matches("**/*_test.*", "pkg/a_test.go"));
    assert!(path_matches("**/test_*", "test_a.py"));
    assert!(path_matches("vendor/**", "vendor/x/y.c"));
    assert!(path_matches("[!.]*", "src"));
    assert!(!path_matches("[!.]*", ".github"));
    assert!(path_matches("[a-c-]x", "-x"));
    for broken in ["[0-9", "/abs", "a//b", "[z-a]"] {
        assert!(validate_path_glob(broken).is_err(), "{broken}");
        assert!(!path_matches(broken, "anything"), "{broken}");
    }
    assert!(name_matches("*[bot]", "dependabot[bot]"));
    assert!(!name_matches("*[bot]", "robot"));
    assert!(name_matches(
        "*Claude*",
        "Claude Opus 5.5 <noreply@anthropic.com>"
    ));
    assert!(!name_matches("*Claude*", "claude"));
    assert!(name_matches("v*", "v1.0.0"));
    assert!(!name_matches("v*", "release-1"));
    assert!(name_matches("v?.*", "v1.2"));
}

#[test]
fn the_policy_has_the_documented_defaults_refuses_unknown_keys_and_extends_languages() {
    let defaults = Policy::from_effective(None).unwrap();
    assert_eq!(defaults, Policy::default());
    assert_eq!(defaults.max_wall_ms, 60_000);
    assert_eq!(defaults.max_commits, 100_000);
    assert_eq!(defaults.max_file_bytes, 1_048_576);
    assert_eq!(defaults.hot_paths, 20);
    assert_eq!(defaults.identity, SurveyIdentityPolicyV1::Name);
    let document = defaults.document();
    assert_eq!(document["max_commits"], "100000", "decimal text");
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
        "max_commits = 1\nidentity = \"email_hash\"\n[languages]\n\"*.pest\" = \"pest\"\n\
         \"*.rs\" = \"rust-2024\"\n",
    )
    .unwrap();
    let policy = Policy::from_effective(Some(&table)).unwrap();
    assert_eq!(policy.max_commits, 1);
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
        ("max_wall_ms = -1\n", "not negative"),
        ("identity = \"email\"\n", "the identity policies are closed"),
        ("decisions = \"docs/[0-9\"\n", "a glob that does not close"),
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
        assert!(!error.contains('/') || broken.contains('/'), "{error}");
    }
}

#[test]
fn listings_batches_and_logs_parse_from_gits_bytes() {
    let listing = format!(
        "100644 blob {A}      12\tREADME.md\0100755 blob {B}       3\tbin/run\0\
         120000 blob {C}       9\tlink\0160000 commit {A}       -\tvendor/sub\0"
    );
    let entries = parse_listing(listing.as_bytes()).unwrap();
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
    let batch = format!("{A} blob 3\na\nb\n{B} missing\n{C} blob 0\n\n");
    let blobs = parse_batch(batch.as_bytes()).unwrap();
    assert_eq!(blobs.get(A).unwrap(), b"a\nb");
    assert!(!blobs.contains_key(B));
    assert_eq!(blobs.get(C).unwrap(), b"");
    assert!(parse_batch(format!("{A} blob 9\nshort\n").as_bytes()).is_err());

    // Two commits as `log -z --numstat` prints them in the Surveyor's format.
    let log = format!(
        "\x1e{B}\x1f{A} {C}\x1f200\x1fAda\x1f\x1f\x1f\0\n\
         \x1e{A}\x1f\x1f100\x1frenovate[bot]\x1f\x1fClaude <c@example.invalid>\x1dBob\x1f\0\n\
         2\t1\tsrc/lib.rs\0-\t-\tlogo.png\0"
    );
    let policy = Policy::default();
    let commits = parse_log(log.as_bytes(), &policy).unwrap();
    assert_eq!(commits.len(), 2);
    assert!(commits[0].merge && commits[0].changes.is_empty());
    assert_eq!(commits[0].identity, "Ada");
    assert!(!commits[0].agent);
    assert!(commits[1].agent);
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
    // A Co-Authored-By value alone marks an agent commit.
    let trailer = format!("\x1e{A}\x1f\x1f1\x1fAda\x1f\x1fGPT-5 <x@example.invalid>\x1f\0\n");
    assert!(parse_log(trailer.as_bytes(), &policy).unwrap()[0].agent);
    let excluded = Policy {
        exclude: vec!["*.png".into()],
        ..Policy::default()
    };
    assert_eq!(
        parse_log(log.as_bytes(), &excluded).unwrap()[1]
            .changes
            .len(),
        1
    );
}

#[test]
fn an_email_is_hashed_by_the_16_hex_rule_and_never_kept() {
    use sha2::{Digest, Sha256};
    let expected = &review_core::hex::encode(&Sha256::digest(b"ada@example.invalid"))[..16];
    assert_eq!(hash16("  Ada@Example.Invalid \n"), expected);
    assert_eq!(
        identity_of(
            "Ada",
            " Ada@Example.Invalid",
            SurveyIdentityPolicyV1::EmailHash
        ),
        expected
    );
    assert_eq!(
        identity_of(" Ada ", "ada@example.invalid", SurveyIdentityPolicyV1::Name),
        "Ada"
    );
    // An address written as the name is recorded by the hash rule, never as text.
    let named = identity_of("ada@example.invalid", "", SurveyIdentityPolicyV1::Name);
    assert_eq!(named, expected);
    let policy = Policy {
        identity: SurveyIdentityPolicyV1::EmailHash,
        ..Policy::default()
    };
    assert!(measure::log_format(&policy).contains("%ae"));
    assert!(!measure::log_format(&Policy::default()).contains("%ae"));
}

fn commit(id: &str, time: u64, identity: &str, agent: bool, changes: &[(&str, u64)]) -> Commit {
    Commit {
        id: id.into(),
        merge: false,
        time,
        identity: identity.into(),
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
fn history_and_contributors_count_windows_at_or_after_cutoff_minus_the_window() {
    let day = 86_400;
    let cutoff = 1_000 * day;
    let commits = vec![
        commit(
            A,
            cutoff - 30 * day,
            "Ada",
            false,
            &[("src/a.rs", 3), ("README.md", 1)],
        ),
        commit(
            B,
            cutoff - 31 * day,
            "Bot",
            true,
            &[("src/a.rs", 1), ("gone.rs", 1)],
        ),
        commit(C, cutoff - 400 * day, "Ada", false, &[("src/b.rs", 9)]),
    ];
    let head: BTreeSet<&str> = ["src/a.rs", "README.md", "src/b.rs"].into();
    let policy = Policy {
        hot_paths: 2,
        ..Policy::default()
    };
    let history = measure::history_section(&commits, 5, cutoff, &head, &policy);
    assert_eq!(history.windows.days_30.commits.get(), 1, "at the boundary");
    assert_eq!(history.windows.days_90.commits.get(), 2);
    assert_eq!(history.windows.days_365.commits.get(), 2);
    assert_eq!(history.commits.get(), 3);
    assert_eq!(
        history.oldest_reached.as_deref(),
        Some(C),
        "5 reachable, 3 reached"
    );
    assert_eq!(history.first_commit_unix_s.get(), cutoff - 400 * day);
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
    let all = Policy::default();
    let history = measure::history_section(&commits, 3, cutoff, &head, &all);
    assert!(history.oldest_reached.is_none());
    assert_eq!(
        history
            .hot_paths
            .iter()
            .find(|hot| hot.path == "gone.rs")
            .and_then(|hot| hot.removed),
        Some(true)
    );

    let contributors = measure::contributors_section(&commits, cutoff, &all);
    assert_eq!(contributors.identities.get(), 2);
    assert_eq!(contributors.agent_commits.get(), 1);
    assert_eq!(contributors.agent_share_bp.get(), 3333, "rounded down");
    assert_eq!(contributors.human_share_bp.get(), 6666);
    assert_eq!(contributors.top_share_bp.get(), 6666);
    assert_eq!(contributors.active_90d.get(), 2);
    let ada = &contributors.by_identity["Ada"];
    assert_eq!(
        (ada.commits.get(), ada.added.get(), ada.removed.get()),
        (2, 13, 3)
    );
    assert!(ada.active_90d);
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
    let candidates = measure::text_candidates(&entries, &policy);
    assert_eq!(candidates, BTreeSet::from([A.into(), B.into(), C.into()]));
    let blobs = BTreeMap::from([
        (A.to_string(), b"a\nb\n".to_vec()),
        (B.to_string(), b"\0bin\n\n".to_vec()),
        (C.to_string(), b"x\n\n".to_vec()),
    ]);
    let tree = measure::tree_section(&entries, Some(&blobs), &policy);
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
    let unread = measure::tree_section(&entries, None, &policy);
    assert_eq!(unread.lines.get(), 0);
    assert_eq!(unread.files_without_lines.get(), 6);
}

#[test]
fn decisions_vocabulary_and_checks_read_names_never_definitions_or_arguments() {
    let adr = b"# ADR-0007: Pick one\n\n**Status:** Superseded by 0008 (2026-01-02, was \
                accepted 2025-12-01).\n";
    let other = b"# ADR-0010: Accepted words in a title\n\nNo status here.\n";
    let records: Vec<(&str, Option<&[u8]>)> = vec![
        ("docs/adr/0007-pick.md", Some(adr)),
        ("docs/adr/0010-words.md", Some(other)),
        ("docs/adr/0002-big.md", None),
    ];
    let decisions = measure::decisions_section(&records);
    assert_eq!(decisions.count.get(), 3);
    assert_eq!(decisions.by_status["superseded"].get(), 1);
    assert_eq!(decisions.by_status["unknown"].get(), 2);
    let numbers: Vec<&str> = decisions.newest.iter().map(|d| d.number.as_str()).collect();
    assert_eq!(numbers, ["0010", "0007", "0002"]);
    let seven = &decisions.newest[1];
    assert_eq!(seven.title.as_deref(), Some("ADR-0007: Pick one"));
    assert_eq!(seven.status.as_deref(), Some("superseded"));
    assert_eq!(seven.date.as_deref(), Some("2026-01-02"));
    assert_eq!(
        decisions.newest[0].status, None,
        "a title is not a status line"
    );

    let context = b"# Terms\n\n**Project**:\nThe identity.\n\n**Survey**:  \nOne record.\n\
                    **Project**:\nagain\n**Not a term** here:\n**Bold**: inline definition\n";
    assert_eq!(measure::terms(context), ["Project", "Survey"]);

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
    let entries = vec![
        Entry {
            kind: EntryKind::File,
            oid: A.into(),
            size: 1,
            path: "Cargo.toml".into(),
        },
        Entry {
            kind: EntryKind::File,
            oid: A.into(),
            size: 1,
            path: ".github/workflows/ci.yml".into(),
        },
        Entry {
            kind: EntryKind::File,
            oid: A.into(),
            size: 1,
            path: "sub/go.mod".into(),
        },
    ];
    let checks = measure::checks_section(
        &entries,
        Some(code_policy),
        &[(".af/pipelines/review.toml", pipeline)],
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
        ("survey/glob.rs", include_str!("glob.rs")),
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
    assert!(include_str!("measure.rs").contains("run_within"));
}
