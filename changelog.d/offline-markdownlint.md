- The markdownlint gate's tool can be installed once and run offline: `make markdownlint-tool`
  installs the exact `markdownlint-cli2` 0.22.1 closure pinned by
  `scripts/markdownlint/package-lock.json` (sha512 per package) read-only, refused unless its tree
  matches the committed digest, and `python3 scripts/markdownlint-tool.py run **/*.md` verifies
  that tree through `PATH` and runs it with no network and no `HOME`, failing closed with exit 2
  when the tool is missing or altered. The code policy's check still runs npx until a separate
  change switches it
  ([ADR-0145](docs/adr/0145-run-the-markdownlint-gate-from-a-pinned-offline-closure.md)).
