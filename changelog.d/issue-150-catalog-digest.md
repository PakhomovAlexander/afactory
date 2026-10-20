- `af catalog digest PACKAGE_DIR` prints the digest a catalog pin records for one local Task
  package: one `sha256:` line, from the same recipe and validation Task capture uses. Every
  regular file under the directory contributes its relative path and bytes. An absent, incomplete
  or malformed package prints nothing on stdout, says why on stderr and exits 1. The command
  writes nothing, runs no Worker, contacts no Provider and is never dispatched to a pinned
  release nor followed by an update check, so it installs and activates nothing. It refuses a
  package past capture's 4,096 files or 16 MiB by each file's size before reading it. Package
  reads now open every entry descriptor-relative without following symlinks, a registry's
  package root included, so an entry swapped mid-read is refused rather than read from outside
  the package, and a package root replaced after its check, even by another valid package, is
  refused rather than digested; a symlinked package root is refused however it is spelled,
  `LINK/` and `LINK/.` included (#150,
  [ADR-0157](docs/adr/0157-print-a-task-package-digest-with-the-pin-recipe.md)).
