"""Hard resource maxima of design section 14. Run input may raise a default only up to these."""

KIB = 1024
MIB = 1024 * KIB
GIB = 1024 * MIB

PAIRS_MIN = 12
PAIRS_MAX = 200
WARMUP_PAIRS_MAX = 10
INSTANCES_MAX = 64
SAMPLE_TIMEOUT_SECONDS_MAX = 120
BUILD_MINUTES_MAX = 45
# One build step's retained log; the rest is counted as truncated.
BUILD_LOG_BYTES_DEFAULT = 4 * 1024 * 1024
BUILD_LOG_BYTES_MAX = 16 * 1024 * 1024
SHARDS_MAX = 16
CONCURRENT_JOBS_MAX = 8
SAMPLE_LOG_BYTES_MAX = 1 * MIB
SHARD_BUNDLE_BYTES_MAX = 512 * MIB
BUILD_ARTIFACT_BYTES_MAX = 4 * GIB
# A packed build directory moved from its build job to the shards (ADR-0177): the artifacts
# within their bound, and the build's manifests and bounded logs beside them.
BUILD_PACK_EXTRA_BYTES = 256 * MIB
BUILD_PACK_ENTRIES_MAX = 4096
WORKTREE_BYTES_MAX = 80 * GIB
FREE_DISK_FLOOR_BYTES = 10 * GIB
RECORD_BYTES_MAX = 64 * KIB
SENTINEL_PAIRS_MAX = 20
# A side-written seed's admission bounds (ADR-0172); its logical bytes are bounded by the
# environment's scratch limit, which every sample's private copy must fit. The largest declared
# seed, the committed repository of 10,000 source files, holds about 20,500 entries.
SEED_ENTRIES_MAX = 100000
SEED_DEPTH_MAX = 64

# Sampling and total wall time, minutes, by tier class.
QUICK_SHARD_SAMPLING_MINUTES_MAX = 30
QUICK_RUN_TOTAL_MINUTES_MAX = 120
LONG_SHARD_SAMPLING_MINUTES_MAX = 90
LONG_RUN_TOTAL_MINUTES_MAX = 180

# Separate diagnostic child runs (package p5, ADR-0174).
DIAGNOSTIC_CASES_DEFAULT = 3
DIAGNOSTIC_CASES_MAX = 8
DIAGNOSTIC_SECONDS_DEFAULT = 10
DIAGNOSTIC_SECONDS_MAX = 30
DIAGNOSTIC_HZ_DEFAULT = 997
DIAGNOSTIC_HZ_MAX = 1999
DIAGNOSTIC_DWARF_STACK_BYTES = 32 * KIB
DIAGNOSTIC_RUN_MINUTES_MAX = 30
COUNTER_SECONDS_MAX = 300
# A child run's profile bytes: the per-shard bundle cap, which includes diagnostics.
DIAGNOSTIC_BYTES_DEFAULT = 128 * MIB
DIAGNOSTIC_BYTES_MAX = SHARD_BUNDLE_BYTES_MAX
# One diagnostic build per side and recipe kind: a CPU profile and a trace build of each side.
DIAGNOSTIC_BUILDS_MAX = 4
# One external tool invocation's output and wall time.
DIAGNOSTIC_TOOL_OUTPUT_BYTES_MAX = 256 * MIB
DIAGNOSTIC_TOOL_SECONDS_MAX = 600

# Archive capacity defaults (package p4).
ARCHIVE_BYTES_DEFAULT = 100 * GIB
ARCHIVE_BYTES_MAX = 1024 * GIB

MAX_JSON_BYTES = 16 * MIB
MAX_SAMPLES_BYTES = SHARD_BUNDLE_BYTES_MAX
