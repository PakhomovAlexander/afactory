//! Design §15 P7 and §16 items 2 and 5: the content level end to end on the Claude fake — the
//! composed Worker input, the released harness content and the full-text index read back by
//! `af telemetry context`, `reasoning` and `search`; nothing of it below `content`; the content
//! switches and allowances; credentials withheld on every blob path; and no Worker prompt ever
//! changed by a telemetry directory full of content.

use std::path::{Path, PathBuf};

use review_telemetry::record::Record;
use review_telemetry::trace::sha256_digest;
use serde_json::{Value, json};

use crate::telemetry_runs::{Machine, copy_tree};
use crate::telemetry_streams::{
    claude_run, fixture_repo, json_of, raw_evidence, store_events, workspace,
};

/// A phrase the scripted harness says in its final message, found by `search`.
const PHRASE: &str = "periwinkle lighthouse keeper";
/// The thinking blocks the scripted harness prints, in order.
const THOUGHTS: [&str; 2] = [
    "first weigh the lamp before the keeper",
    "then trust the tide tables over the almanac",
];
/// Credential material the harness plants in its first tool result and its first thought.
const PLANTED: &str = "password=hunter2-planted-7781";

/// The harness table of a run whose stream carries the planted content.
fn planted() -> Value {
    json!({
        "record_stdin": "request.bin",
        "thinking": [format!("{}\n{PLANTED}", THOUGHTS[0]), THOUGHTS[1]],
        "say": format!("the {PHRASE} answered"),
        "tool_output_prefix": format!("{PLANTED}\n"),
    })
}

/// `af` on `machine` at `level` in `cwd`: the configured telemetry directory.
fn af_at(machine: &Machine, cwd: &Path, level: &str, args: &[&str]) -> std::process::Output {
    machine.af(cwd, &[("AF_TELEMETRY__LEVEL", level)], args)
}

/// Every record of every sealed segment beneath a telemetry directory.
fn records(telemetry: &Path) -> Vec<Record> {
    let mut records = Vec::new();
    for entry in std::fs::read_dir(telemetry.join("v1/segments"))
        .unwrap()
        .flatten()
    {
        if entry.file_name().to_string_lossy().ends_with(".jsonl.gz") {
            records.extend(review_telemetry::segment::reader::read_sealed(&entry.path()).unwrap());
        }
    }
    records
}

/// The run id and the Worker Attempt of the fixture's one `root.nodes.stream` node, the
/// latest run's when the directory holds several.
fn run_and_attempt(machine: &Machine, cwd: &Path, level: &str) -> (String, String) {
    let runs = json_of(&af_at(
        machine,
        cwd,
        level,
        &["telemetry", "runs", "--json"],
    ));
    let run = runs["runs"][0]["run_id"].as_str().unwrap().to_string();
    let attempt = records(&machine.telemetry())
        .into_iter()
        .filter_map(|record| match record {
            Record::SpanStart(start)
                if start.name == "af.attempt"
                    && start.attributes["af.node_id"] == "root.nodes.stream" =>
            {
                start.attributes["af.attempt_id"]
                    .as_str()
                    .map(str::to_string)
            }
            _ => None,
        })
        .next_back()
        .expect("the stream node's Attempt");
    (run, attempt)
}

/// Every event named `name` in the directory's sealed segments.
fn events_named(telemetry: &Path, name: &str) -> Vec<review_telemetry::record::Event> {
    records(telemetry)
        .into_iter()
        .filter_map(|record| match record {
            Record::Event(event) if event.name == name => Some(event),
            _ => None,
        })
        .collect()
}

/// Every blob file's retained bytes beneath a telemetry directory, each read and verified
/// against its digest by the crate's blob reader.
fn blob_texts(telemetry: &Path) -> Vec<String> {
    let layout = review_telemetry::layout::Layout::new(telemetry);
    let mut texts = Vec::new();
    for prefix in std::fs::read_dir(telemetry.join("v1/blobs"))
        .into_iter()
        .flatten()
        .flatten()
    {
        for file in std::fs::read_dir(prefix.path()).unwrap().flatten() {
            let name = file.file_name().to_string_lossy().into_owned();
            let Some(hex) = name.strip_suffix(".gz") else {
                continue;
            };
            match review_telemetry::blob::reader::read(&layout, &format!("sha256:{hex}")) {
                review_telemetry::blob::reader::Content::Bytes(bytes) => {
                    texts.push(String::from_utf8(bytes).unwrap());
                }
                review_telemetry::blob::reader::Content::Unavailable(reason) => {
                    panic!("{name}: {reason}")
                }
            }
        }
    }
    texts
}

/// The one-Attempt invocation fixture planned once on a machine (`af task start` without
/// `--execute`), then executed any number of times from a copy of that same pristine plan and
/// Store with the same writer pid, so two executions differ only in what is under test and in
/// the times they ran at (`telemetry_never_steers` does the same, and fixes the Store's clock
/// too, which a model Worker's real deadline cannot take).
struct Planned {
    work: PathBuf,
    pristine: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    auth: PathBuf,
    providers: PathBuf,
    path: String,
}

const TASK_ID: &str = "telemetry-bench-invocation-one";

