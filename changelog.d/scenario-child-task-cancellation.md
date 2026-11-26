- Scenario recovery respects an operator's durable cancellation of the bound child Task run.
  Cancellation recorded before settlement takes precedence over a result already read by the
  coordinator or a failure at a step deadline or driver reservation limit. Recovery records the
  exact cancelling run and writer epoch. The original intent ends without acceptance and retains
  its accounting.
  Ordinary lost-writer recovery and direct Task resume remain available.
  A missing Task in a readable Store settles its admission failure. An unavailable Store
  or a permission-denied read of a required Task artifact keeps the original intent and
  unresolved charges visible while blocking dispatch and acceptance. Deadlines, driver
  reservation limits and Scenario cancellation still settle.
