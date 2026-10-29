//! Long-form help pages: `af help <topic>`, also rendered as `man af-<topic>`.

/// Every topic: its name, the one-line description `af help` lists, and its page. This table is
/// the one source of topic descriptions: `af help`, `af help <topic>`, the man pages and the
/// browser's `:help` all read it.
pub(crate) const TOPICS: &[(&str, &str, &str)] = &[
    (
        "start",
        "The first run in four steps: Provider, onboard, commit, review",
        START,
    ),
    (
        "concepts",
        "Task, Worker, Attempt, Snapshot and the other words af uses",
        CONCEPTS,
    ),
    ("config", "How af is configured", CONFIG),
    (
        "layers",
        "The configuration ladder and where each file lives",
        LAYERS,
    ),
    ("environment", "Environment variables af reads", ENVIRONMENT),
    ("exit-codes", "What every exit code means", EXIT_CODES),
    ("json", "The --json contract", JSON),
    (
        "errors",
        "Stable error causes and their next steps",
        "Run `af help errors CAUSE` for one explanation.",
    ),
    (
        "providers",
        "Provider logins: the registry, detect, setup, add, status and doctor",
        PROVIDERS,
    ),
    (
        "task-states",
        "The one Task state af task prints: phase, outcome, cause and next command",
        TASK_STATES,
    ),
    (
        "task-file",
        "The Task file: its fields, defaults, context and continuing a reviewed Task",
        TASK_FILE,
    ),
    (
        "agents",
        "Driving af from an agent: plan, run, findings, diff, materialize, deliver, report",
        AGENTS,
    ),
    (
        "self",
        "Installing, updating, pinning, and removing af",
        SELF_TOPIC,
    ),
    (
        "storage",
        "The disk af keeps between runs, and the [storage] table",
        STORAGE,
    ),
    (
        "telemetry",
        "What af records about its own work, the four levels, and the [telemetry] table",
        TELEMETRY,
    ),
];

/// `af help` without a word: every topic once, with its registry description.
pub(crate) fn listing() -> String {
    let width = TOPICS
        .iter()
        .map(|(name, _, _)| name.len())
        .max()
        .unwrap_or(0);
    let mut text = String::from("Topics (`af help TOPIC`):\n");
    for (name, about, _) in TOPICS {
        text.push_str(&format!("  {name:<width$}  {about}\n"));
    }
    text.push_str(
        "\nAny other word is a command path: `af help review run` equals `af review run --help`.",
    );
    text
}

pub(crate) fn find(name: &str) -> Option<&'static (&'static str, &'static str, &'static str)> {
    TOPICS.iter().find(|(topic, _, _)| *topic == name)
}

/// One topic's body. The configuration page ends with the declared `.af/` layout, rendered from
/// `review_config::layout` rather than stored a second time here: that table, the one in
/// the documentation and every classification are the same data.
pub(crate) fn body(topic: &str, text: &str) -> String {
    match topic {
        "config" => format!(
            "{text}\n\nWhat `.af/` holds\n\n{}\n{}\n\n{UNDECLARED}",
            review_config::layout::render_table(),
            review_config::layout::ELSEWHERE,
        ),
        // The tables are rendered from the enum every Task rendering prints, so this page and
        // the binary cannot say different things.
        "task-states" => format!("{text}\n\n{}", task_state_tables()),
        "exit-codes" => format!("{text}\n\n{}", task_exit_codes()),
        "errors" => format!(
            "{text}\n\n{}",
            crate::error_document::explain(None).expect("cause table")
        ),
        _ => text.to_string(),
    }
}

/// The bindings line of the `start` page's review command, and the line under it, as the page
/// is written: what a man page and a machine without a Provider print.
const START_BINDINGS: &str =
    "       --provider correctness=PROVIDER --provider architecture=PROVIDER
   PROVIDER is the ID of a Provider step 1 registered; `af onboard` prints the exact command.";

/// One topic's page as `af help TOPIC` and the browser print it on this machine: [`body`], with
/// the `start` page's review command bound to the Providers this machine's registry holds, as
/// `af onboard` binds them. Man pages print [`body`] alone, which no machine's state shapes.
pub(crate) fn page(topic: &str, text: &str) -> String {
    let body = body(topic, text);
    if topic != "start" {
        return body;
    }
    let registry = crate::providers::read_registry_entries();
    body.replace(START_BINDINGS, &start_bindings(&registry))
}

/// The `schema` of the document `af help TOPIC --json` prints (`schemas/help-topic-v1.json`).
pub(crate) const HELP_TOPIC_SCHEMA: &str = "af/help-topic@1";

/// One page `af help` prints as text: a topic, or `af help errors CAUSE`. The text form and
/// `af help WORDS --json` both render it from here, so the two cannot drift (ADR-0259).
pub(crate) struct HelpPage {
    /// The words after `af help` that name the page: `agents`, `errors CAUSE`.
    name: String,
    /// The description of the title line `af help NAME — DESCRIPTION`; `af help errors CAUSE`
    /// prints no title line.
    description: Option<&'static str>,
    /// The page below its title line and the blank line after it.
    text: String,
}

/// The page `words` name, resolved as the text form resolves them: `errors CAUSE`, then a
/// topic. `None` when they name no page; an error when `errors` is followed by no known cause.
pub(crate) fn help_page(words: &[String]) -> Option<Result<HelpPage, String>> {
    match words {
        [errors, cause] if errors == "errors" => Some(
            crate::error_document::explain(Some(cause)).map(|text| HelpPage {
                name: format!("errors {cause}"),
                description: None,
                text,
            }),
        ),
        [word] => find(word).map(|(topic, about, text)| {
            Ok(HelpPage {
                name: (*topic).to_owned(),
                description: Some(about),
                text: page(topic, text),
            })
        }),
        _ => None,
    }
}

impl HelpPage {
    /// What `af help NAME` prints, without the final newline.
    pub(crate) fn text_form(&self) -> String {
        match self.description {
            Some(about) => format!("af help {} — {about}\n\n{}", self.name, self.text),
            None => self.text.clone(),
        }
    }

    /// What `af help NAME --json` prints: one `af/help-topic@1` document.
    pub(crate) fn document(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": HELP_TOPIC_SCHEMA,
            "name": self.name,
            "description": self.description,
            "text": self.text,
        })
    }
}

/// The `start` page's bindings line for `registry`: the IDs `af onboard` would bind, or the
/// placeholder with the reason none can be named.
fn start_bindings(registry: &crate::providers::RegistryRead) -> String {
    match crate::onboard::first_review_bindings(registry) {
        Some(bindings) => {
            let bound: Vec<String> = bindings
                .iter()
                .map(|(node, id)| format!("--provider {node}={id}"))
                .collect();
            // Within 100 columns: each binding on a line of its own when both are too long.
            let mut line = format!("       {}", bound.join(" "));
            if line.chars().count() > 100 {
                line = format!("       {}", bound.join(" \\\n       "));
            }
            format!(
                "{line}\n   These are the Providers registered on this machine that `af onboard` \
                 binds; it\n   prints the same command."
            )
        }
        None if registry.warning.is_some() => "       \
            --provider correctness=PROVIDER --provider architecture=PROVIDER\n   \
            The Provider registry cannot be read (`af provider status` names why), so PROVIDER\n   \
            stands for a Provider's ID; `af onboard` prints the exact command once it can."
            .to_string(),
        None => "       --provider correctness=PROVIDER --provider architecture=PROVIDER\n   \
            No Provider is registered on this machine yet, so PROVIDER stands for the ID step 1\n   \
            registers; `af onboard` prints the exact command once one is."
            .to_string(),
    }
}

/// The phase, outcome, cause and attention tables of `af help task-states`, from
/// `review_core::task::state`; `docs/tasks.md` carries the same rows.
pub(crate) fn task_state_tables() -> String {
    use review_core::task::state::*;
    let mut out = String::from("Phases\n\n");
    for phase in TaskStatePhaseV1::ALL {
        out.push_str(&format!(
            "  {:<10} {}\n",
            phase.as_str(),
            phase.description()
        ));
    }
    out.push_str("\nOutcomes, with the exit code af task run ends with\n\n");
    for outcome in TaskOutcomeV1::ALL {
        out.push_str(&format!(
            "  {:<18} {}  {}\n",
            outcome.as_str(),
            outcome.exit_code(),
            outcome.description()
        ));
    }
    out.push_str("\nCauses, with the outcome each belongs to\n\n");
    for cause in TaskCauseV1::ALL {
        // A description wider than the page continues under its own column.
        let row = format!(
            "  {:<24} {:<18} {}",
            cause.as_str(),
            cause.outcome().map_or("(retained)", TaskOutcomeV1::as_str),
            cause.description()
        );
        out.push_str(&crate::human::wrap(&row, 100, 46));
        out.push('\n');
    }
    out.push_str("\nAttention groups, as af task list --attention prints them\n\n");
    for group in TaskAttentionV1::ALL {
        let row = format!(
            "  {:<12} {:<27} {}",
            group.as_str(),
            group.title(),
            group.description()
        );
        out.push_str(&crate::human::wrap(&row, 100, 43));
        out.push('\n');
    }
    out.trim_end().to_owned()
}

