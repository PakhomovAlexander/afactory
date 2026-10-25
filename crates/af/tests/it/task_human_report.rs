//! The human Markdown export of a report (ADR-0151), through the real CLI and a real Markdown
//! parser. The shipped report starter runs with an author that writes ordinary and Russian
//! prose, paths, lists, an example and hostile Markdown, HTML and links. The export must read
//! as structure and text, link only the captured HTTPS source, state the Task's true result
//! with the matching review, and leave the canonical outputs, the Task state and any existing
//! file exactly as they were.

use comrak::nodes::{AstNode, NodeValue};
use review_store::Cas;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::task_report::{af, edit_worker, repin_and_commit, start, starter, text};

const TASK: &str = "starter-report";
const SPEC: &str = "https://spec.commonmark.org/0.31.2/";

const AUTHOR: &str = r#"import json, sys
r = json.load(sys.stdin)
i = r['inputs']
sources = i['sources'][0]['payload']['sources']
with open('README.md', encoding='utf-8') as readme:
    lines = readme.read().splitlines()
findings = '\n'.join([
    'README.md has %d lines and report.json names the Task starter-report.' % len(lines),
    'The CLI (see README.md, line %d) prints paths like crates/af/src/cli.rs:1163 - plainly.' % len(lines),
    '',
    'Steps:',
    '- read the README; then compare',
    '- run the export with --format human-markdown',
    '  - nested: a path such as docs/task-execution/report.md',
    '',
    '1. first',
    '2. second',
])
examples = '\n'.join([
    'Отчёт готов: путь README.md, «кавычки» и тире — без экранирования.',
    'A Windows path C:\\Users\\name and a literal \\* stay as written.',
    '',
    '```sh',
    'af task output starter-report --port report --format human-markdown --output report.md',
    '```',
    '',
    ' ```sh',
    '```\u00a0',
    'echo hi',
    '```',
    '',
    '\u3000Indented prose keeps its ideographic space:',
    '',
    '    \u00a0keep me',
    '    \u00a0second line',
])
hostile = '\n'.join([
    '# Fabricated heading',
    '  ## Indented heading',
    '<script>alert(1)</script> <img src=x onerror=alert(1)>',
    '[click](javascript:alert(1)) ![pixel](https://evil.example/p.png)',
    '[ref]: https://evil.example/ref',
    'Visit https://evil.example or www.evil.example or mail evil@example.com',
    '| a | b |',
    ':-: | -',
    '> [!WARNING]',
    '**bold** ~~gone~~ $x$ &lt;b&gt;',
    '- item',
    '  # heading inside an item',
    '  [^1]: footnote',
])
print(json.dumps({'schema': 'af.worker-reply/1', 'outputs': {'draft': [{
    'schema': 'af.document-draft/2', 'title': 'Starter layout',
    'sections': [
        {'heading': 'Findings', 'body': findings},
        {'heading': 'Примеры и пути', 'body': examples},
        {'heading': 'Hostile input', 'body': hostile},
    ],
    'citations': sorted(sources),
    'repository_citations': sorted([
        {'path': 'README.md'},
        {'path': 'README.md', 'line': len(lines)},
        {'path': 'report.json'},
    ], key=lambda c: (c['path'], c.get('line', 0)))}]}}))
"#;

