Full `make check` now runs the Python performance laboratory suite as a mandatory tests-stage
step after the genuine Rust no-run build and ordinary Rust suite, preserving both runner
modes, standalone `make test`, all coverage and the existing build/test deadlines. Lightweight
preflight, catalog/schema and source-integrity checks remain mandatory; selected class wiring,
native process ownership and `.af/` authority are unchanged. A focused regression exercises
the real native wrapper, Make graph and step receipts, including a build failure that prevents
suite execution.
