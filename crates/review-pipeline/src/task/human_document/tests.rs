use super::*;
use crate::task::report_task::ReportDraft;
use comrak::nodes::{AstNode, NodeValue};
use review_core::task::{AcceptanceObligationV1, TASK_REVISION_V1};
use review_source_git::task::{capture_snapshot, source_tree};
use review_source_git::{Entry, EntryKind, Manifest};

/// The covered dialect: CommonMark with the GFM extensions, GitHub alerts, dollar and code
/// math, and comrak's relaxed autolinks, which claim more than GitHub does. The tag filter only
/// changes how raw HTML is printed, and no authored raw HTML survives to be printed.
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
    options.parse.relaxed_tasklist_matching = true;
    options
}

/// The leaf blocks a Markdown parser reads from `markdown`, in order: `p`, `h<level>`, `code`
/// or `li` (a paragraph inside a list item), each with its text. Any node outside plain
/// structure, such as a link, image, HTML, quote, table or emphasis, panics with its kind.
fn leaves(markdown: &str, allowed_links: &[&str]) -> Vec<(String, String)> {
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, markdown, &options());
    let mut out = Vec::new();
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
                let in_item = node
                    .parent()
                    .is_some_and(|p| matches!(p.data.borrow().value, NodeValue::Item(_)));
                out.push((if in_item { "li" } else { "p" }.into(), text(node)));
            }
            NodeValue::Heading(heading) => {
                out.push((format!("h{}", heading.level), text(node)));
            }
            NodeValue::CodeBlock(block) => {
                out.push(("code".into(), block.literal.clone()));
            }
            NodeValue::Link(link) if allowed_links.contains(&link.url.as_str()) => {}
            NodeValue::Strong
                if node
                    .parent()
                    .is_some_and(|p| text(p).starts_with("Result: ")) => {}
            other => panic!("{other:?} in\n{markdown}"),
        }
    }
    out
}

fn text<'a>(node: &'a AstNode<'a>) -> String {
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

fn rendered(body_text: &str) -> String {
    body(body_text).join("\n\n")
}

fn p(text: &str) -> (String, String) {
    ("p".into(), text.into())
}
fn li(text: &str) -> (String, String) {
    ("li".into(), text.into())
}
fn code(text: &str) -> (String, String) {
    ("code".into(), text.into())
}

#[test]
fn ordinary_prose_keeps_its_punctuation_unescaped() {
    let prose = "The CLI (crates/af/src/cli.rs:1163) limits --format to json and markdown: see e.g. \
                 write_output, a snake_case_name and v1.2.3 - nothing else.\n\
                 Русский текст: отчёт «готов», путь crates/af/src/main.rs; 50% done?";
    let markdown = rendered(prose);
    assert_eq!(markdown, prose, "ordinary punctuation needs no escape");
    assert_eq!(leaves(&markdown, &[]), [p(prose)]);
}

#[test]
fn authored_markup_stays_the_text_it_was() {
    // Each case is one paragraph of authored text: what a parser reads back is exactly it.
    let cases = [
        "# Fake heading",
        "  # indented heading",
        "Title\n===",
        "Title\n---",
        "***",
        "___",
        "> quote",
        "> [!WARNING]\n> alert",
        "<script>alert(1)</script>",
        "<img src=x onerror=alert(1)>",
        "<div>\n<b>bold</b>\n</div>",
        "<!-- hidden -->",
        "<?php echo 1; ?>",
        "<![CDATA[x]]>",
        "[click](javascript:alert(1))",
        "![image](https://evil.example/x.png)",
        "[ref]: https://evil.example/ref",
        "  [ref]: /relative 'title'",
        "see [ref] and [ref][] and [text][ref]",
        "<https://evil.example>",
        "https://evil.example/path and www.evil.example and http://x.example",
        "(www.evil.example) and (https://evil.example)",
        "write to user@example.com or mailto:user@example.com or xmpp:user@example.com",
        "[^1]: footnote definition",
        "text[^1] with a reference",
        "a | b\n:-: | -\nc | d",
        "abc\n:---",
        "a | b\n--- | ---",
        "**bold** __bold__ *em* _em_ ~~strike~~ ~sub~ $x^2$ $$y$$ $`z`$",
        "`code` ``double`` ```triple``` text",
        "&lt;script&gt; &#60; &#x3C; &amp; &copy; R&D AT&T",
        "\\* an authored escape and a \\\\ double backslash",
        "C:\\Users\\name\\file.txt and a trailing backslash\\",
        "two trailing spaces  \nnext",
        "Setext under a paragraph\n  ===",
        "  ```\n  not closed",
        "```js onload=x\nbody\n```",
        "~~~\nnever closed",
        "1986\\. a year",
        "-- a dash comment",
        "+not a list",
        "|lone pipe",
        "=== equals",
        ": definition",
        "\u{a0}www.evil.example",
        "__init__ and snake_case_ and _leading",
    ];
    for case in cases {
        let markdown = rendered(case);
        // Only the ASCII padding a line start drops; a no-break space is text.
        let expected = case
            .split('\n')
            .map(|line| line.trim_matches([' ', '\t']))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(leaves(&markdown, &[]), [p(&expected)], "{case:?}");
    }
}

#[test]
fn authored_block_syntax_inside_lists_stays_text() {
    let body = "- first item\n  # heading in an item\n  > quote in an item\n  - nested item\n    \
                ## nested heading\n    [ref]: https://evil.example\n    <script>x</script>\n- [ ] \
                task box\n- [x] done box\n- second\n  1. one\n  2. two\n     ---\n     ***";
    let markdown = rendered(body);
    assert_eq!(
        leaves(&markdown, &[]),
        [
            li("first item\n# heading in an item\n> quote in an item"),
            li("nested item\n## nested heading\n[ref]: https://evil.example\n<script>x</script>"),
            li("[ ] task box"),
            li("[x] done box"),
            li("second"),
            li("one"),
            li("two\n---\n***"),
        ]
    );
}

#[test]
fn plain_text_lists_and_examples_render_as_structure() {
    let body = "Steps:\n- read the code\n- run `cargo test --workspace`\n  then compare\n\n\
                1. first\n2. second\n3. third\n\n5. five\n7. seven\n\n\
                ```sh\ncargo fmt --all\necho \"```\"\n```\n\n\
                Example:\n\n    fn main() {\n        println!(\"hi\");\n    }\n\n\
                | Column | Value |\n|---|---|\n| a | 1 |";
    let markdown = rendered(body);
    assert_eq!(
        leaves(&markdown, &[]),
        [
            p("Steps:"),
            li("read the code"),
            li("run `cargo test --workspace`\nthen compare"),
            li("first"),
            li("second"),
            li("third"),
            li("5. five"),
            li("seven"),
            code("cargo fmt --all\necho \"```\"\n"),
            p("Example:"),
            code("fn main() {\n    println!(\"hi\");\n}\n"),
            code("| Column | Value |\n|---|---|\n| a | 1 |\n"),
        ]
    );
    // A list after only blank lines continues the one above, so 5 cannot follow 3 as a number,
    // while 7 starts a new list after it.
    assert!(
        markdown.contains("1. first\n2. second\n3. third\n\n- 5\\. five\n\n7. seven"),
        "{markdown}"
    );
    assert!(markdown.contains("````sh\n"), "{markdown}");
}