impl Planned {
    fn new(machine: &Machine, name: &str, harness: &Value) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let fixture = workspace().join("fixtures/telemetry-bench/telemetry-bench-invocation-one");
        let base = machine.root.join(name);
        let work = base.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let repo = work.join("repo");
        fixture_repo(&fixture.join("repo"), &repo);
        let (bin, auth) = (base.join("bin"), base.join("auth"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&auth).unwrap();
        std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::copy(
            workspace().join("fixtures/telemetry-bench/invocation-claude.py"),
            bin.join("claude"),
        )
        .unwrap();
        std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        let mut table = json!({"shape":"requested","tool_output_bytes":3 * 65536,
                               "chunk_bytes":65536,"report":false});
        for (key, value) in harness.as_object().into_iter().flatten() {
            table[key] = value.clone();
        }
        std::fs::write(
            auth.join("bench.json"),
            json!({ "harness": table }).to_string(),
        )
        .unwrap();
        let providers = base.join("providers.toml");
        std::fs::write(
            &providers,
            format!(
                "version = 1\n\n[[providers]]\nid = \"claude-bench\"\nkind = \"claude\"\nauth_dir = \"{}\"\n",
                auth.display()
            ),
        )
        .unwrap();
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap()
        .into_string()
        .unwrap();
        let planned = Self {
            pristine: base.join("pristine"),
            state: work.join("store"),
            work,
            repo,
            auth,
            providers,
            path,
        };
        let output = machine.af(
            &planned.repo,
            &planned.env("off", &[]),
            &[
                "task",
                "start",
                "--file",
                fixture.join("task.json").to_str().unwrap(),
                "--state",
                planned.state.to_str().unwrap(),
                "--json",
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        copy_tree(&planned.work, &planned.pristine);
        planned
    }

    fn env<'a>(&'a self, level: &'a str, extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
        let mut env = vec![
            ("AF_TELEMETRY__LEVEL", level),
            ("AF_PROVIDERS_FILE", self.providers.to_str().unwrap()),
            ("PATH", self.path.as_str()),
            ("USER", "fixture"),
        ];
        env.extend_from_slice(extra);
        env
    }

    /// Execute the plan from its pristine Store at `level`; the inspection.
    fn execute(&self, machine: &Machine, level: &str, extra: &[(&str, &str)]) -> Value {
        std::fs::remove_dir_all(&self.work).unwrap();
        copy_tree(&self.pristine, &self.work);
        let mut env = vec![("AF_TEST_WRITER_PID", "4242")];
        env.extend_from_slice(extra);
        let output = machine.af(
            &self.repo,
            &self.env(level, &env),
            &[
                "task",
                "run",
                TASK_ID,
                "--execute",
                "--state",
                self.state.to_str().unwrap(),
                "--json",
            ],
        );
        let inspection: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| panic!("{level}: {}", String::from_utf8_lossy(&output.stderr)));
        assert_eq!(
            inspection["phase"]["kind"], "finished",
            "{level}: {inspection}"
        );
        inspection
    }
}

fn validated(name: &str, document: &Value) {
    crate::schemas::valid(&crate::schemas::validator(name), document);
}

