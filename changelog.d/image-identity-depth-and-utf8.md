- A build or sample image identity from `docker image inspect` whose fields nest deeper than a
  list of strings, or whose bytes are not valid UTF-8 (an environment `PATH` holding byte 0xff),
  now refuses as `build_image_malformed` before any claim, probe container or build, and leaves
  no machine fence. Before, deep nesting below the byte bound could escape as a `RecursionError`
  that kept the fence, and invalid bytes were silently replaced and admitted.
