Provider probes now distinguish a host permission denial from an incompatible CLI. Status and
detect report `probe_denied` with exit 8 and permission guidance; doctor shows agent sandbox
context, and `af help agents` explains what to tell a user running inside an agent.