/// Design §16 item 2 (P7's exit evidence): on the Claude fake at `content`, `context --raw`
/// prints exactly the bytes the Worker received — its source digest is theirs, its length the
/// manifest's — `reasoning` prints the thinking blocks in order, and `search` finds a planted
/// phrase with the span path of the turn that said it; the planted credential is in no blob,
/// no excerpt and no output, and the typed identities beside it (digests, ids) are intact.
#[test]
fn telemetry_content_reads_back_on_the_claude_fake() {
    let machine = Machine::new();
    let run = claude_run(&machine, "run", "content", &planted(), &[]);
    let (run_id, attempt) = run_and_attempt(&machine, &run.repo, "content");
    let received = std::fs::read(run.auth.join("request.bin")).unwrap();
    // `--raw`: the prompt bytes the Worker received, exactly.
    let raw = af_at(
        &machine,
        &run.repo,
        "content",
        &["telemetry", "context", &run_id, &attempt, "--raw"],
    );
    assert!(
        raw.status.success(),
        "{}",
        String::from_utf8_lossy(&raw.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&raw.stdout),
        String::from_utf8_lossy(&received),
        "context --raw prints the bytes the Worker received"
    );
    let context = json_of(&af_at(
        &machine,
        &run.repo,
        "content",
        &["telemetry", "context", &run_id, &attempt, "--json"],
    ));
    validated("telemetry-context-v1.json", &context);
    assert_eq!(context["state"], "captured", "{context}");
    assert_eq!(context["level"], "content");
    let input = &context["input"];
    assert_eq!(input["event"], "gen_ai.content.input");
    assert_eq!(input["state"], "stored");
    assert_eq!(input["source_digest"], sha256_digest(&received));
    assert_eq!(input["original_bytes"], received.len() as u64);
    assert_eq!(context["rendered_bytes"], received.len() as u64);
    assert_eq!(
        input["digest"],
        sha256_digest(&received),
        "nothing withheld"
    );
    assert_eq!(context["system"]["event"], "gen_ai.content.system");
    let manifest: Value =
        serde_json::from_str(context["system"]["content"].as_str().unwrap()).unwrap();
    assert!(
        manifest["entries"]
            .as_array()
            .is_some_and(|entries| entries.len() >= 3)
    );
    // The human view names the Attempt and prints the same prompt.
    let text = af_at(
        &machine,
        &run.repo,
        "content",
        &["telemetry", "context", &run_id, &attempt],
    );
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(
        text.contains(&format!("context of Attempt {attempt}")),
        "{text}"
    );
    assert!(text.contains(&String::from_utf8_lossy(&received).to_string()));

    // The thinking blocks, in order, the planted line withheld.
    let reasoning = json_of(&af_at(
        &machine,
        &run.repo,
        "content",
        &["telemetry", "reasoning", &run_id, &attempt, "--json"],
    ));
    validated("telemetry-reasoning-v1.json", &reasoning);
    assert_eq!(reasoning["state"], "captured", "{reasoning}");
    let thoughts = reasoning["thoughts"].as_array().unwrap();
    assert_eq!(thoughts.len(), 2, "{reasoning}");
    assert_eq!(thoughts[0]["kind"], "thinking");
    assert_eq!(
        thoughts[0]["part"]["content"],
        format!(
            "{}\n{}",
            THOUGHTS[0],
            review_telemetry::redact::WITHHELD_LINE
        )
    );
    assert_eq!(thoughts[1]["part"]["content"], THOUGHTS[1]);
    assert_eq!(thoughts[0]["turn_ordinal"], 1);
    assert!(thoughts[1]["turn_ordinal"].as_u64().unwrap() > 1);

    // The planted phrase, found with the span path of the turn that said it.
    let found = json_of(&af_at(
        &machine,
        &run.repo,
        "content",
        &["telemetry", "search", PHRASE, "--json"],
    ));
    validated("telemetry-search-v1.json", &found);
    let matches = found["matches"].as_array().unwrap();
    assert_eq!(matches.len(), 1, "{found}");
    assert!(
        matches[0]["excerpt"]
            .as_str()
            .unwrap()
            .contains("lighthouse")
    );
    let occurrence = &matches[0]["occurrences"][0];
    assert_eq!(occurrence["event"], "gen_ai.content.output");
    assert_eq!(occurrence["attempt_id"], attempt.as_str());
    assert_eq!(occurrence["run_id"], run_id.as_str());
    let path: Vec<&str> = occurrence["span_path"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();
    assert_eq!(
        path,
        [
            "af.command",
            "af.run",
            "af.node",
            "af.attempt",
            "af.harness",
            "af.turn"
        ]
    );
    // A thought and the tool results are searchable too, with `--run`: the first result (its
    // planted line withheld) and the two identical later ones, stored once.
    for (words, blobs) in [("tide tables", 1), ("captured source", 2)] {
        let found = json_of(&af_at(
            &machine,
            &run.repo,
            "content",
            &["telemetry", "search", words, "--run", &run_id, "--json"],
        ));
        assert_eq!(
            found["matches"].as_array().unwrap().len(),
            blobs,
            "{words}: {found}"
        );
    }

    // The planted credential: in no blob, no excerpt, no output.
    let telemetry = machine.telemetry();
    let texts = blob_texts(&telemetry);
    assert!(texts.len() > 4, "content blobs were written");
    for text in &texts {
        assert!(!text.contains("hunter2"), "a blob holds the credential");
    }
    let refused = json_of(&af_at(
        &machine,
        &run.repo,
        "content",
        &["telemetry", "search", "hunter2", "--json"],
    ));
    assert_eq!(refused["matches"], json!([]));
    let around = json_of(&af_at(
        &machine,
        &run.repo,
        "content",
        &["telemetry", "search", "line withheld", "--json"],
    ));
    for found in around["matches"].as_array().unwrap() {
        assert!(!found["excerpt"].as_str().unwrap().contains("hunter2"));
    }
    for document in [&context, &reasoning, &found] {
        assert!(!document.to_string().contains("hunter2"));
    }
}

/// Design §9, §10 and §15 P7: search exists only at `content`. Below it `context` and
/// `reasoning` say the content was not captured, naming the level, and `search` is refused
/// with exit 2 and a message saying why; at `content` the same search answers.
#[test]
fn telemetry_search_only_at_content() {
    let machine = Machine::new();
    let run = claude_run(&machine, "spans", "spans", &planted(), &[]);
    let (run_id, attempt) = run_and_attempt(&machine, &run.repo, "spans");
    for (command, schema) in [
        ("context", "telemetry-context-v1.json"),
        ("reasoning", "telemetry-reasoning-v1.json"),
    ] {
        let document = json_of(&af_at(
            &machine,
            &run.repo,
            "spans",
            &["telemetry", command, &run_id, &attempt, "--json"],
        ));
        validated(schema, &document);
        assert_eq!(document["state"], "not_captured", "{command}: {document}");
        assert!(
            document["reason"]
                .as_str()
                .unwrap()
                .contains("content was not captured: this execution recorded at level spans"),
            "{document}"
        );
        let text = af_at(
            &machine,
            &run.repo,
            "spans",
            &["telemetry", command, &run_id, &attempt],
        );
        assert!(text.status.success());
        assert!(
            String::from_utf8_lossy(&text.stdout)
                .contains("not captured: content was not captured"),
            "{command}"
        );
    }
    // The compose span's structure is there at `spans`, its bytes are not.
    let context = json_of(&af_at(
        &machine,
        &run.repo,
        "spans",
        &["telemetry", "context", &run_id, &attempt, "--json"],
    ));
    assert!(context["manifest"].is_string());
    assert!(context.get("input").is_none());
    assert!(
        !machine.telemetry().join("v1/blobs").exists()
            || blob_texts(&machine.telemetry()).is_empty()
    );
    for level in ["off", "metrics", "spans"] {
        let refused = af_at(&machine, &run.repo, level, &["telemetry", "search", PHRASE]);
        assert_eq!(refused.status.code(), Some(2), "{level}");
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            stderr.contains("content search exists only at [telemetry] level = \"content\""),
            "{level}: {stderr}"
        );
        let json = af_at(
            &machine,
            &run.repo,
            level,
            &["telemetry", "search", PHRASE, "--json"],
        );
        assert_eq!(json.status.code(), Some(2));
        let document: Value = serde_json::from_slice(&json.stdout).unwrap();
        assert_eq!(document["schema"], "af/error@1");
    }
    // `content_index = false` refuses as well.
    let off = machine.af(
        &run.repo,
        &[
            ("AF_TELEMETRY__LEVEL", "content"),
            ("AF_TELEMETRY__CONTENT_INDEX", "false"),
        ],
        &["telemetry", "search", PHRASE],
    );
    assert_eq!(off.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&off.stderr).contains("content_index = false"));
    // At `content` the same search answers.
    let content = claude_run(&machine, "content", "content", &planted(), &[]);
    let found = json_of(&af_at(
        &machine,
        &content.repo,
        "content",
        &["telemetry", "search", PHRASE, "--json"],
    ));
    assert_eq!(found["matches"].as_array().unwrap().len(), 1, "{found}");
}

