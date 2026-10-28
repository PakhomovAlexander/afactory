- An auth directory below `HOME` reads `~/…` in the doctor, onboarding and `af provider status`
  lists of directories found but not registered also when `HOME` is reached through a symbolic
  link (macOS's `/var`), as it does on Linux. The doctor tests that prove the cut forms of a
  PATH line now use a directory long enough to be cut whatever the temporary root is, so they
  hold on Linux CI's short `/tmp` as on macOS, and the test of a long install path gives `af
  doctor` a PATH it controls, so it proves both forms of the binary's fix (the `af` on PATH, and
  this binary by its path when none is) whatever the host's PATH holds. In `af doctor`'s text
  form a chained fix (`af onboard … --refresh-lock --af VERSION && af self install VERSION`)
  keeps its `&&` or `||` at the end of the first command's line; before, an operator that did
  not fit there started the next line, which a shell rejects (#331).