/// The Task outcome section of `af help exit-codes`.
fn task_exit_codes() -> String {
    use review_core::task::state::TaskOutcomeV1;
    let mut out = String::from(
        "`af task run`, `af task start --execute`, `af task explain` and `af review run --file`\n\
         end with their Task's outcome (`af help task-states`):\n\n",
    );
    let mut codes: Vec<(u8, Vec<&str>)> = Vec::new();
    for outcome in TaskOutcomeV1::ALL {
        match codes
            .iter_mut()
            .find(|(code, _)| *code == outcome.exit_code())
        {
            Some((_, names)) => names.push(outcome.as_str()),
            None => codes.push((outcome.exit_code(), vec![outcome.as_str()])),
        }
    }
    codes.sort();
    for (code, names) in codes {
        let extra = if code == 4 {
            ", or a Task that waits for a person's decision"
        } else {
            ""
        };
        out.push_str(&format!("  {code:<4} {}{extra}\n", names.join(", ")));
    }
    // Hard-wrapped: a page is printed as written, and no line is wider than 100 columns.
    out.push_str(
        "
Where such a code's result goes to stdout alone (under --json, `af task explain`, `af task run`
of a Task that already finished), stderr ends with `Task ID: phase/outcome/cause; next: COMMAND`,
so a non-zero exit always says why there.

`af task wait` and `af task events` end with the same code once the Task settles (4 for a Task
that was cancelled or waits for a person), and with 124 when their --timeout passes first.

`af task show`, `af task list`, `af task report`, `af task status` and `af task findings` exit 0
once they read the Store: the state they print is data, not their outcome.",
    );
    out
}

/// `af help agents` is `docs/agents.md`, byte for byte: the page and the binary cannot say
/// different things (ADR-0211).
const AGENTS: &str = include_str!("../../../docs/agents.md");

const SNIPPET_START: &str = "<!-- af-agent-snippet:start -->\n";
const SNIPPET_END: &str = "<!-- af-agent-snippet:end -->";

/// The repository pointer is extracted from the help page, so help and installed files share
/// exactly one source. The inner markers are also the replacement boundary in AGENTS.md.
pub(crate) fn agent_snippet() -> &'static str {
    AGENTS
        .split_once(SNIPPET_START)
        .expect("agents page has snippet start")
        .1
        .split_once(SNIPPET_END)
        .expect("agents page has snippet end")
        .0
}

/// The version of the skill [`agent_skill`] writes. A change to its bytes takes the next
/// version and moves the bytes it replaces into [`EARLIER_AGENT_SKILLS`] (ADR-0259).
const AGENT_SKILL_VERSION: u32 = 2;

/// The `SKILL.md` both skill directories hold: YAML frontmatter from line 1, which Codex and
/// Claude Code require before they load a skill, then the note that af generated it, then the
/// pointer extracted from the help page.
pub(crate) fn agent_skill() -> String {
    format!(
        "---\nname: af\ndescription: Use when working with af (afactory) in this repository, its \
         Tasks and reviews; start with `af help agents`.\n---\n\n<!-- Generated by `af setup \
         agents` (af skill {AGENT_SKILL_VERSION}); `af setup agents --write` replaces it while \
         its bytes are unchanged. -->\n\n# Afactory\n\n{}\nRun `af help agents` for the full loop.\n",
        agent_snippet()
    )
}

/// Every skill an earlier `af setup agents --write` wrote, byte for byte, by the version it
/// carried. `--write` replaces only these bytes or the current skill's: ownership is the exact
/// bytes, so no marker a foreign file can recompute makes it af's (ADR-0259).
const EARLIER_AGENT_SKILLS: [(u32, &str); 1] = [(
    1,
    "<!-- af-generated-skill:sha256:0c44d5402cbb85cbba220f2b2eb7c174a0b5940c95df3f32796164307e311c25 -->
---
name: af
description: Drive Afactory from a coding agent.
---

# Afactory

<!-- af-agent-help:start -->
For repository work with af, run `af help agents` first.
Read `af help json`, `af help exit-codes`, and `af help task-file` for machine contracts.
Start with `af provider status --json` and `af onboard` to inspect readiness.
Plan a Task with `af task plan --file TASK_FILE --json` before running it.
Follow `state.next_action` in each JSON result; do not guess a next command.
Use `af task findings TASK_ID --json` and `af task diff TASK_ID --json` for evidence.
Never add `--login` inside an agent session; ask the person to log in privately.
<!-- af-agent-help:end -->

Run `af help agents` for the full loop.
",
)];

/// Whether `text` is a skill af generated: the current one or an earlier version, exactly.
pub(crate) fn is_generated_agent_skill(text: &str) -> bool {
    text == agent_skill()
        || EARLIER_AGENT_SKILLS
            .iter()
            .any(|(_, earlier)| *earlier == text)
}

/// What the same table says about a captured Snapshot, and the one knob over it.
const UNDECLARED: &str = "\
`af task plan` and `af task start` classify the Snapshot they captured against that table and
warn about any path under `.af/` no entry names, with the count, the byte total and up to ten
paths; `--json` carries the whole list in `undeclared_af_paths`, and `af task deliver` records it
in the receipt. Nothing is removed: not from the Snapshot, not from the delivered worktree. Set
`[delivery] undeclared_af_paths = \"refuse\"` in `.af/af.toml` to make delivery stop instead,
before it prepares a record or touches git. The value is captured when a Task is planned.";

const START: &str = "\
The first run, in four steps. `af doctor` checks where you are at any point, token-free.

1. Register a Provider: the claude or codex CLI login af's model Workers run under.
     af provider detect --apply
   finds the logins on this machine (~/.claude, ~/.codex, CLAUDE_CONFIG_DIR, CODEX_HOME) and
   registers them; without --apply it only lists them. With no login yet, run this at your own
   terminal, never through an agent: af provider setup claude-main --kind claude --login
   (or codex-main --kind codex). af never reads a credential: the registry holds an ID, a kind
   and a directory. `af provider status` shows what is registered and logged in.

2. Onboard a repository. In a clean git checkout:
     af onboard              preview `.af/`, the review authority (token-free; writes nothing)
     af onboard --apply      write it; the preview prints the exact command to copy
   The preview names the reviewers, the Provider each one needs and the Gate: the repository's
   own acceptance command, such as `make check`. It picks claude or codex Workers from the
   Provider kinds you registered.

3. Review that authority, then commit it:
     git add .af && git diff --cached -- .af && git commit -m 'Configure Afactory'
   Only committed `.af/` is trusted: --policy-rev selects it by revision.

4. Review a change. Make one, then:
     af review plan --policy-rev HEAD --base HEAD --uncommitted        token-free
     af review --policy-rev HEAD --base HEAD --uncommitted \\
       --provider correctness=PROVIDER --provider architecture=PROVIDER
   PROVIDER is the ID of a Provider step 1 registered; `af onboard` prints the exact command.
   The plan shows the change, the route, the Gate and the bindings the run needs. The run executes
   the Gate and the reviewers, spending tokens within the budgets `.af/` sets, and prints the
   verdict and the Findings; `af review report` shows them again.

Then an implementation Task: `af catalog init --profile software --destination task-demo`
creates a token-free starter whose README walks through `af task plan`, `af task run`,
`af task show` and `af task deliver`. `af help concepts` explains the words used here.";

const CONCEPTS: &str = "\
Task       One piece of work with typed inputs, required outputs and limits, run by a Pipeline
           until it is verified or not. `af task list` and `af task show TASK_ID` show them.
Worker     A packaged reviewer, implementer or evaluator: a model (claude, codex) or a command,
           with its prompt and declared inputs. `af onboard` lists a repository's reviewers.
Attempt    One execution of one Worker or check, charged against its budget; a failed one is
           still spent. `af review report --campaign NAME` lists each Attempt and its tokens.
Snapshot   An immutable, content-addressed capture of the repository's files that every Worker
           reads; never the live checkout. `af task show TASK_ID` names a Task's Snapshots.
Round      One run of a review Pipeline over one exact change; it closes only with a real
           verdict. `af review campaigns` shows each Campaign's Rounds.
