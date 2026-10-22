- Calibrate each performance case against the CPU group observed by its paired shard,
  allowing a hosted trial to use different allowed CPUs across shards. Keep whole-trial
  acceptance and all thresholds unchanged; unknown hosts still invalidate the trial.
  Readiness records advance to version 6 and refuse earlier versions
  ([ADR-0194](docs/adr/0194-bind-calibration-to-each-cases-observed-shard-cpu-group.md)).