#[test]
fn many_unclosed_fences_stay_text_found_in_one_pass() {
    // Twenty thousand openers that nothing closes: every line is text. Finding that takes one
    // pass over the lines, not a scan of the rest of the section per opener.
    let many = format!("{}```sh", "```sh\n".repeat(19_999));
    assert_eq!(leaves(&rendered(&many), &[]), [p(&many)]);
    // A closing line at the end turns the same lines into one example opened by the first.
    let closed = format!("{many}\n```");
    let inside = "```sh\n".repeat(19_999);
    assert_eq!(leaves(&rendered(&closed), &[]), [code(&inside)]);
}

#[test]
fn each_fence_closes_at_the_first_later_line_of_its_character_at_least_as_long() {
    let lines = [
        "````",
        "```",
        "~~~",
        "`````",
        "```",
        "~~~~",
        "~~ ~",
        "```",
        "```js onload=x",
    ];
    let closes: Vec<_> = closed_fences(&lines)
        .into_iter()
        .map(|fence| fence.map(|fence| fence.close))
        .collect();
    assert_eq!(
        closes,
        [
            Some(3),
            Some(3),
            Some(5),
            None,
            Some(7),
            None,
            None,
            None,
            None
        ]
    );
    // A closing line sits at most three columns in, as an opener does; one indented further is
    // content of the example.
    let indented = ["  ```rust", "  let x = 1;", "    ```", "   ```"];
    assert_eq!(
        closed_fences(&indented)[0],
        Some(Fence {
            width: 2,
            info: "rust",
            close: 3
        })
    );
}

#[test]
fn a_fence_line_indented_four_columns_is_content_not_a_closer() {
    let body = "Run:\n ```sh\n    ```\necho hi\n```\nDone.";
    let markdown = rendered(body);
    assert_eq!(
        leaves(&markdown, &[]),
        [p("Run:"), code("   ```\necho hi\n"), p("Done.")]
    );
    assert!(
        markdown.contains("````sh\n   ```\necho hi\n````"),
        "{markdown}"
    );
    let tildes = "~~~\n    ~~~\ntext\n~~~";
    assert_eq!(leaves(&rendered(tildes), &[]), [code("    ~~~\ntext\n")]);
    // A tab reaches column four, so it does not close either.
    let tab = "```\n\t```\necho hi\n```";
    assert_eq!(leaves(&rendered(tab), &[]), [code("\t```\necho hi\n")]);
    // A closer zero to three columns in closes, whatever the opener's own indentation.
    for closer in ["```", " ```", "  ```", "   ```"] {
        let body = format!(" ```sh\necho hi\n{closer}\nDone.");
        assert_eq!(
            leaves(&rendered(&body), &[]),
            [code("echo hi\n"), p("Done.")],
            "{closer:?}"
        );
    }
}

#[test]
fn only_ascii_spaces_and_tabs_pad_a_fence_line() {
    // The admitted case: a backtick line followed by a no-break space is content of the
    // example, kept with that character, and the later plain line closes it.
    let body = "Run:\n ```sh\n```\u{a0}\necho hi\n```\nDone.";
    let markdown = rendered(body);
    assert_eq!(
        leaves(&markdown, &[]),
        [p("Run:"), code("```\u{a0}\necho hi\n"), p("Done.")]
    );
    assert!(
        markdown.contains("````sh\n```\u{a0}\necho hi\n````"),
        "{markdown}"
    );
    // Any other whitespace before or after a run of fence characters is content too: such a
    // line never closes an example.
    for space in ['\u{a0}', '\u{2003}', '\u{3000}'] {
        for line in [format!("{space}```"), format!("```{space}")] {
            let body = format!("```\n{line}\necho hi\n```");
            assert_eq!(
                leaves(&rendered(&body), &[]),
                [code(&format!("{line}\necho hi\n"))],
                "{line:?}"
            );
        }
        let tildes = format!("~~~\n~~~{space}\ntext\n~~~");
        assert_eq!(
            leaves(&rendered(&tildes), &[]),
            [code(&format!("~~~{space}\ntext\n"))],
            "{space:?}"
        );
        // Nor does it open one: the lines stay text, as a parser reads the authored lines, with
        // that space.
        for opener in [format!("{space}```sh"), format!("```sh{space}")] {
            let body = format!("{opener}\necho hi\n```");
            let expected = format!("{opener}\necho hi\n```");
            assert_eq!(leaves(&rendered(&body), &[]), [p(&expected)], "{opener:?}");
        }
    }
    let lines = ["```sh", "```\u{a0}", "\u{a0}```", "```\u{2003}", "``` \t"];
    let closes: Vec<_> = closed_fences(&lines)
        .into_iter()
        .map(|fence| fence.map(|fence| fence.close))
        .collect();
    assert_eq!(closes, [Some(4), None, None, None, None]);
    // ASCII spaces and tabs still pad an opener's info word and a closer on either side.
    for (opener, closer) in [
        ("```sh \t", "```  \t"),
        ("``` sh", "   ```\t"),
        ("  ```\tsh", " ```  "),
    ] {
        let body = format!("{opener}\necho hi\n{closer}\nDone.");
        let markdown = rendered(&body);
        assert_eq!(
            leaves(&markdown, &[]),
            [code("echo hi\n"), p("Done.")],
            "{opener:?} {closer:?}"
        );
        assert!(markdown.starts_with("```sh\necho hi\n```"), "{markdown}");
    }
}

const UNICODE_SPACES: [char; 3] = ['\u{a0}', '\u{2003}', '\u{3000}'];

#[test]
fn an_indented_example_keeps_unicode_whitespace_after_its_ascii_indentation() {
    // The admitted case: four ASCII spaces of indentation, then a no-break space that is part
    // of the example.
    let body = "Example:\n\n    \u{a0}echo hi\n\nDone.";
    let markdown = rendered(body);
    assert_eq!(
        leaves(&markdown, &[]),
        [p("Example:"), code("\u{a0}echo hi\n"), p("Done.")]
    );
    assert!(markdown.contains("```\n\u{a0}echo hi\n```"), "{markdown}");
    for space in UNICODE_SPACES {
        // Every line shares the same Unicode space after the indentation: only the indentation
        // goes.
        let body = format!("Example:\n\n    {space}keep me\n    {space}second line\n\nDone.");
        assert_eq!(
            leaves(&rendered(&body), &[]),
            [
                p("Example:"),
                code(&format!("{space}keep me\n{space}second line\n")),
                p("Done."),
            ],
            "{space:?}"
        );
        // Deeper ASCII indentation stays relative to the shared one, across a blank line; a
        // tab indents too.
        let body = format!("    {space}a\n      b\n\n    {space}c");
        assert_eq!(
            leaves(&rendered(&body), &[]),
            [code(&format!("{space}a\n  b\n\n{space}c\n"))],
            "{space:?}"
        );
        let body = format!("\t{space}tab\n\t{space}again");
        assert_eq!(
            leaves(&rendered(&body), &[]),
            [code(&format!("{space}tab\n{space}again\n"))],
            "{space:?}"
        );
        // A table keeps it too.
        let body = format!("  | a |{space}\n  |---|\n  |{space}b |");
        assert_eq!(
            leaves(&rendered(&body), &[]),
            [code(&format!("| a |{space}\n|---|\n|{space}b |\n"))],
            "{space:?}"
        );
    }
    // ASCII indentation alone is still removed exactly as before.
    let body = "    fn main() {\n        body();\n    }";
    assert_eq!(
        leaves(&rendered(body), &[]),
        [code("fn main() {\n    body();\n}\n")]
    );
}