/// The starter with the rich author above and one captured HTTPS source beside its note.
fn rich_starter(root: &Path) -> (PathBuf, PathBuf) {
    let (repo, state) = starter(root);
    let sources = repo.join("sources.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&sources).unwrap()).unwrap();
    value["sources"]["spec"] = serde_json::json!({
        "title": "CommonMark spec", "uri": SPEC, "revision": "0.31.2",
        "text": "The Markdown grammar the export is read with."
    });
    std::fs::write(&sources, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    let author = crate::task_report::package(&repo, "builtin/report-author").join("worker.py");
    std::fs::write(author, AUTHOR).unwrap();
    repin_and_commit(&repo);
    (repo, state)
}

fn output(
    repo: &Path,
    state: &Path,
    port: &str,
    format: &str,
    file: &Path,
) -> std::process::Output {
    af(
        repo,
        &[
            "task",
            "output",
            TASK,
            "--port",
            port,
            "--format",
            format,
            "--output",
            file.to_str().unwrap(),
            "--json",
            "--state",
            state.to_str().unwrap(),
        ],
    )
}

/// Every file under `dir` with its bytes.
fn files(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                out.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

fn options() -> comrak::Options<'static> {
    let mut options = comrak::Options::default();
    options.extension.strikethrough = true;
    options.extension.table = true;
    options.extension.autolink = true;
    options.extension.tasklist = true;
    options.extension.footnotes = true;
    options.extension.alerts = true;
    options.extension.math_dollars = true;
    options.extension.math_code = true;
    options.parse.relaxed_autolinks = true;
    options
}

fn inline_text<'a>(node: &'a AstNode<'a>) -> String {
    let mut out = String::new();
    for child in node.descendants().skip(1) {
        match &child.data.borrow().value {
            NodeValue::Text(text) => out.push_str(text),
            NodeValue::Code(code) => out.push_str(&code.literal),
            NodeValue::SoftBreak => out.push('\n'),
            _ => {}
        }
    }
    out
}

/// What a Markdown parser reads: leaf blocks (`p`, `li`, `h1`, `h2`, `code`) with their text,
/// and every link destination. Any other node, such as an image, raw HTML, a quote, a table,
/// emphasis or a footnote, panics.
struct Parsed {
    leaves: Vec<(String, String)>,
    links: Vec<String>,
}

fn parse(markdown: &str) -> Parsed {
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, markdown, &options());
    let mut parsed = Parsed {
        leaves: Vec::new(),
        links: Vec::new(),
    };
    for node in root.descendants() {
        let value = node.data.borrow().value.clone();
        match value {
            NodeValue::Document
            | NodeValue::List(_)
            | NodeValue::Item(_)
            | NodeValue::Text(_)
            | NodeValue::Code(_)
            | NodeValue::SoftBreak => {}
            NodeValue::Paragraph => {
                let item = node
                    .parent()
                    .is_some_and(|p| matches!(p.data.borrow().value, NodeValue::Item(_)));
                let kind = if item { "li" } else { "p" };
                parsed.leaves.push((kind.into(), inline_text(node)));
            }
            NodeValue::Heading(heading) => parsed
                .leaves
                .push((format!("h{}", heading.level), inline_text(node))),
            NodeValue::CodeBlock(block) => parsed.leaves.push(("code".into(), block.literal)),
            NodeValue::Link(link) => parsed.links.push(link.url.clone()),
            // The generated result line is the only emphasis.
            NodeValue::Strong
                if node
                    .parent()
                    .is_some_and(|p| inline_text(p).starts_with("Result: ")) => {}
            other => panic!("{other:?} in\n{markdown}"),
        }
    }
    parsed
}

fn leaf(kind: &str, text: &str) -> (String, String) {
    (kind.into(), text.into())
}

/// The closing note, as the parser reads it.
fn about(parsed: &Parsed) -> &str {
    parsed
        .leaves
        .iter()
        .find(|(kind, text)| kind.as_str() == "p" && text.starts_with("About this file:"))
        .map(|(_, text)| text.as_str())
        .expect("an About note")
}

const ABOUT: &str = "About this file: a readable presentation of the report output of Task \
                     starter-report, made from the draft and sources that render exactly to its \
                     recorded report.";
const CANONICAL: &str = "the canonical report, which af task output starter-report --port report \
                         --format canonical-markdown writes, not this layout.";

