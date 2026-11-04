- Prepare the crash-recovery container probe's pinned multi-architecture image with its selected
  runtime before launching the child, and pass that exact image to the child. Clean runners no
  longer depend on another probe populating the image cache before the storage barrier.
