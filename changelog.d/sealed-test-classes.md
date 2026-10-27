- `scripts/test-test-classes.py` passes again from an AF-sealed, read-only source
  ([#411](https://github.com/PakhomovAlexander/afactory/issues/411)): the build-receipt fixture makes its
  `copytree` copy of the source user-writable before it writes stand-in scripts into it, so the
  always-on `test-classes` preflight step no longer fails every local af gate with
  `PermissionError`. Every assertion, and the real native wrapper, Make graph, class runner and
  receipt writer the fixture runs, are unchanged.
