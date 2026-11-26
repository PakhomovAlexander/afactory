- Project-origin readers include retained machine storage and live observations in the shared
  Storage Budget before copying SQLite snapshots. Readers coordinate across temporary roots and
  refuse to copy when overlapping telemetry cannot be measured. Admission reserves destination
  allocation for the database, WAL and rebuilt index, and counts telemetry inside owned entries
  of registered external Task Stores once.
