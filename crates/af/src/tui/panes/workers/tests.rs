use serde_json::json;

use super::super::pipelines::git;
use super::*;
use crate::config;

#[test]
fn tally_counts_above_zero_are_chips_in_their_tone() {
    let tally = Tally {
        open: 1,
        ok: 2,
        released: 3,
        ..Tally::default()
    };
    let rows = tally_rows(&tally);
    let expected = "attempts  reserved 1  settled ok 2  settled failed 0  released 3";
    assert_eq!(rows[0].text(), expected);
    let spans: Vec<(&str, Paint)> = rows[0]
        .spans
        .iter()
        .map(|span| (span.text.as_str(), span.paint))
        .collect();
    assert_eq!(
        spans,
        [
            ("attempts ", Paint::Plain),
            (" reserved 1 ", Paint::Chip(Tone::Active)),
            (" settled ok 2 ", Paint::Chip(Tone::Ok)),
            (" settled failed 0 ", Paint::Plain),
            (" released 3", Paint::Plain),
        ]
    );
}

fn commit_all(root: &Path, message: &str) {
    if !root.join(".git").exists() {
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.name", "Fixture"],
            vec!["config", "user.email", "fixture@example.invalid"],
        ] {
            git(root, &args).unwrap();
        }
    }
    git(root, &["add", "-A"]).unwrap();
    git(root, &["commit", "-qm", message]).unwrap();
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn worker(root: &Path, dir: &str, name: &str) {
    let text = format!(
        "schema = \"af.worker/1\"\nname = \"{name}\"\nversion = \"1.2.0\"\n\n[signature]\n\
         effects = [\"read-source\", \"execute-checks\"]\nroles = [\"review\"]\n\n\
         [signature.attempt]\ntokens = 1500\nwall_ms = 60000\n\n[runner]\nkind = \"model\"\n\
         provider_kind = \"claude\"\nmodel = \"claude-opus-5-5\"\neffort = \"high\"\n"
    );
    write(root, &format!("{dir}/worker.toml"), &text);
}

fn reviewer(root: &Path, dir: &str, name: &str) {
    let text = format!(
        "name = \"{name}\"\nversion = \"1.0.0\"\nsubjects = [\"diff\"]\n\n[runner]\n\
         program = \"codex\"\nargs = [{{ value = \"--model\" }}, {{ value = \"gpt-6-sol\" }}, \
         {{ value = \"-c\" }}, {{ value = \"model_reasoning_effort=\\\"high\\\"\" }}]\n"
    );
    write(root, &format!(".af/workers/{dir}/reviewer.toml"), &text);
}

fn scope(root: &Path) -> Scope {
    Scope::project(root.to_path_buf(), config::load(Some(root)).unwrap())
}

fn texts(rows: &[Row]) -> Vec<String> {
    rows.iter().map(Row::text).collect()
}

fn ids(pane: &WorkersPane) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for item in pane.items() {
        match item.children {
            Some(children) => {
                for child in children {
                    found.push((item.id.clone(), child.id));
                }
            }
            None => found.push((String::new(), item.id)),
        }
    }
    found
}

#[test]
fn every_committed_worker_is_found_however_nested_and_grouped_by_source() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    reviewer(root, "bugs", "bugs");
    // A reviewer is one directory under `.af/workers/`; anything deeper is not one.
    reviewer(root, "nested/deeper", "deeper");
    worker(
        root,
        ".af/task-packages/kernel/implementer",
        "kernel/implementer",
    );
    worker(root, ".af/task-packages/a/b/c/d/e/f", "deep/f");
    worker(root, ".af/packages/p", "p");
    worker(root, ".af/vendor/v/w", "v/w");
    // A worker.toml outside the declared directories, and a pipeline package, are not Workers.
    worker(root, ".af/elsewhere/x", "x");
    write(
        root,
        ".af/task-packages/pipe/pipeline.toml",
        "name = \"pipe\"\n",
    );
    commit_all(root, "workers");
    let found = discover(root).unwrap();
    assert_eq!(found.commit.len(), 40);
    let listed: Vec<(&str, &str)> = found
        .entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry.label.as_str()))
        .collect();
    assert_eq!(
        listed,
        [
            (".af/workers/bugs/reviewer.toml", "bugs"),
            (".af/task-packages/a/b/c/d/e/f/worker.toml", "deep/f"),
            (
                ".af/task-packages/kernel/implementer/worker.toml",
                "kernel/implementer"
            ),
            (".af/packages/p/worker.toml", "p"),
            (".af/vendor/v/w/worker.toml", "v/w"),
        ]
    );
    // More than one source has Workers: each is a group named by its directory.
    let mut pane = WorkersPane::default();
    pane.load(&scope(root)).unwrap();
    assert!(
        pane.items().is_empty(),
        "nothing is read before the pane opens"
    );
    pane.open(None);
    let groups: Vec<String> = pane.items().into_iter().map(|item| item.label).collect();
    assert_eq!(
        groups,
        [
            "workers/ (1)",
            "task-packages/ (2)",
            "packages/ (1)",
            "vendor/ (1)"
        ]
    );
    assert_eq!(
        ids(&pane)[0],
        (
            ".af/workers/".into(),
            ".af/workers/bugs/reviewer.toml".into()
        )
    );
    // One source only: the entries are listed flat, with no group.
    let temp = tempfile::tempdir().unwrap();
    let flat = temp.path();
    worker(flat, ".af/task-packages/one", "one");
    worker(flat, ".af/task-packages/two", "two");
    commit_all(flat, "flat");
    let mut pane = WorkersPane::default();
    pane.refresh(&scope(flat)).unwrap();
    let items = pane.items();
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|item| item.children.is_none()));
    assert_eq!(items[0].label, "one");
}

