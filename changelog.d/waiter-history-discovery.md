Task wait now checks indexed Gate-history discovery across final document construction and
artifact verification, detecting new winning checks on other Tasks even when they previously
had no matching history or could not win. A bounded connection-local SQLite observation also
detects collection and row changes without adding history scans to unchanged polling. Exact
artifact checks, native Task semantics and public formats are preserved. See the proposed
[observation decision](../docs/adr/2026-11-11-bracket-native-task-display-observations.md).
