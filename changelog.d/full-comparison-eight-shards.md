- The local executor guide's full release comparison runs the whole nightly family in 8 smaller
  serial shards (2932 slots, 4 serial originals of 2 shards) instead of the tier's 4: on
  v0.13.0 against v0.14.0-rc.1, a measured 4-shard first shard stopped at 738 of 776 slots
  inside its unchanged 45-minute window. Cases, pairs, windows, deadlines and the 12 manual
  telemetry runs are unchanged, and the failed run stays recorded as it is.
