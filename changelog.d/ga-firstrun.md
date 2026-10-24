- The first run says what to do next
  ([ADR-0204](docs/adr/0204-make-the-first-run-say-what-to-do-next.md)).
  `install.sh` ends with the next steps and the PATH line for your shell, takes
  `AF_INSTALL_CHANNEL=rc` (or `--rc`) for the newest pre-release, and names the `minisign` install
  line for your package manager. Bare `af` without a terminal prints first-run hints before its
  usage; at a terminal with no Provider registered it opens on the Providers pane, and the
  Settings pane says `absent  <- af onboard` for a repository without `.af/`. `af onboard` picks
  the runner from the Provider kinds you registered, prints a `providers` block with the
  `--provider` binding for every model Worker (`--probe` adds their login state), and says what
  happened when it cannot fetch a release's checksums. New `af doctor [--json]` (`af/doctor@1`)
  checks the binary, PATH, shell, updates, tools, Providers, storage and the repository,
  token-free. `af help start` and `af help concepts` are new topics, `af --help` lists the
  commands once in first-use order, every flag of every command is documented, and
  `af task output` writes to stdout without `--output`. `af self setup-shell` gains `--remove`
  and says what is left to do; `af self uninstall` names the completion files and man pages it
  leaves, and `--purge` removes them. The README Quickstart is four steps with a first Task.