Campaign   One logical review owning one Ledger of Findings across its Rounds; light by default,
           one closed Round. `af review campaigns` lists them; `af review ledger` shows one.
Store      Where af records a Task's or a Campaign's events and artifacts, outside the
           repository under XDG state. `af storage` shows every Store and its size.
Provider   A named, machine-local login of the claude or codex CLI that model Workers run under;
           af never reads its credential. `af provider status` lists them.
Gate       The checks a Pipeline declares, such as `make check`, that must pass before reviewers
           run; a check that could not run is not a pass. `af onboard` shows the Gate.
Pipeline   The typed graph of Gates and Workers a review or Task runs, pinned in `.af/`.
           `af review plan` names the one a change routes to; `af task explain TASK_ID --tree`.
Candidate  The change under review: the working tree (--uncommitted) or a revision
           (--candidate REV), compared against --base. `af review plan` shows its Change Set.

The full vocabulary, with the terms each word must not be confused with, is CONTEXT.md in the
af repository.";

const CONFIG: &str = "\
Everything a human writes is TOML; everything af writes goes to the Store, except `.af/af.lock`
(machine-written TOML, committed, like Cargo.lock).

The project file is `.af/af.toml` at the git toplevel. `version`, `[project]` `name` and
`min_af`, and `[defaults]` `pipeline` are required: they name the project, the minimum `af`
release it needs and its default review pipeline. Optional `[[routes]]` pick a pipeline by the
changed paths, and an optional `[survey]` table bounds what the Surveyor measures into a Survey
(`af project refresh --help`). Worker packages live under `.af/workers/`, pinned in
`.af/af.lock`; pipelines name them.

  version = 1
  [project]   name = \"myrepo\"      min_af = \"0.7\"
  [defaults]  pipeline = \"review\"

The user file is `~/.config/af/config.toml` (XDG on macOS too). It holds what belongs to the
person and the machine, never to the project: the `[self]` update policy, the `[storage]`
budget (see `af help storage`) and the `[telemetry]` level (see `af help telemetry`). Provider
logins and
cache policy stay in `providers.toml` and `caches.toml` beside it.

`af config show` prints the effective configuration; `--origin` names the file and line each
value came from; `af config paths` lists every file a layer would read. See `af help layers`.

Examples:
  af config show --origin
  af config paths";

