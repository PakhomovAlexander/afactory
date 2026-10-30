- `scripts/changelog-notes.py --check` (run by `make check`) and `--write` refuse a
  `changelog.d/` note that holds a Markdown heading outside a fenced code block, naming the file,
  the line and the fix: a note holds bullets (`changelog.d/README.md`). New
  `scripts/test-changelog-notes.py` proves it in a temporary repository and runs in
  `make release-check` beside the pin tests (issue #461).
- `CHANGELOG.md` passes markdownlint again: the `0.14.0-rc.1` section's
  `# Performance laboratory whole-source repairs` heading, copied from a note, is now a plain
  lead-in line. This is the one sanctioned edit of `CHANGELOG.md` outside a release pull request.