#[test]
fn a_working_tree_that_differs_from_head_is_marked_and_head_is_what_is_shown() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    worker(root, ".af/task-packages/w", "fixture/w");
    write(
        root,
        ".af/task-packages/w/instructions.md",
        "Committed prompt.\n",
    );
    commit_all(root, "worker");
    let mut pane = WorkersPane::default();
    pane.load(&scope(root)).unwrap();
    pane.open(Some(".af/task-packages/w/worker.toml"));
    assert_eq!(pane.items()[0].label, "fixture/w");
    let rows = texts(pane.rows());
    assert_eq!(rows[0], "WORKER  fixture/w");
    // The prompt drifts: the bar marks it, the pane says which file, and still shows HEAD.
    write(
        root,
        ".af/task-packages/w/instructions.md",
        "Edited prompt.\n",
    );
    pane.refresh(&scope(root)).unwrap();
    let item = &pane.items()[0];
    assert_eq!(item.label, "fixture/w *");
    assert!(item.muted);
    let rows = texts(pane.rows());
    assert!(
        rows[0].starts_with("working tree differs from HEAD in instructions.md:"),
        "{rows:#?}"
    );
    assert!(rows.contains(&"Committed prompt.".to_owned()), "{rows:#?}");
    // So does the declaration, and an untracked Worker beside it is not listed at all.
    let file = root.join(".af/task-packages/w/worker.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text.replace("fixture/w", "dirty/w")).unwrap();
    worker(root, ".af/task-packages/untracked", "untracked");
    pane.refresh(&scope(root)).unwrap();
    assert_eq!(pane.items().len(), 1);
    let rows = texts(pane.rows());
    assert!(
        rows[0].contains("in worker.toml, instructions.md:"),
        "{rows:#?}"
    );
    assert_eq!(rows[1], "WORKER  fixture/w");
}

#[test]
fn a_prompt_only_the_working_tree_has_is_drift() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    worker(root, ".af/task-packages/w", "fixture/w");
    commit_all(root, "a worker without its prompt");
    let mut pane = WorkersPane::default();
    pane.load(&scope(root)).unwrap();
    pane.open(Some(".af/task-packages/w/worker.toml"));
    assert_eq!(pane.items()[0].label, "fixture/w");
    // An untracked prompt: `git diff` does not see it, the pane does.
    let prompt = ".af/task-packages/w/instructions.md";
    write(root, prompt, "Not committed yet.\n");
    pane.open(Some(".af/task-packages/w/worker.toml"));
    assert_eq!(pane.items()[0].label, "fixture/w *");
    let rows = texts(pane.rows());
    assert!(
        rows[0].starts_with("working tree differs from HEAD in instructions.md:"),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.starts_with("This package commits no instructions.md")),
        "HEAD is still what is shown: {rows:#?}"
    );
    // A link counts, even one to nothing.
    std::fs::remove_file(root.join(prompt)).unwrap();
    std::os::unix::fs::symlink("missing.md", root.join(prompt)).unwrap();
    pane.open(Some(".af/task-packages/w/worker.toml"));
    assert_eq!(pane.items()[0].label, "fixture/w *");
    std::fs::remove_file(root.join(prompt)).unwrap();
    pane.open(Some(".af/task-packages/w/worker.toml"));
    assert_eq!(pane.items()[0].label, "fixture/w");
}

#[test]
fn a_failed_read_of_head_keeps_the_last_entries_under_the_error() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    worker(root, ".af/task-packages/w", "fixture/w");
    commit_all(root, "worker");
    let mut pane = WorkersPane::default();
    pane.load(&scope(root)).unwrap();
    pane.open(Some(".af/task-packages/w/worker.toml"));
    // HEAD now names a commit the repository does not have.
    let head_ref = String::from_utf8(git(root, &["symbolic-ref", "HEAD"]).unwrap()).unwrap();
    std::fs::write(
        root.join(".git").join(head_ref.trim()),
        "0123456789abcdef0123456789abcdef01234567\n",
    )
    .unwrap();
    pane.refresh(&scope(root)).unwrap();
    assert!(pane.error.is_some());
    assert_eq!(pane.entries.len(), 1, "the last good entries stay");
    assert!(pane.items()[0].muted);
    let rows = texts(pane.rows());
    assert!(
        rows[0].starts_with(" error  HEAD could not be read; shown from the last successful read"),
        "{rows:#?}"
    );
    pane.open(None);
    let rows = texts(pane.rows());
    assert!(
        rows[2].starts_with(" error  HEAD could not be read; the entries below"),
        "{rows:#?}"
    );
    // The kept entries still follow the working tree: drift is read against their commit.
    assert_eq!(pane.items()[0].label, "fixture/w");
    let declaration = root.join(".af/task-packages/w/worker.toml");
    let text = std::fs::read_to_string(&declaration).unwrap();
    std::fs::write(&declaration, format!("{text}# edited\n")).unwrap();
    write(root, ".af/task-packages/w/instructions.md", "Untracked.\n");
    pane.refresh(&scope(root)).unwrap();
    assert!(pane.error.is_some());
    assert_eq!(pane.items()[0].label, "fixture/w *");
    assert_eq!(
        pane.entries[0].drifted,
        [
            ".af/task-packages/w/worker.toml",
            ".af/task-packages/w/instructions.md"
        ]
    );
    // An unborn HEAD lists nothing, and is no error.
    let unborn = temp.path().join("unborn");
    std::fs::create_dir_all(&unborn).unwrap();
    git(&unborn, &["init", "-q", "-b", "main"]).unwrap();
    let found = discover(&unborn).unwrap();
    assert!(found.commit.is_empty() && found.entries.is_empty());
    // A directory that is no repository at all is an error, never an empty list.
    let plain = temp.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert!(discover(&plain).is_err());
}

