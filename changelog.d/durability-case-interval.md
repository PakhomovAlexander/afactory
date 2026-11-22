- End durability case timing at subprocess return or timeout, before orphan cleanup, so terminal
  timing and controller overlap exclude cleanup while descendant cleanup still runs.
