The repository's local Task gate now runs the reviewed fast validation profile (functional,
e2e and uix) and records its selection plan in the private gate run directory. Bare
`scripts/verify.sh`, `make check`, CI gate pull requests and releases continue to run the
full check.