const LAYERS: &str = "\
Lowest to highest. Tables deep-merge, scalars last-wins, arrays replace.

  built-in    the binary's defaults — everything has one
  system      /etc/af/config.toml and /etc/af/conf.d/*.toml
  user        $XDG_CONFIG_HOME/af/config.toml and conf.d/*.toml (~/.config/af)
  directory   .af/af.toml in every directory ABOVE the repository's git toplevel, from / down;
              nearer wins. Outside a repository: every ancestor of the current directory.
  project     .af/af.toml at the git toplevel (found by walking up; nothing below it is read)
  local       .af/af.local.toml beside it, gitignored — this checkout's overrides
  environment AF_<TABLE>__<KEY>, e.g. AF_SELF__AUTO_UPDATE=never

A directory layer's `.af/af.toml` merges into `af config show` for every checkout beneath it.
`[self]`, `[storage]` and `[telemetry]` are machine-only: a directory, project or local layer
that carries one is reported and ignored.

Branch-specific configuration needs no layer: `.af/` is versioned, so the project layer on a
branch is whatever that branch commits. Each git worktree has its own `.af/` copy and its own
`af.local.toml`.

Examples:
  af config paths
  AF_SELF__AUTO_UPDATE=never af config show --origin";

// The first line of a table keeps its indent: a `"\` continuation would strip it.
const ENVIRONMENT: &str =
    "  AF_<TABLE>__<KEY>       override one configuration value (highest layer), e.g.
                          AF_TELEMETRY__LEVEL=content for one run
  AF_VERSION              run this installed version regardless of the project's pin
  AF_SELF_OFFLINE=1       never contact a release source: no pin installs, no update checks
  AF_RELEASE_SOURCE       a local directory of releases (<tag>/<asset>) instead of GitHub
  AF_RELEASE_KEY          a minisign .pub file to verify SHA256SUMS with, not the embedded key
  AF_NIGHTLY_KEY          a minisign .pub file to verify a nightly's SHA256SUMS with, not the
                          embedded nightly key
  AF_PROVIDERS_FILE       absolute path of the provider registry
                          (default: ~/.config/af/providers.toml)
  AF_CACHE_POLICY_FILE    absolute path of machine-local cache policy
                          (default: ~/.config/af/caches.toml)
  AF_DISPATCHED_FROM      set by af when it execs a pinned version; never set it yourself
  AF_TASK_RUN_DETACHED    set by `af task run --detach` on the run it starts; never set it yourself
  XDG_CONFIG_HOME         config          (~/.config)
  XDG_STATE_HOME          state           (~/.local/state)
  XDG_DATA_HOME           installed versions, receipts, man pages (~/.local/share)
  XDG_CACHE_HOME          caches, the release check (~/.cache)
  XDG_BIN_HOME            where the default `af` symlink lives (~/.local/bin)
  NO_COLOR, CLICOLOR_FORCE, EDITOR, CI    honoured as their conventions say
  HOME                    required; must be absolute

Examples:
  AF_TELEMETRY__LEVEL=metrics af config show --origin
  AF_SELF_OFFLINE=1 af self status";

const EXIT_CODES: &str = "  0    pass — the command succeeded; for `review run`, the Round passed
  1    error — af could not do what was asked; stderr names the entity, the knob, and the fix
  2    usage — the command line was wrong; only the failing command's usage is printed
  3    fail — the review or evaluation ran to completion and did not pass
  4    incomplete — the run ended without a verdict (budget, timeout, missing coverage)
  10   outdated — `af self update --check`: a newer release exists

`af provider` classifies its own refusals, so 3 and above name a Provider condition there:

  3    human action required — a login is needed; af prints the exact command a human runs at a
       private interactive terminal and never starts the OAuth exchange itself
  4    Provider CLI missing, cannot start or incompatible — the official `claude` or `codex`
       binary is not on PATH, is not executable, exits non-zero on its own version check, or its
       help page does not declare every option af passes (`setup`, `add`, `status`, `detect`)
  5    registry conflict — the ID or the auth context already belongs to another entry; under
       `detect --apply` it stops at that row, the rows before it stay registered
  6    authentication failed — the Provider CLI answered, and not with a usable login; `add`
       refuses a context that is not logged in with 6 unless --unverified
  7    usage unavailable — `provider status --usage` only: an authenticated Provider's optional
       subscription or quota probe did not answer. Authentication is unaffected
  8    Provider probe denied — the host refused the CLI probe before compatibility or login
       could be established; retry outside the sandbox or with host-approved permissions

Under --json the same codes apply and the document on stdout carries the typed outcome.

Examples:
  af review plan --policy-rev HEAD --base HEAD --uncommitted; echo \"exit $?\"
  af help nonsense   # exit 2";

const JSON: &str = "\
stdout is the result, stderr is progress. With --json every reading command prints exactly one
JSON document on stdout — no banner, no trailing text — so `af … --json | jq` always works.
Errors under --json are one af/error@2 document on stdout (schemas/error-v2.json) with the same
exit code: `command`, `error` (the sentence stderr prints) and `exit_code`. A usage error is one
too, exit 2, with clap's message; `--help` and `--version` are not errors. A refusal af
classifies adds `cause`, `next_action` (the one af command to run next, with the --repo, --state
or --workspace the command was given; null when there is none, or when no pasted line carries
it: a word with a control character, or past 4096 characters) and `details`, the facts behind
the cause:

  usage                exit 2; next_action is the command's --help
  recovery_exhausted   a node used up its recovery allowance: node, count (abandoned
                       Attempts), writer_lost, host_cancelled, allowance
  refresh_refused      exit 4: no Pipeline can run the refreshed source; decision
  task_file_invalid    a Task file field af refuses: file, field, value
  input_unreadable     a file the command names cannot be read: file, what
  interrupted          exit 130 or 143: SIGINT or SIGTERM stopped it; signal, task (null
                       when it ran none); next_action resumes or continues it

Other first-run causes are `task_not_found`, `no_task_catalog`, `no_authority`,
`not_repository`, `provider_not_registered`, `unknown_review_node`, `plan_mismatch`,
`dirty_source`, `review_selectors_missing` and `review_binding_missing`. Run
`af help errors` for the full cause table and `af help errors CAUSE` for one explanation.

A command that classifies its own outcomes — `af provider status` and `af provider setup` — prints
its own versioned document instead, carrying that exit code and an af-authored `diagnostic`;
stderr still gets one diagnostic line, so discarding stdout never hides a refusal. The Provider
documents are af/provider-detect@4 (`af provider detect`), af/provider-status@4,
af/provider-setup@2 and af/provider-models@1; none carries an account, a credential or the
Provider CLI's own words.

One command prints JSON lines instead and takes no --json: `af task events ID` writes one
af/task-event@1 document per line while it follows a Task, and its refusal or usage error is one
af/error@2 line with the same exit code.

Documents carry `kind@version` identifiers where they describe persisted records; a payload shape
change bumps the version rather than reinterpreting an old one. Nothing af prints under --json is
affected by a TTY, colours, or the update check: its notice goes to stderr, only on a TTY, and
never under --json.

Review reading commands:

  af review render --json              af/review-render@2 (schemas/review-render-v2.json),
                                       with the reply schema a model Worker's dispatch passes
  af review ledger --json              af/review-ledger@1 (schemas/review-ledger-v1.json)
  af review show KEY --json            af/review-show@1 (schemas/review-show-v1.json)
  af review report --json              af/review-report@5 (schemas/review-report-v5.json)
                                       --json is --format json; combining them is a usage error
  af review report --format sarif      SARIF 2.1.0, profile af/findings-sarif@1
                                       (schemas/findings-sarif-v1.json)

The Task documents, each named by its `schema`:

  af task show --json                  af/task-inspection@14; a collected Task:
                                       af/task-collected-inspection@3
  af task show --checks --json         af/task-checks@3
  af task list --json                  af/task-list@4, one af/task-list-entry@4 per Task
  af task list --attention --json      af/task-attention@1: seven groups by what each Task
                                       waits for, each Task its af/task-list-entry@4
  af task report --json                af/task-report@3
  af task findings --json              af/task-findings@2, each finding with its agent_prompt,
                                       the round with its coverage and verified_fixed
  af task findings --format sarif      SARIF 2.1.0, profile af/findings-sarif@1
  af task diff --json                  af/task-diff@1
  af task materialize --json           af/task-materialized@1
  af task status --json, wait --json   af/task-status@2
  af task events                       JSON lines, no --json: one af/task-event@1 per event
                                       of the Task's log, with the Store's gap-free
                                       `sequence` (--after N resumes), then one terminal
                                       record with the Task's state and exit_code
  af task run --detach --json          af/task-detached@3
  af task cancel --json                af/task-cancelled@2
  af task plan --json, refused         af/task-selection@3, exit 1, with `error`, `exit_code`
                                       and `next_action`; an infeasible plan carries
                                       planning_diagnostic.required_limits and .reserves

af/task-inspection@14, af/task-collected-inspection@3, af/task-checks@3, each
af/task-list-entry@4, the groups of af/task-attention@1, af/task-report@3, af/task-findings@2,
af/task-status@2, af/task-detached@3, af/task-cancelled@2 and the terminal record of
af/task-event@1 carry the Task's `state` (`af help task-states`), its `next_action` carrying the
selectors the command was given (a report, pasted into a pull request, names no Store path); the
diff, the materialized candidate and the refused selection describe no Task state. A previous
version is refused, never read.

The command catalog, the help pages and the schemas:

  af help --json                       af/command-catalog@1 (schemas/command-catalog-v1.json):
                                       every command with its usage, arguments and effects
  af help COMMAND... --json            the same document for that command and those below it
  af help TOPIC --json                 af/help-topic@1 (schemas/help-topic-v1.json): a page the
                                       text form prints, `af help errors CAUSE` too, when the
                                       words name no command: its `name`, `description` (null
                                       for an error cause) and `text`, the page below its title
                                       line; words that name neither are exit 2
  af schema show ID                    the JSON Schema of any document this page names, by its
                                       ID; `af schema show` lists the IDs

The catalog is generated from the command definitions the binary parses. Each runnable command's
`effects` say what it may do, never that it will:

  tokens     `free`: never calls a model; `charges`: may dispatch model Workers, charged
             against a budget, under `charges_when` (null: any run may)
  writes     `store`: a Task Store or a review Campaign's Store; `files`: files outside af's
             own state, in the repository, a worktree, a branch or a directory or file the
             command names; `machine`: af's machine-local state, the Provider and Project
             registries and Project Stores, configuration, installed releases, shell
             completions and man pages, telemetry and caches; `remote`: something off this
             machine, the gate branches and draft pull request of a Remote Check
  human      `login`: a person completes a Provider's official login, at a private terminal
             or through the host's private channel; `approval`: a developer's signature over
             an exact decision payload, made outside af; `terminal`: an interactive terminal,
             for a screen or an editor
  when       on every write and need: null always, else the flag as typed (`--apply`) or a
             short phrase naming the condition

Housekeeping every command shares is not an effect: telemetry, the storage sweep after a run, the
release check's schedule, a pinned release installed on demand, and the lazy registration of the
repository's Project and checkout.

Examples:
  af task list --json
  af task show nope --json   # exit 1
  af help task run --json
  af schema show af/task-status@2";

const TASK_STATES: &str = "\
Every af task command that shows a Task prints one state for it, in text and under --json alike:
`af task list`, `af task show`, `af task report` and the last line of `af task run`. The state is
`phase/outcome/cause`, for example `finished/gate_failed/check_failed`; `outcome` is present once
the Task ended and `cause` once that outcome is not `verified`. Under --json it is the `state`
object: `phase`, `outcome`, `cause` and `next_action`, null where absent.

  planned --af task run--> running --> finished
     |                      |   ^
     |                      v   | af task run (resume)
     |                   abandoned
     +--> waiting (plan review, resources, input or a decision) --> running

A Task's wall clock starts at its first `af task run`, not when it is planned: the preview says
`deadline starts when you run`, and `af task run` prints the deadline in local time with the time
left (`deadline 17:41:18 +03 (2 h 59 min from now)`).

Every state that ended names the one command to run next, the last line of `af task show` and
`af task run` (`next: af task run ID --retry-gate`), `none` when there is none: `af task findings
ID` after reviewer findings, `af task run ID --retry-gate` after a failed gate the kernel can run
again on the same candidate, `af task show ID --checks` after one it cannot, `af task run ID` to
resume (`af task start --file TASK_FILE` once an interrupted node used up its recovery
allowance), `af task plan --file TASK_FILE --fit` after a budget or deadline ran out (TASK_FILE
with a new `task_id` once the Task ran), `af provider status` or `af provider setup ID --login`
after a Provider failure (`af provider status` after `provider_environment`), and `af task run
NEW_TASK_ID --from ID` after `attempt_wall` when the Worker its wall stopped left edits: they are
kept as a partial candidate the new Task starts from, never delivered on their own. `af task show
ID --checks` prints the gate's last check receipt: each check's failing and flaky tests and the
end of its stderr. A check the kernel stopped at its wall is `check_failed`, never
`preflight_failed`, and every rendering says `check NAME stopped at its 90 min wall after 90 min`
with what to do: make the check faster, or raise its wall in .af/code-policy.toml and plan again;
only a check that did not start says `never started`.

`af task status ID` prints the state in one line without waiting, with the node that runs and the
reserved, charged and remaining tokens (plus unknown usage). It shows the last comparable Gate
duration when recorded. `af task wait ID` blocks until the Task settles and exits with its
outcome's code; `af task events ID` follows it as JSON lines, one per event of its log, and ends
with one terminal record that carries the same state.
A run `af task cancel ID` stopped is `abandoned/cancelled/host_cancelled`, and `af task show`
names who cancelled it. `af help agents` walks the whole loop.

`af task list --attention` groups the Tasks by what they wait for, read from each state alone and
never recorded; the browser's Tasks folder opens on the same groups. The first five need a person
and print each Task with its next command; running Tasks are named on one line and the Tasks that
need nothing are counted. Under --json, `af/task-attention@1` holds all seven groups.";

const TASK_FILE: &str = "\
A Task file (JSON, or TOML for any other extension) is what `af task plan --file` and `af task
start --file` read. Only `task_id`, `kind` and `goal` are required; `schema` defaults to
af.task-file/1. The full shape is schemas/task-file-v1.json, and docs/tasks.md walks through it.

  {\"task_id\": \"pagination\", \"kind\": \"implement\",
   \"goal\": \"Add offset/limit pagination; the design says how.\",
   \"context\": [\"docs/design.md#pagination\", \"src/list.rs\"]}

Project defaults: what a Task file omits comes from `[task.defaults]` of `.af/af.toml` in the
committed Authority Snapshot: `limits` (tokens, max_attempts, wall_ms and the verification
reserve), `pipeline` and its `fallback`. A field the Task file names wins whole; the default
Pipeline applies only to a Task of a kind it accepts. `af task plan` prints on stderr what came
from the defaults (`defaults: limits (...), pipeline kernel/implementation-reviewed (fallback
refuse) from .af/af.toml [task.defaults]`). Without `limits` in either place the plan is refused.

  [task.defaults]
  pipeline = \"kernel/implementation-reviewed\"
  fallback = \"refuse\"
  [task.defaults.limits]
  tokens = 6000000
  max_attempts = 16
  wall_ms = 10800000
  [task.defaults.limits.verification]
  tokens = 2000000
  attempts = 8
  wall_ms = 3600000

Context: each `context` entry names a file of the Task's source tree, with an optional
`#section`. The Worker's requirements end with a \"Read first\" list naming the Snapshot the
Worker reads: the project's standing rules, `.af/rules.md` of that tree, then each context file,
each with the digest of its bytes there (af/TaskReadFirst@2). A changed file is a changed plan; a
missing one is refused. A continued Task reads its candidate, so it lists the candidate's rules,
and its plan says so when they differ from what TASK read.

Acceptance: `\"acceptance\": [{\"id\": \"bounds\", \"text\": \"Negative offsets are refused\"}]` states
1 to 32 items the result must meet, each ID once. They are captured as one af/TaskAcceptance@1
record, carried in the check receipt (af/TaskCheckReceipt@2) and handed to the evaluator on the
Pipeline's optional `acceptance` input; a Pipeline that does not declare it, or hands it to no
`verify` evaluator node, is refused at plan time, and so is a kind other than an `implement`
one, whose domain never judges the items. A passing evaluator names every item ID in its reason
(`bounds: met`), or its reply is refused and the Task does not verify. A Task file without
`acceptance` plans as before.

Continuing a reviewed Task: `af task run NEW --from TASK [--round N] [--goal FILE]` plans NEW
from TASK's own Task file. Its source is the candidate TASK's review round reviewed (the last
round unless --round names one), not the base, and its implementer receives that round's
reviewer findings and the evaluator's verdict as one af/TaskReviewFindings@1 document on the
Pipeline's optional `findings` input; nothing is pasted into the goal. --goal replaces the goal.
Without --execute it plans and waits for `af task run NEW --confirm-plan PLAN_ID`. A Task file
says the same with `\"continue\": {\"task\": \"TASK\", \"round\": 1}`. `af task show NEW` prints
`continues TASK round N`, `--json` carries it as `continues`, and `af task report` names it in the
continued Task's row. A TASK that recorded no review round, but whose implementer its Attempt wall
stopped, continues from the partial tree the kernel sealed then (`exhausted/attempt_wall`): NEW's
source is that tree, and no findings travel with it.

Examples:
  # needs: a Task file ticket.json; stderr names the defaults
  af task plan --file ticket.json
  # needs: a finished reviewed Task fix-1; plans fix-2 from it
  af task run fix-2 --from fix-1
  # needs: the same, and goal.md
  af task run fix-2 --from fix-1 --round 1 --goal goal.md --execute";

const PROVIDERS: &str = "\
A Provider is a login of an official model CLI, `claude` (Claude Code) or `codex` (Codex CLI),
held in one auth directory: CLAUDE_CONFIG_DIR or CODEX_HOME for that CLI. The CLI owns the
credential; af never reads one. The machine-local registry, ~/.config/af/providers.toml, records
only an ID, a kind and an auth directory per Provider, and a review or Task binds a Worker to a
Provider by that ID (--provider NODE=ID). The registry belongs to this machine, not to a
repository, so these commands run in the af you invoke, never through a project's pinned one.

In the order of first use:

  af provider detect            find the CLIs and their auth directories, and say what each
                                directory's CLI reports about its login; writes nothing
  af provider detect --apply    register every logged-in directory that is not registered yet,
                                with the IDs the table showed; never logs in
  af provider setup ID --kind KIND [--auth-dir DIR] --login
                                log in to a directory and register it; run it yourself at a
                                private terminal, never through an agent or a pipe
  af provider add ID --kind KIND --auth-dir DIR
                                register one directory that is already logged in; refused with
                                exit 6 when it is not, unless --unverified
  af provider status            the registry and one token-free login check per directory
  af provider doctor ...        the charged end-to-end check of the Providers a review binds

detect looks for each CLI on PATH, then in ~/.local/bin, ~/.claude/local, /opt/homebrew/bin,
/usr/local/bin, ~/.npm-global/bin, ~/.volta/bin and ~/.bun/bin, and for directories in the CLI
variables, ~/.claude and ~/.codex, the sibling names ~/.claude-*, ~/.config/claude,
~/.config/claude-*, ~/.codex-*, ~/.config/codex and ~/.config/codex-*, the registry, and --dir.
It reads no shell rc file, so a directory a shell alias selects needs --dir. A directory holding
none of its CLI's marker files (.claude.json, .credentials.json or settings.json; auth.json or
config.toml) is listed `empty` and not handed to the CLI, which would create files there;
--all asks anyway. The default ~/.claude counts a ~/.claude.json file beside it as its marker.
status applies the same rule.

Identity: the table and every document show the plan or method (Claude Max (claude.ai),
ChatGPT, API key, not logged in), never an email or an organization. `detect --identity` adds
the account label the CLI reported, only on a terminal and never under --json.

See docs/providers.md, `af help exit-codes` and `af help json`.";

const STORAGE: &str = "\
af keeps a bounded amount of disk between runs (ADR-0144). One budget covers warm build keys,
warm Workspaces, review campaign Stores, the finished Tasks of every Task Store, Project Stores
(counted, never evicted; ADR-0158), Stores this release cannot read, and installed versions.
When af holds more than the budget, caches go first (warm keys, warm Workspaces, installed
versions), then telemetry, then records (review campaigns, unreadable Stores, Surveys, finished
Tasks), least recently used first within each. Nothing in use goes, and nothing used within the
last hour; nor the newest warm key of a project checked in the last 7 days, a finished Task
younger than keep_days or one a delivered Workspace was made from, a Store with a live writer
lease, or a Project Store another release wrote.

Examples:
  # what af holds, per kind, against the budget and the free-disk floor
  af storage
  # also every path it could not measure, and every Store left alone
  af storage --verbose
  af storage prune            # preview the sweep: collection, then the budget
  # perform it now (it also runs after every task and review run)
  af storage prune --apply

The [storage] table, machine layers only (built-in, system, user, AF_STORAGE__<KEY>); a
repository's .af/af.toml cannot set it:

  [storage]
  max_bytes = \"auto\"           the budget: 20 GiB, or 40 GiB when the cache volume has more than
                               100 GiB free (room for one warm key); or integer bytes, or a
                               number with B, KiB, MiB, GiB, TiB
  min_free_bytes = \"10GiB\"     below this free space on any volume af works on, or when it
                               cannot be measured, checks, measurement repetitions and Worker
                               Attempts are refused with insufficient_disk before any token is
                               spent
  auto_gc = false              also collect by age (keep_days, keep_tasks, keep_campaigns) after
                               every `af task run` and `af review run`; off, a run only holds af
                               to max_bytes, and `af storage prune --apply` collects on request
  keep_days = 14               collection never takes anything used in the last keep_days
  keep_tasks = 20              newest finished Tasks kept per Store
  keep_campaigns = 20          newest review campaigns kept
  keep_surveys = 50            newest Surveys kept per Project; protected ones may exceed it
  keep_worker_transcripts = false   keep the history each Claude Worker Attempt and provider
                                    probe leaves in its config dir
  keep_gate_pull_requests = false   leave a finished Task's gate pull request and branches open
  sandbox_root = \"/srv/af\"     unset by default (TMPDIR): where sandboxes and check runtimes
                               are made, and so the volume the floor measures for them
  build_cache_root = \"/srv/c\"  unset by default ($XDG_CACHE_HOME/af/task-build-cache): where
                               warm build keys live, and the volume the floor measures for them

Every check gets its own empty HOME, TMPDIR, AF_CHECK_SCRATCH and XDG_CACHE_HOME, removed after
it: write scratch files there, never to /tmp. A Store made with --state is recorded in
$XDG_STATE_HOME/af/stores.toml so collection reaches it. A Task whose gate leftovers are not
cleaned up yet is never collected. The sweep that ends `af task run` is recorded on that Task:
`af task show` prints its `storage sweep:` line. A Task command prints one line for its sweeps,
`storage: freed 1.2 GiB (2 warm keys)`, and only when they did something; `--verbose` on `af task
run` and `af task start --execute` prints every line instead. When the largest warm key does not
fit max_bytes with half its size to spare, `af storage` and the run say so once, with the budget
that fits. A refusal below the floor names the volume, both numbers and what to move:
`insufficient_disk: /tmp (tmpfs): 1.9 GiB (…) free, floor 10.0 GiB (…); the sandbox root is
/tmp; set TMPDIR or [storage] sandbox_root to a volume with room`.";

const TELEMETRY: &str = "\
Telemetry records what af does while it works, in its own store, never in a Task or Campaign
Store (ADR-0153). The kernel's execution authority never reads it: identity, replay, the Ledger,
budgets, admission and every Worker input are the same whether it is on, off or deleted, and
nothing it captures reaches a prompt. Nothing leaves the machine: this release has no sink.

Levels
  off       nothing: no directory, no subscriber
  metrics   command, run, gate, check, node, Attempt, harness and sweep spans with identity and
            numeric attributes; failures; resource use; the segment header keeps argv[0] and
            the subcommand path only, and no working directory
  spans     every span and event, the configuration record, the Store commit mirror and Finding
            summaries; sizes and digests of content, never the content (the default)
  content   spans plus content blobs: Worker inputs, assistant text, reasoning, tool arguments
            and results, check output (perturbing: it keeps that content)

Credential-looking material is withheld at every level by a bounded heuristic (not a promise to
recognise every secret), before any bound cuts it: a line that
looks like a credential becomes `[line withheld: looks like credential material]`, a JSON member
named like a secret keeps its key and loses its value (`[withheld]`), a configuration row whose
key is named like a secret or itself looks like a credential loses its value and origin (and
such a key is withheld too), a line or document too long to judge whole is withheld whole, an
attribute whose name itself looks like a credential is omitted and counted in `af.redacted`,
and an attribute or a command-line option named like a secret keeps its name and loses its
value whatever its shape. Only a kernel identity (a digest, a 16, 32 or 64 hex id, a segment or
occurrence id, an ordinal) in an identity position (af.run_id, span_id, a digest field, ...)
escapes these rules; any other value is judged as text. Token and charge counters are exact
decimal strings: any other number under their names is dropped and counted. A HOME that gives
no home prefix to replace by `~` is recorded as a problem. A telemetry-only failure (an
unwritable directory, a full segment) is one problem and the run continues.

  af telemetry status           level, directory, bytes, segments, runs, index, problems
  af telemetry reindex          recover crashed writers' segments and rebuild the index
  af telemetry runs             every run: executions, active, execution and elapsed time,
                                outcome, charge, failures, coverage, retained history
  af telemetry show RUN         the span tree: start offset, total and self time, status
  af telemetry turns RUN        one row per model turn: arrival timing, the harness's per-turn
                                usage, stop reason, text and thinking sizes, tool calls
  af telemetry tools RUN        one row per tool call: name (when in the invocation's tool
                                set), arrival timing, outcome, input and output bytes
  af telemetry config RUN       a run's configuration record in one execution: version,
                                configuration with origins (an environment value is <env>),
                                plan, bindings, Providers, harness versions, host, AF_*/OTEL_*
                                names; each section complete, incomplete or unknown
  af telemetry tail [RUN]       follow the live segments: every span end and event as written
  af telemetry ledger RUN       committed Store events, Finding and Demand summaries and each
                                Attempt's charge, in order, with the coverage and history
  af telemetry context RUN ATTEMPT   at content: the Attempt's composed Worker input (the
                                prompt bytes and the manifest); --raw prints the prompt alone
  af telemetry reasoning RUN ATTEMPT at content: the Attempt's thinking blocks and reasoning
                                summaries, in order
  af telemetry search TEXT      at content: the text blobs holding TEXT, with span paths and
                                bounded excerpts; refused below content
  af telemetry gc [--apply]     what the Storage Budget's sweep evicts from the telemetry kind