#[test]
fn a_report_exports_one_readable_file_and_keeps_its_canonical_outputs_and_state() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (repo, state) = rich_starter(&root);
    let done = start(&repo, &state, 0);
    assert_eq!(done["result"]["acceptance"], "satisfied");
    let lines = std::fs::read_to_string(repo.join("README.md"))
        .unwrap()
        .lines()
        .count();
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let document_id = done["result"]["outputs"]["report"]["artifact_ids"][0]
        .as_str()
        .unwrap()
        .to_owned();
    let envelope = cas.get_json(&document_id).unwrap();
    let canonical = envelope["payload"]["text"].as_str().unwrap().to_owned();
    let recorded = files(&state);

    // Canonical Markdown and JSON are written exactly as before.
    let markdown = root.join("canonical.md");
    let written = output(&repo, &state, "report", "canonical-markdown", &markdown);
    assert!(written.status.success(), "{}", text(&written));
    assert_eq!(std::fs::read_to_string(&markdown).unwrap(), canonical);
    let json = root.join("report.json");
    let written = output(&repo, &state, "report", "json", &json);
    assert!(written.status.success(), "{}", text(&written));
    let exported: Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    assert_eq!(exported, envelope);

    let human = root.join("report.md");
    let written = output(&repo, &state, "report", "human-markdown", &human);
    assert!(written.status.success(), "{}", text(&written));
    let reply: Value = serde_json::from_slice(&written.stdout).unwrap();
    assert_eq!(reply["format"], "human-markdown");
    assert_eq!(reply["artifact_id"], document_id.as_str());
    assert_eq!(reply["acceptance"], "satisfied");
    let presentation = std::fs::read_to_string(&human).unwrap();
    let parsed = parse(&presentation);

    // The only link is the captured HTTPS source: a safe citation stays useful.
    assert_eq!(parsed.links, [SPEC], "{presentation}");
    let headings: Vec<_> = parsed
        .leaves
        .iter()
        .filter(|(kind, _)| kind.starts_with('h'))
        .cloned()
        .collect();
    assert_eq!(
        headings,
        [
            leaf("h1", "Starter layout"),
            leaf("h2", "Findings"),
            leaf("h2", "Примеры и пути"),
            leaf("h2", "Hostile input"),
            leaf("h2", "Sources"),
            leaf("h2", "Repository citations"),
            leaf("h2", "Review"),
        ],
        "{presentation}"
    );
    for expected in [
        leaf(
            "p",
            "Result: accepted. The report passed its automated checks and its independent review.",
        ),
        leaf(
            "p",
            &format!(
                "README.md has {lines} lines and report.json names the Task starter-report.\n\
                 The CLI (see README.md, line {lines}) prints paths like crates/af/src/cli.rs:1163 - plainly."
            ),
        ),
        leaf("p", "Steps:"),
        leaf("li", "read the README; then compare"),
        leaf("li", "run the export with --format human-markdown"),
        leaf("li", "nested: a path such as docs/task-execution/report.md"),
        leaf("li", "first"),
        leaf("li", "second"),
        leaf(
            "p",
            "Отчёт готов: путь README.md, «кавычки» и тире — без экранирования.\n\
             A Windows path C:\\Users\\name and a literal \\* stay as written.",
        ),
        leaf(
            "code",
            "af task output starter-report --port report --format human-markdown --output report.md\n",
        ),
        // A fence line followed by a no-break space is content of the example, not its closer.
        leaf("code", "```\u{a0}\necho hi\n"),
        // Only the ASCII indentation of an indented example goes: the no-break spaces after it
        // and the ideographic space opening a line are text.
        leaf("p", "\u{3000}Indented prose keeps its ideographic space:"),
        leaf("code", "\u{a0}keep me\n\u{a0}second line\n"),
        leaf(
            "p",
            "# Fabricated heading\n## Indented heading\n\
             <script>alert(1)</script> <img src=x onerror=alert(1)>\n\
             [click](javascript:alert(1)) ![pixel](https://evil.example/p.png)\n\
             [ref]: https://evil.example/ref\n\
             Visit https://evil.example or www.evil.example or mail evil@example.com\n\
             | a | b |\n:-: | -\n> [!WARNING]\n**bold** ~~gone~~ $x$ &lt;b&gt;",
        ),
        leaf("li", "item\n# heading inside an item\n[^1]: footnote"),
        leaf("li", "CommonMark spec, revision 0.31.2"),
        leaf("li", "Starter note: repo:README.md, revision starter@1"),
        leaf("li", &format!("README.md:{lines}")),
        leaf("li", "report.json"),
        leaf("p", "Automated checks: passed."),
        leaf("p", "Independent review: passed."),
        leaf(
            "p",
            "Cited README lines and the Task file checked against the same Snapshot.",
        ),
    ] {
        assert!(
            parsed.leaves.contains(&expected),
            "{expected:?} in\n{presentation}"
        );
    }
    assert!(
        !parsed.leaves.iter().any(|(_, text)| text == "echo hi"),
        "{presentation}"
    );
    assert_eq!(
        about(&parsed),
        format!("{ABOUT} The automated checks and the independent reviewer judged {CANONICAL}")
    );
    // Ordinary punctuation reads as written in the file itself, and in the canonical report too:
    // it escapes only what is markup where it stands (`af.document/2`).
    let plain = "prints paths like crates/af/src/cli.rs:1163 - plainly.";
    assert!(presentation.contains(plain), "{presentation}");
    assert!(canonical.contains(plain), "{canonical}");
    assert!(canonical.contains("\n\\- read the README"), "{canonical}");
    assert!(
        presentation.contains("Отчёт готов: путь README.md, «кавычки» и тире — без экранирования."),
        "{presentation}"
    );
    // No machine internals: no digests, schemas or artifact types.
    for internal in ["sha256:", "schema", "af/Document", "artifact"] {
        assert!(
            !presentation.contains(internal),
            "{internal}: {presentation}"
        );
    }

    // Nothing is overwritten, and another kind of output is refused with what to use instead.
    let refused = output(&repo, &state, "report", "human-markdown", &human);
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains("Output file must be absent"),
        "{}",
        text(&refused)
    );
    assert_eq!(std::fs::read_to_string(&human).unwrap(), presentation);
    let receipt = root.join("verification.md");
    let refused = output(&repo, &state, "verification", "human-markdown", &receipt);
    assert!(!refused.status.success());
    let message = text(&refused);
    assert!(
        message.contains("presents only a Document or report"),
        "{message}"
    );
    assert!(
        message.contains("--format markdown or --format json"),
        "{message}"
    );
    assert!(!receipt.exists());
    // Exporting is read-only: the Task's recorded state is byte for byte what it was.
    assert_eq!(files(&state), recorded);
}

