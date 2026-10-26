//! Issue #358: two reviewers report the same line with different titles and severities. The
//! Ledger keeps two Findings, because a path or a title never proves identity (ADR-0006); only
//! the operator's explicit `af review group` lists them as one, keeping both IDs and both
//! Reports, and `af review ungroup` lists them apart again. Everything the README says about
//! Findings that look alike, through the real binary.
use super::*;

/// A command reviewer that reports one defect at `calculator.py:2`.
fn reviewer(repo: &Path, name: &str, severity: &str, title: &str) -> PathBuf {
    let path = repo.join(format!("{name}.sh"));
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{{\"findings\":[{{\"severity\":\"{severity}\",\
             \"file\":\"calculator.py\",\"line\":2,\"title\":\"{title}\",\"body\":\"b may be \
             zero\",\"fix\":\"check b first\",\"confidence\":0.9}}],\"benchmark_demands\":[],\
             \"dispositions\":[]}}'\n"
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn two_reviewers(repo: &Path) {
    let correctness = reviewer(repo, "correctness", "major", "Division by zero in div");
    let bugs = reviewer(repo, "bugs", "minor", "div does not guard a zero divisor");
    let node = |id: &str, program: &Path| {
        format!(
            r#"[[nodes]]
id = "{id}"
kind = "reviewer"
inputs = [{{ name = "prior_findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }}]
outputs = [{{ name = "result", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }}]
runner = {{ program = "{}" }}
[[edges]]
from = {{ node = "generation", port = "findings" }}
to = {{ node = "{id}", port = "prior_findings" }}
[[edges]]
from = {{ node = "{id}", port = "result" }}
to = {{ node = "gather", port = "{id}" }}
"#,
            program.display()
        )
    };
    set_pipeline(
        repo,
        &format!(
            r#"version = 2
[subject]
kind = "whole-tree"
[[nodes]]
id = "generation"
kind = "generation"
outputs = [{{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = true, snapshot_affinity = "any" }}]
{}{}[[nodes]]
id = "gather"
kind = "gather"
inputs = [
  {{ name = "correctness", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }},
  {{ name = "bugs", type = "review.kernel/ReviewerResult@2", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }},
]
outputs = [{{ name = "reports", type = "review.kernel/ReportSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }}]
[[nodes]]
id = "ledger"
kind = "ledger"
inputs = [{{ name = "reports", type = "review.kernel/ReportSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }}]
outputs = [
  {{ name = "findings", type = "review.kernel/FindingSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }},
  {{ name = "demands", type = "review.kernel/DemandSet@1", cardinality = "one", optional = false, snapshot_affinity = "same_subject" }},
]
[[edges]]
from = {{ node = "gather", port = "reports" }}
to = {{ node = "ledger", port = "reports" }}
[convergence]
clean_rounds = 1
max_rounds = 3
gate = "major"
"#,
            node("correctness", &correctness),
            node("bugs", &bugs),
        ),
    );
}

/// The Ledger's rows at `calculator.py:2`: key, severity and title, and the alias lines.
fn ledger(repo: &Path, home: &Path, state: &str) -> (Vec<(String, String, String)>, String) {
    let (code, stdout, stderr) = af(
        repo,
        home,
        &["ledger", "--campaign", "dup", "--state", state],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    let rows = stdout
        .lines()
        .filter(|line| line.contains("\tcalculator.py:2\t"))
        .map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            (
                fields[0].to_string(),
                fields[1].to_string(),
                fields[fields.len() - 1].to_string(),
            )
        })
        .collect();
    (rows, stdout)
}

#[test]
fn findings_that_look_alike_stay_apart_until_an_operator_groups_them() {
    let dir = tempfile::tempdir().unwrap();
    let (repo, home, _) = fixture(dir.path());
    std::fs::write(
        repo.join("calculator.py"),
        "def div(a, b):\n    return a / b\n",
    )
    .unwrap();
    two_reviewers(&repo);
    git(&repo, &home, &["add", "-A"]);
    git(&repo, &home, &["commit", "-q", "-m", "two reviewers"]);
    let state = dir.path().join("dup").to_string_lossy().into_owned();

    let (code, stdout, stderr) = af_light(
        &repo,
        &home,
        &["run", "--campaign", "dup", "--state", &state],
    );
    assert_eq!(
        code, 3,
        "the Round closes with Findings\n{stdout}\n{stderr}"
    );

    // Same file and line, different titles and severities: two Findings, nothing merged.
    let (rows, text) = ledger(&repo, &home, &state);
    assert_eq!(rows.len(), 2, "{text}");
    let major = rows.iter().find(|row| row.1 == "major").unwrap().clone();
    let minor = rows.iter().find(|row| row.1 == "minor").unwrap().clone();
    assert_ne!(major.0, minor.0, "{text}");
    assert_eq!(major.2, "Division by zero in div");
    assert_eq!(minor.2, "div does not guard a zero divisor");
    assert!(!text.contains("aliases"), "{text}");

    // The operator says they are one defect: one row under INTO, FROM its alias, the higher
    // severity, both Reports.
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &[
            "group",
            "--campaign",
            "dup",
            "--state",
            &state,
            &minor.0,
            &major.0,
        ],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(
        stdout.trim(),
        format!("grouped {} into {}", minor.0, major.0)
    );
    let (rows, text) = ledger(&repo, &home, &state);
    assert_eq!(rows, [major.clone()], "{text}");
    assert!(
        text.contains(&format!("\n  aliases: {}\n", minor.0)),
        "{text}"
    );

    let (code, shown, stderr) = af(
        &repo,
        &home,
        &["show", "--campaign", "dup", "--state", &state, &minor.0],
    );
    assert_eq!(code, 0, "{shown}\n{stderr}");
    assert!(
        shown.starts_with(&format!("Division by zero in div [{}]\n", major.0)),
        "the alias shows its group: {shown}"
    );
    assert!(shown.contains(&format!("aliases={}", minor.0)), "{shown}");
    assert!(shown.contains("severity=major"), "{shown}");
    for reviewer in ["reviewer=correctness", "reviewer=bugs"] {
        assert!(shown.contains(reviewer), "{reviewer}: {shown}");
    }

    // Grouping is explicit and one-way per pair: FROM is grouped already.
    let (code, _, stderr) = af(
        &repo,
        &home,
        &[
            "group",
            "--campaign",
            "dup",
            "--state",
            &state,
            &minor.0,
            &major.0,
        ],
    );
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("already grouped"), "{stderr}");

    // Ungrouping lists them apart again, each with its own severity and title.
    let (code, stdout, stderr) = af(
        &repo,
        &home,
        &[
            "ungroup",
            "--campaign",
            "dup",
            "--state",
            &state,
            &minor.0,
            &major.0,
        ],
    );
    assert_eq!(code, 0, "{stdout}\n{stderr}");
    assert_eq!(
        stdout.trim(),
        format!("ungrouped {} from {}", minor.0, major.0)
    );
    let (mut rows, text) = ledger(&repo, &home, &state);
    rows.sort();
    let mut expected = vec![major, minor];
    expected.sort();
    assert_eq!(rows, expected, "{text}");
    assert!(!text.contains("aliases"), "{text}");
}
