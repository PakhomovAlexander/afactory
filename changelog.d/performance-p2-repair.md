- The performance laboratory's native probe driver now refuses what it used to pass or damage:
  `cli.py judge` refuses a recorded request whose case, contract, parameters or repetitions
  differ from the registry, `probe` and `judge` refuse a golden document the reference generator
  does not derive, a persistence sample without exact `logical_bytes` fails, and an explicit
  `probe --timeout` outside 1 to 120 seconds launches nothing. The driver bounds a probe's
  stdout and stderr while reading them and kills and reaps the direct child on overflow or
  timeout; the probe removes `probe-work` only while it is the directory it created, and the
  state persistence sample refreshes its production lease immediately before its region
  ([ADR-0164](docs/adr/0164-judge-only-declared-probe-requests-against-a-reference-checked-golden.md)).
