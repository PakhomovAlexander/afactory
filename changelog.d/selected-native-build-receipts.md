Selected native validation now records successful workspace and selected perf feature compilation
before running class suites, so the existing warm-check test allowance starts at a real build
boundary. Inventory JSON and compiler diagnostics are preserved; failed default or feature builds
stop before tests and cannot claim successful class completion. Focused native and hosted runner
regressions cover phase order and build failures without changing deadlines or coverage.
