- Fix macOS builds of the Project-origin reader by comparing SQLite WAL lock types using the
  native `flock` field type on macOS and Linux.
