# Review campaign `task-contracts-p01-20260910`

- Runs recorded: 5
- Ledger round: 1
- Final verdict: fail (exhausted)
- Wall-clock: 7m45s
- Findings: 8 open, 0 pending, 0 fixed, 0 rejected, 0 wontfix, 0 contested

## Runs

| Run | Round | Epoch | Verdict | Tokens |
| ---: | ---: | ---: | --- | ---: |
| 1 | 1 | 1 | incomplete (5 missing nodes) | 14691 |
| 2 | 1 | 2 | incomplete (5 missing nodes) | 26117 |
| 3 | 1 | 2 | incomplete (5 missing nodes) | 26117 |
| 4 | 1 | 3 | incomplete (5 missing nodes) | 37543 |
| 5 | 1 | 4 | fail (exhausted) | 483057 |

## Spend

| Round | Epoch | Reviewer | Attempt tokens | Provider tokens | Total tokens | Round wall |
| ---: | ---: | --- | ---: | ---: | ---: | ---: |
| 1 | 1 | bugs | 0 | 5710 | 5710 | - |
| 1 | 1 | correctness | 0 | 3271 | 3271 | - |
| 1 | 1 | performance | 0 | 5710 | 5710 | - |
| 1 | 2 | bugs | 0 | 5710 | 5710 | - |
| 1 | 2 | correctness | 0 | 6 | 6 | - |
| 1 | 2 | performance | 0 | 5710 | 5710 | - |
| 1 | 3 | bugs | 0 | 5710 | 5710 | - |
| 1 | 3 | correctness | 0 | 6 | 6 | - |
| 1 | 3 | performance | 0 | 5710 | 5710 | - |
| 1 | 4 | bugs | 152628 | 5710 | 158338 | 7m45s |
| 1 | 4 | correctness | 147664 | 6 | 147670 | 7m45s |
| 1 | 4 | performance | 133796 | 5710 | 139506 | 7m45s |

### Attempts

- Round 1, **bugs**, Attempt `4fc6837f2d5369bd4bdb03aae6`: selected, 152628 tokens (cap 300000), 7m45s (in 1569646, out 15558, cache-read 1432576, cache-write 0, reasoning 12467)

- Round 1, **correctness**, Attempt `6b3e8a53541c1b44649bf23be0`: selected, 147664 tokens (cap 300000), 5m25s (in 578, out 24747, cache-read 2046842, cache-write 122339)

- Round 1, **performance**, Attempt `f06f61f3b4d8b64a5d280c394f`: selected, 133796 tokens (cap 300000), 5m38s (in 1097771, out 10745, cache-read 974720, cache-write 0, reasoning 8036)

## Demands

- **[required, open] Typed round-trip preserves content identity for every Task contract** (`sha256:ba8febf828e5cbe8bf47e7fd5c4f019ab360d53e11a8e231cf3a5321bd321b43`)
  - Why: ADR-0046 binds approval and resume to an immutable plan identity; the current identity test only canonicalizes raw JSON values with single-element sets, so re-serialization drift is unmeasured.
  - Suggested method: For each positive fixture, deserialize into the typed struct, serialize back with serde_json::to_value, and assert content_id equality; additionally permute every set-typed array in the fixture and assert admission either rejects the permutation or yields the same content_id.
  - Source: correctness

## Findings

### Blocker

None.

### Major

- **[open, scope=in, severity=major, effective=major] Satisfied results can contain an empty required output** (`sha256:de8f7f5493b29fc86e2a06c93feda935c7697939bdb0824a4c83e572b8e38415`) at `crates/review-core/src/task.rs:427`
  - Body: TaskResultV1 reuses ArtifactInputV1 validation for outputs. A result with acceptance="satisfied", nonempty evidence, no missing obligations, and an output such as {"artifact_ids":[],"artifact_type":"af/CheckedDocument@1","cardinality":"many"} passes both the schema and Rust validation. That output contains no artifact, so a missing required output can be recorded as satisfied instead of retaining exit 4 as ADR-0046 requires.
  - Fix: Add required-output-specific validation that rejects empty artifact_ids for every TaskResultV1 output. Give result outputs a schema definition with minItems: 1 while retaining empty many-valued Task inputs if intended, and add a negative parity fixture for this satisfied-result case.
  - Reports: bugs round 1 scope=in at crates/review-core/src/task.rs:427 `sha256:80dd911f04caed7757ab435a6f2a1b7fabde37ff191f270dbbddcae06f2954f9`
  - Resolution/history, round 1: reported - (no note)

