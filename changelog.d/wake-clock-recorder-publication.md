- Keep attached Pi wake polling alive across checked source/checkpoint clock races, after fresh
  serialized validation, and publish Linux run/cancellation recorders atomically without exposing
  a second hard link. Invalid clocks, corrupt evidence and native admission uncertainty still
  refuse safely. See the proposed [decision](../docs/adr/2026-11-15-capture-coherent-wake-clocks-and-publish-single-link-recorders.md).
