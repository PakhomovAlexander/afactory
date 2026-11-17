- Coordinate release CLI fixture controls with current-driver observations before advancing
  merge and publication. Preserve exact validation receipts and exactly-once effects across
  interruption and resume, with waits outside the fixture database transaction.
- Bound driver wait and output-drain joins by one deadline, preserve split UTF-8 output,
  and release late barrier children when their owning driver exits.
- Run six native publication/recovery fixtures under the existing real-time test
  exclusivity rule. Preserve all deadlines, cadence, faults and accounting assertions;
  show retained driver status if cancellation shutdown reaches its deadline.
