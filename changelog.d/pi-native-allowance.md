- Pi receives the trusted remaining Attempt allowance and caps each native SDK inference's
  output. Observed exhaustion or unsafe accounting stops subsequent inference; one inference
  can still overshoot because prompt usage is reported afterward. Bounded safe per-inference
  evidence survives failures, and file-only Workers receive exact array-envelope/schema guidance.
  See the [decision](https://github.com/PakhomovAlexander/afactory/blob/main/docs/adr/2026-11-03-bound-pi-native-inferences-by-remaining-allowance.md).
