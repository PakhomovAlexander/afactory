Telemetry experiments now refuse negative, nonfinite or unrepresentable physical measurements
before summarization, including individual samples hidden by a valid median. Result v2 retains
a bounded sanitized original numeric token, precise field path and source identities with a
side/round/experiment diagnostic, then publishes a typed partial or failed result using the
existing cleanup and verification boundaries. Signed derived differences remain valid. Offline
verification replays retained refusals; upload and collection still require verified bundles.
Older result shapes fail closed without fallback. Stand-in regressions cover admission,
finalization, tampering and incomplete-result retention; the original hosted failure's field
remains unknown.
Refused paths retain complete numeric property names such as `machine.load1` and resolve only
canonical bounded paths through the exact schema. Original nonzero tokens cannot underflow
silently to zero, including negative raw latencies and signed derived figures; signed zero
remains valid. Regressions cover mixed/all-refused load results, original-byte drain/read
underflow, masked samples, long exponents, schema parity and offline/upload tampering.