#[test]
fn a_negative_review_reads_as_changes_requested_with_the_reviewers_words() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (repo, state) = rich_starter(&root);
    edit_worker(
        &repo,
        "report-verifier",
        "'outcome': 'passed' if accepted else 'failed',\n    'summary': 'Cited README lines and the Task file checked against the same Snapshot.'",
        "'outcome': 'failed',\n    'summary': 'The count is wrong.\\n- recount <b>README.md</b>, see [x](javascript:alert(1))'",
    );
    let done = start(&repo, &state, 3);
    assert_eq!(done["result"]["acceptance"], "unsatisfied");
    assert_eq!(done["result"]["domain_conclusion"], "changes_requested");
    let human = root.join("report.md");
    let written = output(&repo, &state, "report", "human-markdown", &human);
    assert!(written.status.success(), "{}", text(&written));
    let reply: Value = serde_json::from_slice(&written.stdout).unwrap();
    assert_eq!(reply["acceptance"], "unsatisfied");
    let presentation = std::fs::read_to_string(&human).unwrap();
    let parsed = parse(&presentation);
    assert_eq!(parsed.links, [SPEC], "{presentation}");
    for expected in [
        leaf(
            "p",
            "Result: changes requested. The report passed its automated checks, but the independent reviewer asked for changes.",
        ),
        leaf("p", "Independent review: changes requested."),
        leaf("p", "The reviewer's summary, as recorded:"),
        leaf("p", "The count is wrong."),
        leaf(
            "li",
            "recount <b>README.md</b>, see [x](javascript:alert(1))",
        ),
    ] {
        assert!(
            parsed.leaves.contains(&expected),
            "{expected:?} in\n{presentation}"
        );
    }
    assert_eq!(
        about(&parsed),
        format!("{ABOUT} The automated checks and the independent reviewer judged {CANONICAL}")
    );
}