#[test]
fn prose_headings_and_lists_keep_unicode_padding_as_text() {
    for space in UNICODE_SPACES {
        // Prose: a line start, a line end and an otherwise empty line keep the space; a line
        // holding only it is text, not a blank line between paragraphs.
        let body = format!("{space}Indented prose{space}\nnext line{space}\n{space}\nlast");
        let markdown = rendered(&body);
        assert_eq!(leaves(&markdown, &[]), [p(&body)], "{markdown}");
        // Line-start syntax after the space stays text, and its first character is escaped.
        for case in [
            format!("{space}# not a heading"),
            format!("{space}- not an item"),
            format!("{space}1. not a list"),
            format!("{space}> not a quote"),
            format!("{space}==="),
            format!("Title\n{space}---"),
        ] {
            assert_eq!(leaves(&rendered(&case), &[]), [p(&case)], "{case:?}");
        }
        // An item keeps the space after its marker and at its end; a continuation line keeps
        // it after the indentation the renderer chose.
        let body = format!("- {space}item{space}\n  {space}continued\n- second");
        assert_eq!(
            leaves(&rendered(&body), &[]),
            [
                li(&format!("{space}item{space}\n{space}continued")),
                li("second")
            ],
            "{space:?}"
        );
        // A generated heading keeps authored Unicode padding and drops only ASCII padding.
        let title = format!("{space}Заголовок {space}");
        let markdown = format!("## {}", heading(&format!(" \t{title}\t ")));
        assert_eq!(
            leaves(&markdown, &[]),
            [("h2".to_string(), title)],
            "{markdown}"
        );
    }
    // A line of ASCII spaces and tabs is still a blank line, and ASCII padding still goes.
    let body = "  first paragraph  \n \t \n second paragraph\t";
    assert_eq!(
        leaves(&rendered(body), &[]),
        [p("first paragraph"), p("second paragraph")]
    );
}

#[test]
fn inert_code_reads_back_exactly_with_ascii_space_padding() {
    for text in [
        " a ",
        " \t ",
        " \u{a0} ",
        "\u{3000}x\u{3000}",
        "\tx\t",
        "   ",
        " ",
        " a`b ",
        "a ` b",
    ] {
        let markdown = format!("x {}", inert_code(text));
        assert_eq!(
            leaves(&markdown, &[]),
            [p(&format!("x {text}"))],
            "{text:?}: {markdown}"
        );
    }
}

const TASK: &str = "report-task";
/// An identity no record of the chain has: another policy, or other requirements.
const OTHER: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

/// One link of a recorded review chain to break.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Break {
    Nothing,
    /// The check receipt names another verifier policy than the acceptance receipt.
    CheckPolicy,
    /// The check receipt names another plan of the same revision than the acceptance step.
    CheckPlan,
    /// The acceptance step ran under a plan of another revision of the same Task.
    PlanRevision,
    /// The acceptance receipt names a verifier policy the revision does not.
    VerifierPolicy,
    /// The check receipt resolved citations against another Snapshot's Manifest.
    Manifest,
    /// The evaluation judged other requirements than the revision's.
    Requirements,
    /// The acceptance receipt does not retain its check receipt.
    VerificationRetains,
    /// The check receipt does not retain the captured sources it judged.
    ChecksRetain,
    /// The evaluation does not retain the requirements it judged.
    EvaluationRetains,
    /// The evaluation does not retain the captured sources its payload names.
    EvaluationSources,
    /// The acceptance receipt was recorded by another node than its invocation names.
    Producer,
    /// The check receipt is a kernel record, not a started Attempt of this Task.
    CheckProducer,
    /// The acceptance invocation binds other ports than the acceptance step does.
    Invocation,
}

const BREAKS: [Break; 13] = [
    Break::CheckPolicy,
    Break::CheckPlan,
    Break::PlanRevision,
    Break::VerifierPolicy,
    Break::Manifest,
    Break::Requirements,
    Break::VerificationRetains,
    Break::ChecksRetain,
    Break::EvaluationRetains,
    Break::EvaluationSources,
    Break::Producer,
    Break::CheckProducer,
    Break::Invocation,
];

struct Chain {
    cas: Cas,
    _dir: tempfile::TempDir,
    task: TaskRevisionV1,
    result: TaskResultV1,
    document_id: String,
    port: &'static str,
}

fn fixture<T: DeserializeOwned>(name: &str) -> T {
    let root = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .join("fixtures/task-contracts/v1");
    serde_json::from_slice(&std::fs::read(root.join(name)).unwrap()).unwrap()
}

fn kernel(operation: &str) -> Producer {
    Producer::KernelOperation {
        run_id: "fixture".into(),
        node_id: None,
        operation_id: format!("{operation}@1"),
    }
}

/// A started Attempt of `node` in this Task's run.
fn attempt(node: &str) -> Producer {
    Producer::Attempt {
        run_id: task_run_id(TASK).unwrap(),
        node_id: node.into(),
        attempt_id: "a".repeat(26),
    }
}

fn put_by(
    cas: &Cas,
    ty: &str,
    producer: Producer,
    inputs: Vec<String>,
    snapshot: Option<&str>,
    value: impl serde::Serialize,
) -> String {
    cas.put_artifact(
        ty,
        producer,
        inputs,
        snapshot.map(String::from),
        serde_json::to_value(value).unwrap(),
    )
    .unwrap()
    .0
}

fn put(
    cas: &Cas,
    ty: &str,
    inputs: Vec<String>,
    snapshot: Option<&str>,
    value: impl serde::Serialize,
) -> String {
    put_by(cas, ty, kernel("fixture"), inputs, snapshot, value)
}

fn one(id: &str, ty: &str, snapshot: Option<&str>) -> ArtifactInputV1 {
    ArtifactInputV1 {
        artifact_ids: vec![id.into()],
        artifact_type: ty.into(),
        cardinality: PortCardinality::One,
        snapshot_id: snapshot.map(String::from),
    }
}

/// A recorded plan of the revision `revision_id`, binding its inputs and authority; `name`
/// tells two plans of one revision apart.
fn plan(cas: &Cas, task: &TaskRevisionV1, revision_id: &str, name: &str) -> String {
    let mut plan: ExecutionPlanV1 = fixture("execution-plan.json");
    plan.task_revision_id = revision_id.into();
    plan.authority = task.authority.clone();
    plan.inputs = task.inputs.clone();
    put_by(
        cas,
        EXECUTION_PLAN_V1,
        kernel(name),
        vec![revision_id.into()],
        None,
        &plan,
    )
}

/// A recorded revision of `TASK` with `inputs` and one obligation met by `evidence` under
/// `policy`, its identity, and a plan of it.
fn revision(
    cas: &Cas,
    inputs: &BTreeMap<String, ArtifactInputV1>,
    evidence: &str,
    policy: &str,
    goal: &str,
) -> (TaskRevisionV1, String, String) {
    let mut task: TaskRevisionV1 = fixture("task-revision.json");
    task.task_id = TASK.into();
    task.goal = goal.into();
    task.inputs = inputs.clone();
    task.acceptance = BTreeMap::from([(
        "verified".to_string(),
        AcceptanceObligationV1 {
            evidence_type: evidence.into(),
            verifier_policy: policy.into(),
        },
    )]);
    let revision_id = put(cas, TASK_REVISION_V1, vec![], None, &task);
    let plan_id = plan(cas, &task, &revision_id, "first");
    (task, revision_id, plan_id)
}

