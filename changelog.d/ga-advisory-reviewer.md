- A review Pipeline may leave an `advisory` reviewer of the captured review policy unbound.
  `[review.reviewers]` can name a reviewer only some Pipelines run (the `uix` reviewer of
  `kernel/review-light`) while the stock `kernel/review-code` Pipelines still plan on the same
  policy; before, the review reduce refused any Pipeline that did not bind every reviewer. A
  bound reviewer with no runtime result is still missing evidence and the round stays
  incomplete; a `required` reviewer must still be bound. Compiled signatures carry the allowed
  names in a new optional `unbound_allowed` field that older plans read as empty.