/// Design §8 and §15 P7: with `content_reasoning = false` and `content_tool_io = false` at
/// `content`, no thinking block and no tool argument or result reaches a blob, the index or any
/// command; the composed input and the assistant's text still do.
#[test]
fn telemetry_content_switches_hold() {
    let machine = Machine::new();
    let run = claude_run(
        &machine,
        "run",
        "content",
        &planted(),
        &[
            ("AF_TELEMETRY__CONTENT_REASONING", "false"),
            ("AF_TELEMETRY__CONTENT_TOOL_IO", "false"),
        ],
    );
    let (run_id, attempt) = run_and_attempt(&machine, &run.repo, "content");
    let texts = blob_texts(&machine.telemetry());
    assert!(
        !texts.is_empty(),
        "the composed input and the text are blobs"
    );
    for text in &texts {
        for absent in ["tide tables", "weigh the lamp", "captured source", "lib.rs"] {
            assert!(!text.contains(absent), "{absent} reached a blob");
        }
    }
    assert!(
        texts.iter().any(|text| text.contains(PHRASE)),
        "the assistant's text is kept"
    );
    for (words, expected) in [("tide tables", 0), ("captured source", 0), (PHRASE, 1)] {
        let found = json_of(&af_at(
            &machine,
            &run.repo,
            "content",
            &["telemetry", "search", words, "--json"],
        ));
        assert_eq!(
            found["matches"].as_array().unwrap().len(),
            expected,
            "{words}"
        );
    }
    let reasoning = json_of(&af_at(
        &machine,
        &run.repo,
        "content",
        &["telemetry", "reasoning", &run_id, &attempt, "--json"],
    ));
    assert_eq!(reasoning["state"], "not_captured", "{reasoning}");
    assert_eq!(
        reasoning["reason"],
        "content was not captured: [telemetry] content_reasoning = false"
    );
    assert_eq!(reasoning["thoughts"], json!([]));
    let context = json_of(&af_at(
        &machine,
        &run.repo,
        "content",
        &["telemetry", "context", &run_id, &attempt, "--json"],
    ));
    assert_eq!(context["state"], "captured");
    // No span carries a tool or reasoning content event; the switches are on the ids event.
    let telemetry = machine.telemetry();
    for name in [
        "af.content.tool_input",
        "af.content.tool_output",
        "af.content.reasoning",
    ] {
        assert!(events_named(&telemetry, name).is_empty(), "{name}");
    }
    assert!(!events_named(&telemetry, "gen_ai.content.output").is_empty());
    let ids = events_named(&telemetry, "af.harness.ids");
    assert!(!ids.is_empty());
    for event in ids {
        assert_eq!(event.attributes["af.content.reasoning"], false);
        assert_eq!(event.attributes["af.content.tool_io"], false);
    }
}

