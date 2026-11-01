- Add a native Pi Provider using an embedded AF SDK host, pinned to Pi 1.4.0 with Node
  >=22.19 and explicitly registered `openai-codex` stored OAuth. Task, Review and doctor share
  exact account/model admission, confined file tools and partial usage accounting. Registration,
  status and model discovery are token-free; paid live smoke was not run. See
  [ADR-0271](../docs/adr/0271-own-the-pi-sdk-host-and-confine-its-tools.md).
- Resolve Pi test fixtures from the runtime Workspace when reusing compiled gate tests. Discover
  installed SDK test paths portably and verify nested writes and typed replies through a real
  SDK agent turn with mocked transport; report missing real-SDK coverage explicitly.
