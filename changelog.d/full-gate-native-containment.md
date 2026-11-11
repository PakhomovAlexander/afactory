- Run the existing live sandbox probes, timeout cleanup and exact native Task/Review containment
  test from full `make check` and `scripts/verify.sh --profile full`, with hard failure when a
  runtime or proof is unavailable. Preserve Rust-only `make test`, selective e2e execution and
  gate-owned reports; regress actual sequential/parallel full routing and failure propagation.
