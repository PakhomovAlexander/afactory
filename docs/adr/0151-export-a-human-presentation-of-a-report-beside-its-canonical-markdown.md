# ADR-0151: Export a human presentation of a report beside its canonical Markdown

**Status:** proposed (2026-10-19).

## Context

A report or Document reaches a person through `af task output`. `--format markdown` writes the
recorded `af/Document@1` text. `--format json` writes the whole artifact envelope. The text is
what the Document profile's renderer made from the draft (`render_validated` in
`crates/review-pipeline/src/task/document.rs`). That renderer escapes every ASCII punctuation
character of the title, the headings and every authored section, so that plain text can never
become Markdown syntax. These bytes are the ones the checks, the independent verifier and
acceptance are bound to
([ADR-0058](0058-verify-document-artifacts-through-the-common-task-runtime.md),
[ADR-0133](0133-accept-reports-bound-to-an-exact-source-snapshot.md)). The escaping is
deliberate, and nothing in the Store is corrupted.

To a reader, though, the result is `crates\/af\/src\/cli\.rs\:1163`. Authored lists and
examples run together into one paragraph, and an author's code fence shows up as escaped
backticks. The canonical text also does not say whether the report was accepted. That verdict
sat only in the verification and evaluation JSON, so people were sent those receipts along
with the Markdown. None of it is a report a person can read.

## Considered options

- **Escape less in the canonical renderer.** Rejected. This would change the bytes ADR-0058 and
  ADR-0133 bind to checks, evaluations and acceptance. No recorded Document would render again
  from its draft (`Report differs from its exact rendered draft and sources`), so the change
  would need a new Document version. It would also turn authored plain text into trusted markup.
- **Unescape the canonical Markdown on export.** Rejected. Removing backslashes cannot tell an
  escape from a backslash the author wrote. It also brings back every authored heading, link,
  HTML tag and reference definition the escapes had neutralized.
- **Treat the draft as Markdown.** Rejected. `DocumentDraft` sections are plain text by
  contract. Authored HTML, links and headings would become active content.
- **Record a second, reviewed presentation artifact.** Rejected. That is a report-generation
  design, and it would pay for a second review only to change layout.
- **Derive an unrecorded presentation from the validated draft at export time.** Chosen.

## Decision

1. `af task output TASK --port PORT --format human-markdown --output FILE` writes a human
   presentation of an `af/Document@1` output of the Document or report profile. Any other type
   is refused, and the refusal names `--format markdown` and `--format json`. The `json` and
   `markdown` formats write the same bytes as before, and `af task report` and its pull request
   block do not change.
2. The presentation is made only from the Document's own draft and sources, read by the
   identities the Document names and retains. The canonical renderer must reproduce the
   Document's text from them byte for byte. If it does not, the export is refused and nothing is
   written. The authored title, sections and text keep their order and wording.
3. The file states the Task's acceptance in words. Its first line is `Result: accepted`,
   `Result: changes requested` or `Result: incomplete`, followed by the reason: failed checks,
   the reviewer asking for changes, an inconclusive review, or no review. A `Review` section
   gives the checks' outcome, naming any failed checks and failed citations, then the
   independent review's outcome and the evaluation's summary. The summary is untrusted text and
   is rendered under the same rules as the draft.
4. Only exactly matching evidence is attached, for the Document and the report profile alike.
   The acceptance receipt must be one of the result's outputs and counted in its evidence, and
   must judge this Document. Its provenance must be this Task's own:
   - its invocation's plan is an `af/ExecutionPlan@1` of exactly the revision the result names,
     binding that revision's inputs and authority, and the receipt was recorded by that
     invocation's kernel operation;
   - its verifier policy is one the revision names for this evidence type, and the check receipt
     names the same policy and the same plan;
   - the invocation binds exactly the acceptance step's ports (the Document, the check receipt,
     the evaluation when one ran and, for a report, the Task's own source), and the receipt
     retains every artifact they name;
   - the check receipt and the evaluation are started Attempts of this Task's run, name the same
     Document, sources, check receipt, source Snapshot (for a report) and the revision's
     requirements, and retain the inputs the kernel requires of them;
   - for a report, the check receipt names the Manifest of the Task's source Snapshot, and the
     Task's captured sources are the Document's. When those sources are not empty, the
     evaluation retains them too: only the recorded empty set may go unbound at the verifier
     (ADR-0133), so a review whose verifier never received the captured sources is refused.

   The receipt's outcome must be the one those give. A receipt of another Document is not
   attached; the file says that no receipt judges this output. An inconsistent or stale chain
   refuses the export.
5. The presentation says what it is. It ends with a note that it is a readable presentation,
   naming the command that writes the canonical report, and saying what judged that report: the
   automated checks and the independent reviewer when an evaluation was recorded; only the
   checks, which failed so no reviewer ran, or after which no review was recorded; or nothing,
   when no receipt judges the output. It never says a reviewer judged a report no reviewer saw.
   The file holds no digests, schemas or envelopes.