#[test]
fn the_pin_is_the_committed_lock_entry_at_this_package() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    worker(root, ".af/task-packages/pinned", "fixture/p");
    worker(root, ".af/vendor/shadow", "fixture/p");
    worker(root, ".af/packages/loose", "fixture/loose");
    write(
        root,
        CATALOG,
        "schema = \"af.task-catalog/2\"\n[packages.\"fixture/p\"]\nversion = \"1.2.0\"\n\
         digest = \"sha256:2cb281039434ea0273af7390db032a2a823ece9acfad4243b6095d8f0c2f2fb1\"\n\
         path = \".af/task-packages/pinned/\"\n",
    );
    reviewer(root, "bugs", "bugs");
    reviewer(root, "renamed", "correctness");
    reviewer(root, "free", "free");
    write(
        root,
        LOCK,
        "version = 1\n[workers.bugs]\nversion = \"1.0.0\"\ndigest = \"sha256:628f116e7e\"\n\
         [workers.correctness]\nversion = \"1.0.0\"\ndigest = \"sha256:5e8328803396\"\n",
    );
    commit_all(root, "pins");
    let entries = discover(root).unwrap().entries;
    let pin = |id: &str| {
        let entry = entries.iter().find(|entry| entry.id == id).unwrap();
        (entry.pin.clone(), pin_text(entry))
    };
    let here = Pin::Here {
        version: Some("1.2.0".into()),
        digest: Some(
            "sha256:2cb281039434ea0273af7390db032a2a823ece9acfad4243b6095d8f0c2f2fb1".into(),
        ),
    };
    assert_eq!(
        pin(".af/task-packages/pinned/worker.toml"),
        (here, ".af/task-catalog.toml  1.2.0  2cb28103".into())
    );
    assert_eq!(
        pin(".af/vendor/shadow/worker.toml").1,
        "pinned elsewhere: .af/task-catalog.toml pins fixture/p at .af/task-packages/pinned"
    );
    assert_eq!(
        pin(".af/packages/loose/worker.toml"),
        (Pin::Unpinned, "unpinned".into())
    );
    assert_eq!(
        pin(".af/workers/bugs/reviewer.toml").1,
        ".af/af.lock  1.0.0  628f116e"
    );
    // `af.lock` pins a reviewer by name at `.af/workers/<name>`: a directory of another name
    // is not the package the lock pins.
    assert_eq!(
        pin(".af/workers/renamed/reviewer.toml").1,
        "pinned elsewhere: .af/af.lock pins correctness at .af/workers/correctness"
    );
    assert_eq!(pin(".af/workers/free/reviewer.toml").0, Pin::Unpinned);
}

#[test]
fn identity_shows_only_what_the_declaration_carries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    worker(root, ".af/task-packages/model", "kernel/model");
    write(
        root,
        ".af/task-packages/command/worker.toml",
        "schema = \"af.worker/1\"\nname = \"fixture/command\"\n[runner]\nkind = \"command\"\n\
         [runner.command]\nprogram = \"/usr/bin/python3\"\n[[runner.command.args]]\n\
         value = \"-B\"\nprovenance = \"literal\"\n[[runner.command.args]]\n\
         value = \"@package/worker.py\"\nprovenance = \"literal\"\n",
    );
    reviewer(root, "bugs", "bugs");
    commit_all(root, "declarations");
    let mut pane = WorkersPane::default();
    pane.load(&scope(root)).unwrap();
    let identity = |pane: &mut WorkersPane, id: &str| {
        pane.open(Some(id));
        let rows = texts(pane.rows());
        let start = rows
            .iter()
            .position(|row| row.starts_with("IDENTITY"))
            .unwrap();
        rows[start..start + 11].to_vec()
    };
    assert_eq!(
        identity(&mut pane, ".af/task-packages/model/worker.toml"),
        [
            "IDENTITY  worker.toml at HEAD",
            "name      kernel/model",
            "version   1.2.0",
            "schema    af.worker/1",
            "path      .af/task-packages/model",
            "pin       unpinned",
            "runner    kind model  provider claude  model claude-opus-5-5  effort high",
            "args      -",
            "attempt   tokens 1500  wall_ms 60000",
            "effects   read-source, execute-checks",
            "roles     review",
        ]
    );
    assert_eq!(
        identity(&mut pane, ".af/task-packages/command/worker.toml")[2..],
        [
            "version   -",
            "schema    af.worker/1",
            "path      .af/task-packages/command",
            "pin       unpinned",
            "runner    kind command  program /usr/bin/python3  model -  effort -",
            "args      -B @package/worker.py",
            "attempt   tokens -  wall_ms -",
            "effects   -",
            "roles     -",
        ]
    );
    // A reviewer declares its model and effort in its args; they are read as the kernel reads
    // them, and the args are shown as declared.
    let rows = identity(&mut pane, ".af/workers/bugs/reviewer.toml");
    assert_eq!(rows[0], "IDENTITY  reviewer.toml at HEAD");
    assert_eq!(rows[3], "schema    -");
    assert_eq!(
        rows[6],
        "runner    kind -  program codex  model gpt-6-sol  effort high"
    );
    assert_eq!(
        rows[7],
        "args      --model gpt-6-sol -c 'model_reasoning_effort=\"high\"'"
    );
}

