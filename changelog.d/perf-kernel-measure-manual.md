The guarded kernel experiment's refusal checks remain in the perf gate, while its expensive
judged comparison is now an ignored manual measurement. Run `make perf-kernel-measure` before a
pull request that changes the protected harness, experiment runtime or kernel receipts. The
measurement assertions and repetitions are unchanged.
