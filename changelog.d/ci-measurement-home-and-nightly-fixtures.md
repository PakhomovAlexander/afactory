- Create owner-private dedicated measurement homes in the live Docker smoke and cleanup
  fixtures regardless of CI umask, preserving private account isolation and cleanup checks.
- Supply the current release declaration and its source parent in the nightly-tag rejection
  fixture so it exercises tag validation rather than failing on a missing contract file.
- Regenerate the protected performance harness inventory and staged policy for these fixture
  changes, preserving the release harness check and all protected inputs.
- Explicitly model Linux in the release launcher's account-mount contract fixture and cover
  Darwin's unchanged command path without Linux account files or mount preparation.
- Canonicalize the private wake recorder fixture on macOS and test Linux seal durability
  separately from non-Linux refusal without allocating wake identity or provenance keys.