#[test]
fn failed_checks_read_as_changes_requested_and_name_the_citation() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (repo, state) = rich_starter(&root);
    let author = crate::task_report::package(&repo, "builtin/report-author").join("worker.py");
    let script = std::fs::read_to_string(&author).unwrap();
    std::fs::write(
        &author,
        script.replace(
            "{'path': 'report.json'},",
            "{'path': 'report.json'}, {'path': 'report.json', 'line': 100000},",
        ),
    )
    .unwrap();
    repin_and_commit(&repo);
    let done = start(&repo, &state, 3);
    assert_eq!(done["result"]["acceptance"], "unsatisfied");
    let human = root.join("report.md");
    let written = output(&repo, &state, "report", "human-markdown", &human);
    assert!(written.status.success(), "{}", text(&written));
    let presentation = std::fs::read_to_string(&human).unwrap();
    let parsed = parse(&presentation);
    for expected in [
        leaf(
            "p",
            "Result: changes requested. The report failed its automated checks, so no independent review ran.",
        ),
        leaf("p", "Automated checks: failed (repository citations)."),
        leaf(
            "li",
            "report.json:100000: the line is past the end of the file",
        ),
        leaf(
            "p",
            "Independent review: did not run, because the automated checks failed.",
        ),
    ] {
        assert!(
            parsed.leaves.contains(&expected),
            "{expected:?} in\n{presentation}"
        );
    }
    assert!(
        !presentation.contains("reviewer's summary"),
        "{presentation}"
    );
    // No reviewer ran, and the closing note does not say one judged the report.
    assert_eq!(
        about(&parsed),
        format!(
            "{ABOUT} The automated checks judged {CANONICAL} They failed, so no independent reviewer ran."
        )
    );
    assert!(!presentation.contains("reviewer judged"), "{presentation}");
}

#[test]
fn a_captured_query_link_reads_back_exactly_as_captured() {
    // A parser decodes character references in a link destination; the generated link must not.
    const QUERY: &str = "https://example.com/search?q=&copy;&n=&#169;&h=&#xA9;&a=&amp;&lang=ru";
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let (repo, state) = rich_starter(&root);
    let sources = repo.join("sources.json");
    let mut value: Value = serde_json::from_slice(&std::fs::read(&sources).unwrap()).unwrap();
    value["sources"]["query"] = serde_json::json!({
        "title": "Query & entities", "uri": QUERY, "revision": "q1",
        "text": "A search whose query holds entity-like text."
    });
    std::fs::write(&sources, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    repin_and_commit(&repo);
    let done = start(&repo, &state, 0);
    assert_eq!(done["result"]["acceptance"], "satisfied");
    let cas = Cas::open_existing(state.join("cas")).unwrap();
    let document_id = done["result"]["outputs"]["report"]["artifact_ids"][0]
        .as_str()
        .unwrap()
        .to_owned();
    let canonical = cas.get_json(&document_id).unwrap()["payload"]["text"]
        .as_str()
        .unwrap()
        .to_owned();

    let human = root.join("report.md");
    let written = output(&repo, &state, "report", "human-markdown", &human);
    assert!(written.status.success(), "{}", text(&written));
    let presentation = std::fs::read_to_string(&human).unwrap();
    let parsed = parse(&presentation);
    // Both captured HTTPS sources link, each to exactly its captured location.
    assert_eq!(parsed.links, [QUERY, SPEC], "{presentation}");
    assert!(
        parsed
            .leaves
            .contains(&leaf("li", "Query & entities, revision q1")),
        "{presentation}"
    );
    // The canonical report is written exactly as recorded, its location unchanged.
    let markdown = root.join("canonical.md");
    let written = output(&repo, &state, "report", "canonical-markdown", &markdown);
    assert!(written.status.success(), "{}", text(&written));
    assert_eq!(std::fs::read_to_string(&markdown).unwrap(), canonical);
    assert!(canonical.contains(&format!("(<{QUERY}>)")), "{canonical}");
}
