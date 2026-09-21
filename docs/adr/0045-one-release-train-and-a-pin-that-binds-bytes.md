# One release train, and a pin that binds bytes

**Status:** accepted (2026-09-06) — implements the target design of the hub's release-lifecycle
audit of the same date and hub ADR-0007; revises ADR-0044 §2 (dispatch verified "against the
release checksums") and executes ADR-0043 (`.review/` is no longer read for new Campaigns).
Superseded in part by [ADR-0113](0113-ga-reads-only-what-ga-writes.md): the 0.7.1 `af_version` lock
shape, unsigned pre-0.8.0 releases, the 0.7.1 floor, and replay of `.review/` Campaigns.

Release `v0.7.1` shipped `af self` and dispatch, but three things stayed manual or unbound: a
release was a hand-edited version bump and a hand-pushed tag with no changelog, no signature, and
no `make check` on the tagged commit; a lock pinned a version *name*, so the on-demand install
trusted whatever `SHA256SUMS` the release served at that moment; and every consumer still held
the retired `.review/` layout with no command to leave it. The audit also found that the
installer never recorded an activation (the first `af self rollback` had nothing to return to),
that a downgrade below `0.7.1` stranded the user with a binary that has no `af self`, that
"latest" meant creation order in the installer and semver in the binary, and that two dispatches
installing the same pin raced.

The decision, in four parts that ship together:

1. **One release train.** `scripts/release.sh X.Y.Z --compat "…"` opens the release PR: the
   workspace version bump and a `CHANGELOG.md` section written from the pull requests merged
   since the last release, with a mandatory *Authority compatibility* line. Merging it is the
   only human act. `release.yml` runs on every push to `main`: a commit whose version is
   untagged *and* has its changelog section is tagged (annotated) by the workflow; a manually
   pushed tag enters the same path. The tagged commit runs `make check` on Linux and macOS,
   every target is built, every built binary plans the consumer fixtures, `SHA256SUMS` is
   signed, and only then is the release published. Build jobs have read-only tokens; only the
   tag and publish steps may write. Targets: `aarch64-apple-darwin` natively, and static
   `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` through pinned `cargo-zigbuild`
   and `zig` (the aarch64 binary is executed under user-mode qemu for its checks). No glibc floor.
2. **A pin that binds bytes.** `.af/af.lock` gains an `[af]` table: the release that wrote the
   lock and, under `[af.digests]`, the archive digest for every published target, copied from the
   release's verified `SHA256SUMS`. Dispatch installs a pinned release on demand only when the
   downloaded archive matches the lock's digest for this target; an installed copy must carry
   that digest in its receipt; a pin without a digest for this target is never installed on
   demand (`af self install` trusts the release checksums explicitly, `af onboard --refresh-lock`
   run online records every target). The 0.7.1 shape (`af_version` alone) is still read and is
   rewritten as the table. Only a receipted binary writes a pin: a source build pins nothing and
   leaves an existing pin untouched. `af onboard --refresh-lock --af VERSION` runs the refresh
   under that release, which is how a pin moves forward.
3. **Signed checksums, embedded key.** Every release from `0.8.0` on ships `SHA256SUMS.minisig`;
   the public key is committed at `crates/af/keys/release.pub` and embedded at build time;
   the secret key lives only in an Actions secret. `af self` refuses a release from that era
   whose checksums are unsigned or do not verify, verifies older releases when they happen to
   carry a signature, and accepts their bare checksums otherwise. A build without the key
   verifies checksums only and says so (`af self status`, `af --version --json`); the release
   workflow refuses to publish such a build.
4. **The floor, the first activation, one "latest", one installer at a time.** Nothing older
   than `0.7.1` is ever activated or dispatched to; a lock that pins one is treated as an older
   pin this binary cannot honour (it runs and says so), an explicit request is refused. The
   installer activates the binary it installed *through that binary* (`af self update --version`),
   so the activation is recorded and `af self rollback` works from the first update on. Both the
   installer and the binary pick "latest" by semantic version. Installs take a lock file under
   `versions/`. `remove` and `prune` keep the default and every version a project seen on this
   machine still pins.

With `.review/` no longer read for new Campaigns, `af onboard` on a legacy repository previews
the `.af/` it becomes — every pipeline with the format upgrades it needs, every reviewer package
those pipelines reference byte for byte (unreferenced ones are named and left behind), a project
file, a lock — and `--migrate --apply` writes it absent-only, leaving
`.review/` for the consumer to delete after review. Stored Campaigns whose manifests name
`.review/…` stay replayable. The kernel's own `.review/` directory is gone; the consumer fixture
mirrors the hub under `.af/`, and the old layout survives only as a test fixture for the migration.

## Considered options

- **cargo-dist** for the whole train — it generates an installer, checksums, a manifest, Homebrew
  and binstall metadata, and attestations from one config. Rejected for now: its workflow cannot
  be exercised before a tag is pushed, its installer could not fetch the then-private release,
  and it brings a second toolchain that must itself be pinned. The current workflow, extended,
  stays readable and every step of it runs locally. Revisit when the repository is public.
- **release-plz** for the release PR — conventional-commit driven bumps and changelogs. Rejected:
  the repository does not use conventional commits, breaking releases are decided by ADR rather
  than inferred, and a pushed tag from `GITHUB_TOKEN` would not trigger the tag workflow. A
  forty-line script that writes the same PR from merged pull-request titles keeps the human's
  one act and needs no extra actor.
- **GitHub artifact attestations** as provenance — keyless, verified by `gh`, which every
  consumer already has. Rejected while the repository is private on a plan that does not offer
  them; minisign has one key, one file, and a verifier of a few hundred lines.
- **Keep "verify against the release checksums" under a lock** (ADR-0044) — simpler, and the
  launcher already bound bytes the same way. Rejected: with `--clobber` uploads, a pin without a
  digest binds nothing; the lock already pins packages by digest, and the binary is one more.
- **A `dist-manifest.json`** beside `SHA256SUMS` — rejected as redundant: the signed checksum
  file already lists every asset and digest.
- **Windows and Intel macOS** — rejected for this release: neither has a consumer, the default
  symlink is Unix-only, and the installer now says so instead of promising a target that is not
  built.

## Consequences

- A release is: `make release VERSION=X.Y.Z COMPAT="…"`, review the PR, merge. Nothing else.
- Consumers pin bytes. The hub moves to `.af/` with `af onboard --migrate --apply`, pins
  `v0.8.0` through a receipted binary, and shrinks its launcher to bootstrap-only (hub ADR-0007).
- The release key must exist before the first `0.8.0` cut (`crates/af/keys/README.md`);
  until then the train refuses to publish, by design.
- The installer's own trust root is `gh` authentication over TLS plus the release checksums;
  it verifies the signature too only when `minisign` is on `PATH` and `AF_RELEASE_KEY` names the
  public key. Every later install goes through the binary, which always verifies.
- `AF_RELEASE_KEY`, like `AF_RELEASE_SOURCE`, is a developer knob that overrides a trust root;
  both are documented as such in `af help environment` and never read from a project layer.
- Releases before `0.8.0` remain installable (checksums only) and dispatchable down to `0.7.1`;
  nothing older can be activated. Linux archives before `0.8.0` were glibc builds under a
  different target name and are not dispatchable from a musl build.
