Selected native execution explicitly builds the entire plan's variants before its first
successful build receipt. Independent hosted class runs build only their own variants, so a
full plan no longer causes functional, e2e or uix jobs to compile perf-only features. Hosted
perf still completes both feature builds before its build receipt. Full-plan receipts,
inventory audits, required-build failure handling, process supervision and deadlines remain
unchanged. Focused fixtures cover full native execution, every hosted class and failed builds.
Hosted timing reports recognize only the four canonical outer class labels, preserving nested
`class-build` evidence without treating it as a fifth class or counting build time twice.
CLI report/aggregation fixtures cover zero durations, unknown missing outer timings, exact
selected receipts and refusal of missing, failed, duplicate or invalid outer timing evidence.