/// The plan the acceptance step runs under, the plan the checks name, the policy the
/// acceptance receipt names and the policy the checks name, with `broken` broken.
fn plans_and_policies(
    cas: &Cas,
    task: &TaskRevisionV1,
    revision_id: &str,
    plan_id: String,
    policy: &str,
    evidence: &str,
    broken: Break,
) -> (String, String, String, String) {
    let accept_plan = if broken == Break::PlanRevision {
        let (other, other_id, _) = revision(cas, &task.inputs, evidence, policy, "Another goal.");
        plan(cas, &other, &other_id, "other")
    } else {
        plan_id
    };
    let check_plan = if broken == Break::CheckPlan {
        plan(cas, task, revision_id, "second")
    } else {
        accept_plan.clone()
    };
    let verifier_policy = if broken == Break::VerifierPolicy {
        OTHER.to_string()
    } else {
        policy.to_string()
    };
    let check_policy = if broken == Break::CheckPolicy {
        OTHER.to_string()
    } else {
        verifier_policy.clone()
    };
    (accept_plan, check_plan, verifier_policy, check_policy)
}

/// The acceptance receipt's producer: the kernel operation of `invocation`, or of another node.
fn accept_producer(cas: &Cas, invocation: &TaskInvocationV1, broken: Break) -> Producer {
    let mut recorded = invocation.clone();
    if broken == Break::Producer {
        recorded.node = "root.nodes.other".into();
    }
    invocation_producer(cas, &recorded, None).unwrap()
}

/// What an acceptance receipt retains: every artifact its invocation binds, and `also`.
fn accept_retains(
    invocation: &TaskInvocationV1,
    also: &[&str],
    checks_id: &str,
    broken: Break,
) -> Vec<String> {
    let mut retained: Vec<String> = invocation
        .inputs
        .values()
        .flat_map(|input| input.artifact_ids.iter().cloned())
        .chain(also.iter().map(|id| id.to_string()))
        .collect();
    if broken == Break::VerificationRetains {
        retained.retain(|id| id != checks_id);
    }
    retained
}

fn acceptance(outcome: ReceiptOutcomeV1) -> TaskAcceptanceV1 {
    match outcome {
        ReceiptOutcomeV1::Passed => TaskAcceptanceV1::Satisfied,
        ReceiptOutcomeV1::Failed => TaskAcceptanceV1::Unsatisfied,
        ReceiptOutcomeV1::Inconclusive => TaskAcceptanceV1::Inconclusive,
    }
}

/// A captured Snapshot holding `bytes` as `crates/af/src/cli.rs`, the source port that names
/// it and its Manifest's identity.
fn capture(cas: &Cas, bytes: &[u8]) -> (String, ArtifactInputV1, String) {
    let manifest = Manifest::new(vec![Entry {
        path: "crates/af/src/cli.rs".into(),
        kind: EntryKind::File,
        content: cas.put(bytes).unwrap(),
        size: bytes.len() as u64,
    }])
    .unwrap();
    let origin = cas
        .put_json(&serde_json::json!({
            "schema": "af.task-source-origin/1",
            "repository_id": "fixture",
            "source_revision": "0".repeat(40),
            "content_digest": manifest.content_digest(),
        }))
        .unwrap();
    let id = capture_snapshot(cas, &manifest, &origin, None).unwrap();
    let port = source_tree(cas, kernel("source"), &id, vec![origin]).unwrap();
    let manifest_id = source_snapshot(cas, &port).unwrap().1.manifest_id;
    (id, port, manifest_id)
}

fn draft(body: &str) -> DocumentDraftV2 {
    DocumentDraftV2 {
        schema: "af.document-draft/2".into(),
        title: "Отчёт: why --format markdown is hard to read".into(),
        sections: vec![
            DocumentSectionV1 {
                heading: "Findings".into(),
                body: body.into(),
            },
            DocumentSectionV1 {
                heading: "Recommendation".into(),
                body: "Add a presentation (not a rewrite).".into(),
            },
        ],
        citations: BTreeSet::from(["spec".into(), "local".into()]),
        repository_citations: BTreeSet::from([RepositoryCitationV1 {
            path: "crates/af/src/cli.rs".into(),
            line: Some(1163),
        }]),
    }
}

/// The sources a report Task captured, and so what its verifier must have read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Captured {
    /// Two sources the draft cites, bound at the Task and read by the verifier.
    Cited,
    /// The empty set, bound at the Task as a Task file captures it; the verifier goes unbound.
    EmptyBound,
    /// No `sources` port: the seal records the empty set and the verifier goes unbound.
    Unbound,
}

/// A report as the report profile records it: one captured Snapshot, a revision of one Task
/// and its plan, the draft, the Document, its checks, an evaluation with `evaluation` when the
/// checks pass, the acceptance receipt and a result naming them, with `broken` broken.
fn report_chain(passed: bool, evaluation: Option<ReceiptOutcomeV1>, broken: Break) -> Chain {
    report_chain_with(passed, evaluation, broken, Captured::Cited)
}

fn report_chain_with(
    passed: bool,
    evaluation: Option<ReceiptOutcomeV1>,
    broken: Break,
    captured: Captured,
) -> Chain {
    report_chain_citing(passed, evaluation, broken, captured, SPEC)
}

/// The commonly cited specification, captured as the `spec` source.
const SPEC: &str = "https://spec.commonmark.org/0.31.2/";

