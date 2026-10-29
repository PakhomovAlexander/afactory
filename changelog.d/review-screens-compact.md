- `af review plan` lists each changed path only for a Subject of at most three; a larger one
  takes one `change` line with the first paths that fit, how many more and `--long lists each`,
  so a 25-file plan fits one 80x24 screen. `--long` lists every path, and the `--json` Subject
  still carries them all (#425).
- The `af onboard` text preview keeps its lines within 80 columns and a source-build preview
  with no Provider registered within 24 rows: the `profile` reason is shorter, a `providers`
  line too wide puts its `←` fix on a line of its own, a next step's long command continues
  between its arguments, the file count joins the `pipeline` line, the Round maximum joins
  `budget`, and a build without an install receipt says so in one line. `--long` adds the
  `convergence` line, every file and the whole warning; `--json` is unchanged apart from
  shorter next-step wording (#425).
- The software starter's `implementation-reviewed.json` no longer names `strategy`,
  `verification` or `facts.standard`: the `implement` kind's defaults supply review verification
  and `facts.standard = true`, with strategy `reviewed` in place of the `fast` it named, which
  selected nothing because the file names its pipeline (#425).
