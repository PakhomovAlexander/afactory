- Count each confirmed functional quarantine load spawn once, even during shutdown. Require the
  supervisor to acknowledge revoked launches. Missing status, launch failures and cleanup
  failures prevent a report. Bounded macOS signal retries leave time for reaping and group
  disappearance.