Recorded: the commands that run a kernel run (af task run, af task start --execute, af review
run, af provider doctor, af self optimize --execute) record af.command > af.run >
af.authority.load, af.plan (one node record per plan node), af.node > af.attempt, and the
command's af.sweep; every other command records nothing and opens no telemetry directory. From
the first af.run until the seal one consumer thread is the segment's only writer and every
record is in it within flush_latency_ms; at the command's end it writes the command's end and
seals the segment within one bound — 2 s beyond its last progress, each content of a released
harness stream it publishes renewing it — else it is abandoned, publishes nothing, and the
segment is left for the next reader to recover. A run's executions (one per process that worked
on it, numbered by the execution markers, TaskExecutionStarted@1 events of the Task's log, the
Store holds for the leases before it) can
overlap: active time is their union, execution time their sum, elapsed time first start to last
end. Its history is
`prefix:k` or `partial`, never complete, and its open executions are named beside the sealed
ones (retained executions 1–1, open: 2).

Below the run every phase is a span of the crate that owns it (the catalogue is in
docs/architecture.md): af.admission, af.context.compose, af.harness, af.output.parse and
af.usage.account under an Attempt; af.gate and its af.check spans, af.warm.restore and
af.warm.save under a check node; af.snapshot.capture, af.cas.materialize, af.store.append,
af.store.read, af.git, af.remote_check and af.process wherever the run does them. Every process
span carries process.pid, process.command (the program only), its exit, its stream sizes and
af.rusage.user_ms, af.rusage.system_ms and af.rusage.max_rss_bytes, each absent when the
platform did not deliver it. An af.check carries af.check.status and af.check.fingerprint, the
comparability key af/check-fingerprint@1 (or `unknown`). A kernel measurement is a run of its
own (af.run.kind = measure, af.measurement.id), its repetitions child spans. At spans and above
every run an execution works on has its own configuration record, its parts naming the run:
the configuration and machine sections when the run opens, the plan section when its plan
resolves (a run refused before that has the first two), and af.config.text naming the
pipeline's pipeline.toml, the Task file and .af/af.toml by the SHA-256 of their own bytes and
their size. A check fingerprint's runtime paths become placeholders only where a value names
the path or one under it, never a sibling (/tmp/a-extra beside /tmp/a). Every child reap of af goes
through review-process's one reap lock, so a process's af.rusage.* is never another child's.

