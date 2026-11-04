- Linux validation on ephemeral GitHub-hosted Ubuntu runners reclaims unused Android, .NET and
  Haskell SDK payloads before Rust builds, logs disk capacity before and after cleanup, and
  requires 40 GiB free for build/test headroom above the unchanged production 10 GiB disk floor.
  Local, macOS and self-hosted gates keep their existing filesystem contents and test coverage.
