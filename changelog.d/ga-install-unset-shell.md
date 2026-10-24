### Fixed

- `install.sh` no longer stops with `SHELL: parameter not set` where `$SHELL` is unset (containers,
  CI runners, cron). It prints the generic PATH line instead of a shell-specific one.
