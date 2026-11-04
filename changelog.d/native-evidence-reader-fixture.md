- Keep native performance Scenario test readers within the installed executable byte bound by
  stripping only debug symbols from private copies of the current tested binary on Linux and macOS.
  Assert shared binary immutability and reject oversized reader fixtures before hashing or execution.
