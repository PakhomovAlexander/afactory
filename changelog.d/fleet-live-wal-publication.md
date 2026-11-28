- Fleet Project origins now respect SQLite's live WAL commit boundary, keeping unpublished
  Task decisions out of the inbox until SQLite publishes them or recovers the cold WAL.