At spans and above every event the kernel commits to a Store is mirrored after its commit, on
any path (an append, a Task transition, a Round publication, an integration), as af.store.event
with its type, sequence, id, causation, artifact references and the payload's digest and size;
a Finding summary is recorded for every Report admitted into the Ledger and whenever a
summarized field changes, with the admitting Worker's bound model when the kernel recorded it,
and a Demand summary keyed by its Demand id, with its own Subject and that Subject's Snapshot,
and its lifecycle (an admitted Evidence reuse that satisfies a stale Demand again is a
satisfaction); `af telemetry ledger` orders every Store log by its committed sequence. A mirror
failure is one problem, never a Store error, and makes the run's coverage incomplete. At
metrics and above every committed accounting transition records the Attempt's charge as the
Task projection holds it (af.usage.state, ordered by the committed event's sequence) and every
sidecar write its merged row (af.usage.wall, ordered within one execution only); an Attempt's
figures are its latest charge, run totals are sums of those, and the sidecar series never
enters them.

At spans and above every harness's streams are tapped as they arrive: the supervisor's drain
offers each chunk to a bounded queue (observer_queue_bytes) without ever waiting, a full queue
drops the chunk and counts it, and the consumer frames lines and records validated structure
only, timed by arrival: a Claude assistant message (stream-json) or a Codex turn is af.turn
with the harness's own per-turn usage, a Claude message's turn ending as soon as its line is
read (a further line of the same message is af.turn.continued on that turn, folded into its
row by turns), a Claude tool_use block or a Codex command or MCP item is af.tool with its
input and output bytes, and the other lines are events, the Claude result line by its size
only. Every identifier the harness supplies is af.corr in a live record; every untrusted
string (text, thinking, tool arguments and results, errors, unparsed lines) is only a size. A
frame too long to judge is withheld whole; a dropped chunk is af.stream.gap and the stream
incomplete. Once the process has ended Telemetry takes its own release decision:
login-challenge material in either stream or in a Codex Attempt's -o final message (read for
every observed process, whatever its ending), a final message that cannot be read whole, an
adapter that replaced the streams, or a stream the output cap truncated (a prefix where the
decision needs the whole), withholds everything pending (af.stream.withheld);
otherwise af.harness.ids lists each correlation id with its original once it passes the
credential rules (af.harness.ids.count admitted, the list going on in af.harness.ids.part
events past one record; an identifier past 4096 per process or 256 bytes has no mapping,
counted in af.harness.ids.omitted with af.harness.ids.analysis incomplete). Claude is asked
for --output-format stream-json --verbose at spans and content and for json at off and
metrics; the adapter reads the result line either way (a JSON object without a type is an
envelope only under json), and the Store keeps that line and stderr (Codex: its turn and
error lines and the -o final message), never native text around them, so accounting,
classification and the Store's bytes are the same at every level. With Claude Code 2.1.305
and claude-opus-5-5 the stream's thinking blocks carry no text, so Claude reasoning is sizes
only.

