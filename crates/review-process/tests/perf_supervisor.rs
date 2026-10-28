//! The authority-built `perf-supervisor` example as a process (ADR-0166): its closed request,
//! framed reply, monotonic child interval, capped output, deadline, signal forwarding and the
//! lifecycle it can and cannot establish in process-group containment, its in-order stdout
//! sequences, and the host side of a run deadline that cancels its sample through it (ADR-0171).
//! PID-namespace containment needs the supervisor to be PID 1 of a Linux container; outside one
//! it refuses.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// A wall for fixtures whose subject is not the deadline (ADR-0114).
const LOAD_SAFE_WALL_MS: u64 = 120_000;

#[path = "support/executables.rs"]
mod executables;
#[path = "support/recorded.rs"]
mod recorded;
use executables::{DIRECTORY, identity, layout, snapshot};

/// The supervisor this test process runs: its own link to a snapshot of the example cargo built
/// into `<target>/<profile>/examples/perf-supervisor` beside this test (ADR-0173, ADR-0174).
///
/// A whole-workspace `cargo test` or nextest run builds every example, but selecting this test
/// alone (`cargo test --test perf_supervisor`) builds none, and an example left by an earlier
/// build may be stale. So the same cargo is asked to build it into this test's own target and
/// profile, with the workspace's feature resolution: after a whole-workspace build that is a
/// fingerprint check, never a second, different binary (ADR-0167).
///
/// That uplifted path is cargo's, not this test's: every build puts the example there again,
/// and on macOS it always copies, so one test's build removed the file another test was starting
/// (`NotFound`). `review-pipeline`'s tests build and start the same example, so the build and
/// the snapshot happen under the one boundary every test builder and launcher of a workspace
/// output takes, `support/executables.rs`, and tests start only this process's own link to that
/// snapshot, which no build touches.
fn supervisor() -> PathBuf {
    static BUILT: OnceLock<PathBuf> = OnceLock::new();
    BUILT.get_or_init(build_supervisor).clone()
}

fn build_supervisor() -> PathBuf {
    executables::built(
        "--example",
        "perf-supervisor",
        "review-process",
        env!("REVIEW_PROCESS_TARGET_TRIPLE_DIR"),
    )
}

