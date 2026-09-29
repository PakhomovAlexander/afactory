- The browser's Workers pane opens in a tenth of a second instead of most of one: it reads every
  committed declaration, prompt and lock with one `git cat-file --batch` and checks their drift
  with one `git diff --literal-pathspecs --name-only`, side by side, where it spawned about four
  git processes per Worker on every open (0.79s to 0.06-0.13s for 14 Workers on a loaded
  machine). A file name with `*` or `[` is now compared literally.
- The Tasks pane lists the Task Stores an earlier (pre-GA) af release wrote as one bar entry,
  `! N old Stores`, and one note saying where they are and that af does not read pre-GA state
  (ADR-0113), instead of an error row each. A Store refused for any other reason still shows its
  own error. `review_core::event::ANOTHER_RELEASE` is the phrase both sides share.
