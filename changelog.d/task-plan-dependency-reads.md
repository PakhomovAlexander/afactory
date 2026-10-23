- Task plan dependency reference checks reuse the validated typed CAS envelope, avoiding a
  repeated decode and identity validation while preserving lock checks, reference sets and
  existing errors ([ADR-0076](docs/adr/0076-decode-and-verify-typed-cas-reads-once.md)).