/// [`report_chain_with`], its `spec` source captured at `spec_uri`.
fn report_chain_citing(
    passed: bool,
    evaluation: Option<ReceiptOutcomeV1>,
    broken: Break,
    captured: Captured,
    spec_uri: &str,
) -> Chain {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let (snapshot_id, source, manifest_id) = capture(&cas, b"fn main() {}\n");
    let snapshot = Some(snapshot_id.as_str());
    let source_id = source.artifact_ids[0].clone();
    let requirements = put(
        &cas,
        "af/Requirements@1",
        vec![],
        None,
        serde_json::json!({"text": "Report it."}),
    );
    let sources = if captured == Captured::Cited {
        ReportSourcesV1 {
            schema: "af.document-sources/1".into(),
            sources: BTreeMap::from([
                (
                    "spec".into(),
                    DocumentSourceV1 {
                        title: "CommonMark spec".into(),
                        uri: spec_uri.into(),
                        revision: "0.31.2".into(),
                        text: "Markdown.".into(),
                    },
                ),
                (
                    "local".into(),
                    DocumentSourceV1 {
                        title: "Local [notes]".into(),
                        uri: "repo:docs/notes.md".into(),
                        revision: "r1".into(),
                        text: "Notes.".into(),
                    },
                ),
            ]),
        }
    } else {
        ReportSourcesV1::empty()
    };
    let sources_id = put(&cas, REPORT_SOURCES_V1, vec![], None, &sources);
    let policy = cas
        .put_json(&serde_json::json!({"fixture": "report policy"}))
        .unwrap();
    let mut inputs = BTreeMap::from([
        (
            "requirements".to_string(),
            one(&requirements, "af/Requirements@1", None),
        ),
        ("source".to_string(), source.clone()),
    ]);
    if captured != Captured::Unbound {
        inputs.insert(
            "sources".to_string(),
            one(&sources_id, REPORT_SOURCES_V1, None),
        );
    }
    let (task, revision_id, plan_id) =
        revision(&cas, &inputs, REPORT_VERIFICATION_V1, &policy, "Report it.");
    let (accept_plan, check_plan, verifier_policy, check_policy) = plans_and_policies(
        &cas,
        &task,
        &revision_id,
        plan_id,
        &policy,
        REPORT_VERIFICATION_V1,
        broken,
    );
    let mut draft = draft("Plain text with a path crates/af/src/cli.rs:1163.\n- one\n- two");
    if captured != Captured::Cited {
        draft.citations.clear();
    }
    let draft_id = put(&cas, DOCUMENT_DRAFT_V2, vec![], snapshot, &draft);
    let document = DocumentV1 {
        schema: "af.document/1".into(),
        draft_id: draft_id.clone(),
        sources_id: sources_id.clone(),
        format: DocumentFormatV1::Markdown,
        text: render_report(&ReportDraft::V2(draft), &sources).unwrap(),
    };
    let document_id = put(
        &cas,
        DOCUMENT_V1,
        vec![draft_id, sources_id.clone(), source_id.clone()],
        snapshot,
        &document,
    );
    let judged_manifest = if broken == Break::Manifest {
        capture(&cas, b"fn main() { changed(); }\n").2
    } else {
        manifest_id.clone()
    };
    let (outcome, results, citation_failures) = if passed {
        (
            ReceiptOutcomeV1::Passed,
            BTreeMap::from([
                ("size".to_string(), ReceiptOutcomeV1::Passed),
                ("repository_citations".to_string(), ReceiptOutcomeV1::Passed),
            ]),
            vec![],
        )
    } else {
        (
            ReceiptOutcomeV1::Failed,
            BTreeMap::from([
                ("size".to_string(), ReceiptOutcomeV1::Passed),
                ("repository_citations".to_string(), ReceiptOutcomeV1::Failed),
            ]),
            vec![ReportCitationFailureV1 {
                citation: RepositoryCitationV1 {
                    path: "crates/af/src/cli.rs".into(),
                    line: Some(1163),
                },
                reason: ReportCitationFailureReasonV1::LineOutOfRange,
            }],
        )
    };
    let checks = ReportCheckReceiptV1 {
        plan_id: check_plan,
        document_id: document_id.clone(),
        sources_id: sources_id.clone(),
        policy_id: check_policy,
        source_snapshot_id: snapshot_id.clone(),
        manifest_id: judged_manifest,
        checks: results,
        citation_failures,
        outcome,
    };
    let mut checks_retain = vec![
        document_id.clone(),
        sources_id.clone(),
        manifest_id.clone(),
        source_id.clone(),
    ];
    if broken == Break::ChecksRetain {
        checks_retain.retain(|id| *id != sources_id);
    }
    let checks_producer = if broken == Break::CheckProducer {
        kernel("checks")
    } else {
        attempt("root.nodes.checks")
    };
    let checks_id = put_by(
        &cas,
        REPORT_CHECK_RECEIPT_V1,
        checks_producer,
        checks_retain,
        snapshot,
        &checks,
    );
    let judged_requirements = if broken == Break::Requirements {
        put(
            &cas,
            "af/Requirements@1",
            vec![],
            None,
            serde_json::json!({"text": "Report something else."}),
        )
    } else {
        requirements.clone()
    };
    let evaluation = evaluation.filter(|_| passed);
    let evaluation_id = evaluation.map(|outcome| {
        let mut retained = vec![
            document_id.clone(),
            sources_id.clone(),
            judged_requirements.clone(),
            checks_id.clone(),
            source_id.clone(),
        ];
        if broken == Break::EvaluationRetains {
            retained.retain(|id| *id != judged_requirements);
        }
        // Only the recorded empty set may go unbound at the verifier.
        if broken == Break::EvaluationSources || captured != Captured::Cited {
            retained.retain(|id| *id != sources_id);
        }
        put_by(
            &cas,
            REPORT_EVALUATION_V1,
            attempt("root.nodes.verify"),
            retained,
            snapshot,
            ReportEvaluationV1 {
                document_id: document_id.clone(),
                sources_id: sources_id.clone(),
                requirements_id: judged_requirements.clone(),
                check_receipt_id: checks_id.clone(),
                source_snapshot_id: snapshot_id.clone(),
                outcome,
                summary: "The <b>cited</b> line is right; see [here](javascript:x).\n- point one"
                    .into(),
            },
        )
    });
    let mut bound = BTreeMap::from([
        (
            "document".to_string(),
            one(&document_id, DOCUMENT_V1, snapshot),
        ),
        (
            "checks".to_string(),
            one(&checks_id, REPORT_CHECK_RECEIPT_V1, snapshot),
        ),
        ("source".to_string(), source.clone()),
    ]);
    if let Some(id) = &evaluation_id {
        bound.insert("evaluation".into(), one(id, REPORT_EVALUATION_V1, snapshot));
    }
    if broken == Break::Invocation {
        bound.remove("source");
    }
    let invocation = TaskInvocationV1 {
        plan_id: accept_plan,
        node: "root.nodes.accept".into(),
        inputs: bound,
    };
    let verification = ReportVerificationV1 {
        invocation: invocation.clone(),
        document_id: document_id.clone(),
        policy_id: verifier_policy,
        check_receipt_id: checks_id.clone(),
        evaluation_id,
        source_snapshot_id: snapshot_id.clone(),
        outcome: derived(outcome, evaluation),
    };
    let verification_id = put_by(
        &cas,
        REPORT_VERIFICATION_V1,
        accept_producer(&cas, &invocation, broken),
        accept_retains(&invocation, &[manifest_id.as_str()], &checks_id, broken),
        snapshot,
        &verification,
    );
    let result = TaskResultV1 {
        task_revision_id: revision_id,
        execution: TaskExecutionV1::Completed,
        acceptance: acceptance(verification.outcome),
        domain_conclusion: "verified".into(),
        outputs: BTreeMap::from([
            ("report".into(), one(&document_id, DOCUMENT_V1, snapshot)),
            (
                "verification".into(),
                one(&verification_id, REPORT_VERIFICATION_V1, snapshot),
            ),
        ]),
        evidence: BTreeSet::from([verification_id]),
        missing_obligations: BTreeSet::new(),
    };
    Chain {
        cas,
        _dir: dir,
        task,
        result,
        document_id,
        port: "report",
    }
}

