- A build or sample image whose `docker image inspect` identity is malformed (a field that is not
  JSON, an ID, OS or architecture that is not a string, repository digests or environment that
  are not a list of strings) now refuses as `build_image_malformed` before any claim, build or
  probe container, and leaves no machine fence, instead of escaping as a JSON or type error that
  kept the fence and disabled every later experiment.
