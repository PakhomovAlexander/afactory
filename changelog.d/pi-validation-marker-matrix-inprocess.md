- Run the Pi validation malformed-marker mutation matrix in-process through production
  `VALIDATION.read_claims` instead of launching the validation CLI per case (scoped issue537
  trial): the marker list, positions, existing skip and diagnostic expectations are unchanged,
  and the hidden-summary-marker case keeps representative CLI stderr/exit coverage.
