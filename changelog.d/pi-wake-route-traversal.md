- Isolate durable Pi wake polling progress per authenticated route, preserving foreground drain
  fairness and earlier-packet session order across alternating sessions and restarts. Account and
  expire bounded route hints without clearing uncertain admission. Add adversarial public CLI and
  closed-contract/scope regressions; preserve the #928 publication and single-route #929 tests.
  See [the proposed decision](https://github.com/PakhomovAlexander/afactory/blob/main/docs/adr/2026-11-14-isolate-pi-wake-route-traversal.md).
