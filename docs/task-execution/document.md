# Document Tasks and the credential-free starter

A document Task uses the same plan compiler, approval guard, scheduler, Attempt budget and Store
as an implementation Task. Its inputs and acceptance are specific to documents; it requires no
code policy, source-tree output or code-test substitute.

```text
Task: publish release notes
  inputs: requirements + captured document sources
                       |
                author Worker             Attempt 1
                       |
                structured draft
                       |
                Markdown renderer         pure operation
                       |
                exact Document
                       |
                source/content checks      protected Attempt 2
                       |
                  checks passed?
                    /       \
                  yes       no
                   |         |
          independent       negative receipt
          verifier          (no verifier call)
          Attempt 3          |
                   \        /
                acceptance assembly
                       |
                Task Result + Document
```

Create a starter in an absent directory:

```sh
af catalog init --profile document --destination notes-demo --json
cd notes-demo
git init
# Review the generated definitions, source data and policy.
git add .
git commit -m 'Configure the document Task starter'
af catalog test --source . --json
af task start --file document.json --json
af task output release-notes --port document --format markdown --output release-notes.md --json
```

The factory runs no Workers, accesses no credentials and creates no Git commit. It builds supported
typed definitions and computes their actual package and verifier-policy digests. `catalog test`
checks the committed interfaces without inference. The tutorial then runs three real command
Attempts: author, checks and an independent verifier. Two Attempts and their wall time are reserved
for verification before the author starts.

The supplied Python substitutes support the explicit release-note goal in `document.json`. The
author includes the captured changes; the verifier checks their inclusion, source revisions and
requested format. Other goals fail acceptance. These substitutes exercise the lifecycle without
credentials or paid inference. Broader authoring uses configured Workers with the same contracts
and independent bindings. The starter's local policy explicitly selects trusted-local isolation;
`require_container = true` requires supported container admission.

## Inputs and contracts

The Task file names `document_sources = "sources.json"`. That safe project-relative JSON or TOML
file is read from the selected captured source revision. It declares `af.document-sources/1` and
a bounded map of source entries with `title`, `uri`, `revision` and `text`. Source fields are data;
they cannot change tools, permissions, checks or Pipeline selection authority. Resume reads the
captured bytes even if the live file or catalog changes.

`af/DocumentDraft@1` contains a title, plain-text sections and named source citations. The renderer
escapes authored Markdown punctuation and creates `af/Document@1`, which records the exact draft
and sources artifact IDs. It emits links only for supported, syntactically safe captured HTTPS
locations. Other locations remain inert text. Checks require the configured sections and size,
citations where requested, and supported source-location syntax. They do not claim that a remote
URL was fetched or that an HTTP server is currently available.

The verifier receives only requirements, sources, the exact Document and its passing check receipt.
Its `af/DocumentEvaluation@1` names all four input artifact IDs. Missing or stale evidence cannot
accept another document. A negative verdict remains negative. A missing verifier yields incomplete
acceptance. The public `af/DocumentVerification@1` receipt binds the acceptance invocation, exact
Document and policy, check receipt and selected evaluation.

Compiler lineage ties checks, evaluation and public acceptance to the same Document through typed
`same_as` ports. Runtime receipt validation also checks exact artifact IDs. The installed document
profile treats any Worker producing a DocumentDraft as an author when enforcing verifier
independence. Removing an effect declaration cannot make the author its own verifier. Data-only
Workers start in a fresh empty environment and cannot publish filesystem changes through it.

## Inspection and output

```sh
af task explain release-notes --json
af task run release-notes --json
af task output release-notes --port verification --format json --output verification.json --json
```

Finished replay spends no further Attempts. `task output` writes one recorded output to an absent
file, leaving Task state unchanged. Markdown requires a Document artifact; JSON preserves an
artifact envelope. The command reports the Task's acceptance, so saving an incomplete or rejected
result does not promote its status. It never overwrites a file.

The real CLI tests cover successful capture, planning, execution, Markdown output and exact replay
after live source/catalog edits. Failure cases cover missing sections, unsafe links, stale Document
IDs, negative evaluation and a missing verifier. See the [starter walkthrough](starters.md) and the [issue adapter](issues.md). The
credential-free starter makes no live-model authoring claim.
