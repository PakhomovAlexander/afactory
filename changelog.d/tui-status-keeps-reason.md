- `af` browser: a message on the status line that names a long path, such as the `:cd` refusal
  `<path>: not a directory`, keeps its reason visible. The path is shortened from the left with
  `...` and keeps its final component, instead of the line being cut before the reason. A message
  that fits is unchanged (#229).
