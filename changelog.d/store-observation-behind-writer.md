- Fixed opening an initialized event Store during a concurrent write. Wake polling can observe
  committed state while a recorder holds the writer lock.
