- The performance lab's scratch enforcement fill syncs its file once every 64 MiB written and
  once more after the filesystem refuses a write, instead of after every MiB. A sync after every
  MiB left an 8-GiB build scratch on nested ext4 short of its capacity when the 120 s check
  ended. The refusal rules, byte bound, deadlines and 30 s cleanup reserve are unchanged, and a
  delayed ENOSPC, EDQUOT or EIO from the last sync is still the fill's
  ([ADR-0167](docs/adr/0167-bound-every-supervisor-wait-and-verify-only-what-was-observed.md)).