#[test]
fn the_prompt_is_the_file_the_kernel_sends_and_nothing_else() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    reviewer(root, "bugs", "bugs");
    write(
        root,
        ".af/workers/bugs/reviewer.md",
        "Find bugs.\n\tIndented.\n",
    );
    worker(root, ".af/task-packages/task", "fixture/task");
    write(
        root,
        ".af/task-packages/task/instructions.md",
        "Implement it.\n",
    );
    // Neither the reviewer file nor a lone Markdown file stands in for instructions.md.
    worker(root, ".af/task-packages/bare", "fixture/bare");
    write(
        root,
        ".af/task-packages/bare/reviewer.md",
        "Not a Task prompt.\n",
    );
    write(
        root,
        ".af/task-packages/bare/prompt.md",
        "Not a prompt either.\n",
    );
    commit_all(root, "prompts");
    assert_eq!(Kind::Reviewer.prompt(), "reviewer.md");
    assert_eq!(Kind::Task.prompt(), "instructions.md");
    let mut pane = WorkersPane::default();
    pane.load(&scope(root)).unwrap();
    let prompt = |pane: &mut WorkersPane, id: &str| {
        pane.open(Some(id));
        texts(&pane.rows()[pane.prompt_row..])
    };
    assert_eq!(
        prompt(&mut pane, ".af/workers/bugs/reviewer.toml"),
        ["PROMPT  reviewer.md at HEAD", "Find bugs.", " Indented."]
    );
    // `gf` below the PROMPT rule opens the prompt's working-tree file; above it, the declaration.
    let prompt_file = root.join(".af/workers/bugs/reviewer.md");
    assert_eq!(pane.file(pane.prompt_row), Some(prompt_file.clone()));
    let declaration = root.join(".af/workers/bugs/reviewer.toml");
    assert_eq!(pane.file(0), Some(declaration));
    assert_eq!(
        pane.entry_file(".af/workers/bugs/reviewer.toml"),
        Some(prompt_file)
    );
    assert_eq!(
        prompt(&mut pane, ".af/task-packages/task/worker.toml"),
        ["PROMPT  instructions.md at HEAD", "Implement it."]
    );
    let bare = prompt(&mut pane, ".af/task-packages/bare/worker.toml");
    assert_eq!(bare[0], "PROMPT  instructions.md at HEAD");
    assert!(
        bare[1].starts_with("This package commits no instructions.md"),
        "{bare:#?}"
    );
    assert_eq!(
        bare[2],
        "The kernel sends a Task Worker that file; nothing else is guessed."
    );
    assert_eq!(bare.len(), 3);
    // `gf` at or below the PROMPT rule opens the path the kernel would send, committed or not;
    // the bar still opens the declaration of a package that commits no prompt.
    let missing = root.join(".af/task-packages/bare/instructions.md");
    assert_eq!(pane.file(pane.prompt_row), Some(missing.clone()));
    assert_eq!(pane.file(pane.rows().len() - 1), Some(missing));
    let declaration = root.join(".af/task-packages/bare/worker.toml");
    assert_eq!(pane.file(pane.prompt_row - 1), Some(declaration.clone()));
    assert_eq!(
        pane.entry_file(".af/task-packages/bare/worker.toml"),
        Some(declaration)
    );
    assert_eq!(pane.yank(0).as_deref(), Some("fixture/bare"));
}

#[test]
fn the_user_scope_lists_no_workers_and_says_they_belong_to_a_project() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path();
    let mut scope = Scope::user().unwrap();
    scope.home = Some(home.to_path_buf());
    scope.state = Some(home.join("state"));
    let mut pane = WorkersPane::default();
    pane.load(&scope).unwrap();
    pane.open(None);
    assert!(pane.items().is_empty());
    let rows = texts(pane.rows());
    assert!(
        rows.contains(&"Workers belong to a project; :cd into a repository.".to_owned()),
        "{rows:#?}"
    );
}

/// A compiled graph whose `implement` node runs the slot bound to `fixture/implementer`, and
/// whose `implementer` node, named like that Worker, runs the evaluator's slot.
fn graph() -> Json {
    json!({
        "nodes": {
            "root.nodes.implement": {"operator": {"kind": "primitive",
                "operator": {"op": "worker", "slot": "root.slots.writer"}}},
            "root.nodes.implementer": {"operator": {"kind": "primitive",
                "operator": {"op": "verify", "slot": "root.slots.judge"}}},
            "root.nodes.review": {"operator": {"kind": "review_domain", "review_node": "r",
                "operation": {"kind": "reviewer", "slot": "root.slots.bugs"}}},
            "root.nodes.admit": {"operator": {"kind": "provider_admission",
                "bindings": ["root.slots.writer", "root.slots.judge"]}},
            "root.nodes.check": {"operator": {"kind": "primitive",
                "operator": {"op": "check", "checks": ["unit"]}}}
        },
        "slots": {
            "root.slots.writer": {"worker": "fixture/implementer"},
            "root.slots.judge": {"worker": "fixture/evaluator"},
            "root.slots.bugs": {"worker": "bugs"}
        }
    })
}

fn task(name: &str) -> Worker {
    (Kind::Task, name.to_owned(), None)
}

/// A recorded artifact reader for plans whose bindings name no package artifact.
fn no_package(id: &str) -> Result<Json, String> {
    Err(format!("no package {id}"))
}

/// A recorded plan of `graph` whose bindings record no package digest.
fn plan(graph: Json) -> Json {
    recorded_plan(&graph, &Json::Null)
}

fn record(kind: &str, attempt: &str, extra: Json) -> Json {
    let mut record = json!({"kind": kind, "attempt_id": attempt});
    for (key, value) in extra.as_object().unwrap() {
        record[key] = value.clone();
    }
    json!({"record": record})
}

fn reserved(attempt: &str, invocation: &str) -> Json {
    record("reserved", attempt, json!({"invocation_id": invocation}))
}

fn settled(attempt: &str, charged: &str, result: &str) -> Json {
    let extra = json!({"charged_tokens": charged, "result": {"kind": result}});
    record("settled", attempt, extra)
}

#[test]
fn attempts_are_attributed_through_the_recorded_plan_never_by_name() {
    assert_eq!(
        worker_of(&plan(graph()), "root.nodes.implement"),
        Some(task("fixture/implementer"))
    );
    assert_eq!(
        worker_of(&plan(graph()), "root.nodes.implementer"),
        Some(task("fixture/evaluator"))
    );
    assert_eq!(
        worker_of(&plan(graph()), "root.nodes.review"),
        Some((Kind::Reviewer, "bugs".to_owned(), None))
    );
    // A Provider admission serves several slots, and a check none: no one Worker ran either.
    assert_eq!(worker_of(&plan(graph()), "root.nodes.admit"), None);
    assert_eq!(worker_of(&plan(graph()), "root.nodes.check"), None);
    assert_eq!(worker_of(&plan(graph()), "root.nodes.missing"), None);
    // The earlier plan bound the same node to another Worker: its Attempt is that Worker's.
    let mut earlier = graph();
    earlier["slots"]["root.slots.writer"]["worker"] = json!("fixture/old-writer");
    let document = json!({
        "plan_id": "plan-now",
        "graph": graph(),
        "execution_records": [
            reserved("a1", "inv-implement"),
            settled("a1", "10", "succeeded"),
            reserved("a2", "inv-implementer"),
            settled("a2", "4", "succeeded"),
            reserved("a3", "inv-earlier"),
            settled("a3", "7", "failed"),
            reserved("a4", "inv-admit"),
            settled("a4", "1", "succeeded"),
        ],
        "attempt_walls": []
    });
    let mut invoked = |id: &str| {
        let (node, plan) = match id {
            "inv-implement" => ("root.nodes.implement", "plan-now"),
            "inv-implementer" => ("root.nodes.implementer", "plan-now"),
            "inv-earlier" => ("root.nodes.implement", "plan-before"),
            "inv-admit" => ("root.nodes.admit", "plan-now"),
            other => return Err(format!("no invocation {other}")),
        };
        Ok(Invoked {
            node: node.to_owned(),
            plan_id: Some(plan.to_owned()),
        })
    };
    let mut asked = Vec::new();
    let mut compiled = |id: &str| {
        asked.push(id.to_owned());
        match id {
            "plan-before" => Ok(plan(earlier.clone())),
            other => Err(format!("no plan {other}")),
        }
    };
    let tallies = tally(&document, &mut invoked, &mut compiled, &mut no_package).unwrap();
    assert_eq!(
        asked,
        ["plan-before"],
        "the current plan's graph is the document's"
    );
    let workers: Vec<&str> = tallies.keys().map(|(_, name, _)| name.as_str()).collect();
    assert_eq!(
        workers,
        [
            "fixture/evaluator",
            "fixture/implementer",
            "fixture/old-writer"
        ]
    );
    assert_eq!(tallies[&task("fixture/implementer")].ok, 1);
    assert_eq!(tallies[&task("fixture/implementer")].tokens, 10);
    assert_eq!(tallies[&task("fixture/evaluator")].tokens, 4);
    assert_eq!(tallies[&task("fixture/old-writer")].failed, 1);
}

