# ADR-0147: Stop the Claude model fallback, and name the model that answered

**Status:** accepted (2026-10-15). Extends
[ADR-0102](0102-account-for-every-reported-claude-task-model.md) and
[ADR-0125](0125-charge-a-claude-model-breakdown-that-covers-the-top-level-summary.md): the identity
check and the charge stay as they are; the refusal now names what happened and the next step, a
safety-classifier refusal is its own classification, and the Claude Task adapter turns Claude
Code's automatic model fallback off. Amended the same day: the admission probe states its purpose
(see [The admission probe states its purpose](#the-admission-probe-states-its-purpose)).

## Context

[Issue #263](https://github.com/PakhomovAlexander/afactory/issues/263). Three `af review`
Provider-admission Attempts (2026-10-13 23:47 and 2026-10-14 00:04 on af 0.12.0-rc.1 with Claude
Code 2.1.299, 2026-10-15 01:11 on 0.12.0-rc.3 with 2.1.300), bound to `--model claude-fable-5-1
--effort xhigh` with the input `Reply with exactly: OK`, each returned `is_error:false`,
`result:"OK"`, `num_turns:2`, `safety_stops:2`, `stop_reason:"end_turn"`, `fallback_credit:null`,
and a `modelUsage` with two models: `claude-fable-5-1` (inputTokens 2, outputTokens 0,
cacheCreationInputTokens 3210, cacheReadInputTokens 3001) and `claude-opus-5` (inputTokens 2,
outputTokens 4, cacheCreationInputTokens 6278, cacheReadInputTokens 0). The top-level `usage` was
their sum (input 4, output 4, cache creation 9488, cache read 3001). `model_usage::account`
refused the reply, which is right, the Attempt was charged 9496 tokens, and the campaign ended
`incomplete (3 missing nodes)`. The only reason given was `Claude model usage reports an
unexpected model identity`, in the Task record's diagnostic artifact, never on the run's stdout
or stderr. An operator asking for Claude Fable 5.1 read it as a mysterious model denial.

Eight hand replays of the same invocation on 2026-10-15 made one turn on `claude-fable-5-1` with
`safety_stops:0`: the switch is not deterministic. Claude Code documents it (model configuration,
"Automatic model fallback"): Fable 5.1, Fable 5, Opus 5.5, Sonnet 5.5 and Opus 5 run safety
classifiers; a flagged request is re-run on a category-specific fallback model (Opus 5 or Opus 4.8
for Fable 5.1 and Opus 5.5) and the session stays there; the first request can be flagged because
it carries workspace context; `switchModelsOnFlag` only turns the switch into a question in
interactive sessions. Users report switches on benign input and the setting being ignored
(anthropics/claude-code issues 98875 and 66657).

In Claude Code 2.1.300 the environment variable `CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK` gates the
whole refusal-fallback feature. `CLAUDE_CODE_NO_MODEL_FALLBACK` instead disables every model
substitution, including the one compaction may need ("Compaction unavailable:
CLAUDE_CODE_NO_MODEL_FALLBACK is set and model substitution is disabled"). With
`CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK=1` a normal `claude-fable-5-1` run is unchanged (verified
2026-10-15: one turn, one model, `safety_stops:0`). Without a fallback, a flagged request ends as
a refusal: the API's `stop_reason:"refusal"` with `stop_details.category`, and in Claude Code the
text "<Model>'s safeguards flagged this message (https://www.anthropic.com/legal/aup). This
sometimes happens with safe, normal conversations." af reads `--output-format json`, whose result
object carries `safety_stops`, `num_turns`, `stop_reason`, `usage` and `modelUsage`; af read none
of the first three.

A misspelled model (`claude -p --model claude-fable-5.1`) exits 1 with `is_error:true`, the result
"There's an issue with the selected model (claude-fable-5.1). It may not exist or you may not have
access to it. Run --model to pick a different model.", zero `usage` and no `modelUsage`. af
classified it `unknown`.

## Decision

1. **af never runs a model the Worker did not bind and never accepts a reply from one.**
   ADR-0102 and ADR-0125 stand: `model_usage::account` refuses any other model's activity, and the
   Attempt is charged what the result reports (the recorded result charges 9496).
2. **The fallback is turned off at the source.** The Claude Task adapter passes
   `CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK=1` to every Attempt, for every access mode, beside
   `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` and `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` and with
   their status: a bounded client mitigation, not proof. The identity check in 1 stays the
   authority. `CLAUDE_CODE_NO_MODEL_FALLBACK` is not set. The review-time Claude adapter
   (`ClaudeAdapter` in `crates/review-runner-claude/src/lib.rs`) only renders a package's input for
   `af review render` and starts no process, so it sets none of the three; every Review Worker
   runs through the Task adapter. No isolation flag and no tool grant changes.
3. **A classifier refusal is its own outcome.** `NativeFailureKind::ModelRefused`, spelled
   `model_refused`, is recognised from a parsed result when `safety_stops` is a positive integer,
   or `stop_reason` is `refusal`, or an error result carries the "safeguards flagged this message"
   text, whatever `is_error` says. In an `is_error:true` result, `result` is Claude Code's own
   error text whenever `errors` and `error` yield no string (absent, `null`, `[]`, an array
   without strings, an object without a string `message`), and is classified then; independently,
   an `is_error:true` result whose `result` string contains the safeguards text is a refusal
   whatever `errors` or `error` hold. A result with `safety_stops >= 1` is never a success. The
   diagnostic names the bound model and effort, the count, the category when the result names one
   (`stop_details.category`), that af turned Claude Code's automatic switch off because af never
   runs an unrequested model, and the next step. The Task runtime does not retry an Attempt that
   failed this way (`TaskWorkOutput::refused_input`): the same input is flagged again, so the node
   fails at once with its remaining Attempts unspent. The Attempt is charged exactly the usage the
   result reports.
4. **The identity refusal is specific.** When `modelUsage` names a model other than the bound one,
   the diagnostic names the bound model with its output tokens, each other model with its output
   tokens, `safety_stops` and `num_turns` when present, says Claude Code switched models (a
   safety-classifier fallback when `safety_stops >= 1`, classified `model_refused`; otherwise an
   unexplained switch, `model_switched`), that af refused the reply from the unrequested model,
   that this is neither a credential nor a spelling problem, and what to do. `account` returns an
   owned error to carry it. An entry of `modelUsage` is a **foreign activity** when its key is a
   well-formed ID (`valid_id`) that is neither the bound model nor an alias whose
   `canonicalModel` is the bound model, its four counters parse completely (`Counters::parse`
   finds them not malformed), and at least one of `inputTokens`, `outputTokens`,
   `cacheCreationInputTokens` and `cacheReadInputTokens` is above zero. A switch is at least one
   foreign activity, and the switch decision reads every entry of the map however many it holds;
   the hint names every foreign activity, key and output tokens, in map order. A foreign entry
   with malformed counters (`{}` included) is the malformed-usage refusal only and never a
   switch; one with all four counters at zero is explicitly unused and refused for nothing; one
   with complete counters, none above zero, that leaves a counter out is neither and is refused
   with `Claude model usage leaves another model's activity unreported`. The charge keeps reading
   at most 32 entries, as ADR-0102 says. The switch refusal outranks every other accounting
   reason: whenever a switch is detected, the error is the switch hint, whatever reasons were
   found before or after it, and every other reason found (the map-size bound included) follows
   it, each after a semicolon, so the diagnostic starts with the hint and no accounting fact is
   lost. The charge, `charge_complete` and the usage observation are exactly what the accounting
   rules decide; every other accounting rule and refusal is unchanged.
5. **A model Claude Code does not know is `model_unavailable`.** "issue with the selected model"
   and "may not exist or you may not have access" classify as `ModelUnavailable`. The diagnostic
   quotes the rejected spelling, says Claude Code does not know the model ID or the account cannot
   use it, and names `af provider status` (token-free) and `af provider doctor` (charged).
6. **The calling site says it.** One function, `review_runner_claude::hint::compose`, writes the
   hint for 3, 4 and 5; the Attempt diagnostic starts with it after `Claude Worker failed:`. When
   a Provider-admission or Worker Attempt fails with one of these outcomes, `af task run` and `af
   review run` print one stderr line per failed Attempt of that run: `af: NODE failed: Provider ID
   (KIND), model MODEL, effort EFFORT: HINT`. The hint is read back with `hint::find`, which takes
   only af's own head of the diagnostic up to its fixed next step, so no Worker text after it is
   printed. `af review report` follows its `incomplete (N missing nodes)` final verdict, and `af
   review run` its incomplete verdict line, with the same hint for each missing node it
   explains. `af task show` keeps the full
   diagnostic. A hint holds no secret, no prompt text, no Worker output and no path, and a
   category is printed only as a short lower-case name.
7. **One grammar G for every spelling af prints.** `review_runner_claude::hint` defines it once:
   a spelling starts with an ASCII letter or digit and continues with at most 127 further bytes,
   each an ASCII letter, digit, `-`, `.`, `_` or `:`; it holds no `/`, no `@`, no whitespace and
   no other byte. `shown` prints a value in G verbatim (a Bedrock-style ID such as
   `us.anthropic.claude-opus-5-5-v1:0` included) and any other value as `[not printable]` with
   its byte length, never any part of it; `attempt_line` uses it for the node, the Provider ID
   and kind, the model and the effort. `ClaudeTaskAdapter::new` refuses a `--model` or
   `--effort` outside G before any process starts. The token-free static rules of a Model binding
   live in one function, `TaskProviderIdentity::check_static_rules`: G on the model and on the
   effort first, then the versioned-model rule, so a value outside G is refused with G's words
   whatever else it fails, and the versioned-model rule's words are used only for values inside
   G. `TaskProviderIdentity::execution` applies it, and so does a first pass over every Model
   Worker (`bind_named_models`, in Worker-name order, for `af task plan` and the Task path of `af
   review run`) and over every captured reviewer (`review_task::local_workers`, in node order),
   before any Provider probe starts; its refusal names the Worker or node, the Provider label and
   the rule. Only then does the second pass probe each Provider label once and build the
   executions, so an invalid binding anywhere in a Task or review starts no process. Each refusal
   names the rule and the value's byte length, never the value. A `modelUsage` key outside G is
   still counted as ADR-0102 says and is named `[not printable]` in the hint.
8. **No new public schema version.** The classification lives in the diagnostic prose of the
   existing `af.task-diagnostic/1` shape and in process memory; no record, event or document
   changes shape
   ([ADR-0145](0145-read-only-what-this-release-writes-and-version-every-changed-shape.md)).

## The admission probe states its purpose

**Measured** on 2026-10-15 on one machine with Claude Code 2.1.300, invoked as `claude -p
--output-format json --model claude-fable-5-1 --effort xhigh --safe-mode --restricted
--permission-mode dontAsk --strict-mcp-config --tools Read,Glob,Grep --allowedTools
Read,Glob,Grep`, the prompt on stdin, with the environment `PATH`, `HOME`, `USER`,
`CLAUDE_CONFIG_DIR`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`,
`CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1` and `CLAUDE_CODE_DISABLE_REFUSAL_FALLBACK=1`:

- The admission probe input of rounds 1 to 3, `Reply with exactly: OK` and a newline
  (`PROBE_INPUT` in `crates/review-runner/src/task/provider.rs`), run from an empty or nearly
  empty working directory under the temporary root, was refused by Fable 5.1's safety classifier
  14 times out of 14. Five were af's own admission Attempts: the three campaigns of 2026-10-13, 14
  and 15 recorded in the context above, before the fallback guard, and two `af provider doctor`
  runs on round 3's binary, each `safety_stops` 2, `stop_reason` `refusal`, about 6,500 tokens
  charged. Nine were hand runs from fresh `mktemp -d` directories: empty, holding one README, and a
  git repository with one committed README. Every refused result read the same 11,188 tokens from
  the prompt cache: the request is scored on its static preamble, and a content-free instruction
  leaves nothing else to score.
- The same probe run from a clone of this repository, a real project directory, passed 12 times
  out of 12, in one turn on one model.
- Opus 5.5 passed the same bare probe from an empty directory, and af's kernel admissions bound to
  `claude-opus-5-5` passed three times that day.
- A probe that states its purpose passed 6 times out of 6 from fresh empty directories, in one
  turn, `safety_stops` 0, the reply `OK`, about 360 cache-creation tokens. Its exact text was
  `This is a Provider admission check by the af review tool: it only verifies that this account
  can run the bound model. No file needs to be read and no tool needs to be used. Reply with
  exactly: OK`.
- `LC_ALL=C` changed nothing.

**Decision.** `PROBE_INPUT` is exactly that text followed by one newline, byte for byte, 196
bytes. It is the one constant every admission probe, context digest and test reads; fake CLIs in
tests match stdin against it rather than a copy. The expected reply stays exactly `OK`. The probe
still reads no file and uses no tool, and its working directory stays
`review_sandbox::attempt_directory()`, a fresh empty directory
(`crates/review-pipeline/src/task/provider.rs`). The admission cost allowance of
[ADR-0091](0091-capture-explicit-task-provider-admission-costs.md) is unchanged, and so are every
isolation flag, tool grant, environment guard, charge rule, hint wording and schema.

**Why.** A content-free request is scored on af's static preamble alone, and Fable 5.1's
classifier refused that every time; a request that says what it is for was not refused. Running
the probe from the repository would also pass, but admission proves the account and the bound
model, not a project, and its fixed context must stay free of business inputs; binding another
model for admission would prove the wrong model. The refusal handling of decisions 1 to 8 stays
the authority for the day the classifier fires anyway: such a probe is still `model_refused`,
charged what it reports, not retried and named on stderr.

## Considered options

- **Set `CLAUDE_CODE_NO_MODEL_FALLBACK`.** Rejected. It disables every model substitution,
  compaction's included, so a long Worker session could fail to compact. The refusal fallback is
  the one substitution af must stop.
- **Accept the fallback model's reply when the classifier switched.** Rejected. af never runs or
  accepts a model the Worker did not bind (ADR-0102). The Worker, its plan and its cost were
  approved for the bound model.
- **Retry a refused Attempt.** Rejected. The classifier flags the same input again, so a retry
  spends another Attempt and its tokens on a known outcome. A human may rerun; the hint says how.
- **A new unknown-usage cause for a refusal.** Rejected. A classifier refusal reports its usage
  (the API bills some categories before any output, others not, and counts both against rate
  limits), so it is charged and is not an unknown-usage case. When a refusal does report nothing,
  nothing better than `unreported` is known, and a new cause would change the closed
  `TaskUnknownUsageCauseV1` fixed in `task-execution-record-v5.json` and `task-report-v1.json`,
  which takes a new version (ADR-0145) for no gain. `unknown_usage_cause()` for `ModelRefused` is
  `Unreported`.
- **Read the stream-json `model_refusal_fallback` and `model_refusal_no_fallback` messages.**
  Rejected for now. af reads `--output-format json`, whose one result object carries the counts
  af needs; older CLIs do not emit those messages, and switching the output format would change
  the invocation every adapter test and the capability check pin.
- **Store the classification as a typed field of the diagnostic.** Rejected. It changes the
  `af.task-diagnostic/1` shape and its schema. The hint is af's own words at a fixed place in the
  diagnostic, found by one function beside the one that writes it.

## Consequences

- A flagged request on a client that honours the guard ends as `model_refused` on the bound
  model, is charged what it reports and is not retried; the run's stderr says so with the next
  step. A client that still switches is refused by name, `model_refused` with the safety-classifier
  fallback or `model_switched`, and the hint tells the operator to upgrade Claude Code.
- The recorded fallback result is refused, charged 9496 tokens and named on stderr before any
  business Worker starts, as `tests/it/task_file/model_fallback.rs` in the `af` crate shows.
- The admission probe sends the purpose-stating `PROBE_INPUT` and passes only on the reply `OK`,
  as the same file shows with a fake `claude` that records its stdin.
- A misspelled model reads `model_unavailable` with the spelling it rejected, instead of
  `unknown`.
- A switch is named first even when the result's usage is also malformed or the map holds more
  than 32 entries; the accounting reasons follow the hint, and the stderr line is printed. An
  empty or all-zero foreign entry is never reported as a switch.
- A binding whose model or effort is a path, an `@`-bearing account identifier or anything else
  outside G is refused with G's words before any process starts, and no output echoes it; an
  invalid binding on a later Worker or reviewer is refused before the first Provider is probed.
- `af task run` and `af review run` may print more stderr lines; documents and exit codes are
  unchanged.
