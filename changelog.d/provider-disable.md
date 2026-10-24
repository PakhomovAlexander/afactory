`af provider disable ID --for 2d` now suspends a registered Provider on this machine until an exact
wall-clock expiry; `af provider enable ID` restores it immediately. Positive durations accept
seconds, minutes, hours and days up to 30 days. Separate private versioned state leaves registry
v1 unchanged, survives restarts and refuses new probes and Task private sends while disabled.
Status JSON advances to `af/provider-status@3` with expiry and the enable action. Already-running
calls may finish, and older pinned releases do not honor suspension. Provider documentation also
explains how the trusted independence knobs retain model diversity or restore strict principal
separation without a general bypass.
