# Change notes

One file per pull request, instead of an edit to `CHANGELOG.md`: two open pull requests never
touch the same file, so they never conflict on the changelog.

- A user-visible change adds `changelog.d/<name>.md`, named after its branch or topic
  (`tui-open-speed.md`), holding the entry exactly as it should read in the release notes:
  one or more bullets (lines starting with a dash), wrapped at 100 columns, linking its ADR where there is one.
- Do not edit `CHANGELOG.md` in a pull request. `make release` (`scripts/release.sh`) writes the
  release's section from the merged pull requests' titles and appends every note here, oldest
  first, then removes the notes in the release pull request (`scripts/changelog-notes.py`).
- An authority-compatibility note belongs in the release command's `--compat` line, not here.