/// Design §8 and §15 P7: an Attempt whose harness output exhausts `content_attempt_max_bytes`
/// drops its further content and counts it, with one `budget` problem, and nothing about the
/// Worker changes: the run finishes, the Store receives the same events and raw evidence, and
/// the accounting is the same as with Telemetry `off`. (The process allowance, whose bound is
/// 4 MiB at the least, is held by the crate's fixture.)
#[test]
fn telemetry_content_allowances_hold() {
    let machine = Machine::new();
    // 20 MiB of tool output in 1 MiB results, past the smallest Attempt allowance.
    let planned = Planned::new(
        &machine,
        "run",
        &json!({"tool_output_bytes": 20 << 20, "chunk_bytes": 1 << 20}),
    );
    // The Store's events by kind and change: what the run did, without its times or the
    // heartbeat's lease renewals, which follow the wall clock.
    let kinds = |state: &Path| -> Vec<(String, String, String)> {
        store_events(state)
            .into_iter()
            .map(|event| {
                let payload: Value = serde_json::from_str(&event[10]).unwrap_or(Value::Null);
                let change = payload["change"]["kind"].as_str().unwrap_or("").to_string();
                (event[3].clone(), change, event[6].clone())
            })
            .filter(|(_, change, _)| change != "lease_renewed")
            .collect()
    };
    let off = planned.execute(&machine, "off", &[]);
    let off_events = kinds(&planned.state);
    let off_evidence = raw_evidence(&off, &planned.state);
    let content = planned.execute(
        &machine,
        "content",
        &[
            ("AF_TELEMETRY__CONTENT_ATTEMPT_MAX_BYTES", "16MiB"),
            // A queue the burst never fills, so the stream reaches the tap whole.
            ("AF_TELEMETRY__OBSERVER_QUEUE_BYTES", "64MiB"),
        ],
    );
    // Nothing about the Worker changed: the same Store events of the same Attempts, the same
    // raw evidence (its result line, the same CAS ids) and the same accounting.
    assert_eq!(kinds(&planned.state), off_events, "the Store's events");
    assert_eq!(raw_evidence(&content, &planned.state), off_evidence);
    for field in ["chargeable_tokens", "attempts", "phase"] {
        assert_eq!(content[field], off[field], "{field}");
    }
    let status = json_of(&af_at(
        &machine,
        &planned.repo,
        "content",
        &["telemetry", "status", "--json"],
    ));
    let problems = status["problems"].to_string();
    assert_eq!(
        problems
            .matches("content_attempt_max_bytes reached")
            .count(),
        1,
        "{problems}"
    );
    let dropped: u64 = events_named(&machine.telemetry(), "af.harness.ids")
        .iter()
        .filter_map(|event| event.attributes["af.content.dropped"].as_u64())
        .sum();
    assert!(dropped > 0, "the Attempt's dropped content is counted");
    let retained: usize = blob_texts(&machine.telemetry())
        .iter()
        .map(String::len)
        .sum();
    assert!(retained <= 17 << 20, "{retained} bytes retained");
}

/// Design §16 item 2 (ADR-0153 clause 2): the Worker-context fixture run twice — once with
/// Telemetry `off` on a machine with no telemetry directory, once at `content` on a machine
/// whose telemetry directory is already full of content from an earlier run, its thinking,
/// tool results and Worker input included — gives the Worker the same prompt bytes, and none of
/// the earlier run's content is in it.
#[test]
fn telemetry_content_never_reaches_a_worker_prompt() {
    let machine = Machine::new();
    // An earlier run at `content` fills the telemetry directory with its Worker's input, text,
    // thinking and tool results.
    claude_run(&machine, "earlier", "content", &planted(), &[]);
    let texts = blob_texts(&machine.telemetry());
    assert!(
        texts.iter().any(|text| text.contains("tide tables")),
        "the directory holds an earlier Worker's reasoning"
    );
    assert!(texts.iter().any(|text| text.contains(PHRASE)));
    assert!(
        texts
            .iter()
            .any(|text| text.contains("af.worker-request/1"))
    );
    // The same plan executed with Telemetry off and at content beside that directory.
    let planned = Planned::new(&machine, "plan", &json!({"record_stdin": "request.bin"}));
    planned.execute(&machine, "off", &[]);
    let expected = std::fs::read(planned.auth.join("request.bin")).unwrap();
    planned.execute(&machine, "content", &[]);
    let received = std::fs::read(planned.auth.join("request.bin")).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&received),
        String::from_utf8_lossy(&expected),
        "the prompt bytes are unchanged by a telemetry directory full of content"
    );
    let prompt = String::from_utf8_lossy(&received);
    for absent in [
        "tide tables",
        "weigh the lamp",
        PHRASE,
        "hunter2",
        "af.content",
    ] {
        assert!(!prompt.contains(absent), "{absent} reached a prompt");
    }
    // And the content run kept that prompt for the operator.
    assert!(
        blob_texts(&machine.telemetry())
            .iter()
            .any(|text| text.as_bytes() == received.as_slice())
    );
}

// ------------------------------------------------------------------------------------------
// the goldens

const GOLDEN_STORE: &str = "6767676767676767676767676767676767676767676767676767676767676767";
const GOLDEN_RUN: &str =
    "task:sha256:9898989898989898989898989898989898989898989898989898989898989898";
const GOLDEN_SEGMENT: &str = "1700000400000-4246-2b3c4d5e";
const GOLDEN_ATTEMPT: &str = "golden-content-attempt-1";

