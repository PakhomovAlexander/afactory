# Preflight export fixture

`review-plan.json` is an actual `af 0.9.0-rc.8` token-free export from a disposable
review repository containing the helper patch and its native inspection fixture:

```sh
af review plan --repo af-preflight-review --policy-rev HEAD \
  --base HEAD~1 --candidate HEAD --provider design=claude-personal --json
```

It captures a nonempty Diff over the six helper/documentation changes. Identities and provider
labels are recorded data, not executable configuration or secrets. Tests consume the real
export, then mutate its identity and empty-Diff fields to cover refused combinations.
Native `af review plan` refuses an empty Diff before emitting a successful plan; that case is
therefore deliberately a malformed/mutated fixture, not a claimed successful native capture.

The helper checks the fields it uses; this fixture does not define or authenticate the full
Review plan format. The Task side uses `../bound-inputs/inspection-bound.json`, whose optional
evaluation input and conditional verifier must appear as unknowns in the advisory report.