6. Authored text stays text. The renderer chooses every line start and drops authored leading
   ASCII spaces and tabs there. Only those are layout, as Markdown reads indentation, blank
   lines and padding: a no-break, em, ideographic or other Unicode space is authored text, kept
   wherever it stands, at a line start or end, on a line of its own, after a list marker or in a
   heading. A code span is padded by one ASCII space on each side exactly when Markdown would
   strip one, so tabs and other whitespace inside it are never lost. It escapes the first character of a line's authored text when that
   character could begin a block: any ASCII punctuation except `(`, `)`, `"`, `'`, `,`, `.`,
   `/`, `?` and `;`, and an ordered-list delimiter. Wherever they appear, it escapes `\`, the
   backtick, `*`, `[`, `]`, `<`, `~`, `|` and `$`. It escapes `_` unless the character sits
   between letters or digits, `&` when an entity reference follows, and `#` in headings. A word
   that holds `://`, `www.`, `mailto:`, `xmpp:` or `@` is written as code spans one backtick
   long, with the word's own backticks escaped between them, so no autolink and no code fence
   can form. Ordinary punctuation, paths and non-ASCII text are written as authored.
7. The renderer recognizes only plain-text structure:
   - lists with `-`, `*`, `+` or `•`, or one to nine digits and `.` or `)`, nested by their
     indentation. An item keeps its number; an item that would be renumbered is drawn as a
     bullet that shows its number as text;
   - an authored fence that closes, with at most one plain word of info string. A closing line
     sits at most three columns in, as an opener does; a fence line indented further is content
     of the example. Only ASCII spaces and tabs pad a fence line, before or after its run and
     around its info word: a run beside a no-break or other Unicode space is content, kept as
     written. The closing line of every opener is found in one backward pass over the
     section, so openers that nothing closes cost no scan of the lines after them;
   - a block indented four columns, shown without the ASCII indentation its lines share; any
     other whitespace after that indentation is content of the example;
   - a pipe table.

   Examples and tables are shown verbatim in a code block whose fence is longer than any
   backtick run inside it. Everything else is paragraph text.
8. Only a captured source that is an `https://` location passing `safe_source_location` becomes
   a link, under the canonical renderer's existing rule. Every other location stays inert.
   Rendering fetches nothing. A parser decodes character references in a link destination, and
   that grammar admits `&`, so the destination writes each `&` as `&amp;`. The parsed URL is then
   exactly the captured location: `?q=&copy;` stays `&copy;` and does not become `©`. The
   grammar already refuses `<`, `>`, `\` and whitespace, so nothing else in the destination
   changes. The canonical report keeps its bytes.
9. Covered dialect: CommonMark with the GFM extensions (tables, strikethrough, autolinks in
   comrak's relaxed mode, task lists, footnotes), GitHub alerts and dollar and code math.
   GitHub's repository references, such as `#123` and commit hashes, are site features outside
   this dialect, and so is any other extension.
10. Nothing is recorded. There is no new persisted shape or schema. The export reads the Store
    read-only and writes to an absent file through the same atomic, no-clobber path.
11. The human file is what a person receives. The canonical Markdown and the JSON receipts are
    audit exports, written when someone asks for them.

## Consequences

A person gets one readable file that carries the report's true acceptance and its reviewer's
words. The accepted bytes, receipts and history are unchanged. The presentation is not reviewed
bytes, and it does not claim to be. A layout the plain text does not carry, such as a table, is
shown verbatim rather than redrawn.

Tests:

- The unit tests in `crates/review-pipeline/src/task/human_document/tests.rs` parse the output
  with comrak, a pinned development dependency. They require that a hostile corpus (fabricated
  and indented headings, setext underlines, HTML blocks and tags, javascript, image, reference
  and autolinks, emails, footnotes, table delimiter rows, emphasis, math and entities) reads back
  as exactly its text. The same holds inside nested and continued lists. They check that lists,
  examples and tables render as structure. They build real review chains of both profiles
  (an execution plan and revision of one Task, a captured Snapshot and Manifest, Attempt and
  kernel producers) as positive controls. Each link broken alone refuses the export: another
  check policy, plan or Manifest, a plan of another revision, a verifier policy the revision
  does not name, other requirements, a missing retained input (including an evaluation that does
  not retain nonempty captured sources), another producer or other invocation ports. An empty set
  of sources, bound or unbound at the Task, that the verifier did not receive is a positive
  control. They also refuse unrelated, uncounted or inconsistent evidence and a Document that
  does not render from its draft, check the closing note for a reviewed, an unreviewed, a failed
  and an unjudged report, render twenty thousand unclosed fences as text, and keep a fence line
  indented four columns, or beside a no-break, em or ideographic space, inside its example
  with those characters, while ASCII space and tab padding still opens and closes one. They
  read back exactly an indented example whose lines carry a no-break, em or ideographic space
  after their ASCII indentation, the same spaces at the start and end of prose lines, on a line
  of their own, after a list marker, in a continuation line and in a heading, and code spans
  whose ends hold spaces, tabs or other whitespace. Captured locations with named, numeric and
  hexadecimal entity-like query parts, literal `&amp;` and ordinary multi-parameter ampersands
  link to a URL the parser reads back exactly, both as one line and in a whole report export.
- `crates/af/tests/it/task_human_report.rs` drives the real CLI on the report starter, with an
  author that writes ordinary and Russian prose, paths, lists, examples (one indented, with
  no-break spaces after its indentation) and hostile input. It checks that the canonical
  `markdown` and `json` exports are unchanged, that the only link is the captured HTTPS source,
  that those no-break spaces read back as written, and that negative reviews and failed checks
  read as `changes requested`, and that the closing note of a report whose checks failed says
  no reviewer ran. It also checks that an existing file is never overwritten, that another output
  type is refused, and that the Task's state is byte for byte unchanged. A second captured
  source whose query holds `&copy;`, `&#169;`, `&#xA9;` and `&amp;` links to exactly that URL.