/// A Document as the Document profile records it, data-only, with `broken` broken.
fn document_chain(evaluation: Option<ReceiptOutcomeV1>, broken: Break) -> Chain {
    let dir = tempfile::tempdir().unwrap();
    let cas = Cas::open(dir.path()).unwrap();
    let requirements = put(
        &cas,
        "af/Requirements@1",
        vec![],
        None,
        serde_json::json!({"text": "Write the guide."}),
    );
    let sources = DocumentSourcesV1 {
        schema: "af.document-sources/1".into(),
        sources: BTreeMap::from([(
            "spec".into(),
            DocumentSourceV1 {
                title: "CommonMark spec".into(),
                uri: "https://spec.commonmark.org/0.31.2/".into(),
                revision: "0.31.2".into(),
                text: "Markdown.".into(),
            },
        )]),
    };
    let sources_id = put(&cas, DOCUMENT_SOURCES_V1, vec![], None, &sources);
    let policy = cas
        .put_json(&serde_json::json!({"fixture": "document policy"}))
        .unwrap();
    let inputs = BTreeMap::from([
        (
            "requirements".to_string(),
            one(&requirements, "af/Requirements@1", None),
        ),
        (
            "sources".to_string(),
            one(&sources_id, DOCUMENT_SOURCES_V1, None),
        ),
    ]);
    let (task, revision_id, plan_id) = revision(
        &cas,
        &inputs,
        DOCUMENT_VERIFICATION_V1,
        &policy,
        "Write the guide.",
    );
    let (accept_plan, check_plan, verifier_policy, check_policy) = plans_and_policies(
        &cas,
        &task,
        &revision_id,
        plan_id,
        &policy,
        DOCUMENT_VERIFICATION_V1,
        broken,
    );
    let draft = DocumentDraftV1 {
        schema: "af.document-draft/1".into(),
        title: "Migration guide".into(),
        sections: vec![DocumentSectionV1 {
            heading: "Steps".into(),
            body: "Export with --format human-markdown (see docs/task-execution/report.md).\n\
                   - step one\n- step two"
                .into(),
        }],
        citations: BTreeSet::from(["spec".into()]),
    };
    let draft_id = put(&cas, DOCUMENT_DRAFT_V1, vec![], None, &draft);
    let document = DocumentV1 {
        schema: "af.document/1".into(),
        draft_id: draft_id.clone(),
        sources_id: sources_id.clone(),
        format: DocumentFormatV1::Markdown,
        text: render_document(&draft, &sources).unwrap(),
    };
    let document_id = put(
        &cas,
        DOCUMENT_V1,
        vec![draft_id, sources_id.clone()],
        None,
        &document,
    );
    let checks = DocumentCheckReceiptV1 {
        plan_id: check_plan,
        document_id: document_id.clone(),
        sources_id: sources_id.clone(),
        policy_id: check_policy,
        checks: BTreeMap::from([("size".into(), ReceiptOutcomeV1::Passed)]),
        outcome: ReceiptOutcomeV1::Passed,
    };
    let mut checks_retain = vec![document_id.clone(), sources_id.clone()];
    if broken == Break::ChecksRetain {
        checks_retain.retain(|id| *id != sources_id);
    }
    let checks_producer = if broken == Break::CheckProducer {
        kernel("checks")
    } else {
        attempt("root.nodes.checks")
    };
    let checks_id = put_by(
        &cas,
        DOCUMENT_CHECK_RECEIPT_V1,
        checks_producer,
        checks_retain,
        None,
        &checks,
    );
    let judged_requirements = if broken == Break::Requirements {
        put(
            &cas,
            "af/Requirements@1",
            vec![],
            None,
            serde_json::json!({"text": "Write something else."}),
        )
    } else {
        requirements.clone()
    };
    let evaluation_id = evaluation.map(|outcome| {
        let mut retained = vec![
            document_id.clone(),
            sources_id.clone(),
            judged_requirements.clone(),
            checks_id.clone(),
        ];
        if broken == Break::EvaluationRetains {
            retained.retain(|id| *id != judged_requirements);
        }
        if broken == Break::EvaluationSources {
            retained.retain(|id| *id != sources_id);
        }
        put_by(
            &cas,
            DOCUMENT_EVALUATION_V1,
            attempt("root.nodes.verify"),
            retained,
            None,
            DocumentEvaluationV1 {
                document_id: document_id.clone(),
                sources_id: sources_id.clone(),
                requirements_id: judged_requirements.clone(),
                check_receipt_id: checks_id.clone(),
                outcome,
                summary: "The steps are complete.".into(),
            },
        )
    });
    let mut bound = BTreeMap::from([
        ("document".to_string(), one(&document_id, DOCUMENT_V1, None)),
        (
            "checks".to_string(),
            one(&checks_id, DOCUMENT_CHECK_RECEIPT_V1, None),
        ),
    ]);
    if let Some(id) = &evaluation_id {
        bound.insert("evaluation".into(), one(id, DOCUMENT_EVALUATION_V1, None));
    }
    if broken == Break::Invocation {
        bound.insert(
            "sources".into(),
            one(&sources_id, DOCUMENT_SOURCES_V1, None),
        );
    }
    let invocation = TaskInvocationV1 {
        plan_id: accept_plan,
        node: "root.nodes.accept".into(),
        inputs: bound,
    };
    let verification = DocumentVerificationV1 {
        invocation: invocation.clone(),
        document_id: document_id.clone(),
        policy_id: verifier_policy,
        check_receipt_id: checks_id.clone(),
        evaluation_id,
        outcome: derived(ReceiptOutcomeV1::Passed, evaluation),
    };
    let verification_id = put_by(
        &cas,
        DOCUMENT_VERIFICATION_V1,
        accept_producer(&cas, &invocation, broken),
        accept_retains(&invocation, &[], &checks_id, broken),
        None,
        &verification,
    );
    let result = TaskResultV1 {
        task_revision_id: revision_id,
        execution: TaskExecutionV1::Completed,
        acceptance: acceptance(verification.outcome),
        domain_conclusion: "verified".into(),
        outputs: BTreeMap::from([
            ("document".into(), one(&document_id, DOCUMENT_V1, None)),
            (
                "verification".into(),
                one(&verification_id, DOCUMENT_VERIFICATION_V1, None),
            ),
        ]),
        evidence: BTreeSet::from([verification_id]),
        missing_obligations: BTreeSet::new(),
    };
    Chain {
        cas,
        _dir: dir,
        task,
        result,
        document_id,
        port: "document",
    }
}

fn export(chain: &Chain) -> Result<String, String> {
    export_result(chain, &chain.result)
}

fn export_result(chain: &Chain, result: &TaskResultV1) -> Result<String, String> {
    human_markdown(&chain.cas, TASK, chain.port, result, &chain.task)
}

/// The closing note, as a parser reads it.
fn about(leaves: &[(String, String)]) -> &str {
    leaves
        .iter()
        .find(|(kind, text)| kind.as_str() == "p" && text.starts_with("About this file:"))
        .map(|(_, text)| text.as_str())
        .expect("an About note")
}

const ABOUT: &str = "About this file: a readable presentation of the report output of Task \
                     report-task, made from the draft and sources that render exactly to its \
                     recorded report.";
const CANONICAL: &str = "the canonical report, which af task output report-task --port report \
                         --format markdown writes, not this layout.";

#[test]
fn a_verified_report_shows_its_result_review_and_only_safe_source_links() {
    let chain = report_chain(true, Some(ReceiptOutcomeV1::Passed), Break::Nothing);
    let markdown = export(&chain).unwrap();
    let spec = "https://spec.commonmark.org/0.31.2/";
    let leaves = leaves(&markdown, &[spec]);
    let headings: Vec<_> = leaves
        .iter()
        .filter(|(kind, _)| kind.starts_with('h'))
        .collect();
    assert_eq!(
        headings,
        [
            &(
                "h1".to_string(),
                "Отчёт: why --format markdown is hard to read".to_string()
            ),
            &("h2".into(), "Findings".into()),
            &("h2".into(), "Recommendation".into()),
            &("h2".into(), "Sources".into()),
            &("h2".into(), "Repository citations".into()),
            &("h2".into(), "Review".into()),
        ]
    );
    assert_eq!(
        leaves[1],
        p("Result: accepted. The report passed its automated checks and its independent review.")
    );
    for leaf in [
        p("Plain text with a path crates/af/src/cli.rs:1163."),
        li("one"),
        li("CommonMark spec, revision 0.31.2"),
        li("Local [notes]: repo:docs/notes.md, revision r1"),
        li("crates/af/src/cli.rs:1163"),
        p("Automated checks: passed."),
        p("Independent review: passed."),
        p("The <b>cited</b> line is right; see [here](javascript:x)."),
        li("point one"),
    ] {
        assert!(leaves.contains(&leaf), "{leaf:?} in\n{markdown}");
    }
    assert_eq!(
        about(&leaves),
        format!("{ABOUT} The automated checks and the independent reviewer judged {CANONICAL}")
    );
    assert!(
        markdown.contains(&format!("[CommonMark spec](<{spec}>)")),
        "{markdown}"
    );
    // No digest, schema or envelope reaches the human view.
    assert!(!markdown.contains("sha256:"), "{markdown}");
    assert!(!markdown.contains("af/Document"), "{markdown}");
    assert!(!markdown.contains("schema"), "{markdown}");
    // The canonical report is untouched: it still escapes every punctuation character.
    let document: DocumentV1 =
        serde_json::from_value(chain.cas.get_artifact(&chain.document_id).unwrap().payload)
            .unwrap();
    assert!(
        document
            .text
            .contains("crates\\/af\\/src\\/cli\\.rs\\:1163\\."),
        "{}",
        document.text
    );
}

