# ADR-0179: Link only sanitized views, and bind a diagnostic child to its parent's workloads and its frozen recipe kind

**Status:** accepted (2026-10-20). Closes the diagnostic integrity findings left on package p6a
of the [continuous performance laboratory](../design/continuous-performance.md) by
[ADR-0178](0178-admit-linked-diagnostic-views-by-their-renderer-and-read-dispatched-branches-where-the-checkout-fetched-them.md),
whose decisions stand except where this ADR amends them. The run plan becomes `af.perf-run/4`.
The report stays `af.perf-report/2`, and the profiles stay `af.perf-profiles/4`.

## Context

Both semantic reviewers of the ADR-0178 source recorded three distinct issues, and root checks
reproduced each one with typed synthetic data:

1. **An active original is linked** (`report.py:320`, `:324`). `_admit_views` holds each
   sanitized flamegraph to `svgsafe.sanitize(original)` and lets the original keep active
   content. inferno's `--no-javascript` output does keep it: a `<script>` element and an
   `onload` handler. `diagnostics.page` still linked that original as "inferno's original SVG",
   and `copy_child` published it beside the page. The page's CSP does not govern an SVG that is
   opened separately. A `download` attribute does not make an active SVG a script-free view
   either.
2. **A case's IDs are taken for its workload** (`report.py:296`). `_consistent` compared only
   the instance and case IDs of each selected child case. The run contract never derives an
   instance ID from its parameters. A child plan that kept the instance
   `authority_plan_check__package_issue_255_large` but measured package `issue_255_medium`,
   with its digest and manifest recomputed, was admitted under the parent's case.
3. **A kind no frozen document states** (`report.py:279`). `profiles.json` declares
   `recipe.kind`, and the report classifies a child's evidence by it. The frozen child plan
   carried the suite-shaped recipe, with its ID, digest, features, symbols and frame pointers,
   but no kind. The report could check the claim against nothing. A no-capture trace child
   relabelled `cpu_profile`, or a CPU-profile child relabelled `trace`, with its page and
   manifest recomputed, was admitted and classified as the other kind.

## Considered options

1. **Keep the original link with a warning, or link it only as a download.** Rejected: the
   link is still navigable, and an SVG opened on its own runs its script.
2. **Drop the original, or refuse inferno output that keeps active markup.** Rejected: the
   original is the evidence the sanitized view is checked against, and a valid inferno render
   always keeps that markup.
3. **Compare the child cases by instance and case ID.** Rejected: this is the finding.
4. **Require each child case to equal its parent's case exactly.** Rejected: a diagnostic plans
   its own pairs, orders and shard, so every legitimate child would be refused.
5. **Infer the kind from the recipe ID's suffix or its features.** Rejected: configured recipes
   are elastic. `perf-control` serves both CPU profiles and counters, and an ID is only a name.
6. **Carry the registry in the child bundle.** Rejected: nothing offline anchors those bytes
   that does not also anchor the profiles. The plan is the child's frozen, digest-checked record.

## Decision

1. **Only sanitized SVG is a view** (`diagnostics.page`).
   - **The page.** It embeds and links each sanitized flamegraph. It names inferno's original
     by path and SHA-256 as archived evidence, with no link to it.
   - **The archive.** The original's exact bytes stay in the child bundle and its verified copy
     below the report. Admission still refuses an original the sanitizer refuses and a
     sanitized copy that is not what the sanitizer makes of it.
   - **What stays.** Baseline, candidate and both difference directions remain available.
     Because a child's page must be exactly the page the trusted renderer draws, a page that
     links an original is refused.
2. **A child case is its parent's workload** (`report._same_workload`).
   - **Equal fields.** Each selected child case must equal its parent case in every field
     except `DIAGNOSTIC_CASE_FIELDS`. The fields that must match include parameters, seed,
     kind, family, timing, cache, oracle, execution, AF environment, metrics, tolerance,
     criticality, warmups, sample timeout and budgets.
   - **Allowed differences.** These are the fields a diagnostic plans for itself: `pairs`,
     `rank`, `alpha`, `tail_probability`, `shard`, `warmup_orders` and `measured_orders`. The
     capture bounds and recipe are the plan's `diagnostic` and `profile`, checked as before.
   - **Fails closed.** A case field added later must match until it is explicitly allowed.
