- The browser's Providers pane always shows every login's quota windows: discovery hands the rows
  over, then the usage probe `af provider status --usage` runs follows on its own, so `R` no longer
  stands for a separate probe. Each window is a bar of twenty cells drawn in the brand's colour
  for how much is used (green under half, blue from half, pink from four fifths) on a grey track
  with a hairline between two bars,
  with the percentage used and when the window resets; the `models` section stands one blank row
  under them.
- The Claude usage probe reads the five-hour session window from the `/usage` screen as well as
  the weekly ones: `af provider status --usage` and the browser list `Claude session 5h` first.
- The Providers pane keeps the model catalogs off screen until asked for: its last row says
  `<m> to show models`, and `m` shows every listed Provider's `models` section (one row per exact
  ID) until `m` hides them again.