/// ADR-0143: an Attempt settled with unknown usage is counted as unknown beside the charge,
/// never as spend, unless a later observation charged it.
#[test]
fn unknown_usage_reads_beside_the_charge_until_an_observation_charges_it() {
    let unknown = |attempt: &str| {
        let mut entry = settled(attempt, "0", "failed");
        entry["record"]["unknown_usage"] = json!({"cause": "capacity"});
        entry
    };
    let document = json!({
        "plan_id": "plan",
        "graph": graph(),
        "execution_records": [
            reserved("known", "inv"),
            settled("known", "120", "succeeded"),
            reserved("capacity", "inv"),
            unknown("capacity"),
            reserved("late", "inv"),
            unknown("late"),
            record("usage_observed", "late", json!({"charged_tokens": "3"})),
        ],
        "attempt_walls": []
    });
    let mut invoked = |_: &str| {
        Ok(Invoked {
            node: "root.nodes.implement".to_owned(),
            plan_id: Some("plan".to_owned()),
        })
    };
    let mut compiled = |plan: &str| Err(format!("no plan {plan}"));
    let tallies = tally(&document, &mut invoked, &mut compiled, &mut no_package).unwrap();
    let tally = tallies[&task("fixture/implementer")];
    assert_eq!((tally.tokens, tally.unknown), (123, 1));
    assert_eq!(
        texts(&tally_rows(&tally))[1],
        "tokens    charged 123 (+1 unknown)"
    );
}

#[test]
fn the_accounting_takes_each_attempts_highest_charge_and_skips_released_reservations() {
    let wall = |attempt: &str, ms: u64| json!({"attempt_id": attempt, "elapsed_ms": ms});
    let observed = |attempt: &str, charged: &str| {
        record(
            "usage_observed",
            attempt,
            json!({"charged_tokens": charged}),
        )
    };
    let document = json!({
        "plan_id": "plan",
        "graph": graph(),
        "execution_records": [
            // Settled ok; a usage observation after the settlement raised the charge.
            reserved("ok", "inv"),
            settled("ok", "100", "succeeded"),
            observed("ok", "150"),
            // Settled failed; an earlier observation was higher than the settlement.
            reserved("failed", "inv"),
            observed("failed", "90"),
            settled("failed", "40", "failed"),
            // Released before dispatch: no Attempt, no charge, no wall.
            reserved("released", "inv"),
            record("released", "released", json!({})),
            // Still open: its observed charge counts.
            reserved("open", "inv"),
            observed("open", "5"),
            // A record of an Attempt no reservation names is no Attempt of this Worker.
            settled("stranger", "1000", "succeeded"),
        ],
        "attempt_walls": [
            wall("ok", 1_200),
            wall("failed", 300),
            wall("released", 9_999),
            wall("stranger", 9_999)
        ]
    });
    let mut invoked = |_: &str| {
        Ok(Invoked {
            node: "root.nodes.implement".to_owned(),
            plan_id: Some("plan".to_owned()),
        })
    };
    let mut compiled = |plan: &str| Err(format!("no plan {plan}"));
    let tallies = tally(&document, &mut invoked, &mut compiled, &mut no_package).unwrap();
    let tally = tallies[&task("fixture/implementer")];
    assert_eq!(
        tally,
        Tally {
            open: 1,
            ok: 1,
            failed: 1,
            released: 1,
            tokens: 150 + 90 + 5,
            wall_ms: 1_500,
            walls: 2,
            unknown: 0,
        }
    );
    assert_eq!(tally.attempts(), 3);
    let rows = texts(&tally_rows(&tally));
    assert_eq!(
        rows,
        [
            "attempts  reserved 1  settled ok 1  settled failed 1  released 1",
            "tokens    charged 245",
            "wall      1.5s  (2 of 3 Attempts recorded a wall)",
        ]
    );
    let none = texts(&tally_rows(&Tally {
        ok: 1,
        ..Tally::default()
    }));
    assert_eq!(none[2], "wall      -  (no Attempt recorded a wall)");
    // A charge that is not a decimal refuses the Task, as the Tasks pane refuses it.
    let mut broken = document.clone();
    broken["execution_records"][1]["record"]["charged_tokens"] = json!("ten");
    assert!(super::tally(&broken, &mut invoked, &mut compiled, &mut no_package).is_err());
}