#[test]
fn a_build_that_replaces_the_uplifted_example_never_moves_the_supervisor_a_test_runs() {
    // The P4 gate's failure, reproduced at its boundary (ADR-0173): cargo puts an example in
    // place again by removing the old file, then copying the new one. Between the two the
    // uplifted path does not exist, and starting it fails exactly as the gate's test did. Here
    // that step runs on a private copy, in order, so nothing depends on timing.
    let dir = scratch_path("uplift");
    std::fs::create_dir(&dir).unwrap();
    let owned_dir = dir.join(DIRECTORY);
    std::fs::create_dir(&owned_dir).unwrap();
    let uplifted = dir.join("perf-supervisor");
    std::fs::copy(supervisor(), &uplifted).unwrap();
    let owned = snapshot(&uplifted, &owned_dir);
    assert_ne!(owned, uplifted);
    let before = identity(&owned);

    // The removal: the uplifted path is gone, the snapshot a test starts is not.
    std::fs::remove_file(&uplifted).unwrap();
    let missing = Command::new(&uplifted)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_err();
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
    let reply = finish(spawn_at(
        &owned,
        &canonical(&request("cat", LOAD_SAFE_WALL_MS)),
    ));
    assert_eq!(reply.code, Some(0), "{}", reply.diagnostics);
    assert_eq!(reply.result["termination"]["kind"], "exited");
    assert_eq!(reply.stdout, b"request\n");

    // The copy: a new file at the uplifted path, the same bytes, so the same snapshot, which
    // was neither moved nor rewritten.
    std::fs::copy(&owned, &uplifted).unwrap();
    assert_ne!(identity(&uplifted), before);
    assert_eq!(snapshot(&uplifted, &owned_dir), owned);
    assert_eq!(identity(&owned), before);

    // Another build's bytes are another snapshot; the earlier one stays as it was.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&uplifted)
        .unwrap()
        .write_all(b"\0")
        .unwrap();
    let other = snapshot(&uplifted, &owned_dir);
    assert_ne!(other, owned);
    assert_eq!(identity(&owned), before);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_rebuild_by_another_test_process_leaves_the_running_snapshot_in_place() {
    // What every other process of this test does under nextest: its own build, fresh here
    // unless the sources changed meanwhile. The same bytes are the same snapshot; other bytes
    // are another one; the snapshot already running is never moved or rewritten.
    let owned = supervisor();
    let before = identity(&owned);
    let bytes = std::fs::read(&owned).unwrap();
    let again = build_supervisor();
    assert!(again == owned || std::fs::read(&again).unwrap() != bytes);
    assert_eq!(identity(&owned), before);
    assert!(std::fs::read(&owned).unwrap() == bytes);
    let reply = run(&request("cat", LOAD_SAFE_WALL_MS));
    assert_eq!(reply.code, Some(0), "{}", reply.diagnostics);
}

/// Another crate's test process building an output, as cargo does it, inside the boundary: it
/// takes the exclusive lock, removes the uplifted path, waits for a line on stdin, copies the new
/// file into place, records that the copy finished, and only then releases the lock.
const FOREIGN_BUILDER: &str = r#"
import fcntl, os, shutil, sys
lock, uplifted, source, marker = sys.argv[1:5]
fd = os.open(lock, os.O_CREAT | os.O_WRONLY, 0o644)
fcntl.flock(fd, fcntl.LOCK_EX)
os.remove(uplifted)
sys.stdout.write("removed\n")
sys.stdout.flush()
sys.stdin.readline()
shutil.copy2(source, uplifted)
with open(marker, "w") as stream:
    stream.write("copied")
fcntl.flock(fd, fcntl.LOCK_UN)
"#;

#[test]
fn a_launcher_never_meets_another_crates_build_between_its_removal_and_its_copy() {
    // The cross-crate arrangement of ADR-0174: a builder in another test process (here a
    // separate python3 process, as `review-pipeline`'s tests are separate processes of another
    // test binary) and a launcher that builds nothing, as `af`'s tests start `CARGO_BIN_EXE_af`.
    // Ordering is proven by the builder's marker, not by timing.
    let dir = scratch_path("boundary");
    std::fs::create_dir(&dir).unwrap();
    let owned = dir.join(DIRECTORY);
    std::fs::create_dir(&owned).unwrap();
    let uplifted = dir.join("perf-supervisor");
    let source = dir.join("source");
    std::fs::copy(supervisor(), &uplifted).unwrap();
    std::fs::copy(supervisor(), &source).unwrap();
    let bytes = std::fs::read(&source).unwrap();
    let marker = dir.join("copied");
    let mut builder = Command::new("python3")
        .args(["-I", "-c", FOREIGN_BUILDER])
        .arg(owned.join("lock"))
        .arg(&uplifted)
        .arg(&source)
        .arg(&marker)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("python3 runs the foreign builder");
    let mut stdout = builder.stdout.take().unwrap();
    let mut line = [0_u8; 8];
    stdout.read_exact(&mut line).unwrap();
    assert_eq!(&line, b"removed\n");

    // Outside the boundary the gate's failure is deterministic: the path is missing.
    let missing = Command::new(&uplifted)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_err();
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);

    // Inside it the launcher waits for the builder's whole build, then links the new file.
    let launcher = {
        let (owned, uplifted) = (owned.clone(), uplifted.clone());
        std::thread::spawn(move || executables::held_in(&owned, &uplifted))
    };
    builder.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let link = launcher.join().unwrap();
    assert!(builder.wait().unwrap().success());
    assert!(
        marker.is_file(),
        "the launcher linked before the builder finished"
    );
    assert_eq!(link.parent(), Some(owned.as_path()));
    assert_eq!(identity(&link), identity(&uplifted));
    assert!(std::fs::read(&link).unwrap() == bytes);

    // Whatever replaces the uplifted path next, the link is the file it linked.
    std::fs::remove_file(&uplifted).unwrap();
    let reply = finish(spawn_at(
        &link,
        &canonical(&request("cat", LOAD_SAFE_WALL_MS)),
    ));
    assert_eq!(reply.code, Some(0), "{}", reply.diagnostics);
    assert_eq!(reply.stdout, b"request\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn links_of_exited_processes_and_superseded_snapshots_are_removed_but_a_live_link_keeps_its_bytes()
{
    let dir = scratch_path("prune");
    std::fs::create_dir(&dir).unwrap();
    let built = dir.join("perf-supervisor");
    std::fs::write(&built, b"first build").unwrap();
    let first = snapshot(&built, &dir);
    let live = executables::link(&first, &dir);
    assert_eq!(
        live.file_name().unwrap().to_str().unwrap(),
        format!(
            "{}.{}",
            first.file_name().unwrap().to_str().unwrap(),
            std::process::id()
        )
    );
    // A process that has exited: its pid, reaped, names no live process.
    let mut exited = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .unwrap();
    let pid = exited.id();
    exited.wait().unwrap();
    let dead = dir.join(format!(
        "{}.{pid}",
        first.file_name().unwrap().to_str().unwrap()
    ));
    std::fs::hard_link(&first, &dead).unwrap();

    std::fs::write(&built, b"second build").unwrap();
    let second = snapshot(&built, &dir);
    assert_ne!(second, first);
    executables::prune_snapshots(&dir, "perf-supervisor", &second);
    executables::prune_links(&dir);
    assert!(!first.exists(), "a superseded snapshot stays");
    assert!(!dead.exists(), "an exited process's link stays");
    assert!(second.is_file());
    assert_eq!(std::fs::read(&live).unwrap(), b"first build");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_target_layout_comes_from_cargo_not_from_a_cache_tag() {
    // A precreated nested target directory without CACHEDIR.TAG, as a kernel cache or an
    // explicit `--target-dir target/linux-checks` gives, is the target itself.
    let nested = Path::new("/w/target/linux-checks/debug");
    assert_eq!(
        layout(nested, ""),
        (Path::new("/w/target/linux-checks"), None)
    );
    let cache = Path::new("/cache/cargo_target/release");
    assert_eq!(layout(cache, ""), (Path::new("/cache/cargo_target"), None));
    // An explicit cross target keeps its triple below the target directory.
    let cross = Path::new("/w/target/linux-checks/aarch64-unknown-linux-musl/ci");
    assert_eq!(
        layout(cross, "aarch64-unknown-linux-musl"),
        (
            Path::new("/w/target/linux-checks"),
            Some("aarch64-unknown-linux-musl")
        )
    );
    // This test's own build resolves to a directory that holds its profile.
    let exe = std::env::current_exe().unwrap();
    let profile = exe.parent().and_then(Path::parent).unwrap();
    let (target, triple) = layout(profile, env!("REVIEW_PROCESS_TARGET_TRIPLE_DIR"));
    let mut expected = target.to_path_buf();
    if let Some(triple) = triple {
        expected.push(triple);
    }
    expected.push(profile.file_name().unwrap());
    assert_eq!(expected, profile);
}

#[test]
#[should_panic(expected = "is not below the")]
fn a_profile_outside_the_recorded_triple_is_refused() {
    let _ = layout(Path::new("/w/target/debug"), "x86_64-unknown-linux-musl");
}

/// A fresh path below the temporary directory, unique to this process and call.
fn scratch_path(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "af-perf-supervisor-{}-{}-{name}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

fn request(script: &str, timeout_ms: u64) -> Value {
    json!({
        "schema": "af.perf-supervisor-request/4",
        "argv": ["/bin/sh", "-c", script],
        "environment": {"PATH": "/usr/bin:/bin"},
        "cwd": std::env::temp_dir().canonicalize().unwrap().to_str().unwrap(),
        "stdin": "request\n",
        "timeout_ms": timeout_ms,
        "stdout_cap_bytes": 1024,
        "log_cap_bytes": 2048,
        "cleanup_grace_ms": 1000,
        "containment": "process_group",
        "scratch": null,
        "setup": null,
        "stdout_patterns": [],
        "stdout_json": [],
    })
}

/// Canonical bytes, as the trusted driver writes them: sorted keys, compact.
fn canonical(value: &Value) -> Vec<u8> {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let ordered: std::collections::BTreeMap<_, _> = map
                    .iter()
                    .map(|(key, item)| (key.clone(), sorted(item)))
                    .collect();
                Value::Object(ordered.into_iter().collect())
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    let mut out = Vec::new();
    let value = sorted(value);
    // serde_json keeps insertion order with `preserve_order`; the map above is already sorted.
    serde_json::to_writer(&mut out, &value).unwrap();
    out
}

struct Reply {
    code: Option<i32>,
    result: Value,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    diagnostics: String,
}

fn spawn(input: &[u8]) -> std::process::Child {
    spawn_at(&supervisor(), input)
}

fn spawn_at(supervisor: &Path, input: &[u8]) -> std::process::Child {
    let mut child = Command::new(supervisor)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child
}

fn finish(child: std::process::Child) -> Reply {
    let output = child.wait_with_output().unwrap();
    let diagnostics = String::from_utf8_lossy(&output.stderr).into_owned();
    let Some(end) = output.stdout.iter().position(|byte| *byte == b'\n') else {
        return Reply {
            code: output.status.code(),
            result: Value::Null,
            stdout: Vec::new(),
            stderr: Vec::new(),
            diagnostics,
        };
    };
    let result: Value = serde_json::from_slice(&output.stdout[..end]).unwrap();
    let body = &output.stdout[end + 1..];
    let out = result["stdout"]["retained"].as_u64().unwrap() as usize;
    let err = result["stderr"]["retained"].as_u64().unwrap() as usize;
    assert_eq!(
        body.len(),
        out + err,
        "the payload is exactly the declared bytes"
    );
    Reply {
        code: output.status.code(),
        stdout: body[..out].to_vec(),
        stderr: body[out..].to_vec(),
        result,
        diagnostics,
    }
}

fn run(request: &Value) -> Reply {
    finish(spawn(&canonical(request)))
}

#[test]
fn a_sample_is_timed_from_launch_to_observed_exit_and_framed() {
    let reply = run(&request("cat; printf err >&2; exit 3", LOAD_SAFE_WALL_MS));
    assert_eq!(reply.code, Some(0), "{}", reply.diagnostics);
    let result = &reply.result;
    assert_eq!(result["schema"], "af.perf-supervisor-result/5");
    assert!(result["setup"].is_null());
    assert_eq!(result["termination"]["kind"], "exited");
    assert_eq!(result["termination"]["code"], 3);
    assert_eq!(reply.stdout, b"request\n");
    assert_eq!(reply.stderr, b"err");
    let wall: u128 = result["timing"]["child_wall_ns"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(wall > 0);
    assert_eq!(result["timing"]["exit_observation"], "unreaped");
    for overhead in ["before_launch_ns", "cleanup_ns", "after_exit_ns"] {
        let value: u128 = result["timing"][overhead]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(value < 60_000_000_000, "{overhead} is recorded apart");
    }
    assert_eq!(
        result["rusage"]["scope"],
        "reaped_child_and_its_waited_descendants"
    );
    assert!(result["rusage"]["maxrss_bytes"].as_u64().unwrap() > 0);
    assert_eq!(result["supervisor"]["containment"], "process_group");
    assert_ne!(result["lifecycle"]["status"], "cleanup_uncertain");
}

#[test]
fn a_request_that_is_not_canonical_closed_or_bounded_is_refused_without_a_reply() {
    let valid = request("true", LOAD_SAFE_WALL_MS);
    let mut unknown = valid.clone();
    unknown["shell"] = json!("sh -c");
    let mut relative = valid.clone();
    relative["argv"] = json!(["sh", "-c", "true"]);
    let mut deadline = valid.clone();
    deadline["timeout_ms"] = json!(120_001);
    let mut caps = valid.clone();
    caps["log_cap_bytes"] = json!(10);
    let mut patterns = valid.clone();
    patterns["stdout_patterns"] = json!([""]);
    let mut many = valid.clone();
    many["stdout_patterns"] = json!(["a", "b", "c", "d", "e", "f", "g", "h", "i"]);
    let mut scratch = valid.clone();
    scratch["scratch"] = json!("/tmp/scratch");
    let mut old = valid.clone();
    old["schema"] = json!("af.perf-supervisor-request/1");
    let mut relative_setup = valid.clone();
    relative_setup["setup"] = json!({"argv": ["sh"], "environment": {}, "cwd": "/",
                                     "timeout_ms": 1000});
    let mut unbounded_setup = valid.clone();
    unbounded_setup["setup"] = json!({"argv": ["/bin/sh"], "environment": {}, "cwd": "/",
                                      "timeout_ms": 120_001});
    let mut unknown_setup = valid.clone();
    unknown_setup["setup"] = json!({"argv": ["/bin/sh"], "environment": {}, "cwd": "/",
                                    "timeout_ms": 1000, "stdin": "x"});
    let pretty = serde_json::to_vec_pretty(&valid).unwrap();
    let oversized = vec![b' '; 64 * 1024 + 1];
    let cases: Vec<Vec<u8>> = vec![
        canonical(&unknown),
        canonical(&relative),
        canonical(&deadline),
        canonical(&caps),
        canonical(&patterns),
        canonical(&many),
        canonical(&scratch),
        canonical(&old),
        canonical(&relative_setup),
        canonical(&unbounded_setup),
        canonical(&unknown_setup),
        pretty,
        oversized,
    ];
    for input in cases {
        let reply = finish(spawn(&input));
        assert_eq!(reply.code, Some(2), "{}", reply.diagnostics);
        assert!(reply.result.is_null(), "a refused request has no reply");
        assert!(reply.diagnostics.contains("refused request"));
    }
}

#[test]
fn an_output_flood_is_drained_and_counted_beyond_the_caps() {
    let reply = run(&request(
        "head -c 2000000 /dev/zero; head -c 300000 /dev/zero >&2",
        LOAD_SAFE_WALL_MS,
    ));
    let result = &reply.result;
    assert_eq!(result["termination"]["kind"], "exited");
    assert_eq!(result["stdout"]["total"], 2_000_000);
    assert_eq!(result["stdout"]["retained"], 1024);
    assert_eq!(result["stdout"]["truncated"], 2_000_000 - 1024);
    assert_eq!(result["stderr"]["retained"], 1024);
    assert_eq!(result["stderr"]["truncated"], 300_000 - 1024);
}

#[test]
fn declared_stdout_patterns_are_counted_over_output_beyond_the_cap() {
    let mut order = request(
        "i=0; while [ $i -lt 3000 ]; do printf '{\"sequence\":%d}' $i; i=$((i+1)); done",
        LOAD_SAFE_WALL_MS,
    );
    order["stdout_patterns"] = json!(["{\"sequence\":", "{\"sequence\":2999}", "absent"]);
    let reply = run(&order);
    let result = &reply.result;
    assert_eq!(result["stdout"]["retained"], 1024, "{}", reply.diagnostics);
    assert!(result["stdout"]["truncated"].as_u64().unwrap() > 0);
    assert_eq!(result["stdout_pattern_counts"], json!([3000, 1, 0]));
}

/// One selector's observation as the supervisor reports it, from its counts.
fn selected(holders: u64, count: u64, numbers: Option<(u64, u64)>, matches: &[u64]) -> Value {
    json!({"holders": holders, "count": count, "duplicates": 0, "invalid": 0,
           "first_invalid": null, "first": numbers.map(|(first, _)| first),
           "last": numbers.map(|(_, last)| last), "breaks": 0, "first_break": null,
           "matches": matches})
}

#[test]
fn declared_json_fields_are_observed_over_output_beyond_the_cap() {
    // A document of 3,000 entries though 1,024 bytes are retained (ADR-0172): a whole run,
    // then a duplicate, a gap, a swap, a string, a fraction and an exponent where an integer
    // belongs, each reported where the sequence first broke, and a document without its end.
    let whole = json!({"document": {"status": "complete", "offset": null, "detail": ""},
                       "selectors": [selected(3000, 3000, Some((0, 2999)), &[]),
                                     selected(3000, 3000, None, &[2999])]});
    let broken = |breaks: u64, last: u64, value: Value, invalid: bool| {
        let mut expected = whole.clone();
        let sequence = &mut expected["selectors"][0];
        sequence["breaks"] = json!(breaks);
        sequence["last"] = json!(last);
        sequence["first_break"] = json!({"index": 1500, "after": 1499, "value": value});
        if invalid {
            sequence["invalid"] = json!(1);
            sequence["first_invalid"] = json!(1500);
        }
        expected
    };
    let unended = {
        let mut expected = whole.clone();
        expected["document"] = json!({"status": "incomplete", "offset": null,
                                      "detail": "the document is not complete"});
        expected
    };
    let cases = [
        ("", true, whole.clone()),
        (
            "[ $i -eq 1500 ] && n=1499;",
            true,
            broken(2, 2999, json!(1499), false),
        ),
        (
            "[ $i -ge 1500 ] && n=$((i+1));",
            true,
            broken(1, 3000, json!(1501), false),
        ),
        (
            "[ $i -eq 1500 ] && n=1501; [ $i -eq 1501 ] && n=1500;",
            true,
            broken(3, 2999, json!(1501), false),
        ),
        (
            "[ $i -eq 1500 ] && n='\"1500\"';",
            true,
            broken(2, 2999, Value::Null, true),
        ),
        (
            "[ $i -eq 1500 ] && n=1500.5;",
            true,
            broken(2, 2999, Value::Null, true),
        ),
        (
            "[ $i -eq 1500 ] && n=15e2;",
            true,
            broken(2, 2999, Value::Null, true),
        ),
        ("", false, unended),
    ];
    for (change, closed, expected) in cases {
        let mut order = request(
            &format!(
                "printf '{{\"items\":['; i=0; while [ $i -lt 3000 ]; do n=$i; {change} \
                 k=renewed; [ $i -eq 0 ] && k=opened; [ $i -gt 0 ] && printf ','; \
                 printf '{{\"kind\":\"%s\",\"sequence\":%s}}' \"$k\" \"$n\"; \
                 i=$((i+1)); done; printf ']{}'",
                if closed { "}" } else { "" }
            ),
            LOAD_SAFE_WALL_MS,
        );
        order["stdout_json"] = json!([
            {"path": "items.*.sequence", "kind": "integer", "equals": []},
            {"path": "items.*.kind", "kind": "string", "equals": ["renewed"]},
        ]);
        let reply = run(&order);
        let result = &reply.result;
        assert_eq!(result["stdout"]["retained"], 1024, "{}", reply.diagnostics);
        assert!(result["stdout"]["truncated"].as_u64().unwrap() > 0);
        let mut observed = result["stdout_json"].clone();
        if !closed {
            // Where the stream ended is the observation's own; only that it did not end the
            // document is asserted.
            assert!(observed["document"]["offset"].as_u64().unwrap() > 1024);
            observed["document"]["offset"] = Value::Null;
        }
        assert_eq!(observed, expected, "{change}");
    }
    for (selectors, why) in [
        (
            Value::Array(vec![
                json!({"path": "a", "kind": "integer", "equals": []});
                9
            ]),
            "at most 8 selectors",
        ),
        (
            json!([{"path": "a", "kind": "float", "equals": []}]),
            "is not declared",
        ),
        (
            json!([{"path": "a", "kind": "integer", "equals": ["x"]}]),
            "declares no values",
        ),
        (
            json!([{"path": "A.b", "kind": "integer", "equals": []}]),
            "each segment is",
        ),
    ] {
        let mut refused = request("true", LOAD_SAFE_WALL_MS);
        refused["stdout_json"] = selectors;
        let reply = run(&refused);
        assert!(
            reply.result.is_null() && reply.diagnostics.contains(why),
            "{why}: {}",
            reply.diagnostics
        );
    }
}

/// The workspace root, where the trusted Python driver lives.
fn root() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

/// Run `script` in the trusted driver's Python, the laboratory importable from `sys.argv[1]` and
/// `args` after it, and parse the one JSON document it prints.
fn driver(script: &str, args: &[&str]) -> Value {
    let output = Command::new("python3")
        .args(["-I", "-B"])
        .arg("-c")
        .arg(script)
        .arg(root().join("scripts/performance"))
        .args(args)
        .output()
        .expect("python3 runs the trusted driver");
    assert!(
        output.status.success(),
        "the driver failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// The host side of one supervised request through `perflab.supervise.launch`, with the run's
/// total deadline `remaining` seconds away and the given cancellation margin (`-` for the
/// default): the child (or, in `setup` mode, the setup) is an owned nonspawning `sleep` that
/// records its pid in `marker`, and a measured child after a setup would create `marker.child`.
const DEADLINE_DRIVER: &str = r#"
import json, os, sys, tempfile, time
sys.path.insert(0, sys.argv[1])
from perflab import supervise
supervisor, marker, mode, remaining, margin = sys.argv[2:7]
sleeper = ["/bin/sh", "-c", "echo $$ > '%s.tmp' && mv '%s.tmp' '%s'; exec sleep 60"
           % (marker, marker, marker)]
environment = {"PATH": "/usr/bin:/bin"}
cwd = os.path.realpath(tempfile.gettempdir())
argv, setup = sleeper, None
if mode == "setup":
    argv = ["/bin/sh", "-c", "touch '%s.child'" % marker]
    setup = (sleeper, environment, cwd, 120000)
order = supervise.request(argv, environment, cwd, "", 120000, 1024, 2048, grace_ms=1000,
                          setup=setup)
started = time.monotonic()
try:
    supervised = supervise.launch([supervisor], order, deadline=started + float(remaining),
                                  clock=lambda: started,
                                  cancel_margin=None if margin == "-" else float(margin))
    out = {"failure": None, "result": supervised.result}
except supervise.SupervisorFailure as failure:
    out = {"failure": {"launched": failure.launched, "deadline": failure.deadline,
                       "uncertain": failure.uncertain, "detail": str(failure)},
           "result": failure.supervised.result if failure.supervised else None}
out["elapsed"] = time.monotonic() - started
print(json.dumps(out))
"#;

/// Whether the process whose pid `marker` records is gone. Only a pid the fixture itself
/// recorded is examined.
fn recorded_process_gone(marker: &Path) -> bool {
    let pid: i32 = std::fs::read_to_string(marker)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) == Err(nix::errno::Errno::ESRCH)
}

/// Report the fixture recorded in `marker` if it outlived a failing assertion. It is the
/// supervisor's child, not this test's, so it is never signalled here: its ID may name another
/// test's process by now, and the fixture's own `sleep` ends within a minute (issue #331 item 7).
fn end_recorded(marker: &Path) {
    if let Some(recorded) = recorded::Recorded::from_marker(marker) {
        drop(recorded::Leftovers(vec![recorded]));
    }
}

fn run_deadline_driver(marker: &Path, mode: &str, remaining: &str, margin: &str) -> Value {
    driver(
        DEADLINE_DRIVER,
        &[
            supervisor().to_str().unwrap(),
            marker.to_str().unwrap(),
            mode,
            remaining,
            margin,
        ],
    )
}

#[test]
fn the_run_deadline_cancels_the_actual_child_through_the_supervisor_and_confirms_its_end() {
    // Root-reproduced (ADR-0171): a total deadline used to kill only the supervisor and leave
    // its child running. 14 s remain and the reserve is three 1 s graces and 1 s, so the host
    // cancels after 10 s, far past the child's start under any load, through the supervisor.
    let marker = scratch_path("deadline-child");
    let out = run_deadline_driver(&marker, "child", "14", "1");
    let gone = marker.exists() && recorded_process_gone(&marker);
    end_recorded(&marker);
    let _ = std::fs::remove_file(&marker);
    let failure = &out["failure"];
    assert_eq!(failure["launched"], true, "{out}");
    assert_eq!(failure["deadline"], true, "{out}");
    assert_eq!(failure["uncertain"], false, "{out}");
    assert!(
        failure["detail"]
            .as_str()
            .unwrap()
            .contains("within 10.000 s, the run's total deadline"),
        "{out}"
    );
    let result = &out["result"];
    assert_eq!(result["termination"]["kind"], "cancelled", "{out}");
    assert_eq!(result["termination"]["signal"], 15);
    assert_ne!(result["lifecycle"]["status"], "cleanup_uncertain", "{out}");
    assert!(
        gone,
        "the measured child ended before the supervisor replied: {out}"
    );
    assert!(out["elapsed"].as_f64().unwrap() < 14.0, "{out}");
}

#[test]
fn a_run_deadline_during_the_setup_cancels_it_and_launches_no_measured_child() {
    let marker = scratch_path("deadline-setup");
    let out = run_deadline_driver(&marker, "setup", "14", "1");
    let gone = marker.exists() && recorded_process_gone(&marker);
    let child = marker.with_extension("child");
    let launched = child.exists();
    end_recorded(&marker);
    let _ = std::fs::remove_file(&marker);
    let _ = std::fs::remove_file(&child);
    let result = &out["result"];
    assert_eq!(out["failure"]["uncertain"], false, "{out}");
    assert_eq!(result["termination"]["kind"], "setup_failed", "{out}");
    assert_eq!(result["setup"]["termination"]["kind"], "cancelled", "{out}");
    assert!(gone, "the setup ended before the supervisor replied: {out}");
    assert!(
        !launched,
        "no measured child launched after a cancelled setup: {out}"
    );
}

#[test]
fn a_run_deadline_too_near_to_cancel_in_time_launches_nothing() {
    // The root's reproduction: the default margins and a 0.5 s total deadline.
    let marker = scratch_path("deadline-refused");
    let out = run_deadline_driver(&marker, "child", "0.5", "-");
    let launched = marker.exists();
    end_recorded(&marker);
    let _ = std::fs::remove_file(&marker);
    assert_eq!(
        out["failure"],
        json!({"launched": false, "deadline": true, "uncertain": false,
               "detail": "the run's total deadline leaves 0.500 s, less than the 14.000 s this \
                          request's cancellation takes; nothing was launched"}),
        "{out}"
    );
    assert!(!launched);
}

/// The task_show oracle of `fixtures/performance/cli.toml` judging what the actual supervisor
/// observed of an `awk` child printing a synthetic 10,000-entry Task history shaped as `mode`
/// says, under the request a shard makes: the retained prefix is 196,608 bytes, far less than
/// the document, so only the supervisor's whole-stream observation shows its semantics.
const TASK_SHOW_DRIVER: &str = r#"
import json, os, sys, tempfile
sys.path.insert(0, sys.argv[1])
from perflab import clicases, supervise
supervisor, mode, program = sys.argv[2:5]
doc = clicases.load(os.path.join(sys.argv[1], "..", "..", "fixtures/performance/cli.toml"))
entry = [command for command in doc["adapters"]["af_cli_v1"]["commands"]
         if command["parameters"].get("command") == "task_show"][0]
cwd = os.path.realpath(tempfile.gettempdir())
order = supervise.request(["/usr/bin/awk", "-v", "mode=" + mode, program],
                          {"PATH": "/usr/bin:/bin", "LC_ALL": "C"}, cwd, "", 120000,
                          196608, 262144, grace_ms=1000, patterns=clicases.patterns(entry),
                          selectors=clicases.selectors(entry))
supervised = supervise.launch([supervisor], order)
result = supervised.result
passed, detail, _ = clicases.judge(entry, {"scratch": cwd}, supervise.returncode(result),
                                   supervised.stdout, result["stdout"]["truncated"],
                                   result["stdout_pattern_counts"], result["stdout_json"])
print(json.dumps({"passed": passed, "detail": detail, "truncated": result["stdout"]["truncated"],
                  "retained": result["stdout"]["retained"], "json": result["stdout_json"]}))
"#;

/// The synthetic history: entry i numbered i, except as `mode` changes it. `spaced` adds JSON
/// whitespace between every token and `reordered_keys` writes every object's members in
/// another order; both are the same document. The root's counterexamples (ADR-0172): a
/// `fraction` or `exponent` number in the middle, every entry but the first `moved` into an
/// unrelated array, and the document `unclosed`.
const TASK_SHOW_AWK: &str = r#"BEGIN {
  ws = mode == "spaced" ? " \n  " : "";
  last = mode == "reordered_keys";
  printf "{%s", ws;
  if (!last) printf "\"schema\":%s\"x\",%s\"task_id\":%s\"perf-probe-task\",%s", ws, ws, ws, ws;
  printf "\"history\":%s[", ws;
  for (i = 0; i < 10000 + (mode == "extra"); i++) {
    s = i;
    if (mode == "duplicate" && i == 5001) s = 5000;
    if (mode == "missing" && i >= 5000) s = i + 1;
    if (mode == "reordered" && i == 5000) s = 5001;
    if (mode == "reordered" && i == 5001) s = 5000;
    if (mode == "four" && i >= 3 && i < 9999) s = 3;
    if (mode == "fraction" && i == 5000) s = "5000.5";
    if (mode == "exponent" && i == 5000) s = "5000e1";
    k = i == 0 ? "opened" : i == 1 ? "plan_proposed" : i == 2 ? "plan_admitted" : "lease_renewed";
    sep = i ? "," ws : "";
    if (mode == "moved" && i == 1) sep = "]," ws "\"unrelated\":[";
    if (last)
      printf "%s{\"transition\":%s{\"change\":{\"kind\":\"%s\"}},%s\"sequence\":%s%s}", sep, ws, k, ws, ws, s;
    else
      printf "%s{\"sequence\":%s%s,%s\"transition\":{\"change\":{\"kind\":\"%s\"}}}", sep, ws, s, ws, k;
  }
  printf "]%s,%s\"execution_records\":[]", ws, ws;
  if (last) printf ",\"task_id\":\"perf-probe-task\",\"schema\":\"x\"";
  if (mode != "unclosed") printf "%s}", ws;
  printf "\n";
}"#;

#[test]
fn the_task_show_oracle_judges_the_whole_history_the_supervisor_observed() {
    let supervisor = supervisor();
    let judged = |mode: &str| {
        driver(
            TASK_SHOW_DRIVER,
            &[supervisor.to_str().unwrap(), mode, TASK_SHOW_AWK],
        )
    };
    let whole = json!({"document": {"status": "complete", "offset": null, "detail": ""},
                       "selectors": [selected(1, 1, None, &[1]),
                                     selected(10000, 10000, Some((0, 9999)), &[]),
                                     selected(10000, 10000, None, &[9997])]});
    // Equivalent whitespace and member order are the same document (ADR-0172).
    for mode in ["valid", "spaced", "reordered_keys"] {
        let out = judged(mode);
        assert_eq!(out["passed"], true, "{mode}: {out}");
        assert_eq!(out["retained"], 196608, "{mode}: {out}");
        assert!(
            out["truncated"].as_u64().unwrap() > 300_000,
            "{mode}: {out}"
        );
        assert_eq!(out["json"], whole, "{mode}");
    }
    for (mode, why) in [
        (
            "duplicate",
            "rule 3 (json_stream_sequence): value 5002 of history.*.sequence is 5000 \
                       after 5000",
        ),
        ("missing", "is 5001 after 4999"),
        ("reordered", "is 5001 after 4999"),
        ("extra", "holds 10001 values numbered 0..10000"),
        ("four", "is 3 after 3"),
        (
            "fraction",
            "value 5001 of history.*.sequence is not a plain nonnegative integer",
        ),
        (
            "exponent",
            "value 5001 of history.*.sequence is not a plain nonnegative integer",
        ),
        (
            "moved",
            "holds 1 values numbered 0..0, not 10000 numbered 0..9999",
        ),
        (
            "unclosed",
            "rule 2 (json_stream_equals): stdout is not one complete JSON document \
                      (incomplete",
        ),
    ] {
        let out = judged(mode);
        assert_eq!(out["passed"], false, "{mode}: {out}");
        assert_eq!(out["retained"], 196608, "{mode}: {out}");
        let detail = out["detail"].as_str().unwrap();
        assert!(detail.contains(why), "{mode}: {detail}");
    }
}

#[test]
fn a_sleeping_child_times_out_and_its_group_is_killed() {
    let started = Instant::now();
    let reply = run(&request("sleep 60 & exec sleep 60", 300));
    assert_eq!(reply.result["termination"]["kind"], "timeout");
    assert!(reply.result["timing"]["child_wall_ns"].is_null());
    assert!(started.elapsed() < Duration::from_secs(60));
    assert_eq!(reply.result["lifecycle"]["pipes_held"], false);
}

#[test]
fn a_new_session_escape_holding_a_pipe_makes_cleanup_uncertain() {
    // A double fork into a new session leaves the process group, so only the pipe it holds
    // reveals it; process-group containment never claims to have ended it. The escapee writes
    // its pid from inside its session while it holds the pipes, and the child exits only after
    // that handshake, so cleanup cannot catch it before it escaped.
    let marker = scratch_path("escaped");
    let script = format!(
        "M='{}'; \
         if command -v setsid >/dev/null 2>&1; then \
           setsid sh -c 'echo $$ > \"$0.tmp\" && mv \"$0.tmp\" \"$0\"; exec sleep 30' \"$M\" & \
         else \
           perl -e 'use POSIX; fork and exit; POSIX::setsid() or die; \
             open(my $f, \">\", \"$ARGV[0].tmp\") or die; print $f \"$$\\n\"; close $f; \
             rename(\"$ARGV[0].tmp\", $ARGV[0]) or die; exec \"sleep\", \"30\"' \"$M\"; \
         fi; \
         i=0; while [ ! -f \"$M\" ] && [ $i -lt 3000 ]; do sleep 0.01; i=$((i+1)); done; \
         [ -f \"$M\" ] || exit 9; exit 0",
        marker.display()
    );
    let reply = run(&request(&script, LOAD_SAFE_WALL_MS));
    let escaped = std::fs::read_to_string(&marker)
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok());
    let _ = std::fs::remove_file(&marker);
    let escaped = escaped.expect("the escapee completed its handshake");
    let pid = nix::unistd::Pid::from_raw(escaped);
    let session = nix::unistd::getsid(Some(pid));
    // The escapee is nobody's child here: end it and wait until it is gone.
    let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
    let deadline = Instant::now() + Duration::from_millis(LOAD_SAFE_WALL_MS);
    while nix::sys::signal::kill(pid, None).is_ok() {
        assert!(
            Instant::now() < deadline,
            "escaped fixture {escaped} survived"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(session.map(|session| session.as_raw()), Ok(escaped));
    assert_eq!(reply.result["termination"]["kind"], "exited");
    assert_eq!(reply.result["termination"]["code"], 0);
    assert_eq!(reply.result["lifecycle"]["pipes_held"], true);
    assert_eq!(reply.result["lifecycle"]["status"], "cleanup_uncertain");
}

#[test]
fn a_descendant_inheriting_the_pipes_is_killed_with_the_group() {
    let reply = run(&request("sleep 30 & exit 0", LOAD_SAFE_WALL_MS));
    let lifecycle = &reply.result["lifecycle"];
    assert_eq!(lifecycle["pipes_held"], false);
    if cfg!(target_os = "linux") {
        assert_eq!(lifecycle["observation"], "process_group_scan");
        assert_eq!(lifecycle["descendants_at_exit"], 1);
        assert_eq!(lifecycle["status"], "unexpected_descendants");
    } else {
        // Without an enumeration the descendant is unobservable, never reported as absent.
        assert_eq!(lifecycle["observation"], "pipes_only");
        assert!(lifecycle["descendants_at_exit"].is_null());
        assert_eq!(lifecycle["status"], "no_observed_descendants");
    }
}

#[test]
fn a_termination_signal_to_the_supervisor_is_forwarded_and_cancels_the_sample() {
    let marker = scratch_path("ready");
    let script = format!(
        "trap 'echo forwarded; exit 0' TERM; : > '{}'; while :; do sleep 0.05; done",
        marker.display()
    );
    let child = spawn(&canonical(&request(&script, LOAD_SAFE_WALL_MS)));
    let deadline = Instant::now() + Duration::from_millis(LOAD_SAFE_WALL_MS);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "the fixture never became ready");
        std::thread::sleep(Duration::from_millis(10));
    }
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    let reply = finish(child);
    let _ = std::fs::remove_file(&marker);
    assert_eq!(reply.code, Some(0), "{}", reply.diagnostics);
    assert_eq!(reply.result["termination"]["kind"], "cancelled");
    assert_eq!(reply.result["termination"]["signal"], 15);
    assert!(reply.result["timing"]["child_wall_ns"].is_null());
    assert_eq!(reply.stdout, b"forwarded\n");
}

#[test]
fn namespace_containment_outside_a_pid_namespace_is_refused_not_weakened() {
    let scratch = scratch_path("namespace");
    std::fs::create_dir_all(&scratch).unwrap();
    let mut order = request("touch launched", LOAD_SAFE_WALL_MS);
    order["containment"] = json!("pid_namespace");
    order["scratch"] = json!(scratch.to_str().unwrap());
    order["cwd"] = json!(scratch.to_str().unwrap());
    let reply = run(&order);
    let _ = std::fs::remove_dir_all(&scratch);
    assert_eq!(reply.result["termination"]["kind"], "launch_failed");
    assert!(
        reply.result["termination"]["detail"]
            .as_str()
            .unwrap()
            .contains("PID 1")
    );
    assert_eq!(
        reply.result["lifecycle"]["observation"],
        "pid_namespace_scan"
    );
    assert!(!scratch.join("launched").exists());
}

#[test]
fn a_child_that_cannot_launch_is_reported_as_an_outcome() {
    let mut order = request("true", LOAD_SAFE_WALL_MS);
    order["argv"] = json!(["/nonexistent/perf-sample-program"]);
    let reply = run(&order);
    assert_eq!(reply.result["termination"]["kind"], "launch_failed");
    assert_eq!(reply.result["timing"]["exit_observation"], "none");
    assert!(reply.result["rusage"].is_null());
    let mut child = spawn(b"");
    let mut out = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    assert!(out.is_empty());
    assert_eq!(child.wait().unwrap().code(), Some(2));
}

fn with_setup(script: &str, setup: &str, timeout_ms: u64) -> Value {
    let mut order = request(script, LOAD_SAFE_WALL_MS);
    order["setup"] = json!({
        "argv": ["/bin/sh", "-c", setup],
        "environment": {"PATH": "/usr/bin:/bin"},
        "cwd": std::env::temp_dir().canonicalize().unwrap().to_str().unwrap(),
        "timeout_ms": timeout_ms,
    });
    order
}

#[test]
fn a_fixture_setup_runs_first_untimed_and_is_reported_apart() {
    // The setup writes the fixture the measured child reads, and takes far longer than the
    // child: its time must not be part of the child's interval.
    let marker = scratch_path("fixture");
    let setup = format!(
        "sleep 0.5; echo fixture > '{0}.tmp' && mv '{0}.tmp' '{0}'; echo set-up >&2",
        marker.display()
    );
    let reply = run(&with_setup(
        &format!("cat '{}'", marker.display()),
        &setup,
        LOAD_SAFE_WALL_MS,
    ));
    let _ = std::fs::remove_file(&marker);
    let result = &reply.result;
    assert_eq!(
        result["termination"]["kind"], "exited",
        "{}",
        reply.diagnostics
    );
    assert_eq!(result["termination"]["code"], 0);
    assert_eq!(reply.stdout, b"fixture\n");
    let setup = &result["setup"];
    assert_eq!(setup["termination"]["kind"], "exited");
    assert_eq!(setup["termination"]["code"], 0);
    assert_eq!(setup["exit_observation"], "unreaped");
    assert_eq!(setup["stderr_tail"], "set-up\n");
    let setup_wall: u128 = setup["wall_ns"].as_str().unwrap().parse().unwrap();
    let child_wall: u128 = result["timing"]["child_wall_ns"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        setup_wall >= 500_000_000,
        "the setup slept 0.5 s: {setup_wall}"
    );
    assert!(
        child_wall < setup_wall,
        "the child's interval ({child_wall} ns) excludes the setup ({setup_wall} ns)"
    );
}

#[test]
fn a_failed_setup_launches_no_measured_child() {
    let marker = scratch_path("never");
    for (setup, timeout_ms, expected) in [
        ("echo broken >&2; exit 7", LOAD_SAFE_WALL_MS, "it exited 7"),
        ("sleep 60", 300, "it exceeded its deadline"),
    ] {
        let reply = run(&with_setup(
            &format!("touch '{}'", marker.display()),
            setup,
            timeout_ms,
        ));
        let result = &reply.result;
        assert_eq!(
            result["termination"]["kind"], "setup_failed",
            "{}",
            reply.diagnostics
        );
        assert!(
            result["termination"]["detail"]
                .as_str()
                .unwrap()
                .contains(expected),
            "{}",
            result["termination"]["detail"]
        );
        assert!(result["timing"]["child_wall_ns"].is_null());
        assert_eq!(result["timing"]["exit_observation"], "none");
        assert!(result["rusage"].is_null());
        assert!(reply.stdout.is_empty());
        assert!(
            !marker.exists(),
            "the measured child never ran after a failed setup"
        );
    }
    let reply = run(&with_setup(
        "true",
        "echo broken >&2; exit 7",
        LOAD_SAFE_WALL_MS,
    ));
    assert_eq!(reply.result["setup"]["stderr_tail"], "broken\n");
}
