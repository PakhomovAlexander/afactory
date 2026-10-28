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

/// Issue #358: the README's install step promises a line to paste only to the shells `install.sh`
/// writes one for, and says what a bin directory it writes none for gets instead: no line, but
/// the advice to install with another `XDG_BIN_HOME`
/// (`install_sh_gives_no_path_line_for_a_bin_directory_no_path_entry_can_hold`).
#[test]
fn readme_install_step_promises_a_path_line_only_where_the_installer_prints_one() {
    let installer = workspace_file("install.sh");
    for arm in [
        "      fish) ",
        "      zsh|bash) ",
        "      *) printf '  add %s to PATH",
    ] {
        assert!(installer.contains(arm), "install.sh: {arm}");
    }
    let remedy = "set XDG_BIN_HOME to a directory without one, then install again";
    assert_eq!(installer.matches(remedy).count(), 2, "install.sh");

    let readme = workspace_file("README.md");
    let install = collapsed(
        readme
            .split("<a id=\"install\"></a>")
            .nth(1)
            .unwrap()
            .split("2. **Connect a Provider**")
            .next()
            .unwrap(),
    );
    let path = &install[install.find("not on `PATH`").expect("the PATH guidance")..];
    for shell in ["fish", "bash", "zsh", "another shell"] {
        assert!(path.contains(shell), "{shell}\n{path}");
    }
    for unfit in [
        "`:`",
        "control character",
        "no such line",
        "`XDG_BIN_HOME`",
        "install again",
    ] {
        assert!(path.contains(unfit), "{unfit}\n{path}");
    }
    assert!(!install.contains("for your shell"), "{install}");
    // A `:` is why no PATH entry holds a directory; a control character is only why the
    // installer prints no line, since a PATH entry may hold one.
    assert!(!path.contains("fits in no `PATH` entry"), "{path}");
    assert!(path.contains("a `:` separates `PATH` entries"), "{path}");
    assert!(path.contains("will not print a line"), "{path}");
}

/// Issue #358: the README's guidance on Findings that look alike names commands that exist with
/// the arguments it gives them, says that they stay apart until an operator groups them unless an
/// explicit corroboration or one exact rule occurrence attaches them, and agrees with
/// `af review group --help`. Each command runs against a Campaign that does not
/// exist: an error (exit 1), never a usage error (exit 2).
#[test]
fn readme_explains_findings_that_look_alike_with_commands_that_parse() {
    let readme = workspace_file("README.md");
    let section = readme
        .split("### Findings that look alike\n")
        .nth(1)
        .unwrap()
        .split("\n### ")
        .next()
        .unwrap();
    let flat = collapsed(section);
    for statement in [
        "Looking alike does not make them one Finding: a Finding's identity comes from its first \
         Report, never from its path or title, and af never merges claims that only look alike",
        "such Reports stay separate Findings unless one of two things attaches them",
        "docs/adr/0006-finding-identity-is-path-independent.md",
        "every Report and each Report's own verification stay",
    ] {
        assert!(flat.contains(statement), "{statement}\n{flat}");
    }
    // Look-alike presentation never joins Reports, but the two attachments the reducer makes
    // without an operator do (`canonical_stage_keys`): an explicit corroboration, and one exact
    // rule occurrence from any reviewer
    // (`reviewers_naming_one_rule_occurrence_share_a_finding_and_keep_every_report`). The guidance
    // names both and never claims the relation is the only way in.
    let never_enough = flat
        .split(". ")
        .find(|sentence| sentence.contains("never enough"))
        .unwrap_or_else(|| panic!("no sentence says what is never enough\n{flat}"));
    for presentation in ["path", "line", "title"] {
        assert!(never_enough.contains(presentation), "{never_enough}");
    }
    for attachment in [
        "corroborat",
        "`rule_id`",
        "`occurrence_key`",
        "any reviewer",
    ] {
        assert!(flat.contains(attachment), "{attachment}\n{flat}");
    }
    // Nor does the opening promise that look-alike Reports always stay apart: the attachments
    // above are the exceptions it names.
    for unqualified in [
        "only through an explicit relation",
        "They stay two Findings",
    ] {
        assert!(!flat.contains(unqualified), "{unqualified}\n{flat}");
    }
    let commands: Vec<&str> = flat
        .split('`')
        .skip(1)
        .step_by(2)
        .filter(|span| span.starts_with("af review "))
        .collect();
    assert_eq!(
        commands,
        [
            "af review ledger --campaign NAME",
            "af review group --campaign NAME FROM INTO",
            "af review show --campaign NAME FROM",
            "af review ungroup --campaign NAME FROM INTO",
        ]
    );
    let home = tempfile::tempdir().unwrap();
    for command in commands {
        let output = crate::common::af()
            .args(command.split(' ').skip(1))
            .current_dir(home.path())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{command}: {stderr}");
        assert!(stderr.contains("not found"), "{command}: {stderr}");
    }
    let group = help(&["review", "group", "--help"]);
    assert!(
        group.contains(
            "af never groups Findings by itself: two Reports at the same place with different \
             titles or severities stay two Findings until an operator groups them, unless"
        ),
        "{group}"
    );
    for attachment in [
        "`rule_id`",
        "`occurrence_key`",
        "corroborates",
        "(ADR-0006)",
    ] {
        assert!(group.contains(attachment), "{attachment}\n{group}");
    }
    assert!(
        group.contains("`af review ungroup FROM INTO` undoes it"),
        "{group}"
    );
}

