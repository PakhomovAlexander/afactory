# ADR-0152: Push the release pin with a deploy key that bypasses main's ruleset

**Status:** accepted (2026-10-19). Supersedes in part
[ADR-0138](0138-the-repository-pins-its-newest-release.md): the credential its `pin` job pushes
with, and the consequence that the pin commit starts no workflow run.

## Context

[Issue #163](https://github.com/PakhomovAlexander/afactory/issues/163). The repository ruleset
`main-protect` forbids deleting a branch, force-pushing to it and updating it without a pull
request, but from its creation on 2026-09-21 its branch condition included nothing, so it applied
to no branch and `main` was unprotected. On 2026-10-19 the owner pointed it at `refs/heads/main`.

ADR-0138's `pin` job pushes the pin commit straight to `main` with the workflow token, and its
consequences already said a ruleset requiring pull requests would refuse that push unless GitHub
Actions could bypass it. GitHub does not allow that here: in a repository owned by a personal
account, adding the GitHub Actions app as a bypass actor fails with "Actor GitHub Actions
integration must be part of the ruleset source or owner organization". The actor types that
remain are repository roles and deploy keys.

## Considered options

- **Leave `main` unprotected.** Keeps the push working, and keeps a `main` anyone with write
  access can delete or force-push. This is what #163 reported.
- **Let the repository admin role bypass, and push with the owner's personal access token.** A
  personal token kept as a secret carries the owner's authority over every repository it reaches,
  far more than one push to one branch needs, and the bypass would also cover the owner's own
  pushes from a workstation.
- **Open a pull request from the workflow.** Rejected by ADR-0138 for reasons that still hold: a
  pull request opened with the workflow token starts no checks, and it adds a second human act to
  a release that ADR-0045 keeps at one.
- **Push with a deploy key that the ruleset lets bypass.** A deploy key reaches this repository
  only, holds no user identity, and is usable only by a job that is given its secret.

## Decision

- **Deploy keys are the ruleset's only bypass actor.** `main-protect` targets `refs/heads/main`
  and keeps its rules unchanged: no deletion, no non-fast-forward update, and a pull request for
  every other update, with no required approvals. Its bypass list holds one entry, deploy keys,
  in mode `always`. No role, team or app may bypass.
- **One write deploy key, used only by the `pin` job.** Its private half exists only in the
  repository secret `PIN_DEPLOY_KEY`. The `pin` job checks `main` out with that key
  (`actions/checkout`'s `ssh-key`, which it keeps configured for the job), so the push in
  `scripts/pin-release.sh VERSION --push` goes through it. The job's own token drops to
  `contents: read`: it still reads the release's `SHA256SUMS` through `gh`, and pushes nothing.
  `scripts/pin-release.sh` is unchanged; ADR-0138's rule that only the pin may move still bounds
  what the key pushes.

## Consequences

- A push made with a deploy key starts workflows, unlike one made with the workflow token. The pin
  commit therefore starts the release workflow on `main` like any merge: `resolve` finds no new
  release, because the workspace version did not change, and its `check` job runs main CI on the
  pin commit. ADR-0138's "it starts no workflow run of its own" no longer holds.
- Every write deploy key of the repository can bypass the ruleset, not only this one. Adding
  another write deploy key is therefore a change to `main`'s protection and is reviewed as one.
- No workflow but the `pin` job reads `PIN_DEPLOY_KEY`. GitHub withholds secrets from workflows
  run for pull requests from forks, and `pr-report.yml`, which runs as the base branch under
  `pull_request_target`, does not reference it.
- If the key or the secret is removed, the job's push fails and `scripts/pin-release.sh` says to
  pin by hand: run it without `--push` and open a pull request, as ADR-0138 already allows. To
  rotate the key, replace the deploy key and the secret together.
- The ruleset lives in repository settings, not in the tree. A change to its target, rules or
  bypass list is recorded by a new ADR, as this one records the first.