At content, and only there, Telemetry also keeps content as blobs, each redacted whole before
any truncation and named by one event: the composed Worker input of af.context.compose (the
prompt bytes, gen_ai.content.input, and the manifest, gen_ai.content.system); once the release
decision withheld nothing, a harness's assistant text and Codex agent messages
(gen_ai.content.output), its thinking blocks and reasoning items (af.content.reasoning, only
with content_reasoning) and its tool arguments and results (af.content.tool_input and
af.content.tool_output, only with content_tool_io); a check's stdout and stderr
(af.check.output); every mirrored Store event's payload; an admitted Finding's body
(af.finding.body); and .af/af.toml, the pipeline's pipeline.toml and the Task file
(af.config.text). A Worker request keeps identities only where its schema and, in a payload,
the schema of an artifact type af owns declare one; the Task's requirements and every other
payload are judged whole. An Attempt's content is bounded by content_attempt_max_bytes and
content outside any Attempt by content_process_max_bytes, counting what was stored: past
either, further content is dropped and counted (a dropped occurrence, the seal's dropped, one
budget problem) and nothing about the Worker changes. The content is the operator's: no
prompt, Warm Set, Worker Notes or optimization source ever reads it. `af telemetry context
RUN ATTEMPT [--raw]` prints an Attempt's composed input, `af telemetry reasoning RUN ATTEMPT`
its thinking and reasoning in order, and `af telemetry search 'text' [--since D] [--run RUN]`
searches the full-text index content_fts that every af telemetry command keeps at ingest when
content_index = true (and empties when it is false), reading a bounded number of matches and
showing an excerpt only of a blob it verified, a missing or corrupt one listed unreadable; below
content context and reasoning say the content was not captured and search is refused (exit 2).

The [telemetry] table, machine layers only (built-in, system, user, AF_TELEMETRY__<KEY>); a
repository's .af/af.toml cannot set it, and a value outside its bound refuses the load naming
the key:

  [telemetry]
  level = \"spans\"                      off | metrics | spans | content
  dir = \"\"                             default $XDG_STATE_HOME/af/telemetry; an absolute path
                                       that neither contains nor lies inside a Store, a cache
                                       or another telemetry directory
  max_bytes = \"4GiB\"                   256MiB to [storage] max_bytes; the storage sweep evicts
                                       sealed segments past it, status warns when over
  flush_bytes = \"64KiB\"                4KiB to 16MiB: the buffer written in one write
  flush_latency_ms = 1000              50 to 10000: the longest a record waits while a run is open
  observer_queue_bytes = \"4MiB\"        256KiB to 64MiB: the stream queue; full, it drops chunks
  segment_max_bytes = \"256MiB\"         16MiB to 4GiB: one process's segment, every record kind,
                                       the last 64 KiB kept for its seal
  content_max_blob_bytes = \"16MiB\"     64KiB to 256MiB: larger content keeps head and tail
  content_attempt_max_bytes = \"512MiB\" 16MiB to 8GiB: content one Attempt may keep
  content_process_max_bytes = \"64MiB\"  4MiB to 1GiB: content outside any Attempt
  content_native_body_max_chars = 1048576   4096 to 8388608
  content_reasoning = true             keep thinking blocks and reasoning summaries at content
  content_tool_io = true               keep tool arguments and results at content
  content_model_bodies = true          keep raw API bodies at content
  content_index = true                 maintain the full-text index over text blobs
  harness_native = false               receive the harness's own telemetry on loopback
  sink = \"none\"                        none: no sink exists; otlp is refused, not implemented
                                       in this release
  sink_endpoint = \"\"                   reserved: a value is refused, not implemented
  sink_headers_file = \"\"               reserved: a value is refused, not implemented
  sink_timeout_ms = 5000               100 to 60000; reserved

Layout
  $XDG_STATE_HOME/af/telemetry/v1/segments/   one JSONL segment per process: live under its
                                              writer's lock, sealed and compressed at exit
  $XDG_STATE_HOME/af/telemetry/v1/blobs/      content-addressed, compressed, shared
  $XDG_STATE_HOME/af/telemetry/v1/index.sqlite   rebuildable; `af telemetry reindex`
  $XDG_STATE_HOME/af/telemetry-dirs.v1.json   every telemetry directory ever written here

`af telemetry status` accounts every byte beneath the telemetry directory: a file or directory
that matches none of the above, or a symbolic link anywhere beneath it, is counted once under an
`unrecognized` entry (its count, its allocated bytes and up to 16 of its names), never read,
followed or removed. It rechecks each registered directory's overlap: none, overlapping, or
unknown when the Store registry cannot be read (named among the problems; the exit code is
unchanged).

Every registered telemetry directory is the kind `telemetry` in `af storage`, whatever the
current level or dir: one entry per segment, whose bytes are what its eviction frees, and the
directory's shared remainder (index, locks, shared blobs, temporaries). A sealed segment's last
use is its seal time. The sweep evicts sealed and corrupt segments least recently used first
past [telemetry] max_bytes or the whole budget, never one in use or used within the hour, each
whole or not at all — any of its files (its manifest included) changed since the inventory
leaves it in place, reported as changed — and reclaims temporary files, unreferenced blobs and
free index pages. A directory overlapping other accounted state is `overlapping`: counted once,
never evicted as telemetry.

There is no telemetry keep_days: collection by age is [storage] keep_days under [storage]
auto_gc or an applied prune (af storage prune --apply, af telemetry gc --apply). See
docs/telemetry.md.

Examples:
  af telemetry status
  AF_TELEMETRY__LEVEL=off af config show --origin";

const SELF_TOPIC: &str = "\
Layout
  $XDG_DATA_HOME/af/versions/<v>/af          every installed version, immutable once verified
  $XDG_DATA_HOME/af/versions/<v>/receipt.toml what was installed, from where, verified how
  $XDG_BIN_HOME/af  ->  versions/<v>/af       the default; the symlink is the record
  $XDG_STATE_HOME/af/self.toml                activation history, last check, pinned projects seen
  $XDG_CACHE_HOME/af/self/latest.toml         the cached release check

Pins
  A project's `.af/af.lock` records the af release that wrote it and, per target, the digest of
  that release's archive: the pin names bytes, not just a version. When you run `af` inside such a
  project, af execs that version; an installed copy must carry the lock's digest. If it is
  absent, `[self] install_pins` decides: \"ask\" (the default) asks on the terminal, naming the
  version, the lock and whether it is older than this af or a pre-release, and without a
  terminal installs nothing; \"always\" installs; \"never\" does not (true and false mean always
  and never). Whatever is not installed leaves this af to run an older pin's command, printing
  `af self install <v>`; a newer pin is refused. `af review plan` never installs. An install
  from a lock needs the archive to match both the lock's digest for this machine's target and
  the release's signed SHA256SUMS. A pin without a digest for this target is never installed
  on demand: `af self install <v>` trusts the release checksums explicitly, and
  `af onboard --refresh-lock` run online records every target. AF_SELF_OFFLINE=1 refuses too.
  AF_VERSION=<v> overrides the pin for one invocation; `af onboard --refresh-lock --af <v>`
  moves the pin to <v> (that release writes the lock).

  Only a released, receipted binary writes a pin; a source build leaves the lock unpinned or the
  existing pin untouched. Nothing older than 0.8.0 — the oldest supported release — is ever
  dispatched to or made the default.

What binds bytes
  The release's SHA256SUMS, which every release signs (minisign) with the key embedded in the
  binary at build time; an unsigned or badly signed SHA256SUMS is refused. Under a lock, the
  lock's digest as well: both must match. `af self status` shows whether this build carries the
  key; a receipt's verified_by says which checks installed each version (lock+signature,
  minisign, or sha256sums when a build without a key, or install.sh under
  AF_INSTALL_ALLOW_UNSIGNED=1, checked the checksum alone).

