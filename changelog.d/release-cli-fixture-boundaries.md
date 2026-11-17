- Coordinate release CLI fixture controls with current-driver observations before advancing
  merge and publication. Preserve exact validation receipts and exactly-once effects across
  interruption and resume, with waits outside the fixture database transaction.
- Bound driver wait and output-drain joins by one deadline, preserve split UTF-8 output,
  and release late barrier children when their owning driver exits.
