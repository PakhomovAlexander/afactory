# Contributing to Afactory

Thanks for helping. This file is the short version of how changes land; `AGENTS.md` and
`CONTEXT.md` hold the domain language and the working agreements in full, and `docs/adr/`
holds every design decision. Read the ADR that covers the area you are touching before you
change it.

## Toolchain

`rust-toolchain.toml` pins the toolchain (`1.88.0`, with `clippy` and `rustfmt`); `rustup`
installs it on the first `cargo` invocation, so there is nothing to choose. Edition 2024,
workspace version in `Cargo.toml`. The container probes need Docker; nothing else needs a
daemon.

## Before every pull request

```sh
make check
```

That is `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`,
`cargo test --locked`, and `fixtures/synthetic/generate.sh --check`. CI runs exactly this, so
a green local run is a green PR. Clippy warnings are errors; fix them rather than allowing
them.

If you touch `crates/review-sandbox`, also run the live probes:

```sh
make review-kernel-container-probes
```

They need a usable Docker daemon and stay outside `make check` because a missing daemon must
fail loudly there, never skip.

## Tests and fixtures

- Unit tests live next to the code; integration tests live in `crates/<crate>/tests/`, one
  file per subject (`capture.rs`, `crash_replay.rs`, `container_probes.rs`, …), with shared
  helpers under `tests/support/` or `tests/common/`.
- Synthetic fixtures under `fixtures/synthetic/` are generated: change the generator, run
  `fixtures/synthetic/generate.sh`, and commit the output; `--check` in `make check` refuses
  drift. Consumer fixtures under `fixtures/consumers/` are planned with the built binary by
  `fixtures/consumers/check.sh` and by the release workflow.
- A test that reproduces a bug goes in first and fails; the fix follows in the same PR.

## Design changes and ADRs

A change to a contract, a wire shape, a gate, a budget, a sandbox boundary, or the release
train is a design change and gets an ADR in `docs/adr/`. `docs/adr/README.md` is the
authority on the shape; in short: take the next free number (`0100-…` follows `0099-…`),
name the file `NNNN-kebab-case-title.md` with a short imperative slug, open with a status line
carrying the status and date (`**Status:** accepted (YYYY-MM-DD)` in most records), state the
context, list the considered options with the reason each was rejected, record the decision,
and end with `## Consequences`. Add the record to the index in `docs/adr/README.md`. An
accepted ADR is immutable: a changed decision is a new ADR that names what it supersedes. A
partially superseded ADR gains a status-line note linking the new one; a fully superseded ADR
is deleted with its index entry, and git history keeps it. Look at
`docs/adr/0045-one-release-train-and-a-pin-that-binds-bytes.md` for the shape. Reference the
ADR from the PR and from the CHANGELOG line.

## Commit messages

Imperative subject, under about 72 characters, describing the change rather than the activity.
The history mixes conventional prefixes with a scope (`fix(task): preserve validated Review
result number semantics`, `docs(task): record integrated fixes`, `perf(task): …`, `test: …`)
and plain imperative subjects (`Record Task retry and legacy context changes in a new ADR`).
Either is fine; the prefixes are used but not required. Domain terms keep their capitalisation
(`Task`, `Review`, `Snapshot`, `Gate`) as in `CONTEXT.md`.

## Pull requests

- One topic per PR. Split unrelated fixes even when they are small.
- `make check` passes; say so in the PR template checklist.
- A user-visible change adds a line under `[Unreleased]` in `CHANGELOG.md`; the release
  script turns those into the release notes.
- A design change links its ADR.
- Never weaken a contract, a fixture, a gate, a budget, or a sandbox boundary to make a test
  pass. If a test is wrong, say why in the PR; if the boundary is wrong, that is an ADR.
- Keep generated artifacts (fixtures, schemas) in sync in the same PR that changes their
  source.

## What does not belong here

This repository is the kernel and the `af` CLI. Project-specific pipelines, reviewer packages,
prompts, and provider bindings belong in the consuming repository's `.af/` tree; `af onboard`
and `af self` are how a consumer picks them up. A change that only one project needs is a
policy change there, not a code change here.

## Licence

Afactory is licensed under the Apache License, Version 2.0 (`LICENSE`). By contributing you
agree that your contributions are licensed under the same terms; there is no CLA and no
sign-off requirement.

## Security

Vulnerabilities go through private reporting, never a public issue: see `SECURITY.md`.