Updates — `[self]` in ~/.config/af/config.toml
  update_check = true      look for a newer release, at most once per check_every
  check_every = \"24h\"
  auto_update = \"notify\"   notify | always | never
  channel = \"stable\"       stable | rc | nightly   (af self channel switches)
  install_pins = \"ask\"     ask | always | never
  keep_versions = 3

  The check never runs on the hot path. After an eligible command — any but help, --version,
  `af self`, `af config`, `af doctor`, completions and the browser — af spawns a detached child
  that refreshes the cache and exits, with or without a terminal and under --json too; `af self
  status` schedules it too when it is due, and `af self status --refresh` runs it at once. The
  child holds none of the command's pipes, and never changes its output or exit status.
  check_every throttles invocations, counted from the last attempt, a failed one included: no
  timer, daemon or service runs between them. Releases come anonymously over HTTPS (curl) from
  a public repository, through gh only when that is refused; every call is bounded (curl: 5 s to
  connect, 30 s to list releases; gh: 30 s), and a failure's reason (the HTTP status or curl's
  error) is said before any `gh auth login` hint. The cache (af/self-check@4) records the last
  answer, the last attempt and why it failed, which `af self status` prints:
  `last check: <time> · latest X · last attempt <time> failed: <reason>`. A check that has held
  latest.lock for over an hour counts as no check, and status names it with `rm <lock>`. A
  pre-release af asks for pre-releases whatever the channel, and is told `latest stable X ·
  latest pre-release Y (you run R: af self update --rc)`; a release older than the running one
  is never shown as simply `latest`. CI, AF_SELF_OFFLINE, update_check = false and auto_update =
  \"never\" turn the check off; `af self update --check` offline prints `offline: check skipped`
  and exits 0. Inside a pinned project the af that dispatches schedules it, once. Only an
  interactive run — stderr a terminal, no --json — prints the cached notice, one line on stderr;
  under a pin, the af that dispatches prints it. With `always`, the child installs the newer release
  and retargets the default, unless `af self update` or `rollback` changed it meanwhile; the
  running process and every pinned project are untouched. Its \"updated the default\" notice is
  shown only while the activation it announces stands: one a later update or rollback made
  stale, even an update back to the same version, is dropped. `af self update` without
  --version never moves the default to a release older than the running one.

Channels
  stable installs the newest release, rc the newest release or pre-release, nightly the newest
  nightly build of main by its date (`nightly-YYYY.MM.DD`, version <core>-nightly.<YYYYMMDD>;
  Linux only). `af self channel` prints the configured channel and its layer; `af self channel
  stable|rc|nightly` writes it into the user layer, keeping the file's other keys and comments,
  then installs the channel's newest as the default, even an older one, and says so; with
  --no-update it only records. `af self update` and the check follow the channel; on nightly a
  newer nightly is told by its date. A nightly's SHA256SUMS is verified against the nightly key
  alone (crates/af/keys/nightly.pub, embedded at build time, or AF_NIGHTLY_KEY), never the
  release key, and a release's never against it: an af without one refuses the nightly channel.
  A pinned project runs its pin whatever the channel, and a nightly is never pinned.

Commands
  af self status [--refresh] | update [--check] | channel [CHANNEL] [--no-update]
  af self rollback | install V | remove V | prune
  af self setup-shell [--write | --remove] | uninstall [--purge]
  Update, rollback, remove, prune, and uninstall refuse to touch a binary af did not install.
  uninstall refuses while a background check runs, since it may be installing a release: run
  it again when the check has finished. A check that starts after an uninstall writes nothing,
  and a command that was running during an uninstall --purge recreates none of what it removed.
  From a cargo/source-installed af at the default path, `af self install V` explicitly replaces
  that running executable with the verified release and adopts the self-managed layout. An
  unrelated file at the default path is never replaced.
  remove and prune keep the default and every version a project seen on this machine pins,
  read again under the lock every activation holds. A pinned release runs only once its pin is
  recorded under that lock: when it cannot be, the command fails and says why.

Examples:
  af self status
  af self update --check   # exits 10 when a newer release exists";

#[cfg(test)]
mod tests {
    use super::*;

    /// A topic that is a table starts as one: its first row keeps the indent of the rows below
    /// it, which a `"\` string continuation would strip. A definition list (`af help concepts`)
    /// starts its terms at the margin and indents each definition's continuation under its text.
    #[test]
    fn a_tables_first_row_keeps_its_indent() {
        for (name, _, text) in TOPICS {
            let mut lines = text.lines();
            let first = lines.next().unwrap();
            let second = lines.next().unwrap_or_default();
            let indented = |line: &str| line.starts_with("  ");
            let definition_column = first.find(char::is_whitespace).and_then(|end| {
                first[end..]
                    .find(|c: char| !c.is_whitespace())
                    .map(|start| end + start)
            });
            let definition_list = !indented(first)
                && definition_column == Some(second.len() - second.trim_start().len());
            if indented(second) && !second.trim_start().starts_with('$') && !definition_list {
                assert!(
                    indented(first),
                    "af help {name}: its first line {first:?} lost the indent of {second:?}"
                );
            }
        }
        assert!(EXIT_CODES.starts_with("  0    pass"));
        assert!(ENVIRONMENT.starts_with("  AF_<TABLE>__<KEY>"));
    }

    /// ADR-0259: the skill opens with its frontmatter, and the earlier versions are what the
    /// earlier generator wrote: its marker is the digest of the rest, and the current skill is
    /// none of them, so a write that changes the skill has a version to upgrade from.
    #[test]
    fn the_agent_skill_opens_with_frontmatter_and_earlier_versions_are_exact() {
        use sha2::{Digest, Sha256};
        let skill = agent_skill();
        assert!(skill.starts_with("---\nname: af\ndescription: "), "{skill}");
        assert!(skill.contains(agent_snippet()));
        assert!(is_generated_agent_skill(&skill));
        let mut versions = vec![AGENT_SKILL_VERSION];
        for (version, earlier) in EARLIER_AGENT_SKILLS {
            assert!(version < AGENT_SKILL_VERSION && !versions.contains(&version));
            versions.push(version);
            assert_ne!(earlier, skill);
            assert!(is_generated_agent_skill(earlier));
            let (marker, body) = earlier.split_once('\n').unwrap();
            let digest: String = Sha256::digest(body.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            assert_eq!(
                marker,
                format!("<!-- af-generated-skill:sha256:{digest} -->")
            );
            assert!(!is_generated_agent_skill(&format!("{earlier}\n")));
        }
    }

    /// One registry: every topic once, findable by its name, listed by `af help` with exactly
    /// the description `af help TOPIC` heads its page with.
    #[test]
    fn every_topic_is_listed_once_with_its_registry_description() {
        let listing = listing();
        let mut names = std::collections::BTreeSet::new();
        for (name, about, text) in TOPICS {
            assert!(names.insert(*name), "{name} twice");
            assert_eq!(find(name).map(|(found, _, _)| *found), Some(*name));
            assert!(!about.is_empty() && !text.is_empty(), "{name}");
            let rows = listing
                .lines()
                .filter(|line| line.split_whitespace().next() == Some(*name))
                .collect::<Vec<_>>();
            assert_eq!(rows.len(), 1, "{name}: {listing}");
            assert!(rows[0].trim_end().ends_with(about), "{name}: {}", rows[0]);
        }
        assert_eq!(
            &TOPICS[..2]
                .iter()
                .map(|(name, _, _)| *name)
                .collect::<Vec<_>>(),
            &["start", "concepts"]
        );
    }

    /// ADR-0259: a page's `af/help-topic@1` document and its text form are one rendering: the
    /// text form is the title line from the document's name and description, a blank line and
    /// the document's text; `af help errors CAUSE` has no title line. `start` is left out: its
    /// page reads this machine's Provider registry, which a unit test does not touch.
    #[test]
    fn a_help_page_document_is_its_text_form_below_the_title_line() {
        for (name, about, _) in TOPICS.iter().filter(|(name, _, _)| *name != "start") {
            let page = help_page(&[name.to_string()]).unwrap().unwrap();
            let document = page.document();
            assert_eq!(document["schema"], HELP_TOPIC_SCHEMA);
            assert_eq!(document["name"], *name);
            assert_eq!(document["description"], *about);
            assert_eq!(
                page.text_form(),
                format!(
                    "af help {name} — {about}\n\n{}",
                    document["text"].as_str().unwrap()
                )
            );
        }
        let words = ["errors".to_owned(), "review_binding_missing".to_owned()];
        let page = help_page(&words).unwrap().unwrap();
        let document = page.document();
        assert_eq!(document["name"], "errors review_binding_missing");
        assert_eq!(document["description"], serde_json::Value::Null);
        assert_eq!(
            page.text_form(),
            crate::error_document::explain(Some("review_binding_missing")).unwrap()
        );
        assert_eq!(document["text"], page.text_form());
        let unknown = ["errors".to_owned(), "nope".to_owned()];
        assert!(help_page(&unknown).unwrap().is_err());
        assert!(help_page(&["review".to_owned()]).is_none());
        assert!(help_page(&[]).is_none());
    }
}
