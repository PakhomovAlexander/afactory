- The identity-recheck deadline test's fixture shell now publishes its PID marker by writing a
  temporary file and renaming it, so a deadline kill leaves no marker or a complete one, never
  the empty file that failed a kernel gate with `ParseIntError`; a new regression pins it
  (issue #278, [ADR-0155](docs/adr/0155-publish-a-fixture-marker-whole-or-not-at-all.md)).
