# Generic native experiment contract fixture

This fixture retains the `kernel/experiment` Pipeline and `release_build` measure contracts
of [ADR-0132](../../docs/adr/0132-measure-and-compare-source-candidates-in-the-kernel.md).
It is independent of the installed operator catalog. The shared implementer package comes from
[native catalog contract fixtures](../native-catalog-contracts/README.md), rather than a retired
installed directory. These fixture identities are not current operator selections or aliases.

```sh
af catalog test --source . --manifest fixtures/kernel-experiment/catalog.toml --json
```

`crates/af/tests/it/task_experiment.rs` installs the explicit generic authority and staged measure
into a disposable repository, checks the contracts and plans with isolated Providers and zero
Attempts. Native paired measurement and comparison tests remain separate from planning evidence.
The measured command is `scripts/measure-release.sh`.

For current operator work use `perf/research-experiment-codex` and its accepted research,
independent Review, local gates and measured acceptance in the
[versioned launch runbook](../issue-workflows/README.md). Installation remains root work;
this fixture supplies no execution or publication authority.
