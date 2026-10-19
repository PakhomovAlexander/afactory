//! The directory registry's overlap rule, and nothing at `off` (design §6, §8, §12).

use review_telemetry::dirs::{self, Accounted};
use review_telemetry::level::{Level, Settings};
use review_telemetry::record::{Attrs, SealExit, SpanKind};
use review_telemetry::{Start, Telemetry};

fn start(root: &std::path::Path, directory: std::path::PathBuf, level: Level) -> Start {
    let state = root.join("state/af");
    Start {
        settings: Settings {
            level,
            ..Settings::default()
        },
        directory,
        registry: dirs::location(&root.join("state")),
        accounted: vec![
            Accounted::container(&state, "the state root"),
            Accounted::new(state.join("task/local"), "the Task Store root"),
            Accounted::new(root.join("stores/task-a"), "a registered Task Store"),
            Accounted::new(
                root.join("cache/af/task-build-cache"),
                "the warm check cache",
            ),
            Accounted::new(root.join("data/af/versions"), "the release installations"),
        ],
        af_version: "0.0.0-test".into(),
        af_commit: "test".into(),
    }
}

/// Run a span through Telemetry started at `directory` and return its problems.
fn run(root: &std::path::Path, directory: std::path::PathBuf) -> Vec<String> {
    let telemetry = Telemetry::start(start(root, directory, Level::Spans));
    if let Some(writer) = telemetry.writer() {
        let span = writer.span_start(None, None, "af.command", SpanKind::Internal, Attrs::new());
        if let Some(span) = span {
            writer.span_end(
                &span,
                review_telemetry::record::SpanStatus::Ok,
                Attrs::new(),
                0,
            );
        }
    }
    let finished = telemetry.finish(SealExit::Ok);
    finished
        .problems
        .into_iter()
        .map(|(_, reason)| reason)
        .collect()
}

#[test]
fn telemetry_dirs_overlap_refused() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    // The default directory registers and records.
    let default = root.join("state/af/telemetry");
    assert!(run(&root, default.clone()).is_empty());
    assert!(default.join("v1/segments").is_dir());
    let registry = dirs::read(&dirs::location(&root.join("state"))).unwrap();
    assert_eq!(registry.len(), 1);

    for (directory, what) in [
        (
            root.join("stores/task-a/telemetry"),
            "a registered Task Store",
        ),
        (default.join("nested"), "another telemetry directory"),
        (root.join("state"), "the state root"),
        (root.join("cache/af"), "the warm check cache"),
    ] {
        let problems = run(&root, directory.clone());
        assert_eq!(problems.len(), 1, "{}: {problems:?}", directory.display());
        assert!(problems[0].contains(what), "{what}: {}", problems[0]);
        assert!(
            !directory.join("v1").exists(),
            "{}: nothing written",
            directory.display()
        );
    }
    // Refused roots are never registered.
    assert_eq!(
        dirs::read(&dirs::location(&root.join("state")))
            .unwrap()
            .len(),
        1
    );
    // A directory beside everything else is accepted and registered.
    assert!(run(&root, root.join("elsewhere/telemetry")).is_empty());
    assert_eq!(
        dirs::read(&dirs::location(&root.join("state")))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn telemetry_off_touches_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let directory = root.join("state/af/telemetry");
    let mut telemetry = Telemetry::start(start(&root, directory.clone(), Level::Off));
    assert!(telemetry.writer().is_none());
    telemetry.install_subscriber();
    // With no subscriber installed, a span site does nothing.
    let span = tracing::info_span!("af.command", af.run_id = "run");
    span.in_scope(|| tracing::info!(af.event = "af.note", "nothing"));
    drop(span);
    let finished = telemetry.finish(SealExit::Ok);
    assert_eq!(finished.sealed, None);
    assert!(finished.problems.is_empty());
    assert!(!directory.exists(), "no telemetry directory at off");
    assert!(!root.join("state").exists(), "no registry at off");
    // A level that records but writes nothing creates nothing either: the segment is lazy.
    let idle = Telemetry::start(start(&root, directory.clone(), Level::Spans));
    assert_eq!(idle.finish(SealExit::Ok).sealed, None);
    assert!(!directory.exists());
}
