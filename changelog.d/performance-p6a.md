- The performance laboratory now runs continuously through `.github/workflows/performance.yml`:
  pull requests to `main`, pushes to `main`, a nightly schedule and explicit dispatch all use one
  planner, runner, comparator and report, every script running from the trusted control
  revision. Gate branches are skipped, pull requests run only on ephemeral hosted runners with
  read-only permissions and no secrets, and enforcement stays advisory; before an activation
  exists the full check is unavailable and an advisory control bootstrap runs, and a policy
  change adds both proposed-policy bootstraps (`af.perf-schedule/1`). One credential-free job
  builds each run once and packs it with its modes for verified shards (`af.perf-build-pack/1`);
  shards upload their evidence failed or not, and the aggregation keeps missing shards
  incomplete. The report links validated diagnostic child runs and its rerun command keeps the
  frozen configuration (`af.perf-report/2`). Diagnostic runs now keep the scratch of an execution
  whose end was not confirmed, and count a raw trace against the evidence room while it is
  written
  ([ADR-0177](docs/adr/0177-schedule-continuous-runs-from-trusted-control-and-move-builds-once-to-bounded-shards.md)).
