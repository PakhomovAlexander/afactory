- The TUI shows state in the brand's colours: chips of ink on blue (running), green (passed) and
  pink (failed or awaiting approval) on a Task's STATE, its stage marks, the progress in the bar,
  a Provider's STATUS and the Workers pane's Attempt counts, and an `error` chip before every
  error row. The status line turns pink while it carries an error, and the help header's worker
  is drawn in solid pink. Text on the terminal's own ground is never coloured, so every colour
  reads on a light or a dark terminal; `NO_COLOR` still leaves attributes only.