3. **The plan freezes the recipe's kind** (`af.perf-run/4`).
   - **The field.** `diagnostic.recipe` is `{kind, base_recipe, tools}` from the registry
     entry, or null for a diagnostic plan that froze its parent's verdict recipe
     (`plan --scope diagnostic`). It is covered by the plan digest.
   - **The writer and the reader.** `planning._diagnostic` freezes the field, and
     `validate_run` holds it to the kind's registry rules (`diagcontracts.check_kind`, shared
     with the registry) against the frozen recipe. `Profile` refuses a plan that does not
     freeze the registry's entry and writes the frozen kind into `profiles.json`.
   - **The report.** It refuses a child whose plan froze no diagnostic recipe. It also refuses
     a child whose profiles claim another kind or base recipe than the plan froze, and a child
     whose evidence is not of that kind: a CPU profile holds only captures and renders, a trace
     only traces and counters only counters. Children are classified by the plan's frozen kind.
   - **Earlier versions.** `af.perf-run/3` and earlier are refused, with no reader for them.
4. **Golden comparisons, generated runs.** Every comparison fixture's sample records name the
   plan digest, so moving to `af.perf-run/4` would rewrite about 1.4 MB of committed samples.
   Only `fixtures/performance/compare/index.json` and each `verdict.json` stay committed. The
   fixture test generates the run bundles with `compare_fixtures.write_all`, compares their
   verdicts and exit codes with the goldens, and checks that each golden names the generated
   bundle's manifest identity, which pins every generated byte.

## Consequences

- **Tests.** `test_report_diagnostics.Admission` adds:
  - **An active original.** A positive captured child whose originals hold inferno's `<script>`
    and `onload` is admitted. The originals are archived byte for byte and linked from neither
    page. Every SVG either page links or embeds is a sanitized view, script-free and unchanged
    by the sanitizer. All four flamegraphs remain available.
  - **Workloads.** Hash-consistent negatives change one selected case's parameters under the
    same IDs, its seed's writer contract, its seeded parameters, its timing region, its oracle,
    its AF environment and its cache. Each one is a valid plan, and each is refused. A positive
    child with its own orders is admitted.
  - **Kinds.** The root no-capture trace-to-`cpu_profile` relabel and the reviewer's
    `cpu_profile`-to-`trace` and `cpu_profile`-to-`counters` relabels are refused with the page
    redrawn and the manifest resealed. So are a plan relabelled against its own recipe, which
    its writer refuses to write and its reader refuses to read, a plan that froze no diagnostic
    recipe, and a counters child relabelled `cpu_profile` in both documents, whose evidence
    alone shows what it is. CPU-profile, trace and counter children keep their classifications.
  - **Shared assertion.** `test_report.assert_safe_page` now requires every linked or embedded
    SVG to be script-free and a fixed point of the sanitizer.
  - **Mutation check.** With the three checks disabled, or the original linked again, the
    twelve new refusals and the positive SVG assertion fail.
- **Fixtures.** `schema/run` moves to `af.perf-run/4`. It adds
  `valid-diagnostic-trace-recipe.json` and `invalid-diagnostic-kind-not-its-recipe.json`, and
  `invalid-version.json` now holds `af.perf-run/3`. `schemas/performance-run-v4.json` replaces
  `-v3`.
- **Pending operational evidence.** Unchanged from ADR-0177. No real capture, browser,
  hosted run, calibration or profile is inferred from these fixture tests.
- **Not changed.** The 15-second lease, the 8-MiB envelope cap, the renderer's CSP, the
  sanitizer, the parent's verdict and bundles, the child bundles, branch resolution, build-once
  transport and exact reproduction.
