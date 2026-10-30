- A SIGTERM or SIGINT to a local performance controller (`executor comparison-run`, the
  executor service) during a shard now cancels the whole controller. Before, the shard's own
  handler consumed the signal and the controller went on to the next shard and the next queued
  original. Now the shard seals its `interrupted` record, no later shard or original starts,
  and the original's scratch is removed. With that cleanup confirmed the original ends
  `interrupted` with its partial evidence and the machine fence settles; unconfirmed, it is
  `uncertain` and the fence stays. The original is never replayed. A shard run alone still
  just ends `interrupted`.
- A telemetry build record (`af.perf-telemetry-build/2`, inside `af.perf-telemetry-result/3`)
  accepts the build image exactly as build admission does (ADR-0170): a local image ID
  `sha256:<64 hex>` or a manifest digest `name@sha256:<64 hex>`. Before, a telemetry build in
  an image named by its local ID compiled both sides and was then refused at the record. A tag,
  a tag before a digest, an uppercase name or a short ID is still refused, and records and
  results of the previous versions are refused.