/// One execution of the golden run at `content`: its Attempt's composed input and manifest,
/// and a Claude harness whose stream carries two thinking blocks, a tool call and the final
/// text, released and kept as blobs, every time from an injected clock.
fn record_content_golden(root: &Path) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use review_telemetry::blob::{Document, Media};
    use review_telemetry::content::{Scope, event};
    use review_telemetry::level::Level;
    use review_telemetry::record::{Attrs, SealExit, SpanKind, SpanStatus};
    use review_telemetry::segment::writer::{Writer, WriterOptions};
    use review_telemetry::stream::{Harness, Release, Stream, Tap, TapOptions};
    use review_telemetry::subscriber::{RecordedSpan, SpanIdentity};

    let clock = Arc::new(AtomicU64::new(1_700_000_400_000 * 1_000_000));
    let mut options = WriterOptions::new(root, Level::Content);
    options.argv = vec!["af".into(), "task".into(), "run".into(), "golden".into()];
    options.cwd = PathBuf::from("/work");
    options.af_version = "0.0.0-golden".into();
    options.af_commit = "0123456789ab".into();
    options.redactor = review_telemetry::redact::Redactor::new();
    options.segment_id = Some(GOLDEN_SEGMENT.into());
    options.clock = Some(Arc::new(move || {
        u128::from(clock.fetch_add(1_000_000, Ordering::SeqCst))
    }));
    let writer = Writer::new(options).unwrap();
    let execution = review_telemetry::trace::Execution {
        store_id: GOLDEN_STORE.into(),
        run_id: GOLDEN_RUN.into(),
        execution_id: GOLDEN_SEGMENT.into(),
        ordinal: 1,
        store_sequence_at_start: 2,
    };
    let trace = execution.trace_id();
    let command = writer
        .span_start(None, None, "af.command", SpanKind::Internal, Attrs::new())
        .unwrap();
    let span = |name: &str, parent: &str, kind: SpanKind, attrs: Attrs| {
        writer
            .span_start(Some(&trace), Some(parent), name, kind, attrs)
            .unwrap()
    };
    let identity = execution.attributes().enumerated("af.run.kind", "task");
    let run = span("af.run", &command, SpanKind::Internal, identity.clone());
    let node = span(
        "af.node",
        &run,
        SpanKind::Internal,
        Attrs::new().id("af.node_id", "root.nodes.review"),
    );
    let attempt = span(
        "af.attempt",
        &node,
        SpanKind::Internal,
        Attrs::new()
            .id("af.attempt_id", GOLDEN_ATTEMPT)
            .id("af.node_id", "root.nodes.review"),
    );
    let scope = Scope::Attempt(attempt.clone());
    let digest = |fill: char| format!("sha256:{}", fill.to_string().repeat(64));
    let request = json!({
        "schema": "af.worker-request/1",
        "instructions": "Review the lighthouse.",
        "inputs": {"source": [{"artifact_id": digest('a'), "artifact_type": "af/SourceTree@1",
                               "snapshot_id": digest('b'), "payload": {"snapshot_id": digest('b')}}]},
        "feedback": [],
    })
    .to_string();
    let manifest = json!({"entries": [
        {"name": "source", "required_by": "declared input port", "artifact_id": digest('a'),
         "artifact_type": "af/SourceTree@1", "rendered_bytes": 200, "estimated_tokens": 50}],
        "rendered_bytes": request.len(), "estimated_tokens": request.len().div_ceil(4)})
    .to_string();
    let compose = span(
        "af.context.compose",
        &attempt,
        SpanKind::Internal,
        Attrs::new()
            .id("af.context.manifest", digest('c'))
            .int("af.context.bytes", request.len() as i64)
            .decimal(
                "af.context.estimated_tokens",
                request.len().div_ceil(4).to_string(),
            )
            .text("af.context.layers", "source,output_schemas,reply_format"),
    );
    let manifest_id = Attrs::new().id("af.context.manifest", digest('c'));
    writer.content_event(
        &compose,
        event::INPUT,
        manifest_id.clone(),
        request.as_bytes(),
        Media::Kernel(Document::WorkerRequest),
        &scope,
    );
    writer.content_event(
        &compose,
        event::SYSTEM,
        manifest_id,
        manifest.as_bytes(),
        Media::Kernel(Document::Record),
        &scope,
    );
    writer.span_end(&compose, SpanStatus::Ok, Attrs::new(), 4);
    let harness = span(
        "af.harness",
        &attempt,
        SpanKind::Process,
        Attrs::new()
            .enumerated("gen_ai.operation.name", "invoke_agent")
            .enumerated("gen_ai.provider.name", "anthropic"),
    );
    let mut tap = Tap::new(
        &writer,
        TapOptions {
            harness: Harness::Claude,
            parent: SpanIdentity {
                span: RecordedSpan {
                    span_id: harness.clone(),
                    trace_id: Some(trace.clone()),
                },
                identity: identity.0,
                depth: 4,
            },
            provider: "anthropic",
            bound_model: Some("claude-fixture-1".into()),
            tools: vec!["Read".into()],
            frame_max_bytes: 16 << 20,
            content_allowance_bytes: 512 << 20,
            grants: vec![],
            content_reasoning: true,
            content_tool_io: true,
            scope,
        },
    );
    let lines = [
        json!({"type":"system","subtype":"init","session_id":"golden-session","model":"claude-fixture-1"}),
        json!({"type":"assistant","message":{"id":"msg_golden_1","model":"claude-fixture-1",
               "stop_reason":"tool_use","content":[
                   {"type":"thinking","thinking":"The keeper logs the lamp hourly."},
                   {"type":"tool_use","id":"toolu_golden_1","name":"Read",
                    "input":{"file_path":"src/lamp.rs"}}]}}),
        json!({"type":"user","message":{"content":[{"type":"tool_result",
               "tool_use_id":"toolu_golden_1","content":"pub fn lamp() -> bool { true }\n"}]}}),
        json!({"type":"assistant","message":{"id":"msg_golden_2","model":"claude-fixture-1",
               "stop_reason":"end_turn","content":[
                   {"type":"thinking","thinking":"The lamp is lit; nothing to report."},
                   {"type":"text","text":"No findings: the periwinkle lighthouse is lit."}]}}),
    ];
    let start = 1_700_000_400_100 * 1_000_000;
    let mut offset = 0;
    for (index, line) in lines.iter().enumerate() {
        let bytes = format!("{line}\n");
        tap.chunk(
            &writer,
            Stream::Stdout,
            offset,
            bytes.as_bytes(),
            start + index as u128 * 5_000_000,
        );
        offset += bytes.len() as u64;
    }
    tap.finish(
        &writer,
        &Release {
            stdout_bytes: offset,
            ..Release::default()
        },
    );
    writer.span_end(
        &harness,
        SpanStatus::Ok,
        Attrs::new()
            .int("dropped_chunks", 0)
            .int("gaps", 0)
            .boolean("af.stream.withheld", false),
        4,
    );
    writer.span_end(&attempt, SpanStatus::Ok, Attrs::new(), 3);
    writer.span_end(&node, SpanStatus::Ok, Attrs::new(), 2);
    writer.span_end(&run, SpanStatus::Ok, Attrs::new(), 1);
    writer.span_end(&command, SpanStatus::Ok, Attrs::new(), 0);
    writer.finish(SealExit::Ok).unwrap();
}

