The repository's local Task gate now runs the reviewed fast validation profile (functional,
e2e and uix), records its selection plan in the private gate run directory, and records the
shared `test-build` step before class tests. Bare
`scripts/verify.sh`, `make check`, CI gate pull requests and releases continue to run the
full check.
