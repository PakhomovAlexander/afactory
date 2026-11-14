- Fix release CLI test fixtures copied from sealed read-only source: keep disposable schema
  files and owned directories writable and avoid copying required feature files twice. Cover
  contract edit, create and delete operations and release planning against read-only source
  files and directories without changing source bytes or permissions.
