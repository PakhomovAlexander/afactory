Make the telemetry collection interruption fixture reach verification, authority expansion,
authority retention, pruning and history rendering independently of preparation cost. A scoped
test clock separates real preparation from the unchanged one-second work/cleanup deadline;
real timer interruption, incomplete terminal accounting, partial-import checks and lock release
remain asserted. Production deadlines, telemetry semantics and test-class selection are unchanged.
