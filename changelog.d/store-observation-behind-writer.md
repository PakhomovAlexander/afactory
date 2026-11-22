- Fixed opening an initialized event Store during a concurrent write. Wake polling can observe
  committed state while a recorder holds the writer lock. Store opens validate the required
  tables, indexes and wake triggers even when the database declares the current version.