fn golden_path(name: &str) -> PathBuf {
    workspace()
        .join("crates/af/tests/it/telemetry_golden")
        .join(name)
}

/// Compare `actual` with the golden `name`; `AF_UPDATE_TELEMETRY_GOLDEN=1` rewrites it.
fn golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("AF_UPDATE_TELEMETRY_GOLDEN").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert_eq!(actual, expected, "{name} differs from its golden");
}

/// `af telemetry ARGS --dir root` at `level`, with the test's private home and XDG directories.
fn af_dir(root: &Path, level: &str, args: &[&str]) -> std::process::Output {
    let mut command = crate::common::af();
    command
        .env("AF_TELEMETRY__LEVEL", level)
        .args(args)
        .arg("--dir")
        .arg(root);
    command.output().unwrap()
}

/// Design §10: `af telemetry context`, `reasoning` and `search` over one recorded `content`
/// execution, in text and as `af/telemetry-context@1`, `af/telemetry-reasoning@1` and
/// `af/telemetry-search@1`, against goldens; each document matches its schema and refuses a
/// member its schema does not name; `context --raw` prints the request's retained bytes alone.
#[test]
fn telemetry_content_commands_match_their_goldens() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("telemetry");
    record_content_golden(&root);
    let trace = review_telemetry::trace::trace_id(GOLDEN_STORE, GOLDEN_RUN);
    let normalize = |text: String| {
        text.replace(&root.display().to_string(), "<root>")
            .replace(&trace, "<trace>")
    };
    let context = json_of(&af_dir(
        &root,
        "content",
        &["telemetry", "context", GOLDEN_RUN, GOLDEN_ATTEMPT, "--json"],
    ));
    let reasoning = json_of(&af_dir(
        &root,
        "content",
        &[
            "telemetry",
            "reasoning",
            GOLDEN_RUN,
            GOLDEN_ATTEMPT,
            "--json",
        ],
    ));
    let search = json_of(&af_dir(
        &root,
        "content",
        &["telemetry", "search", "lighthouse", "--json"],
    ));
    for (name, document, pointers) in [
        (
            "telemetry-context-v1.json",
            &context,
            ["", "/run", "/index", "/input", "/system"].as_slice(),
        ),
        (
            "telemetry-reasoning-v1.json",
            &reasoning,
            [
                "",
                "/run",
                "/harnesses/0",
                "/thoughts/0",
                "/thoughts/0/part",
            ]
            .as_slice(),
        ),
        (
            "telemetry-search-v1.json",
            &search,
            ["", "/index", "/matches/0", "/matches/0/occurrences/0"].as_slice(),
        ),
    ] {
        let validator = crate::schemas::validator(name);
        crate::schemas::valid(&validator, document);
        for pointer in pointers {
            let mut extended = document.clone();
            extended
                .pointer_mut(pointer)
                .unwrap_or_else(|| panic!("{name}{pointer}"))
                .as_object_mut()
                .unwrap()
                .insert("unnamed".into(), Value::from(1));
            assert!(
                !validator.is_valid(&extended),
                "{name}{pointer} accepts an unnamed member"
            );
        }
    }
    assert_eq!(context["state"], "captured");
    assert_eq!(reasoning["thoughts"].as_array().unwrap().len(), 2);
    assert_eq!(search["matches"].as_array().unwrap().len(), 2, "{search}");
    for (name, document) in [
        ("context.json", &context),
        ("reasoning.json", &reasoning),
        ("search.json", &search),
    ] {
        golden(
            name,
            &normalize(serde_json::to_string_pretty(document).unwrap() + "\n"),
        );
    }
    for (args, name) in [
        (
            vec!["telemetry", "context", GOLDEN_RUN, GOLDEN_ATTEMPT],
            "context.txt",
        ),
        (
            vec!["telemetry", "reasoning", GOLDEN_RUN, GOLDEN_ATTEMPT],
            "reasoning.txt",
        ),
        (vec!["telemetry", "search", "lighthouse"], "search.txt"),
    ] {
        let text = af_dir(&root, "content", &args);
        assert!(
            text.status.success(),
            "{}",
            String::from_utf8_lossy(&text.stderr)
        );
        golden(name, &normalize(String::from_utf8(text.stdout).unwrap()));
    }
    let raw = af_dir(
        &root,
        "content",
        &["telemetry", "context", GOLDEN_RUN, GOLDEN_ATTEMPT, "--raw"],
    );
    assert_eq!(
        String::from_utf8(raw.stdout).unwrap(),
        context["input"]["content"].as_str().unwrap()
    );
    // An unknown Attempt is refused, exit 1.
    let unknown = af_dir(
        &root,
        "content",
        &["telemetry", "context", GOLDEN_RUN, "nope"],
    );
    assert_eq!(unknown.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("no Attempt nope"));
}

