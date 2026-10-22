- The terminal browser's integration tests pass the parent's `TMPDIR` to `af` when it is set, as
  they pass `PATH`, so they no longer fail before their assertions on a host whose default
  temporary directory has less free space than af requires; af's requirement is unchanged.
