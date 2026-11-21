//! Provider fixtures create private directories and files; security tests widen modes explicitly.

use std::fs::{DirBuilder, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub(crate) fn tempdir() -> io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .permissions(Permissions::from_mode(0o700))
        .tempdir()
}

pub(crate) fn create_dir(path: impl AsRef<Path>) -> io::Result<()> {
    DirBuilder::new().mode(0o700).create(path)
}

pub(crate) fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)
}

pub(crate) fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    // Set the creation mode only: a test's deliberately unsafe existing file stays unsafe.
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?
        .write_all(contents.as_ref())
}
