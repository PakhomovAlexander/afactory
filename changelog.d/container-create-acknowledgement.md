- Settle container creation before starting Gate or Worker payloads, remove only acknowledged
  daemon IDs, and preserve writable roots when creation or cleanup remains uncertain.
- Version Worker storage recovery intents to `af.worker-storage/2` with durable creation fences;
  earlier intents fail closed. Execution and cleanup deadlines and containment bounds stay fixed.
- Update crash and development test transports for create/start: stop only after the durable
  workload ID, forward helpers unchanged, and execute only stored commands in their private context.
- Admit start clients against the original absolute deadline at the shared process spawn boundary,
  including launch and retry time; expired or cancelled admission starts no client.
- Recover acknowledged containers already removed before a crash by requiring a bounded, successful,
  complete exact-ID listing in the same private daemon context; uncertain intents remain preserved.
