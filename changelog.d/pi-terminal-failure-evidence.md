- Pi withholds short credential-source URL query secrets when provider errors echo only their
  decoded values, including repeated parameters; unrepresentable secrets withhold all diagnostics.
- Failed native Attempts retain a versioned, context-bound `af/NativeFailureEvidence@1` artifact.
  Task reports use the final validated category independently of diagnostic text and complete
  accounting: a rate-limit error mentioning `model_unavailable` reports `provider_capacity` while
  preserving its safe diagnostic and exact charge.
