- Fix the CLI test fixture on hosts where the temporary directory and Cargo output use
  different filesystems; verify both shared hard links and the child-copy fallback.
