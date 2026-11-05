- Add native implementation preparation, signed commit/PR publication and owned cleanup Actions,
  a reusable `impl` bundle/package builder, an executable correctness reviewer contract and up to
  two bounded implementation repairs on the same PR. Completion requires real human merge and
  safe cleanup; changed-base conflicts remain unresolved when their source evidence is unavailable.
  See the [lifecycle decision](../docs/adr/2026-11-05-bind-implementation-lifecycle-to-native-scenario-authority.md).
- Use `gpt-6-sol` high for the required executable correctness reviewer. Discard warm Cargo
  homes containing installed subcommands so they cannot override a pinned Rust toolchain.
- Keep truncation-size formatting compatible with the pinned Rust 1.88 toolchain and newer
  Clippy's divisibility lint, without suppressing warnings or changing the toolchain pin.
- Include `scenario impl-source` in `af help --json` with its token-free file-write effects,
  and keep lifecycle help examples copyable within the terminal width contract.
- Recover a journaled empty owned worktree reservation after interruption, refusing changed
  directories and partial Git registrations. Exercise the crash window with real local Git.
- Select the generated pipeline's reviewed implementation guarantee in the shipped impl fixture,
  and remove only published schema metadata from local Worker copies. Cover generated package
  admission with native Task planning and Scenario template pinning.