#[test]
fn an_owned_shard_runs_the_operator_its_owner_registered() {
    let mut graph = graph();
    graph["owned_children"] = json!({"root.nodes.review": {"operator": {"kind": "review_domain",
        "review_node": "r", "operation": {"kind": "reviewer", "slot": "root.slots.bugs"}}}});
    let document = json!({
        "plan_id": "plan",
        "graph": graph,
        "execution_records": [
            reserved("s1", "inv-slice1"),
            settled("s1", "30", "succeeded"),
            reserved("s2", "inv-slice2"),
            settled("s2", "12", "failed"),
            // A child no set registers is no shard: its node is not in the graph.
            reserved("x", "inv-stray"),
            settled("x", "99", "succeeded"),
        ],
        "owned_child_sets": [{"record": {"plan_id": "plan", "parent_invocation_id": "inv-review",
            "children": [
                {"node": "root.nodes.review.slice1", "invocation_id": "inv-slice1"},
                {"node": "root.nodes.review.slice2", "invocation_id": "inv-slice2"}
            ]}}],
        "attempt_walls": [{"attempt_id": "s1", "elapsed_ms": 2_000}]
    });
    let mut invoked = |id: &str| {
        let node = match id {
            "inv-review" => "root.nodes.review",
            "inv-slice1" => "root.nodes.review.slice1",
            "inv-slice2" => "root.nodes.review.slice2",
            "inv-stray" => "root.nodes.review.slice9",
            other => return Err(format!("no invocation {other}")),
        };
        Ok(Invoked {
            node: node.to_owned(),
            plan_id: Some("plan".to_owned()),
        })
    };
    let mut compiled = |plan: &str| Err(format!("no plan {plan}"));
    let tallies = tally(&document, &mut invoked, &mut compiled, &mut no_package).unwrap();
    let bugs = (Kind::Reviewer, "bugs".to_owned(), None);
    assert_eq!(tallies.keys().collect::<Vec<_>>(), [&bugs]);
    assert_eq!(
        tallies[&bugs],
        Tally {
            ok: 1,
            failed: 1,
            tokens: 42,
            wall_ms: 2_000,
            walls: 1,
            ..Tally::default()
        }
    );
}

#[test]
fn a_reviewer_and_a_task_package_sharing_a_name_keep_their_own_attempts() {
    let mut graph = graph();
    graph["slots"]["root.slots.writer"]["worker"] = json!("bugs");
    let document = json!({
        "plan_id": "plan",
        "graph": graph,
        "execution_records": [
            reserved("w", "inv-implement"),
            settled("w", "10", "succeeded"),
            reserved("r", "inv-review"),
            settled("r", "3", "failed"),
        ],
        "attempt_walls": []
    });
    let mut invoked = |id: &str| {
        let node = match id {
            "inv-implement" => "root.nodes.implement",
            _ => "root.nodes.review",
        };
        Ok(Invoked {
            node: node.to_owned(),
            plan_id: None,
        })
    };
    let mut compiled = |plan: &str| Err(format!("no plan {plan}"));
    let tallies = tally(&document, &mut invoked, &mut compiled, &mut no_package).unwrap();
    assert_eq!(tallies[&task("bugs")].ok, 1);
    assert_eq!(tallies[&task("bugs")].tokens, 10);
    let review = &tallies[&(Kind::Reviewer, "bugs".to_owned(), None)];
    assert_eq!((review.failed, review.tokens), (1, 3));

    // In the pane, each package shows only its own kind's Attempts.
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    reviewer(root, "bugs", "bugs");
    worker(root, ".af/task-packages/bugs", "bugs");
    write(root, ".af/af.lock", "[workers.bugs]\nversion = \"1.0.0\"\n");
    write(
        root,
        CATALOG,
        "[packages.bugs]\npath = \".af/task-packages/bugs\"\nversion = \"1.2.0\"\n",
    );
    commit_all(root, "two packages named bugs");
    let mut pane = WorkersPane::default();
    pane.load(&scope(root)).unwrap();
    pane.open(None);
    pane.stores = vec![Store {
        shown: "state".to_owned(),
        tallies: Ok(tallies),
    }];
    let state = |pane: &mut WorkersPane, id: &str| {
        pane.selected = Some(id.to_owned());
        pane.rebuild();
        let rows = texts(pane.rows());
        rows.into_iter()
            .find(|row| row.starts_with("tokens"))
            .unwrap()
    };
    assert_eq!(
        state(&mut pane, ".af/workers/bugs/reviewer.toml"),
        "tokens    charged 3"
    );
    assert_eq!(
        state(&mut pane, ".af/task-packages/bugs/worker.toml"),
        "tokens    charged 10"
    );
}

#[test]
fn every_open_reads_the_stores_and_the_working_tree_again() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let (repo, state) = crate::tui::tests::hub_with_tasks(&root);
    let scope = crate::tui::tests::hub_scope(&root, &repo);
    let id = ".af/task-packages/fixture/implementer/worker.toml";
    let mut pane = WorkersPane::default();
    pane.load(&scope).unwrap();
    let attempts = |pane: &mut WorkersPane| {
        pane.open(Some(id));
        let rows = texts(pane.rows());
        rows.into_iter()
            .find(|row| row.starts_with("attempts"))
            .unwrap()
    };
    // Only the second Task ran the digest `HEAD` pins; the first ran the earlier package.
    let before = attempts(&mut pane);
    assert!(before.contains("settled ok 1"), "{before}");
    // Another Attempt settles under the same `HEAD`: reopening counts it.
    crate::tui::tests::record_task(&root, &repo, &state, "pagination-again");
    let after = attempts(&mut pane);
    assert!(after.contains("settled ok 2"), "{before} then {after}");
    // The declaration drifts in the working tree: reopening marks it.
    let declaration = repo.join(id);
    let text = std::fs::read_to_string(&declaration).unwrap();
    std::fs::write(&declaration, format!("{text}\n# edited\n")).unwrap();
    pane.open(Some(id));
    let rows = texts(pane.rows());
    assert!(
        rows[0].starts_with("working tree differs from HEAD in worker.toml"),
        "{rows:#?}"
    );
    assert!(pane.items().iter().any(|item| item.label.ends_with(" *")));
}

