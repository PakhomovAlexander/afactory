//! The crate layering is enforced here, not only documented.
//!
//! `AGENTS.md` (ADR-0026) requires that source capture, gates, and sandbox providers never
//! depend on reviewer adapters, and that process supervision stays in a dependency-neutral leaf;
//! the README's layout lists the infrastructure crates beneath composition, configuration, and
//! the CLI. Before this test, adding `review-runner = { path = "../review-runner" }` to
//! `review-sandbox` passed every gate. Now it fails `make check`.
//!
//! The graph comes from `cargo metadata --no-deps`, so the assertion is over the manifests as
//! Cargo reads them — every dependency kind, dev and build included, because a test-only edge
//! still pulls reviewer contracts into an infrastructure crate's build graph.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;

/// Infrastructure crates: contracts, executors, storage, capture, checks, sandboxes, the graph,
/// attempt accounting, and the broker. None of them may reach anything in [`UPPER`].
const LOWER: [&str; 10] = [
    "review-core",
    "review-parallel",
    "review-process",
    "review-store",
    "review-source-git",
    "review-check",
    "review-sandbox",
    "review-graph",
    "review-attempt",
    "review-broker",
];

/// Reviewer adapters, the pipeline definition format, composition, and the CLI.
const UPPER: [&str; 6] = [
    "review-runner",
    "review-runner-codex",
    "review-runner-claude",
    "review-config",
    "review-pipeline",
    "reviewctl",
];

/// Leaves that depend on no workspace crate at all (ADR-0026: "the leaf depends only on
/// platform process support"; `review-parallel` is the shape it copied).
const LEAVES: [&str; 2] = ["review-process", "review-parallel"];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// Workspace member -> workspace members it depends on directly, every dependency kind.
fn workspace_edges() -> BTreeMap<String, BTreeSet<String>> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--locked",
            "--offline",
        ])
        .current_dir(workspace_root())
        .output()
        .expect("cargo metadata runs");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata is JSON");
    let packages = metadata["packages"]
        .as_array()
        .expect("metadata lists packages");
    let members: BTreeSet<String> = packages
        .iter()
        .map(|package| package["name"].as_str().expect("package name").to_string())
        .collect();
    packages
        .iter()
        .map(|package| {
            let name = package["name"].as_str().expect("package name").to_string();
            let edges = package["dependencies"]
                .as_array()
                .expect("package dependencies")
                .iter()
                .map(|dependency| dependency["name"].as_str().expect("dependency name"))
                .filter(|dependency| members.contains(*dependency))
                .map(str::to_string)
                .collect();
            (name, edges)
        })
        .collect()
}

fn reachable(edges: &BTreeMap<String, BTreeSet<String>>, from: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut frontier = vec![from.to_string()];
    while let Some(next) = frontier.pop() {
        for dependency in edges.get(&next).into_iter().flatten() {
            if seen.insert(dependency.clone()) {
                frontier.push(dependency.clone());
            }
        }
    }
    seen
}

#[test]
fn infrastructure_crates_never_reach_adapters_config_composition_or_the_cli() {
    let edges = workspace_edges();
    for name in LOWER.iter().chain(UPPER.iter()).chain(LEAVES.iter()) {
        assert!(
            edges.contains_key(*name),
            "`{name}` is not a workspace member; update this test with the rename so the rule \
             keeps binding"
        );
    }
    for lower in LOWER {
        let reach = reachable(&edges, lower);
        for upper in UPPER {
            assert!(
                !reach.contains(upper),
                "infrastructure crate `{lower}` reaches `{upper}` through {reach:?}; \
                 AGENTS.md/ADR-0026 forbid that direction"
            );
        }
    }
}

#[test]
fn process_and_parallel_leaves_depend_on_no_workspace_crate() {
    let edges = workspace_edges();
    for leaf in LEAVES {
        assert!(
            edges[leaf].is_empty(),
            "`{leaf}` must stay a dependency-neutral leaf but depends on {:?}",
            edges[leaf]
        );
    }
}
