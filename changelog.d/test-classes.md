PR validation now defaults to functional, e2e and CLI/TUI interaction tests, with performance checks
available through explicit class labels. A reviewed catalog partitions every Rust test and Python
check; stable validation gates require all selected work and report excluded classes as NOT RUN.
Local `make check` stays full. Release PR labels directly control validation without a metadata commit;
first tag creation captures the single exact merged release PR and freezes its provenance and plan
in SHA/version-bound annotated tags,
with unconfigured/manual tags defaulting full and packaged binary/archive/signature smoke always on.
Per-class JUnit and step reports distinguish execution time, summed test-seconds and runner cost,
and provide clearly labeled historical replay estimates. Separate Performance measurements follow
PR selection, report exemptions explicitly and remain asynchronous after release publication.
`make test-labels` previews idempotent label setup (explicit `APPLY=1`); release preparation checks the
profile label before mutations, and sealed AF gates keep Python evidence outside the source.
Fresh CI checkouts create the configured timing directory before recording; class runner costs and
shares count only class intervals, with base/container work reported separately.
