//! The setup guides and help state what the CLI does (issue #162): the installed Task-kind
//! profiles, the installer's conditional signature check, empty-Diff plan inspection versus the
//! run guard, and the safe project-relative catalog destination. Token-free: no Provider, Gate or
//! model runs.
use std::path::PathBuf;

use review_config::task::kind::TaskKindProfile;

fn workspace_file(path: &str) -> String {
    let workspace = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    std::fs::read_to_string(workspace.join(path)).unwrap()
}

/// Text with its line wrapping collapsed to single spaces.
fn collapsed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn help(args: &[&str]) -> String {
    let output = crate::common::af().args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    collapsed(&String::from_utf8_lossy(&output.stdout))
}

/// Every installed profile, in the order the shared-catalog guide lists them. A new variant
/// fails to compile in `profile_name` until it is added here and to the guide.
const PROFILES: [&str; 8] = [
    "implementation",
    "reviewed_implementation",
    "repair_allowed_implementation",
    "review",
    "document",
    "report",
    "optimization_analysis",
    "optimization_candidate",
];

fn profile_name(profile: TaskKindProfile) -> &'static str {
    match profile {
        TaskKindProfile::Implementation => "implementation",
        TaskKindProfile::ReviewedImplementation => "reviewed_implementation",
        TaskKindProfile::RepairAllowedImplementation => "repair_allowed_implementation",
        TaskKindProfile::Review => "review",
        TaskKindProfile::Document => "document",
        TaskKindProfile::Report => "report",
        TaskKindProfile::OptimizationAnalysis => "optimization_analysis",
        TaskKindProfile::OptimizationCandidate => "optimization_candidate",
    }
}

#[test]
fn shared_catalog_guide_lists_exactly_the_installed_kind_profiles() {
    for name in PROFILES {
        let profile: TaskKindProfile = serde_json::from_value(serde_json::json!(name)).unwrap();
        assert_eq!(profile_name(profile), name);
    }
    // A starter name is not a kind profile, and no other handler can be named.
    for name in ["software", "all", "custom_handler"] {
        assert!(serde_json::from_value::<TaskKindProfile>(serde_json::json!(name)).is_err());
    }
    let guide = collapsed(&workspace_file("docs/task-execution/shared-catalogs.md"));
    let listed = guide
        .split("The installed profiles are exactly ")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let names: Vec<&str> = listed.split('`').skip(1).step_by(2).collect();
    assert_eq!(names, PROFILES, "{listed}");
    assert!(
        guide.contains("a `kind.toml` naming any other `profile` is refused"),
        "{guide}"
    );
    assert!(
        guide.contains("These are not the starter names that `af catalog init --profile` takes"),
        "{guide}"
    );
}

#[test]
fn task_prerequisites_keep_signature_and_checksum_only_installs_apart() {
    // The installer the guide describes checks the signature with minisign, and proceeds
    // without it only under AF_INSTALL_ALLOW_UNSIGNED=1.
    let installer = workspace_file("install.sh");
    assert!(installer.contains("verified_by=sha256sums"));
    assert!(installer.contains("if command -v minisign >/dev/null 2>&1; then"));
    assert!(installer.contains("verified_by=minisign"));
    assert!(installer.contains("elif [ \"${AF_INSTALL_ALLOW_UNSIGNED:-}\" = 1 ]; then"));
    assert!(installer.contains("only the checksum is verified"));

    let guide = collapsed(&workspace_file("docs/tasks.md"));
    for statement in [
        "and the signature with `minisign`: put it on `PATH` before you run the installer \
         ([install](../README.md#install)), which refuses without it",
        "Under `AF_INSTALL_ALLOW_UNSIGNED=1` it proceeds anyway, reports that only the checksum \
         is verified, and the install receipt records `verified_by = \"sha256sums\"` rather than \
         `\"minisign\"`; that checksum-only install does not meet this prerequisite",
        "`receipt.verified_by` is shown by `af self status --json`",
    ] {
        assert!(guide.contains(statement), "{statement}\n{guide}");
    }
    assert!(!guide.contains("(the release's `install.sh` does this)"));
}

#[test]
fn preflight_guide_separates_empty_plan_inspection_from_the_run_guard() {
    let guide = collapsed(&workspace_file("docs/task-execution/preflight.md"));
    assert!(
        !guide.contains("refuses native empty-Diff planning"),
        "{guide}"
    );
    for statement in [
        "`af review plan` inspects it and succeeds: it exits 0, exports `subject.empty` as `true`",
        "its text output marks the subject `EMPTY — run will refuse`",
        "`af review run` is the guard: it refuses the empty Diff before Gates, Provider admission \
         or Worker dispatch",
        "([Quickstart](../../README.md#quickstart))",
        "is the `empty_intended_diff` conflict (exit 2), never a successful diff check",
    ] {
        assert!(guide.contains(statement), "{statement}\n{guide}");
    }
    let helper = workspace_file("scripts/task-preflight.py");
    assert!(helper.contains("issue(\"empty_intended_diff\""));
}

#[test]
fn catalog_destination_must_be_an_absent_project_relative_path() {
    let catalog = help(&["catalog", "--help"]);
    assert!(
        catalog.contains(
            "The destination is a safe project-relative path under --repo (default `.`) that must \
             not exist yet: an absolute path"
        ),
        "{catalog}"
    );
    assert!(
        catalog
            .contains("`init --profile` names a starter set, not an installed Task-kind profile"),
        "{catalog}"
    );
    assert!(
        catalog.contains("af catalog init --profile software --destination task-demo"),
        "{catalog}"
    );
    for args in [
        ["catalog", "init", "--help"],
        ["catalog", "sync", "--help"],
        ["task", "export", "--help"],
    ] {
        let text = help(&args);
        assert!(
            text.contains(
                "Absent directory, as a safe path relative to --repo (an absolute path is refused)"
            ),
            "{text}"
        );
    }

    let root = tempfile::tempdir().unwrap();
    let init = |destination: &str| {
        crate::common::af()
            .current_dir(root.path())
            .args([
                "catalog",
                "init",
                "--profile",
                "document",
                "--destination",
                destination,
            ])
            .output()
            .unwrap()
    };
    let stderr =
        |output: &std::process::Output| String::from_utf8_lossy(&output.stderr).into_owned();
    let absolute = root.path().join("absolute-demo");
    for destination in [
        absolute.to_str().unwrap(),
        "../escape",
        ".git/demo",
        "./demo",
        "a//b",
    ] {
        let refused = init(destination);
        assert!(!refused.status.success(), "{destination}");
        assert!(
            stderr(&refused).contains("Catalog destination must be a safe project-relative path"),
            "{destination}: {}",
            stderr(&refused)
        );
    }
    assert!(!absolute.exists());
    assert!(!root.path().join("demo").exists());

    std::fs::create_dir(root.path().join("existing")).unwrap();
    let existing = init("existing");
    assert!(!existing.status.success());
    assert!(
        stderr(&existing).contains("Catalog destination must be absent"),
        "{}",
        stderr(&existing)
    );
    assert_eq!(
        std::fs::read_dir(root.path().join("existing"))
            .unwrap()
            .count(),
        0
    );

    let created = init("demo");
    assert!(created.status.success(), "{}", stderr(&created));
    assert!(root.path().join("demo").is_dir());
}
