//! Records cargo's own artifact layout for `tests/perf_supervisor.rs` (ADR-0168).
//!
//! The integration test builds the `perf-supervisor` example into the target directory and
//! profile it was itself built into, so it must know whether its profile directory sits at the
//! target root (`<target>/<profile>`) or below an explicit `--target` triple
//! (`<target>/<triple>/<profile>`). A directory name or a `CACHEDIR.TAG` cannot tell: cargo writes
//! that tag in both places, and a precreated or cache-restored target has none. Cargo itself can:
//! a build script runs from the host layout, which never has a triple, while `OUT_DIR` is in the
//! layout of the target being built. Only the relative triple directory is recorded, so a
//! relocated target directory still resolves.

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let exe = std::env::current_exe().expect("the build script knows its own path");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    // `<layout>/<profile>/build/<package>-<hash>/build-script-build` and
    // `<layout>/<profile>/build/<package>-<hash>/out`.
    let host = exe.ancestors().nth(4).expect("the host layout root");
    let target = out.ancestors().nth(4).expect("the target layout root");
    let triple = if target == host {
        String::new()
    } else if target.parent() == Some(host) {
        name(target)
    } else {
        panic!(
            "cargo layout not understood: build script {} with OUT_DIR {}",
            exe.display(),
            out.display()
        );
    };
    println!("cargo:rustc-env=REVIEW_PROCESS_TARGET_TRIPLE_DIR={triple}");
}

fn name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .expect("a UTF-8 triple directory")
        .to_owned()
}