/// Issue #358: `af telemetry tail --json` is the one command whose `--json` streams JSON Lines,
/// one `af/telemetry-tail@1` object per line, and the README and `af help json` name it as the
/// exception to the one-document rule instead of promising one document for every `--json`.
/// Every other `--help` describes its `--json` as one document; the recorded tail stream
/// (`telemetry_golden/tail.jsonl`) is several lines, each its own object, and not one document.
/// A tail that fails with an error ends the stream with one `af/error@2` line after the records
/// it printed (`telemetry_tail_refuses_interior_corruption_of_a_live_segment`); a run two Stores
/// record exits 2 with no further stdout line, the Stores named on stderr
/// (`telemetry_tail_refuses_a_second_store_that_appears_while_following`). Both surfaces say
/// which is which, as `docs/telemetry.md` does, and none promises the error line for every
/// failure.
#[test]
fn json_guidance_names_telemetry_tail_as_the_one_json_lines_stream() {
    let mut streams = Vec::new();
    let mut documents = 0;
    for (page, text) in crate::help_examples::pages() {
        let lines: Vec<&str> = text.lines().collect();
        let Some(at) = lines.iter().position(|line| {
            let option = line.trim_start();
            line.starts_with(' ') && (option == "--json" || option.starts_with("--json "))
        }) else {
            continue;
        };
        let mut described = lines[at]
            .trim_start()
            .trim_start_matches("--json")
            .to_owned();
        for line in &lines[at + 1..] {
            let continued = line.trim_start();
            if continued.is_empty() || continued.starts_with('-') {
                break;
            }
            described.push(' ');
            described.push_str(continued);
        }
        let described = collapsed(&described);
        if described.contains("per line") {
            streams.push(page);
        } else {
            documents += 1;
        }
    }
    assert_eq!(streams, ["af telemetry tail --help"]);
    assert!(
        documents >= 20,
        "only {documents} one-document --json pages"
    );

    let recorded = workspace_file("crates/af/tests/it/telemetry_golden/tail.jsonl");
    assert!(serde_json::from_str::<serde_json::Value>(&recorded).is_err());
    let records: Vec<serde_json::Value> = recorded
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(records.len() > 1, "{recorded}");
    for record in &records {
        assert_eq!(record["schema"], "af/telemetry-tail@1", "{record}");
    }

    let readme = workspace_file("README.md");
    // The opening summary promises no one document for every `--json`: it names the stream and
    // points to the rules below.
    let intro = collapsed(
        readme
            .split("- **Agents can drive it.**")
            .nth(1)
            .unwrap()
            .split("\n\n")
            .next()
            .unwrap(),
    );
    assert!(intro.contains("`af telemetry tail --json`"), "{intro}");
    assert!(intro.contains("JSON Lines"), "{intro}");
    assert!(intro.contains("(#for-agents)"), "{intro}");
    assert!(!intro.contains("prints one JSON document"), "{intro}");
    let agents = collapsed(
        readme
            .split("## For agents\n")
            .nth(1)
            .unwrap()
            .split("\n## ")
            .next()
            .unwrap(),
    );
    assert!(
        agents.contains(
            "A command whose `--help` lists `--json` prints one document on stdout under it, with \
             progress on stderr, except `af telemetry tail --json`: it streams JSON Lines, one \
             `af/telemetry-tail@1` object per line"
        ),
        "{agents}"
    );
    // The flags the README names for the commands that print JSON another way are the ones their
    // `--help` lists, and no surface promises `--json` to most commands.
    assert!(
        agents.contains(
            "`af review report` takes `--format json` instead, and `af review campaigns` either \
             flag for the same document"
        ),
        "{agents}"
    );
    assert!(help(&["review", "campaigns", "--help"]).contains("--json"));
    assert!(!help(&["review", "report", "--help"]).contains("--json"));
    assert!(!collapsed(&readme).contains("Most commands"), "README.md");
    assert!(
        agents.contains(
            "A tail that fails with an error, such as a segment it cannot read, even after \
             records, ends the stream with one `af/error@2` line carrying its exit code, so tell \
             each line by its `schema`. A run two Stores record is no such error: shared when \
             tail starts or by a second Store that appears while it follows, tail exits 2 with \
             no further line on stdout and names both Stores for `--store` on stderr, so check \
             the exit code too."
        ),
        "{agents}"
    );
    let topic = help(&["help", "json"]);
    assert!(
        topic.contains(
            "A command whose --help lists --json prints exactly one JSON document on stdout \
             under it — no banner, no trailing text — so `af … --json | jq` works, with one \
             exception: `af telemetry tail --json` streams JSON Lines, one \
             af/telemetry-tail@1 object per line"
        ),
        "{topic}"
    );
    assert!(
        collapsed(&topic).contains(
            "A tail that fails with an error, such as a segment it cannot read, even after \
             records, ends the stream with one af/error@2 line carrying its exit code, so tell \
             each line by its `schema`. A run two Stores record is no such error: shared when \
             tail starts or by a second Store that appears while it follows, tail exits 2 with \
             no further line on stdout and names both Stores for --store on stderr, so check the \
             exit code too."
        ),
        "{topic}"
    );
    let telemetry = collapsed(&workspace_file("docs/telemetry.md"));
    assert!(
        telemetry.contains(
            "a tail that fails with an error, such as a segment it cannot read, even after \
             records, ends the stream with one `af/error@2` line \
             ([`schemas/error-v2.json`](../schemas/error-v2.json)) carrying its exit code. A run \
             two Stores record is no such error: tail exits 2 with no further line on stdout and \
             names both Stores for `--store` on stderr."
        ),
        "docs/telemetry.md"
    );
    // No surface promises the error line for every failed tail.
    for (surface, text) in [
        ("README.md", agents.as_str()),
        ("af help json", collapsed(&topic).as_str()),
        ("docs/telemetry.md", telemetry.as_str()),
    ] {
        assert!(
            !text.contains("A tail that fails, even after records")
                && !text.contains("a tail that fails, even after records"),
            "{surface}"
        );
    }
    assert!(!topic.contains("always works"), "{topic}");
}