/// Design §7.5 at `content`: every mirrored Store event names its payload as a blob whose
/// source is exactly the committed payload (its `source_digest` is `af.store.event.payload_digest`)
/// and whose identities and counters are kept, and an admitted Finding's body is a blob named
/// by `af.finding.body` keyed by the Finding key, its planted credential line withheld; below
/// `content` neither exists.
#[test]
fn telemetry_content_mirror_keeps_payloads_and_finding_bodies() {
    use crate::telemetry_ledger::campaign_fixture as campaign;
    use review_store::{Cas, EventStore};
    use review_telemetry::level::Level;

    for level in [Level::Spans, Level::Content] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("telemetry");
        let store_dir = tempfile::tempdir_in(temp.path()).unwrap();
        let recording = crate::telemetry_ledger::Recording::start(
            &root,
            level,
            store_dir.path(),
            campaign::RUN,
            1,
        );
        let cas = Cas::open(store_dir.path().join("cas")).unwrap();
        let mut store = EventStore::open(store_dir.path().join("events.sqlite")).unwrap();
        store.observe_commits(recording.mirror());
        recording.within(|| {
            let round = campaign::opened_round(&mut store, &cas);
            let report = campaign::stage(&format!(
                r#"{{"reports":[{{"severity":"major","file":"src/a.rs","line":7,
                     "title":"Retry loop can spin forever",
                     "body":"The retry loop never yields.\n{PLANTED}\n","fix":"f","confidence":0.9}}],
                    "benchmark_demands":[],"dispositions":[]}}"#
            ));
            campaign::admit(
                &mut store,
                &cas,
                &round,
                &[("deep", "01jd8m4qz9k7v3n2p6r8t0w201", &report)],
            );
        });
        let payloads: Vec<Value> = store
            .replay(campaign::RUN)
            .unwrap()
            .into_iter()
            .map(|event| event.payload)
            .collect();
        drop(store);
        let writer = recording.writer.clone();
        recording.finish();
        let path = writer
            .layout()
            .sealed_segment(&writer.segment_id().unwrap());
        let records = review_telemetry::segment::reader::read_sealed(&path).unwrap();
        let references: Vec<review_telemetry::record::BlobRef> = records
            .iter()
            .filter_map(|record| match record {
                Record::BlobRef(occurrence) => Some(occurrence.clone()),
                _ => None,
            })
            .collect();
        let events: Vec<&review_telemetry::record::Event> = records
            .iter()
            .filter_map(|record| match record {
                Record::Event(event) if event.name == "af.store.event" => Some(event),
                _ => None,
            })
            .collect();
        assert_eq!(events.len(), payloads.len());
        let bodies: Vec<&review_telemetry::record::Event> = records
            .iter()
            .filter_map(|record| match record {
                Record::Event(event) if event.name == "af.finding.body" => Some(event),
                _ => None,
            })
            .collect();
        if level == Level::Spans {
            assert!(events.iter().all(|event| event.blob.is_none()));
            assert!(bodies.is_empty());
            assert!(references.is_empty());
            continue;
        }
        let layout = review_telemetry::layout::Layout::new(&root);
        let read = |occurrence: &str| -> Vec<u8> {
            let reference = references
                .iter()
                .find(|reference| reference.occurrence_id == occurrence)
                .unwrap();
            match review_telemetry::blob::reader::content(&layout, reference) {
                review_telemetry::blob::reader::Content::Bytes(bytes) => bytes,
                review_telemetry::blob::reader::Content::Unavailable(reason) => panic!("{reason}"),
            }
        };
        for (event, payload) in events.iter().zip(&payloads) {
            let occurrence = event.blob.as_deref().expect("a payload blob at content");
            let reference = references
                .iter()
                .find(|reference| reference.occurrence_id == occurrence)
                .unwrap();
            assert_eq!(
                event.attributes["af.store.event.payload_digest"],
                reference.source_digest.as_str(),
                "the occurrence's source is the committed payload"
            );
            let kept: Value = serde_json::from_slice(&read(occurrence)).unwrap();
            // Every identity of the payload is kept where its record types one.
            fn identities(value: &Value, name: &str, out: &mut Vec<String>) {
                match value {
                    Value::Object(map) => {
                        for (key, member) in map {
                            identities(member, key, out);
                        }
                    }
                    Value::Array(items) => {
                        for item in items {
                            identities(item, name, out);
                        }
                    }
                    Value::String(text)
                        if (name.ends_with("_id") || name.ends_with("_ids"))
                            && review_telemetry::redact::is_kernel_identity(text) =>
                    {
                        out.push(text.clone());
                    }
                    _ => {}
                }
            }
            let (mut before, mut after) = (Vec::new(), Vec::new());
            identities(payload, "", &mut before);
            identities(&kept, "", &mut after);
            assert_eq!(after, before, "{kept}");
        }
        assert_eq!(bodies.len(), 1, "one body for the one admitted Finding");
        let key = bodies[0].attributes["af.finding.id"].as_str().unwrap();
        assert!(review_telemetry::redact::is_digest(key), "{key}");
        let body = String::from_utf8(read(bodies[0].blob.as_deref().unwrap())).unwrap();
        assert_eq!(
            body,
            format!(
                "The retry loop never yields.\n{}\n",
                review_telemetry::redact::WITHHELD_LINE
            )
        );
        let joined: String = records.iter().map(|record| format!("{record:?}")).collect();
        assert!(!joined.contains("hunter2"));
    }
}
