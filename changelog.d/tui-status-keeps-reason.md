- `af` browser: a message on the status line that names a long path, such as the `:cd` refusal
  `<path>: not a directory`, keeps its reason visible. The path is shortened from the left with
  `...`, whole leading components first and then, when its final component alone is too wide,
  inside that component, instead of the line being cut before the reason. A quoted path with
  spaces is shortened as one path. A message that fits is unchanged (#229).