#[test]
fn state_counts_only_the_digest_the_committed_pin_records() {
    let bindings = json!({"root.slots.writer": {"package_digest": "sha256:new"}});
    let mut before = graph();
    before["slots"]["root.slots.judge"]["worker"] = json!("unused");
    let document = json!({
        "plan_id": "plan-now",
        "graph": graph(),
        "plan": {"bindings": bindings},
        "execution_records": [
            reserved("new", "inv-now"),
            settled("new", "10", "succeeded"),
            reserved("old", "inv-before"),
            settled("old", "7", "failed"),
        ],
        "attempt_walls": []
    });
    let mut invoked = |id: &str| {
        let plan = if id == "inv-now" {
            "plan-now"
        } else {
            "plan-before"
        };
        Ok(Invoked {
            node: "root.nodes.implement".to_owned(),
            plan_id: Some(plan.to_owned()),
        })
    };
    let old = json!({"root.slots.writer": {"package_digest": "sha256:old"}});
    let mut compiled = |_: &str| Ok(recorded_plan(&before, &old));
    let tallies = tally(&document, &mut invoked, &mut compiled, &mut no_package).unwrap();
    let at = |digest: &str| {
        (
            Kind::Task,
            "fixture/implementer".to_owned(),
            Some(digest.to_owned()),
        )
    };
    assert_eq!(tallies[&at("sha256:new")].ok, 1);
    assert_eq!(tallies[&at("sha256:old")].failed, 1);

    // The pane shows the pinned digest's Attempts, and counts the other digest apart.
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    worker(root, ".af/task-packages/implementer", "fixture/implementer");
    write(
        root,
        CATALOG,
        "[packages.\"fixture/implementer\"]\npath = \".af/task-packages/implementer\"\n\
         version = \"1.2.0\"\ndigest = \"sha256:new\"\n",
    );
    commit_all(root, "pinned at the new digest");
    let mut pane = WorkersPane::default();
    pane.load(&scope(root)).unwrap();
    pane.open(Some(".af/task-packages/implementer/worker.toml"));
    pane.stores = vec![Store {
        shown: "state".to_owned(),
        tallies: Ok(tallies),
    }];
    pane.rebuild();
    let rows = texts(pane.rows());
    let from = rows
        .iter()
        .position(|row| row.starts_with("attempts"))
        .unwrap();
    assert_eq!(
        rows[from],
        "attempts  reserved 0  settled ok 1  settled failed 0  released 0"
    );
    assert_eq!(rows[from + 1], "tokens    charged 10");
    assert_eq!(
        rows[from + 3],
        "other     1 Attempts ran this name at an unpinned digest"
    );
}

#[test]
fn a_declaration_without_a_name_yanks_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(
        root,
        ".af/task-packages/nameless/worker.toml",
        "schema = \"af.worker/1\"\n",
    );
    commit_all(root, "a worker that names nothing");
    let mut pane = WorkersPane::default();
    pane.load(&scope(root)).unwrap();
    let id = ".af/task-packages/nameless/worker.toml";
    pane.open(Some(id));
    // The directory stands in as its label, never as its name.
    assert_eq!(pane.items()[0].label, "nameless");
    assert_eq!(pane.yank(0), None);
    assert_eq!(pane.entry_name(id), None);
}

#[test]
fn a_captured_review_attempt_is_its_reviewers_at_the_package_it_ran() {
    // A captured Review binds the reviewer slot to a Review dependency: its own digest covers
    // Campaign data; the reviewer package it ran is its original package digest.
    let bindings = json!({
        "root.slots.bugs": {"package_digest": "sha256:dependency",
            "package_artifact_id": "sha256:dep-artifact"},
        "root.slots.writer": {"package_digest": "sha256:task-package",
            "package_artifact_id": "sha256:task-artifact"}
    });
    let document = json!({
        "plan_id": "plan",
        "graph": graph(),
        "plan": {"bindings": bindings},
        "execution_records": [
            reserved("r", "inv-review"),
            settled("r", "5", "succeeded"),
            reserved("w", "inv-implement"),
            settled("w", "9", "succeeded"),
        ],
        "attempt_walls": []
    });
    let mut invoked = |id: &str| {
        let node = match id {
            "inv-review" => "root.nodes.review",
            _ => "root.nodes.implement",
        };
        Ok(Invoked {
            node: node.to_owned(),
            plan_id: None,
        })
    };
    let mut compiled = |plan: &str| Err(format!("no plan {plan}"));
    let mut package = |id: &str| match id {
        "sha256:dep-artifact" => Ok(json!({"type": "af/LegacyReviewDependency@1",
            "payload": {"name": "bugs", "original_package_digest": "sha256:reviewer"}})),
        "sha256:task-artifact" => Ok(json!({"type": "af/TaskPackage@1",
            "payload": {"digest": "sha256:task-package"}})),
        other => Err(format!("no package {other}")),
    };
    let tallies = tally(&document, &mut invoked, &mut compiled, &mut package).unwrap();
    let reviewer = (
        Kind::Reviewer,
        "bugs".to_owned(),
        Some("sha256:reviewer".to_owned()),
    );
    assert_eq!(tallies[&reviewer].tokens, 5);
    let writer = (
        Kind::Task,
        "fixture/implementer".to_owned(),
        Some("sha256:task-package".to_owned()),
    );
    assert_eq!(tallies[&writer].tokens, 9);
    assert_eq!(tallies.len(), 2);
    // A binding whose package cannot be read refuses the Task, never guesses its digest.
    let mut unreadable = |id: &str| Err(format!("no package {id}"));
    assert!(tally(&document, &mut invoked, &mut compiled, &mut unreadable).is_err());
}

#[test]
fn a_task_worker_may_sit_at_its_source_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    worker(root, ".af/vendor", "fixture/vendored");
    write(
        root,
        CATALOG,
        "[packages.\"fixture/vendored\"]\npath = \".af/vendor\"\nversion = \"1.2.0\"\n",
    );
    commit_all(root, "a package at the vendor root");
    assert_eq!(
        source_of(".af/vendor/worker.toml"),
        Some((".af/vendor/", Kind::Task))
    );
    let mut pane = WorkersPane::default();
    pane.load(&scope(root)).unwrap();
    pane.open(Some(".af/vendor/worker.toml"));
    assert_eq!(pane.items()[0].label, "fixture/vendored");
    let rows = texts(pane.rows());
    assert!(
        rows.contains(&"path      .af/vendor".to_owned()),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.starts_with("pin       .af/task-catalog.toml  1.2.0")),
        "{rows:#?}"
    );
}

