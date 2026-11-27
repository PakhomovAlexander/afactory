- Retain the callable restricted Codex app-server transport, private authenticated loopback
  gateway and raw per-call collector hooks. The broker, native Task settlement and installed
  benchmark commands remain separate integration work; no live smoke or performance gain is
  claimed. See [the transport API and conformance commands](../docs/agent-bench-transport.md).
- Make gateway tool validation use the same multiline SSE event framing as raw usage capture.
  Credential-free regressions reject multiline forbidden calls, retain known usage and stop
  subsequent sends, with both LF and CRLF fixture coverage.
- Require successful installed app-server conformance and confirmed lifecycle cleanup on macOS
  under a pinned `sandbox-exec` deny-fork profile, with negative fork/spawn/subprocess probes.
  Register its ignored e2e identity as installed conformance without weakening policy drift checks.
