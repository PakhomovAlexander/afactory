- Add a deterministic native nightly release Scenario with immutable tag input, asynchronous
  durable build/comparison/report/publication phases and a daily 03:00 Europe/Moscow Ant.
  Require the full verified native performance family; report measured regressions and block
  missing or incomplete evidence. Add explicit reviewed `baseline_ref` selection without fallback,
  offline deployment preparation and exact committed control/producer checks. Retire the old
  service chain and replace only the authorized nightly public signing identity. Refuse a selected
  source with a different embedded nightly key before build and on recovery/publication. See the
  [decision](../docs/adr/2026-11-16-orchestrate-native-nightly-releases.md).
- Authority compatibility: Scenario family v7, machine policy v6, adapter request v5, Ant/Pin/Tick
  v8, Ant State/List v9, inspection v4, Store v11 and nightly config v2 refuse earlier shapes.
  Finish or discard prior state and review new pins; interval scheduling and retention remain.
  Existing nightly clients can explicitly override the nightly public key; the stable key is unchanged.

Generic adapter receipt containers also advance to GitHub state v5 and Git workspace state v3
because they persist the new request version. Their signed publication authority is unchanged.