/// Every link destination a Markdown parser reads from `markdown`, in order.
fn links(markdown: &str) -> Vec<String> {
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, markdown, &options());
    root.descendants()
        .filter_map(|node| match &node.data.borrow().value {
            NodeValue::Link(link) => Some(link.url.clone()),
            _ => None,
        })
        .collect()
}

/// Captured locations whose query or fragment a parser could read as character references.
const ENTITY_LIKE: [&str; 8] = [
    "https://example.com/?q=&copy;",
    "https://example.com/?q=&copy",
    "https://example.com/?a=&#169;&b=&#xA9;&c=&#XA9;",
    "https://example.com/?q=&amp;&r=&amp;amp;",
    "https://example.com/search?q=rust&lang=ru&page=2",
    "https://example.com/p?x=&#0;&y=&nbsp;&z=&quot;#&lt;frag&gt;",
    "https://example.com/?q=%26copy%3B&next=&",
    "https://example.com/a&b/c;d",
];

#[test]
fn a_generated_link_reads_back_as_exactly_its_captured_location() {
    // The fault this guards: written raw, the destination decodes `&copy;` to `©`.
    assert_eq!(
        links("- [spec](<https://example.com/?q=&copy;>)"),
        ["https://example.com/?q=©"]
    );
    for uri in ENTITY_LIKE {
        assert!(safe_source_location(uri), "{uri}");
        let line = format!("- [spec](<{}>), revision 1", link_destination(uri));
        assert_eq!(links(&line), [uri], "{line}");
    }
    // Locations without an ampersand are written exactly as captured.
    let spec = "https://spec.commonmark.org/0.31.2/";
    assert_eq!(link_destination(spec), spec);
}

#[test]
fn a_report_links_each_captured_query_exactly_and_keeps_its_canonical_text() {
    for uri in ENTITY_LIKE {
        let chain = report_chain_citing(
            true,
            Some(ReceiptOutcomeV1::Passed),
            Break::Nothing,
            Captured::Cited,
            uri,
        );
        let markdown = export(&chain).unwrap();
        // The source list stays the same structure and text; its one link is the captured URI.
        let leaves = leaves(&markdown, &[uri]);
        assert!(
            leaves.contains(&li("CommonMark spec, revision 0.31.2")),
            "{markdown}"
        );
        assert_eq!(links(&markdown), [uri], "{markdown}");
        // The canonical report still holds the captured location byte for byte.
        let document: DocumentV1 =
            serde_json::from_value(chain.cas.get_artifact(&chain.document_id).unwrap().payload)
                .unwrap();
        assert!(
            document.text.contains(&format!("(<{uri}>)")),
            "{}",
            document.text
        );
    }
}

#[test]
fn a_negative_or_missing_review_is_stated_plainly() {
    let refused = export(&report_chain(
        true,
        Some(ReceiptOutcomeV1::Failed),
        Break::Nothing,
    ))
    .unwrap();
    let leaves_refused = leaves(&refused, &["https://spec.commonmark.org/0.31.2/"]);
    for leaf in [
        p(
            "Result: changes requested. The report passed its automated checks, but the independent reviewer asked for changes.",
        ),
        p("Independent review: changes requested."),
        p("The reviewer's summary, as recorded:"),
    ] {
        assert!(leaves_refused.contains(&leaf), "{leaf:?} in\n{refused}");
    }
    assert_eq!(
        about(&leaves_refused),
        format!("{ABOUT} The automated checks and the independent reviewer judged {CANONICAL}")
    );
    let unreviewed = export(&report_chain(true, None, Break::Nothing)).unwrap();
    let leaves_unreviewed = leaves(&unreviewed, &["https://spec.commonmark.org/0.31.2/"]);
    for leaf in [
        p(
            "Result: incomplete. The report passed its automated checks; no independent review was recorded.",
        ),
        p("Independent review: not recorded."),
    ] {
        assert!(
            leaves_unreviewed.contains(&leaf),
            "{leaf:?} in\n{unreviewed}"
        );
    }
    // Only the checks judged this report, and the closing note says no more than that.
    assert_eq!(
        about(&leaves_unreviewed),
        format!(
            "{ABOUT} The automated checks judged {CANONICAL} No independent review was recorded."
        )
    );
    assert!(!unreviewed.contains("reviewer judged"), "{unreviewed}");
}

#[test]
fn failed_checks_say_no_reviewer_ran_from_the_result_to_the_closing_note() {
    let markdown = export(&report_chain(false, None, Break::Nothing)).unwrap();
    let leaves = leaves(&markdown, &["https://spec.commonmark.org/0.31.2/"]);
    for leaf in [
        p(
            "Result: changes requested. The report failed its automated checks, so no independent review ran.",
        ),
        p("Automated checks: failed (repository citations)."),
        li("crates/af/src/cli.rs:1163: the line is past the end of the file"),
        p("Independent review: did not run, because the automated checks failed."),
    ] {
        assert!(leaves.contains(&leaf), "{leaf:?} in\n{markdown}");
    }
    assert_eq!(
        about(&leaves),
        format!(
            "{ABOUT} The automated checks judged {CANONICAL} They failed, so no independent reviewer ran."
        )
    );
    assert!(!markdown.contains("reviewer judged"), "{markdown}");
    assert!(!markdown.contains("reviewer's summary"), "{markdown}");
}

#[test]
fn stale_or_unrelated_evidence_is_never_attached() {
    let chain = report_chain(true, Some(ReceiptOutcomeV1::Passed), Break::Nothing);
    let recorded = chain
        .cas
        .get_artifact(&chain.result.outputs["verification"].artifact_ids[0])
        .unwrap();
    let original: ReportVerificationV1 = serde_json::from_value(recorded.payload.clone()).unwrap();
    // A verification that judges another Document is not this report's review, and the file
    // claims no checks or review for it.
    let mut unrelated = chain.result.clone();
    let mut verification = original.clone();
    verification.document_id = OTHER.into();
    let id = put_by(
        &chain.cas,
        REPORT_VERIFICATION_V1,
        recorded.producer.clone(),
        recorded.input_artifacts.clone(),
        recorded.subject_snapshot_id.as_deref(),
        &verification,
    );
    let snapshot = recorded.subject_snapshot_id.as_deref();
    unrelated.outputs.insert(
        "verification".into(),
        one(&id, REPORT_VERIFICATION_V1, snapshot),
    );
    unrelated.evidence = BTreeSet::from([id]);
    let markdown = export_result(&chain, &unrelated).unwrap();
    let parsed = leaves(&markdown, &["https://spec.commonmark.org/0.31.2/"]);
    assert!(
        markdown.contains("No acceptance receipt of this Task judges this exact output"),
        "{markdown}"
    );
    assert!(!markdown.contains("Independent review:"), "{markdown}");
    assert_eq!(
        about(&parsed),
        format!(
            "{ABOUT} No recorded checks or review judge this output; af task output report-task \
             --port report --format markdown writes its canonical report."
        )
    );
    // A receipt that is an output but not counted evidence is not shown either.
    let mut uncounted = chain.result.clone();
    uncounted.evidence.clear();
    let markdown = export_result(&chain, &uncounted).unwrap();
    assert!(markdown.contains("No acceptance receipt"), "{markdown}");
    // A receipt whose stated outcome its own checks and evaluation do not give is refused.
    let mut forged = chain.result.clone();
    let mut verification = original;
    verification.outcome = ReceiptOutcomeV1::Failed;
    let id = put_by(
        &chain.cas,
        REPORT_VERIFICATION_V1,
        recorded.producer.clone(),
        recorded.input_artifacts.clone(),
        snapshot,
        &verification,
    );
    forged.outputs.insert(
        "verification".into(),
        one(&id, REPORT_VERIFICATION_V1, snapshot),
    );
    forged.evidence = BTreeSet::from([id]);
    let error = export_result(&chain, &forged).unwrap_err();
    assert!(
        error.contains("does not judge this exact report"),
        "{error}"
    );
    // A revision of another Task never lends its result a review.
    let mut foreign = chain.task.clone();
    foreign.task_id = "another-task".into();
    let error = human_markdown(&chain.cas, TASK, "report", &chain.result, &foreign).unwrap_err();
    assert!(error.contains("revision of another Task"), "{error}");
}

