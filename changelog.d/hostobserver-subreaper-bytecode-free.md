- The Linux-only host observer regression that runs a child subreaper no longer writes Python
  bytecode into a writable checkout. Its child runs with `-I`, which ignores
  `PYTHONDONTWRITEBYTECODE`, and does not inherit the parent's `-B`, so in CI it created
  `perflab/__pycache__`, and every later authority check refused that source. The child now
  suppresses bytecode itself and imports a disposable writable copy of `perflab`; the test
  asserts that the copy stays bytecode-free and that the copy was the source imported.
