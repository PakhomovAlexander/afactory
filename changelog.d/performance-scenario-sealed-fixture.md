- Fix the Performance Scenario launcher's hostile import test on sealed, read-only gate sources:
  provision its disposable installation before sealing it and restore directory permissions for
  cleanup. Adapter pinning and production installation permissions are unchanged.
