- The performance supervisor bounds every wait. If it cannot observe a child's exit without
  reaping it, it still enforces the deadline and cancellation and reports the exit as
  `unobserved` or reaped by polling, with no timed interval. A failed process enumeration, reap
  or kill makes the cleanup uncertain and keeps the original error; it is never `clean`. The
  shared `review_process::sample` refuses PID-namespace containment unless it runs as a
  namespace's PID 1. The supervisor result is now `af.perf-supervisor-result/2` and the
  capability report `af.perf-capabilities/2`. `cli.py capabilities --verify-scratch-enforcement`
  verifies a scratch bound only by the filesystem's own ENOSPC or EDQUOT after progress, with
  the filesystem then reporting itself full; I/O and permission errors, no progress, the fill's
  time bound and a failed cleanup refuse it. `cargo test --test perf_supervisor` builds the
  example it tests. On macOS, cache metadata normalization no longer goes through `/dev/fd`,
  whose lookup could fail intermittently when many processes ran at once
  ([ADR-0167](docs/adr/0167-bound-every-supervisor-wait-and-verify-only-what-was-observed.md)).
