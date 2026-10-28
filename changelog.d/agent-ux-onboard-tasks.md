`af onboard --tasks` now previews and adds pinned, model-driven Task authority. In a
new repository, `--apply` installs review and Task policy together. In a repository
already onboarded for review, it adds absent Task files and appends
`[task.defaults]` without changing existing review policy bytes. The command refuses
existing Task targets or a `[task]` table, and the missing-catalog error points to
onboarding.
