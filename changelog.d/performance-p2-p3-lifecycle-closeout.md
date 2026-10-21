- The performance laboratory no longer kills only a sample's supervisor when its backstop or
  the run's total deadline expires. The supervisor is sent SIGTERM and cancels the sample
  through the same supervision: a running child or setup is ended and verified gone, and a
  child not yet launched never is. A launch the deadline could not cancel in time is not made.
  When nothing confirms that a sample ended, the shard stops (`cleanup_uncertain`). It then
  keeps the sample's private scratch and the shard's seeds for recovery and names them in
  `shard.json`. A diagnostic `cli.py probe` keeps its scratch the same way. The run's total
  deadline now also bounds a shard's preflight: each Git read of the authority, the container
  runtime query and the scratch enforcement fill ends 30 s before it. A step the deadline ended
  refuses the shard as `run_deadline`, and no later step starts. The `task show` oracle now
  requires the Task history to be numbered exactly 0..9999, each once and in order, over the
  whole output, which the supervisor observes past any retained log. The supervisor request,
  supervisor result and CLI registry contracts are now `af.perf-supervisor-request/3`,
  `af.perf-supervisor-result/4` and `af.perf-cli/4`, and earlier versions are refused
  ([ADR-0171](docs/adr/0171-cancel-samples-through-their-supervisor-carry-the-deadline-through-preflight-and-observe-history-order.md)).
