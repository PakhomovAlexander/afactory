# Migrating `.review/` to `.af/`

Repositories that onboarded before `.af/` existed carried `.review/pipelines/*.toml`,
`.review/review.lock` and `.review/reviewers/`. Since `v0.8.0` that layout is no longer read for
new Campaigns ([ADR-0043](adr/0043-drop-legacy-review-authority-in-v0-8-0.md), executed by
[ADR-0045](adr/0045-one-release-train-and-a-pin-that-binds-bytes.md)); `af review plan|run` refuse
a `.review/…` pipeline path and name the command that moves the policy. Stored Campaigns whose
manifests recorded `.review/…` paths stay replayable.

## What `af onboard` does

- `af onboard` on a repository with `.review/` and no `.af/` previews the `.af/` it becomes:
  every pipeline with the format upgrades it needs applied (comments intact; `heavy` becomes
  `review`, the `.af/` default), every reviewer package those pipelines reference byte for byte
  as a Worker package (unreferenced packages are named and left behind), a project file, and a
  lock pinning Workers, pipelines, and — when a receipted release runs the command — the `af`
  release itself. It writes nothing.
- `af onboard --migrate --apply` writes that `.af/` atomically and only when absent, leaving
  `.review/` in place. Review the diff, commit it on the trusted base branch, delete `.review/`,
  run `af onboard` to validate, then `af review plan`.
- Scaffolding `.af/` beside `.review/` is refused so a repository never carries two authorities.

## What is preserved

Persisted `review.kernel/*` types, events and stored Campaign replay are untouched. Consumers run
their pinned release's `af review plan` after every pin bump, and the release workflow plans
`fixtures/consumers/` with every built binary so a format change that would reject a consumer's
policy is caught before the release is published.