#[test]
fn one_git_process_reads_every_committed_file_and_one_checks_their_drift() {
    use super::super::pipelines::{blobs, differing, listed};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    write(root, "a.toml", "a = 1\n");
    write(root, "dir/b.md", "bee\n");
    // A name a pathspec would take as a glob matching `a.toml` too.
    write(root, "star*.toml", "star\n");
    commit_all(root, "files");
    let head = head(root).unwrap();
    let objects = listed(root, &head, ".").unwrap();
    let names: Vec<&str> = objects.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        ["a.toml", "dir/b.md", "star*.toml"],
        "blobs only, no trees"
    );
    let ids: Vec<&str> = objects.values().map(String::as_str).collect();
    let read = blobs(root, &ids).unwrap();
    assert_eq!(read[&objects["a.toml"]], b"a = 1\n");
    assert_eq!(read[&objects["dir/b.md"]], b"bee\n");
    assert!(blobs(root, &[]).unwrap().is_empty());
    // An id the object store cannot give back, or one naming a tree, is a failed read.
    let absent = blobs(root, &["0123456789abcdef0123456789abcdef01234567"]).unwrap_err();
    assert!(absent.contains("cannot read committed object"), "{absent}");
    let tree = String::from_utf8(git(root, &["rev-parse", "HEAD:dir"]).unwrap()).unwrap();
    let tree = blobs(root, &[tree.trim()]).unwrap_err();
    assert!(tree.contains("is not a file"), "{tree}");
    // Only object ids go to git: nothing a caller passes can split the request stream.
    assert!(blobs(root, &["HEAD:a.toml\nHEAD:dir"]).is_err());
    write(root, "star*.toml", "changed\n");
    std::fs::remove_file(root.join("dir/b.md")).unwrap();
    let differ = differing(root, &head, &["a.toml", "dir/b.md", "star*.toml"]);
    let differ: Vec<&str> = differ.iter().map(String::as_str).collect();
    assert_eq!(
        differ,
        ["dir/b.md", "star*.toml"],
        "a literal name, never a glob"
    );
    // A git that cannot answer marks every path.
    let all = differing(
        root,
        "0123456789abcdef0123456789abcdef01234567",
        &["a.toml"],
    );
    assert!(all.contains("a.toml"));
}

/// A committed package directory may hold any byte git allows, a newline included: the read
/// asks for object ids, so such a path lists like any other.
#[test]
fn a_worker_whose_directory_name_holds_a_newline_is_listed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    worker(root, ".af/task-packages/a\nb", "fixture/newline");
    write(
        root,
        ".af/task-packages/a\nb/instructions.md",
        "Across lines.\n",
    );
    worker(root, ".af/task-packages/plain", "fixture/plain");
    commit_all(root, "a newline in a directory name");
    let found = discover(root).unwrap();
    let labels: Vec<&str> = found.entries.iter().map(|e| e.label.as_str()).collect();
    assert_eq!(labels, ["fixture/newline", "fixture/plain"]);
    assert_eq!(found.entries[0].prompt.as_deref(), Some("Across lines.\n"));
    assert!(found.entries.iter().all(|entry| entry.drifted.is_empty()));
}

/// Opening the pane re-reads everything (ADR-0122), so what a read costs must not grow with
/// what it lists: the same git processes for 2 Workers as for 30.
#[test]
fn a_read_spawns_as_many_git_processes_for_thirty_workers_as_for_two() {
    let spawns = |count: usize| {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        for n in 0..count {
            worker(
                &root,
                &format!(".af/task-packages/w{n}"),
                &format!("fixture/w{n}"),
            );
            write(
                &root,
                &format!(".af/task-packages/w{n}/instructions.md"),
                "Go.\n",
            );
        }
        commit_all(&root, "workers");
        let before = super::super::pipelines::spawned::count(&root);
        let found = discover(&root).unwrap();
        assert_eq!(found.entries.len(), count);
        super::super::pipelines::spawned::count(&root) - before
    };
    let (few, many) = (spawns(2), spawns(30));
    assert_eq!(few, many, "git processes for 2 Workers, then for 30");
    assert!(many <= 6, "{many} git processes for one read");
}

/// `git diff` trusts the index flags; the pane does not: a `skip-worktree` or
/// `assume-unchanged` declaration edited in the working tree is still marked, and a flagged
/// file left alone is not.
#[test]
fn an_index_flag_does_not_hide_drift() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    worker(root, ".af/task-packages/skip", "fixture/skip");
    worker(root, ".af/task-packages/assume", "fixture/assume");
    worker(root, ".af/task-packages/quiet", "fixture/quiet");
    commit_all(root, "three workers");
    let skip = ".af/task-packages/skip/worker.toml";
    let assume = ".af/task-packages/assume/worker.toml";
    let quiet = ".af/task-packages/quiet/worker.toml";
    git(root, &["update-index", "--skip-worktree", skip]).unwrap();
    git(root, &["update-index", "--assume-unchanged", assume, quiet]).unwrap();
    for path in [skip, assume] {
        let text = std::fs::read_to_string(root.join(path)).unwrap();
        std::fs::write(root.join(path), format!("{text}# edited\n")).unwrap();
    }
    let found = discover(root).unwrap();
    let drifted = |id: &str| {
        let entry = found.entries.iter().find(|entry| entry.id == id).unwrap();
        entry.drifted.clone()
    };
    assert_eq!(drifted(skip), [skip]);
    assert_eq!(drifted(assume), [assume]);
    assert!(drifted(quiet).is_empty());
    // A flagged file removed from the working tree differs too.
    std::fs::remove_file(root.join(quiet)).unwrap();
    let found = discover(root).unwrap();
    let entry = found
        .entries
        .iter()
        .find(|entry| entry.id == quiet)
        .unwrap();
    assert_eq!(entry.drifted, [quiet]);
}