#[test]
fn every_broken_link_of_a_report_review_refuses_the_export() {
    // The intact chain is the positive control each break is measured against.
    export(&report_chain(
        true,
        Some(ReceiptOutcomeV1::Passed),
        Break::Nothing,
    ))
    .unwrap();
    for broken in BREAKS {
        let error =
            export(&report_chain(true, Some(ReceiptOutcomeV1::Passed), broken)).unwrap_err();
        assert!(
            error.contains("does not judge this exact report"),
            "{broken:?}: {error}"
        );
    }
}

/// The recorded evaluation of `chain` and the sources its Document names.
fn evaluation_and_sources(chain: &Chain) -> (ArtifactEnvelope, String) {
    let verification: ReportVerificationV1 = serde_json::from_value(
        chain
            .cas
            .get_artifact(&chain.result.outputs["verification"].artifact_ids[0])
            .unwrap()
            .payload,
    )
    .unwrap();
    let document: DocumentV1 =
        serde_json::from_value(chain.cas.get_artifact(&chain.document_id).unwrap().payload)
            .unwrap();
    let evaluation = chain
        .cas
        .get_artifact(&verification.evaluation_id.unwrap())
        .unwrap();
    (evaluation, document.sources_id)
}

#[test]
fn a_review_counts_only_when_its_verifier_read_the_captured_sources() {
    // Positive control: the evaluation retains the captured sources it judged.
    let chain = report_chain(true, Some(ReceiptOutcomeV1::Passed), Break::Nothing);
    let (evaluation, sources_id) = evaluation_and_sources(&chain);
    assert!(evaluation.input_artifacts.contains(&sources_id));
    export(&chain).unwrap();
    // Only the retained reference is missing: every payload still names the right sources.
    let chain = report_chain(
        true,
        Some(ReceiptOutcomeV1::Passed),
        Break::EvaluationSources,
    );
    let (evaluation, sources_id) = evaluation_and_sources(&chain);
    let value: ReportEvaluationV1 = serde_json::from_value(evaluation.payload.clone()).unwrap();
    assert_eq!(value.sources_id, sources_id);
    assert!(!evaluation.input_artifacts.contains(&sources_id));
    let error = export(&chain).unwrap_err();
    assert!(
        error.contains("does not judge this exact report"),
        "{error}"
    );
    // The recorded empty set may go unbound at the verifier, whether the Task bound it or not.
    for captured in [Captured::EmptyBound, Captured::Unbound] {
        let chain = report_chain_with(
            true,
            Some(ReceiptOutcomeV1::Passed),
            Break::Nothing,
            captured,
        );
        assert_eq!(
            chain.task.inputs.contains_key("sources"),
            captured == Captured::EmptyBound
        );
        let (evaluation, sources_id) = evaluation_and_sources(&chain);
        assert!(!evaluation.input_artifacts.contains(&sources_id));
        let markdown = export(&chain).unwrap();
        let leaves = leaves(&markdown, &[]);
        for leaf in [
            p(
                "Result: accepted. The report passed its automated checks and its independent review.",
            ),
            p("Independent review: passed."),
            li("crates/af/src/cli.rs:1163"),
        ] {
            assert!(
                leaves.contains(&leaf),
                "{captured:?}: {leaf:?} in\n{markdown}"
            );
        }
        assert!(!markdown.contains("## Sources"), "{markdown}");
    }
}

#[test]
fn a_verified_document_shows_its_review_and_every_broken_link_refuses() {
    let markdown = export(&document_chain(
        Some(ReceiptOutcomeV1::Passed),
        Break::Nothing,
    ))
    .unwrap();
    let spec = "https://spec.commonmark.org/0.31.2/";
    let leaves = leaves(&markdown, &[spec]);
    for leaf in [
        ("h1".to_string(), "Migration guide".to_string()),
        ("h2".into(), "Steps".into()),
        p("Export with --format human-markdown (see docs/task-execution/report.md)."),
        li("step one"),
        li("step two"),
        p("Result: accepted. The report passed its automated checks and its independent review."),
        p("Independent review: passed."),
        p("The steps are complete."),
    ] {
        assert!(leaves.contains(&leaf), "{leaf:?} in\n{markdown}");
    }
    assert!(
        about(&leaves).ends_with("The automated checks and the independent reviewer judged the canonical report, which af task output report-task --port document --format markdown writes, not this layout."),
        "{markdown}"
    );
    assert!(!markdown.contains("sha256:"), "{markdown}");
    let unreviewed = export(&document_chain(None, Break::Nothing)).unwrap();
    assert!(
        unreviewed.contains("No independent review was recorded."),
        "{unreviewed}"
    );
    assert!(!unreviewed.contains("reviewer judged"), "{unreviewed}");
    for broken in BREAKS {
        if broken == Break::Manifest {
            continue;
        }
        let error = export(&document_chain(Some(ReceiptOutcomeV1::Passed), broken)).unwrap_err();
        assert!(
            error.contains("does not judge this exact report"),
            "{broken:?}: {error}"
        );
    }
}

#[test]
fn a_document_that_does_not_render_from_its_draft_or_another_type_is_refused() {
    let chain = report_chain(true, Some(ReceiptOutcomeV1::Passed), Break::Nothing);
    let original = chain.cas.get_artifact(&chain.document_id).unwrap();
    let mut document: DocumentV1 = serde_json::from_value(original.payload.clone()).unwrap();
    document.text.push_str("\nAn extra line.\n");
    let snapshot = original.subject_snapshot_id.as_deref();
    let id = put(
        &chain.cas,
        DOCUMENT_V1,
        original.input_artifacts.clone(),
        snapshot,
        &document,
    );
    let mut result = chain.result.clone();
    result
        .outputs
        .insert("report".into(), one(&id, DOCUMENT_V1, snapshot));
    let error = export_result(&chain, &result).unwrap_err();
    assert!(
        error.contains("differs from its draft and sources"),
        "{error}"
    );
    let error =
        human_markdown(&chain.cas, TASK, "verification", &chain.result, &chain.task).unwrap_err();
    assert!(
        error.contains("presents only a Document or report"),
        "{error}"
    );
    assert!(
        error.contains("--format markdown or --format json"),
        "{error}"
    );
}
