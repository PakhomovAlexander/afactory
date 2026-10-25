- `af self --help` lists `status` with a one-line summary that no longer wraps into a phantom
  `release` command; `af self status --help` keeps the full description. The examples of `af
  provider add --help`, `af task run --help` and `af task show --help` that need existing state
  now say what they need (`# needs: …`), as every other help example does. `af provider doctor`
  keeps `--campaign NAME` required: its admission evidence belongs to a Campaign that `af review
  run --campaign` reuses.
