- Settle container creation before starting Gate or Worker payloads, remove only acknowledged
  daemon IDs, and preserve writable roots when creation or cleanup remains uncertain.
- Version Worker storage recovery intents to `af.worker-storage/2` with durable creation fences;
  earlier intents fail closed. Execution and cleanup deadlines and containment bounds stay fixed.
