# hub

A consuming project for the `af` browser (`docs/design/tui.md`, ADR-0119). Its tests render
every pane from this project at 100x30 and compare the text.

The hub is layered. Its pinned Task packages are exactly the ones `fixtures/task-runtime/pagination`
pins in `.af/task-catalog.toml`, so a test copies that fixture first and this directory over it,
then commits the result as a fresh repository. The pins then live in one place, and the hub adds
only what the browser shows beyond them:

- `.af/af.toml`, the project layer the Settings pane reads;
- `.af/pipelines/review.toml`, a review Pipeline the Pipelines pane lists beside the
  `fixture/implementation` package.