- **[open, scope=in, severity=major, effective=major] The same exhausted result validates as both exit 3 and exit 4** (`sha256:a0eb580f585f2efa26a599da2bc4b7cbe47b59e936e134a9b926b2aeebd33a24`) at `crates/review-core/src/task/review.rs:216`
  - Body: For execution=Exhausted and acceptance=Inconclusive, both ConvergenceExhausted and Incomplete pass validate_result(), but their exit codes are 3 and 4 respectively. A missing-output exhaustion can therefore be labeled convergence exhaustion and lose the mandatory incomplete precedence. The cross-product is not tested; the existing test exercises only one label for each intended combination.
  - Fix: Make the combinations mutually exclusive: require Unsatisfied acceptance for ConvergenceExhausted, and require Inconclusive acceptance plus non-completed execution for Incomplete. Add a cross-product regression proving Exhausted/Inconclusive is accepted only as Incomplete and retains exit 4.
  - Reports: bugs round 1 scope=in at crates/review-core/src/task/review.rs:216 `sha256:dc20a055d24de1dc39967ea4271d6c788effaba899aa6885d56f020fcd2c2fbf`
  - Resolution/history, round 1: reported - (no note)

- **[open, scope=in, severity=major, effective=major] Valid set ordering is silently changed across typed round-trips** (`sha256:569cb4877873a42b9abf9391ca59d11030ebba26f4ce9bf2464ff8d84eec1a15`) at `crates/review-core/src/task.rs:60`
  - Body: unique_set accepts any unique array order and stores it in a BTreeSet, which serializes in sorted order. For example, a schema-valid authority with allowed_effects=["write-source","read-source"] validates, but deserializing and reserializing produces ["read-source","write-source"], changing its canonical content ID. The same problem affects evidence, missing obligations, coverage sets, independence sets, checks, applicability kinds, and plan acceptance producers. The identity test hashes raw fixture Values and never performs a typed round-trip, so it misses this exact-identity break.
  - Fix: Require incoming set arrays to already use one documented canonical ascending order before constructing each BTreeSet, then add unsorted semantic-negative fixtures and typed deserialize/serialize/content-ID identity tests for every Task contract containing a set.
  - Reports: bugs round 1 scope=in at crates/review-core/src/task.rs:60 `sha256:0458af8267ef45c17d9b92cbf5835393d80acf6df9e0c05407db79c5704d635a`
  - Resolution/history, round 1: reported - (no note)

- **[open, scope=in, severity=major, effective=major] Set fields accept unsorted wire order but re-serialize sorted, so one typed value has multiple content identities** (`sha256:520a73bfba9d5a7a6da54646bdf56884634f6c05a8a95e6b5231707a8baa9282`) at `crates/review-core/src/task.rs:68`
  - Body: `unique_set`/`unique_set_map` deserialize into `BTreeSet`, rejecting duplicates but accepting any element order. `Serialize` then emits the sorted order. Canonicalization in review-store keeps array order (`write_value` for `Value::Array`), so `allowed_effects: ["write","read"]` and `["read","write"]` are the same `TaskAuthorityV1`/`ExecutionPlanV1` but produce different `content_id`s. This affects `TaskAuthorityV1.allowed_effects/data_destinations`, `TaskResultV1.evidence/missing_obligations`, `PipelinePortV1.covers`, `WorkerSlotV1.independent_from`, `PipelineApplicabilityV1.kinds`, `TaskOperatorV1::Check.checks` and `ExecutionPlanV1.acceptance`. Path: persist a plan whose wire form has an unsorted `allowed_effects`, approve its envelope id, then any component that loads the typed plan and re-persists it (resume, Store normalization, compiler output) yields a new plan_id and `PlanDecisionV1::approves` no longer matches, contradicting ADR-0046's 'same immutable plan' resume rule. Every existing kernel wire contract avoids this by requiring sorted unique Vecs (`ChangeSetV1::validate_scope_shape`, `CacheManifestV1`); no pre-existing public type uses `BTreeSet` on the wire. The identity test only canonicalizes the raw `Value` and all fixture sets have one element, so the drift is untested.
  - Fix: In `unique_set` (and via it `unique_set_map`) require strictly ascending input: keep `last` and return `Err("set entries must be sorted and unique")` when `item <= last`. Add negative fixtures (layer `semantic`) with unsorted `allowed_effects` and unsorted `acceptance.checked` producers. Extend `task_contract_identity.rs` to deserialize each positive fixture into its typed struct, `serde_json::to_value` it and assert the same `content_id`, and add a schema-side `"$comment"` or doc note that set arrays are canonical ascending.
  - Reports: correctness round 1 scope=in at crates/review-core/src/task.rs:68 `sha256:5d28ed8be3d694d0bffbd1b2efa6e5c007284b0e1bbccb1c809b2d739fbe21da`
  - Resolution/history, round 1: reported - (no note)

