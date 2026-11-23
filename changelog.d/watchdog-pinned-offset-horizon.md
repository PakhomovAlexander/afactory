- Resolve native scheduler and watchdog occurrences within pinned timezone intervals so an unused
  historical offset at or beyond the horizon cannot block a due alert. Keep the exclusive horizon,
  earliest fold occurrence and snap-forward handling of proven gaps; refuse occurrences missing
  pinned rules. Shared pinned-date fixtures keep native due times and watchdog resolution in
  agreement.
