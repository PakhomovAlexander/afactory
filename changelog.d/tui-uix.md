- The browser's Providers pane always shows every login's quota windows: discovery hands the rows
  over, then the usage probe `af provider status --usage` runs follows on its own, so `R` no longer
  stands for a separate probe. Each window is a bar of twenty cells, solid in the brand's colour
  for how much is used (green under half, blue from half, pink from four fifths) on a grey track,
  with the percentage used and when the window resets, and a blank row between two bars.
- The Claude usage probe reads the five-hour session window from the `/usage` screen as well as
  the weekly ones: `af provider status --usage` and the browser list `Claude session 5h` first.
