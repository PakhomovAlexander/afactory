# ADR-0196: Recognize the Codex device prompt by its frame, not its version

**Status:** proposed (2026-10-22). Refines the Codex prompt clause of
[ADR-0139](0139-deliver-native-login-challenges-to-a-verified-private-requester.md); every other
credential, delivery, lifetime and completion fence of ADR-0137 and ADR-0139 stays in force.

## Context

ADR-0139 parses Codex's `login --device-auth` prompt as one complete, characterized frame. The
adapter held that frame as a single literal, and the literal included the version the CLI
displays in its banner, `0.159.2`. An official Codex 0.163.0 therefore prints the same prompt
and is refused as `unsupported` before any challenge exists (issue #305), while 0.159.2 still
yields one. Nothing else in the frame differs.

The prompt comes from `device_code_prompt` and `print_device_code_prompt` in
`codex-rs/login/src/device_code_auth.rs`. They are identical in
[rust-v0.158.0](https://github.com/openai/codex/blob/rust-v0.158.0/codex-rs/login/src/device_code_auth.rs),
[rust-v0.159.1](https://github.com/openai/codex/blob/rust-v0.159.1/codex-rs/login/src/device_code_auth.rs),
[rust-v0.159.2](https://github.com/openai/codex/blob/rust-v0.159.2/codex-rs/login/src/device_code_auth.rs)
and [rust-v0.163.0](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/login/src/device_code_auth.rs)
apart from the version interpolated at run time. The version is presentation, not protocol: the
URL, the code, the SGR positions and the phishing warning are what the adapter relies on.

This shows that pinning the banner version was brittle. It is not evidence about the cause of
any later browser or OAuth problem, which this decision does not address.

## Considered options

- **Add 0.163.0 beside 0.159.2.** Fixes today's release and breaks again on the next one, which
  changes nothing the adapter relies on. Rejected.
- **A minimum version, a maximum pin or an allowlist.** A minimum says older releases with the
  same frame are wrong, which the sources do not show; a maximum or allowlist recreates the
  brittleness. Rejected: no numeric bound in either direction.
- **Search the output for the URL and a code-shaped token, or strip ANSI first.** Turns an exact
  frame into a heuristic over arbitrary prose and terminal controls, which ADR-0139 forbids and
  which would accept injected links or tokens. Rejected.
- **Accept any text between the banner's brackets.** Lets a prerelease, build metadata,
  whitespace, controls or a token through a field no release fills with them. Rejected.
- **Fall back to an older parser, or claim an older version on failure.** A downgrade or
  spoofing path. Rejected; an unrecognized frame fails closed.

## Decision

1. The Codex frame is split around the displayed version: the fixed banner
   `\nWelcome to Codex [v` with its SGR, the version, and the fixed remainder through the code's
   opening SGR. Every byte other than the version and the user code must equal the
   characterized frame, with SGR in its exact positions.
2. The version is at most 32 ASCII bytes of digits and dots and must parse with the `semver`
   crate as a canonical stable `MAJOR.MINOR.PATCH`: no prerelease, no build metadata, no
   leading zero, no missing or extra component, and its canonical rendering equal to the bytes.
   There is no minimum, maximum or list of versions.
3. Streaming is unchanged in kind. While the bytes so far can still begin a valid frame, the
   adapter waits; a version longer than its bound is refused before its terminator arrives; a
   malformed version or frame is refused as `unsupported`; an exit before the complete frame
   fails. No challenge exists before the complete frame and its suffix.
4. Everything after the version is as before: the official HTTPS destination, the bounded
   uppercase/digit/hyphen code alphabet, the suffix, the total output bound, zeroization,
   duplicate and trailing-output refusal, process cleanup, successful native exit plus the
   separate token-free status probe before registration, and private delivery.

## Consequences

- Official 0.158.0, 0.159.1, 0.159.2 and 0.163.0, and later stable releases that keep the
  frame, yield a challenge; tests cover them, synthetic later versions and the malformed cases.
- A release that changes any other byte of the prompt still fails closed as `unsupported` until
  it is characterized. This is not a claim that every earlier or later CLI works.
- The stderr failure categories of the lifetime guard remain characterized against 0.159.2; an
  unrecognized diagnostic still falls back to `authentication_failed`.
