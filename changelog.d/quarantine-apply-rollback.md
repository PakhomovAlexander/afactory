- Keep the quarantine catalog intact and clean up newly published issue bodies if
  `test-quarantine.py apply --write` fails during publication. Rollback restores bodies replaced
  by independent writers during cleanup. If a new claim blocks restoration, apply keeps the
  replacement in staging, reports its recovery path, and removes the failed apply's staged bodies.
  Applies that share an issue directory serialize publication and rollback across catalogs.
  Concurrent writes to the same catalog are also serialized. A staging cleanup warning after
  publication leaves the apply successful and its issue commands available.