- **[open, scope=in, severity=major, effective=major] Expanded negative fixtures consume roughly 50k avoidable review tokens** (`sha256:5df129ada210afbea1aa08f455a476ebc240a4a4ec7d31ba7fc869e77c20fd2c`) at `fixtures/task-contracts/v1/negative.json:1`
  - Body: This 66,184-byte file repeats complete payloads for 40 mostly one-field negative mutations. It accounts for about 32% of the 204,023-byte Change Set and is delivered to every required reviewer. Under the repository's four-bytes-per-token context estimator, this contributes roughly 16.5k estimated input tokens per reviewer, or 49.6k across the three first Attempts; retries multiply the cost. The repeated payloads also increase every checkout, parse, and future change review without adding independent information.
  - Fix: Replace the expanded values with a compact language-neutral mutation corpus naming a positive base fixture, JSON Pointer, mutation operation/value, layer, and reason. Expand those recipes in the parity test before schema and Rust validation, and add a deterministic check that every recipe produces the intended invalid payload.
  - Reports: performance round 1 scope=in at fixtures/task-contracts/v1/negative.json:1 `sha256:9dc84cd37e869f3313ee7aed2ba77aa73a88f70585ea7669347b783b67ac892e`
  - Resolution/history, round 1: reported - (no note)

### Minor

- **[open, scope=in, severity=minor, effective=minor] Default independence policy can never be satisfied by a Command Worker pair or a Command/Model pair** (`sha256:4953c3abc5a6494f95a3822a94dc0a3685b8069dc27168568d40b08662347abb`) at `crates/review-core/src/task/plan.rs:142`
  - Body: The fallback arm requires all three `distinct_*` flags to be false whenever either binding is `Command`. `IndependencePolicyV1::default()` sets `distinct_principals: true`, so under the default policy two distinct-package Command Workers (or a Command verifier independent from a Model author) always fail with a message about Provider diversity even though no Provider distinctness was demanded. ADR-0046 says Command Workers 'require no invented model or Provider identity' and that independence rests on distinct effective package digests plus session isolation. To admit any Command verifier a project must relax `distinct_principals` globally, which also relaxes it for Model/Model pairs. The test `provider_aliases_and_multi_role_packages_cannot_bypass_independence` locks this in.
  - Fix: Treat `Command` bindings as satisfying principal/provider/model distinctness by package identity: in the mixed and Command/Command arms only enforce `a.package_digest != b.package_digest` (already checked) and return `Ok(())`, or add an explicit policy field `command_workers_by_package: bool` to `IndependencePolicyV1` (and the orphaned `independence` schema def) so trusted policy states the choice. Adjust the last assertion in the independence test accordingly and add a Command/Command positive case.
  - Reports: correctness round 1 scope=in at crates/review-core/src/task/plan.rs:142 `sha256:4dd40cfe4447ff17550b4d2907c0ba04a829acf7067ad9aa9473967ebaa689fe`
  - Resolution/history, round 1: reported - (no note)

- **[open, scope=in, severity=minor, effective=minor] Semantic-layer negative fixtures are never asserted to pass the schema, so the shape/semantic taxonomy is unverified** (`sha256:6dbcc88a6831dc572398f78c1a248ca42c93597ae6a6ad5edeaa986d7371be43`) at `crates/review-core/tests/schema_parity/task_contracts.rs:57`
  - Body: `task_contract_positive_and_negative_fixtures` runs `assert_invalid` only for `layer == "shape"` and runs Rust rejection for all cases. A case labelled `semantic` that actually fails the schema (or a schema tightening that starts rejecting it) goes unnoticed, which defeats the README's claim that 'semantic cases must fail Rust cross-field validation; JSON Schema alone cannot prove those invariants'. Fixtures README and ADR-0046 cite this split as the parity evidence.
  - Fix: Add an `else { assert_valid(&format!("{contract}-v1.json"), value); }` branch so semantic cases must be schema-valid and Rust-invalid, and panic on any `layer` value other than `shape`/`semantic`.
  - Reports: correctness round 1 scope=in at crates/review-core/tests/schema_parity/task_contracts.rs:57 `sha256:75ebbba7c2352e11fb60cb99851083fc8404184a43c8d4b5f040a4e414b0a3f7`
  - Resolution/history, round 1: reported - (no note)

- **[open, scope=in, severity=minor, effective=minor] Every schema assertion reparses and recompiles the shared Task schema** (`sha256:d3a72ae86a32dcae27a56b736f18491e391217093e86955a9b85c6fb663a7a1c`) at `crates/review-core/tests/schema_parity.rs:102`
  - Body: Each validator() call rereads and parses all three resources, including the new 34,791-byte task-contracts-v1.json, and recompiles the requested schema. The 52-schema validity loop plus the new Task fixture assertions invoke this helper at least 106 times, so this change alone reparses at least 3,687,846 bytes of identical Task-schema text, even for unrelated legacy schemas; other existing assertions increase the count further.
  - Fix: Load shared resources once and reuse compiled validators by schema name. At minimum, register task-contracts-v1.json only for the ten wrappers that reference its URN and prebuild one validator per Task contract for reuse across its positive and negative cases.
  - Reports: performance round 1 scope=in at crates/review-core/tests/schema_parity.rs:102 `sha256:4600db77a65f85cb0ef4bb59517b271de6c2a53b2a29bb06bd99b05a203d4af4`
  - Resolution/history, round 1: reported - (no note)

### Recorded, not blocking this Subject

None.
