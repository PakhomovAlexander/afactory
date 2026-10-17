- A `task_collected` tombstone (`af/TaskCollected@1`) that spells `unknown_usage_attempts: 0`
  is now refused on admission, event validation and replay, as its schema already refused it;
  a tombstone without the field still reads as no unknown usage, and written bytes are
  unchanged ([#194](https://github.com/PakhomovAlexander/afactory/issues/194)).
