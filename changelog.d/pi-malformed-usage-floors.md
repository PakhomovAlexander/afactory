- Pi retains independently valid native token counters as incomplete usage and safe
  per-inference evidence when optional counters are malformed, inconsistent or conflicting.
  Invalid accounting still fails the Attempt and prevents further native inference; Kimi
  keeps its full input-plus-output charge and OpenAI keeps its cache-discount convention.
  See the [native inference allowance decision](../docs/adr/2026-11-03-bound-pi-native-inferences-by-remaining-allowance.md).
