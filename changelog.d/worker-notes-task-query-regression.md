- Preserve private Notes retrieval for real Task Attempt identities while retaining credential
  checks on Worker-authored Notes text and paths, following the
  [knowledge decision](docs/adr/2026-11-04-select-bounded-worker-knowledge-through-typed-ports.md).
- Add Task runtime regression coverage for private Notes queries across unrelated source changes,
  checking Worker reply admission, same-slot routing and current Head Delta context evidence.
