- A warm check layer is reused by content, not by modification times: af records, per Cargo
  package, the content each layer last built and stamps only the packages whose content differs,
  so a crate another Snapshot compiled can no longer be linked (#266) and unchanged crates keep
  their build. A check that finds the layer busy waits for it until its deadline, saying every
  minute whom it waits for (`[warm] clone_when_busy = true` builds in a private copy instead), and
  the Storage Budget never evicts a layer a check holds. `[warm] build_wall_ms` gives the build
  its own deadline, ending at the gate's `build` step, so `check_wall_ms` is left for the tests; a
  layer evicted under a running build is named as the cause. An `offline = true` check runs with
  no network from the layer's `npm_cache`, its `npm_packages` pinned by SHA-256 and filled once;
  this repository's markdownlint gate now runs that way (#230). Sealing leaves out the generated
  files a Worker's runs left (`__pycache__/`, `*.pyc`, `/target/`, `node_modules/`, the tree's
  `.gitignore` and `.af/seal-ignore`), lists them in `af.seal-ignored/1`, and refuses a FIFO,
  socket or device file by name (#160). `af task show --checks` prints each check's layer, lock,
  source groups and phases; its `--json` document is now `af/task-checks@2`
  ([ADR-0198](docs/adr/0198-key-warm-check-layers-by-content-lock-them-split-the-build-deadline-and-seal-without-generated-files.md)).
