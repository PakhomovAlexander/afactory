- `af review show` escapes control characters in a missing Finding key before printing its error,
  so the key cannot split or restyle the stderr line (#298).
