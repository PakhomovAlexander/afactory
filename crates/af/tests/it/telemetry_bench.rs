//! Telemetry's performance contract (design §12, §15 P5): the overhead script's one-pair smoke
//! over the three pinned bench fixtures, its gate rule on recorded distributions, and the
//! read-latency corpus generator's determinism. Every script runs with a private HOME and private
//! XDG roots, and so does every `af` the scripts start.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

use crate::schemas;

fn workspace() -> PathBuf {
    std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

const FIXTURES: [&str; 3] = [
    "telemetry-bench-concurrent",
    "telemetry-bench-dense",
    "telemetry-bench-store",
];

/// A private machine for one script: its HOME and XDG roots below one temporary directory.
struct Machine {
    _root: tempfile::TempDir,
    root: PathBuf,
}

impl Machine {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().canonicalize().unwrap();
        for name in ["home", "config", "state", "data", "cache", "work"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        Self {
            _root: directory,
            root,
        }
    }

    /// `python3 scripts/NAME ARGS` with this machine's roots.
    fn script(&self, name: &str, args: &[&str]) -> Output {
        Command::new("python3")
            .arg(workspace().join("scripts").join(name))
            .args(args)
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("TMPDIR", self.root.join("work"))
            .env("AF_SELF_OFFLINE", "1")
            .env_remove("AF_TELEMETRY__LEVEL")
            .output()
            .unwrap()
    }
}

fn text(output: &Output) -> String {
    format!(
        "exit {:?}\n{}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn document(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| panic!("{}", text(output)))
}

/// Every object of `schema` names all its properties as required, so a document that validates
/// carries exactly the fields the schema names: one shape, one version (ADR-0145).
fn every_property_required(schema: &Value) {
    match schema {
        Value::Object(object) => {
            if let Some(Value::Object(properties)) = object.get("properties") {
                let mut named: Vec<&String> = properties.keys().collect();
                let mut required: Vec<&str> = object["required"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|name| name.as_str().unwrap())
                    .collect();
                named.sort();
                required.sort();
                assert_eq!(named, required, "{schema}");
                assert_eq!(object["additionalProperties"], json!(false), "{schema}");
            }
            object.values().for_each(every_property_required);
        }
        Value::Array(items) => items.iter().for_each(every_property_required),
        _ => {}
    }
}

fn schema_document(name: &str) -> Value {
    serde_json::from_slice(&std::fs::read(workspace().join("schemas").join(name)).unwrap()).unwrap()
}

/// `pointer`'s object with one field the schema does not name is refused.
fn refuses_unnamed(validator: &jsonschema::Validator, document: &Value, pointers: &[&str]) {
    for pointer in pointers {
        let mut extended = document.clone();
        extended
            .pointer_mut(pointer)
            .unwrap_or_else(|| panic!("{pointer}"))
            .as_object_mut()
            .unwrap()
            .insert("unnamed".into(), json!(1));
        assert!(
            !validator.is_valid(&extended),
            "{pointer}: an unnamed field passed"
        );
    }
}

/// Design §12 and §15 P5, the one-pair smoke `make check` runs: one pair of every bench fixture
/// at `--scale smoke` through the real binary. Every run exits as its fixture expects, leaves no
/// telemetry at `off` and a sealed segment at `spans`; the dense fixture yields its drain
/// latency; the document validates against `af/telemetry-overhead@1`, is `not_judged` (one pair,
/// smoke scale), and `--judge` recomputes exactly the figures it printed.
#[test]
fn telemetry_overhead_smoke() {
    let machine = Machine::new();
    let work = machine.root.join("work/overhead");
    let output = machine.script(
        "telemetry-overhead.py",
        &[
            "--af",
            crate::common::AF,
            "--pairs",
            "1",
            "--scale",
            "smoke",
            "--work",
            work.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(output.status.code(), Some(0), "{}", text(&output));
    let measured = document(&output);
    let validator = schemas::validator("telemetry-overhead-v1.json");
    schemas::valid(&validator, &measured);
    every_property_required(&schema_document("telemetry-overhead-v1.json"));
    let names: Vec<&str> = measured["fixtures"]
        .as_array()
        .unwrap()
        .iter()
        .map(|fixture| fixture["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, FIXTURES, "{measured}");
    for fixture in measured["fixtures"].as_array().unwrap() {
        let pairs = fixture["pairs"].as_array().unwrap();
        assert_eq!(pairs.len(), 1, "{fixture}");
        let pair = &pairs[0];
        assert_eq!(pair["off"]["exit_code"], 0, "{fixture}");
        assert_eq!(pair["spans"]["exit_code"], 0, "{fixture}");
        assert_eq!(
            pair["off"]["segments"], 0,
            "nothing recorded at off: {fixture}"
        );
        assert!(
            pair["spans"]["segments"].as_u64().unwrap() >= 1,
            "a sealed segment at spans: {fixture}"
        );
        assert!(
            fixture["store_events"].as_u64().unwrap() > 0,
            "the run appended Store events: {fixture}"
        );
    }
    assert_eq!(measured["drain"]["fixture"], "telemetry-bench-dense");
    assert_eq!(measured["drain"]["pairs"].as_array().unwrap().len(), 1);
    assert_eq!(measured["protocol"]["scale"], "smoke");
    assert_eq!(measured["gate"]["verdict"], "not_judged", "{measured}");
    // The work directory held every run, each removed once measured.
    for fixture in FIXTURES {
        let runs: Vec<String> = std::fs::read_dir(work.join(fixture))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(runs, ["pristine"], "{fixture}");
    }
    // The rule recomputes what the measurement printed.
    let recorded = machine.root.join("work/smoke.json");
    std::fs::write(&recorded, serde_json::to_vec(&measured).unwrap()).unwrap();
    let judged = machine.script(
        "telemetry-overhead.py",
        &["--judge", recorded.to_str().unwrap(), "--json"],
    );
    assert_eq!(judged.status.code(), Some(0), "{}", text(&judged));
    assert_eq!(document(&judged), measured);
    // One shape: a field the schema does not name is refused wherever it is added.
    refuses_unnamed(
        &validator,
        &measured,
        &[
            "",
            "/af",
            "/machine",
            "/protocol",
            "/fixtures/0",
            "/fixtures/0/pairs/0",
            "/fixtures/0/pairs/0/off",
            "/drain",
            "/drain/pairs/0",
            "/gate",
        ],
    );
}

/// Recorded wall times, `off` then `spans`, in milliseconds: 20 pairs per fixture.
const CONCURRENT: [(f64, f64); 20] = [
    (2911.4, 2930.2),
    (2876.0, 2899.1),
    (2950.3, 2961.0),
    (2899.7, 2921.5),
    (2930.1, 2937.4),
    (2888.8, 2910.0),
    (2941.2, 2950.9),
    (2902.5, 2914.8),
    (2919.9, 2944.1),
    (2895.0, 2903.3),
    (2908.6, 2925.7),
    (2925.3, 2936.0),
    (2880.4, 2899.9),
    (2934.8, 2948.2),
    (2913.1, 2920.6),
    (2899.0, 2917.3),
    (2944.7, 2956.1),
    (2906.2, 2921.9),
    (2921.0, 2933.4),
    (2890.5, 2907.7),
];
const DENSE: [(f64, f64); 20] = [
    (631.2, 636.0),
    (628.4, 633.1),
    (640.0, 641.9),
    (635.5, 640.2),
    (629.9, 636.8),
    (633.3, 635.0),
    (638.1, 644.7),
    (630.6, 634.2),
    (636.7, 639.5),
    (632.0, 637.7),
    (634.4, 638.0),
    (629.1, 633.9),
    (637.5, 640.1),
    (631.8, 636.3),
    (635.0, 637.2),
    (633.9, 639.8),
    (628.7, 632.0),
    (639.2, 642.5),
    (630.3, 635.6),
    (634.8, 636.9),
];
const STORE: [(f64, f64); 20] = [
    (12750.2, 12801.4),
    (12688.9, 12760.0),
    (12811.3, 12840.7),
    (12702.6, 12769.8),
    (12795.0, 12828.3),
    (12733.1, 12790.6),
    (12760.4, 12811.9),
    (12699.8, 12744.5),
    (12822.7, 12861.0),
    (12741.5, 12800.2),
    (12770.0, 12809.4),
    (12715.9, 12768.3),
    (12789.6, 12830.0),
    (12728.2, 12771.7),
    (12801.8, 12852.6),
    (12744.0, 12790.1),
    (12693.3, 12750.8),
    (12780.5, 12822.2),
    (12722.9, 12777.0),
    (12766.1, 12804.9),
];
/// Drain latency, `off` then `spans`, in milliseconds, of the dense fixture's pairs.
const DRAIN: [(f64, f64); 20] = [
    (0.645, 0.601),
    (0.562, 0.645),
    (0.606, 0.628),
    (0.591, 0.613),
    (0.633, 0.640),
    (0.610, 0.598),
    (0.587, 0.622),
    (0.640, 0.651),
    (0.599, 0.605),
    (0.615, 0.631),
    (0.602, 0.590),
    (0.628, 0.644),
    (0.594, 0.611),
    (0.637, 0.629),
    (0.608, 0.626),
    (0.583, 0.600),
    (0.621, 0.639),
    (0.612, 0.607),
    (0.596, 0.618),
    (0.630, 0.642),
];

fn run(wall_ms: f64, segments: u64) -> Value {
    json!({"wall_ms": wall_ms, "exit_code": 0, "peak_rss_bytes": 40_000_000, "segments": segments})
}

/// A recorded document: its pairs as measured, every figure left for the rule to compute.
fn recorded(fixtures: &[(&str, &[(f64, f64)])], drain: &[(f64, f64)], scale: &str) -> Value {
    let fixtures: Vec<Value> = fixtures
        .iter()
        .map(|(name, pairs)| {
            json!({
                "name": name,
                "command": "task start --execute",
                "store_events": 48,
                "pairs": pairs.iter().enumerate().map(|(index, (off, spans))| json!({
                    "order": if index % 2 == 0 { ["off", "spans"] } else { ["spans", "off"] },
                    "off": run(*off, 0),
                    "spans": run(*spans, 1),
                    "difference_pct": 0.0,
                })).collect::<Vec<_>>(),
                "median_pct": 0.0,
                "interval_pct": [0.0, 0.0],
                "passed": false,
            })
        })
        .collect();
    json!({
        "schema": "af/telemetry-overhead@1",
        "af": {"path": "/recorded/af", "version": "af 0.12.0"},
        "machine": {"os": "darwin", "arch": "arm64", "cpus": 14, "load1": 2.5},
        "protocol": {"pairs": 20, "scale": scale, "seed": 20261021, "resamples": 10000,
                     "median_bound_pct": 2.0, "upper_bound_pct": 3.0},
        "fixtures": fixtures,
        "drain": {
            "fixture": "telemetry-bench-dense",
            "bound_ms": 10.0,
            "pairs": drain.iter().map(|(off, spans)| json!({
                "off_ms": off, "spans_ms": spans, "difference_ms": 0.0
            })).collect::<Vec<_>>(),
            "median_ms": 0.0,
            "interval_ms": [0.0, 0.0],
            "passed": false,
        },
        "gate": {"verdict": "not_judged", "reasons": []},
    })
}

/// `--judge` over `document`: its exit code and the recomputed document.
fn judge(machine: &Machine, recorded: &Value) -> (Option<i32>, Value) {
    let path = machine.root.join("work/recorded.json");
    std::fs::write(&path, serde_json::to_vec(recorded).unwrap()).unwrap();
    let output = machine.script(
        "telemetry-overhead.py",
        &["--judge", path.to_str().unwrap(), "--json"],
    );
    let code = output.status.code();
    let judged = if code == Some(1) {
        Value::Null
    } else {
        document(&output)
    };
    (code, judged)
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}

fn round3(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// Design §12: the gate passes when every fixture's median paired overhead is at most 2% and the
/// upper bound of its 95% bootstrap interval at most 3%, and the dense fixture's drain latency
/// median difference at most 10 ms — on recorded distributions, at the bounds and past each, and
/// it is judged only over all three fixtures at full scale with 20 pairs.
#[test]
fn telemetry_overhead_gate_rule() {
    let machine = Machine::new();
    let validator = schemas::validator("telemetry-overhead-v1.json");
    let all = |concurrent: &[(f64, f64)], dense: &[(f64, f64)], store: &[(f64, f64)]| {
        vec![
            ("telemetry-bench-concurrent", concurrent.to_vec()),
            ("telemetry-bench-dense", dense.to_vec()),
            ("telemetry-bench-store", store.to_vec()),
        ]
    };
    let build = |fixtures: Vec<(&str, Vec<(f64, f64)>)>, drain: &[(f64, f64)], scale: &str| {
        let borrowed: Vec<(&str, &[(f64, f64)])> = fixtures
            .iter()
            .map(|(name, pairs)| (*name, pairs.as_slice()))
            .collect();
        recorded(&borrowed, drain, scale)
    };

    // The recorded distributions pass, and every figure is the rule's own.
    let (code, passed) = judge(
        &machine,
        &build(all(&CONCURRENT, &DENSE, &STORE), &DRAIN, "full"),
    );
    assert_eq!(code, Some(0), "{passed}");
    schemas::valid(&validator, &passed);
    assert_eq!(passed["gate"], json!({"verdict": "pass", "reasons": []}));
    for (fixture, pairs) in
        passed["fixtures"]
            .as_array()
            .unwrap()
            .iter()
            .zip([&CONCURRENT[..], &DENSE[..], &STORE[..]])
    {
        let differences: Vec<f64> = pairs
            .iter()
            .map(|(off, spans)| round3((spans - off) / off * 100.0))
            .collect();
        // Each rounded to the thousandth, Python's ties to even against Rust's away from zero.
        assert!(
            (fixture["median_pct"].as_f64().unwrap() - round3(median(differences.clone()))).abs()
                < 0.002,
            "{fixture}"
        );
        let interval = &fixture["interval_pct"];
        let (low, high) = (interval[0].as_f64().unwrap(), interval[1].as_f64().unwrap());
        let (least, most) = differences
            .iter()
            .fold((f64::MAX, f64::MIN), |(l, m), v| (l.min(*v), m.max(*v)));
        assert!(least <= low && low <= fixture["median_pct"].as_f64().unwrap());
        assert!(fixture["median_pct"].as_f64().unwrap() <= high && high <= most);
        assert_eq!(fixture["passed"], true);
    }
    let drain: Vec<f64> = DRAIN
        .iter()
        .map(|(off, spans)| round3(spans - off))
        .collect();
    assert!((passed["drain"]["median_ms"].as_f64().unwrap() - round3(median(drain))).abs() < 0.002);
    // The same pairs judged again give the same document: the bootstrap is seeded.
    assert_eq!(judge(&machine, &passed).1, passed);

    // Exactly at the bounds passes: every pair 2% over, so the interval is [2%, 2%].
    let at_bound: Vec<(f64, f64)> = (0..20).map(|_| (1000.0, 1020.0)).collect();
    let (code, judged) = judge(
        &machine,
        &build(all(&at_bound, &DENSE, &STORE), &[(0.5, 10.5); 20], "full"),
    );
    assert_eq!(code, Some(0), "{judged}");
    assert_eq!(judged["fixtures"][0]["median_pct"], 2.0);
    assert_eq!(judged["fixtures"][0]["interval_pct"], json!([2.0, 2.0]));
    assert_eq!(judged["drain"]["median_ms"], 10.0);
    assert_eq!(judged["gate"]["verdict"], "pass");

    // A median past 2% fails, naming the fixture.
    let slow: Vec<(f64, f64)> = CONCURRENT
        .iter()
        .map(|(off, spans)| (*off, spans + off * 0.025))
        .collect();
    let (code, judged) = judge(&machine, &build(all(&slow, &DENSE, &STORE), &DRAIN, "full"));
    assert_eq!(code, Some(3), "{judged}");
    assert_eq!(judged["gate"]["verdict"], "fail");
    assert_eq!(judged["fixtures"][0]["passed"], false);
    assert!(
        judged["gate"]["reasons"][0]
            .as_str()
            .unwrap()
            .starts_with("telemetry-bench-concurrent: median +"),
        "{judged}"
    );

    // A median within 2% whose interval reaches past 3% fails: eleven pairs 1.5% over and nine
    // 6% over.
    let spread: Vec<(f64, f64)> = (0..20)
        .map(|index| (1000.0, if index < 11 { 1015.0 } else { 1060.0 }))
        .collect();
    let (code, judged) = judge(
        &machine,
        &build(all(&CONCURRENT, &spread, &STORE), &DRAIN, "full"),
    );
    assert_eq!(code, Some(3), "{judged}");
    let dense = &judged["fixtures"][1];
    assert_eq!(dense["median_pct"], 1.5, "{judged}");
    assert!(dense["interval_pct"][1].as_f64().unwrap() > 3.0, "{judged}");
    assert_eq!(dense["passed"], false);
    assert_eq!(judged["gate"]["verdict"], "fail");

    // A drain latency 12 ms slower at `spans` fails the gate though every overhead passes.
    let late: Vec<(f64, f64)> = DRAIN.iter().map(|(off, _)| (*off, off + 12.0)).collect();
    let (code, judged) = judge(
        &machine,
        &build(all(&CONCURRENT, &DENSE, &STORE), &late, "full"),
    );
    assert_eq!(code, Some(3), "{judged}");
    assert_eq!(judged["drain"]["passed"], false);
    assert!(
        judged["fixtures"]
            .as_array()
            .unwrap()
            .iter()
            .all(|fixture| fixture["passed"] == true)
    );
    assert_eq!(judged["gate"]["verdict"], "fail");

    // Fewer than 20 pairs, the smoke scale or a missing fixture is never judged, even when a
    // figure is past its bound: the exit is 0 and the reasons say why.
    for (document, why) in [
        (
            build(
                all(&CONCURRENT[..19], &DENSE[..19], &STORE[..19]),
                &DRAIN,
                "full",
            ),
            "19 pairs, fewer than 20",
        ),
        (
            build(all(&slow, &DENSE, &STORE), &DRAIN, "smoke"),
            "scale `smoke`, not `full`",
        ),
        (
            build(
                vec![
                    ("telemetry-bench-concurrent", CONCURRENT.to_vec()),
                    ("telemetry-bench-dense", DENSE.to_vec()),
                ],
                &DRAIN,
                "full",
            ),
            "2 of the 3 fixtures measured",
        ),
    ] {
        let (code, judged) = judge(&machine, &document);
        assert_eq!(code, Some(0), "{judged}");
        assert_eq!(judged["gate"]["verdict"], "not_judged", "{judged}");
        assert!(
            judged["gate"]["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason == why),
            "{why}: {judged}"
        );
        schemas::valid(&validator, &judged);
    }

    // The bounds are the protocol's: a document that names looser ones is refused, never judged.
    let mut looser = build(all(&slow, &DENSE, &STORE), &DRAIN, "full");
    looser["protocol"]["median_bound_pct"] = json!(5.0);
    assert_eq!(judge(&machine, &looser).0, Some(1));
    let mut looser = build(all(&CONCURRENT, &DENSE, &STORE), &late, "full");
    looser["drain"]["bound_ms"] = json!(20.0);
    assert_eq!(judge(&machine, &looser).0, Some(1));

    // A document of another shape is refused, never judged.
    let mut foreign = build(all(&CONCURRENT, &DENSE, &STORE), &DRAIN, "full");
    foreign["schema"] = json!("af/telemetry-overhead@2");
    assert_eq!(judge(&machine, &foreign).0, Some(1));
    assert!(!validator.is_valid(&foreign));
}

/// Every file below `root`, relative, with its bytes.
fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let name = path.strip_prefix(root).unwrap().display().to_string();
                out.push((name, std::fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// Design §12: `telemetry-corpus-10k` is generated deterministically from a seed. Two
/// generations from one seed are byte-identical at a small scale, however many processes write
/// them, and another seed differs; the generator writes only into an empty directory outside af's
/// state root; af reads every segment it wrote, and the three measurements on it validate
/// against `af/telemetry-read-latency@1`.
#[test]
fn telemetry_corpus_is_deterministic() {
    let machine = Machine::new();
    let generate = |out: &Path, seed: &str, jobs: &str| {
        machine.script(
            "telemetry-corpus.py",
            &[
                "generate",
                "--out",
                out.to_str().unwrap(),
                "--seed",
                seed,
                "--runs",
                "4",
                "--jobs",
                jobs,
            ],
        )
    };
    let (first, second, other) = (
        machine.root.join("work/first"),
        machine.root.join("work/second"),
        machine.root.join("work/other"),
    );
    let output = generate(&first, "7", "3");
    assert!(output.status.success(), "{}", text(&output));
    let summary = document(&output);
    assert_eq!(summary["executions"], 8);
    assert!(generate(&second, "7", "1").status.success());
    assert!(generate(&other, "8", "2").status.success());
    let (first_tree, second_tree) = (tree(&first), tree(&second));
    assert!(first_tree.len() > 80, "{}", first_tree.len());
    assert!(
        first_tree == second_tree,
        "two generations from one seed differ"
    );
    assert!(tree(&other) != first_tree, "another seed is another corpus");
    // The byte target holds at any scale: 2 GiB of segments per 10,000 runs, within 5%.
    let target = (2_u64 << 30) * 4 / 10_000;
    let segment_bytes = summary["segment_bytes"].as_u64().unwrap();
    assert!(
        segment_bytes.abs_diff(target) * 20 <= target,
        "{segment_bytes} against {target}"
    );

    // Never into a directory that holds anything, nor into af's state root.
    let refused = generate(&first, "7", "1");
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("is not empty"));
    let inside = machine.root.join("state/af/corpus");
    let refused = generate(&inside, "7", "1");
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused));
    assert!(!inside.exists(), "nothing written into the state root");

    // af reads all of it: the three reads, measured warm, validate.
    let output = machine.script(
        "telemetry-corpus.py",
        &[
            "measure",
            "--dir",
            first.to_str().unwrap(),
            "--af",
            crate::common::AF,
            "--repeat",
            "1",
            "--json",
        ],
    );
    assert!(
        matches!(output.status.code(), Some(0) | Some(3)),
        "{}",
        text(&output)
    );
    let measured = document(&output);
    let validator = schemas::validator("telemetry-read-latency-v1.json");
    schemas::valid(&validator, &measured);
    every_property_required(&schema_document("telemetry-read-latency-v1.json"));
    let corpus = &measured["corpus"];
    for (field, expected) in [
        ("runs", 4),
        ("executions", 8),
        ("segments", 8),
        ("spans", 800),
        ("events", 1600),
        ("blobs", 80),
        ("blob_files", 80),
        ("shared_blobs", 20),
        ("blob_occurrences", 100),
    ] {
        assert_eq!(corpus[field], expected, "{field}: {corpus}");
    }
    assert_eq!(corpus["name"], "telemetry-corpus-4");
    assert_eq!(corpus["seed"], 7);
    assert_eq!(corpus["segment_bytes"], segment_bytes);
    let names: Vec<&str> = measured["measurements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["fresh_status", "unchanged_status", "show"]);
    for entry in measured["measurements"].as_array().unwrap() {
        assert!(
            entry["warm"]["index_bytes"].as_u64().unwrap() > 0,
            "{entry}"
        );
    }
    refuses_unnamed(
        &validator,
        &measured,
        &[
            "",
            "/af",
            "/machine",
            "/corpus",
            "/cold",
            "/measurements/0",
            "/measurements/0/warm",
        ],
    );
}
