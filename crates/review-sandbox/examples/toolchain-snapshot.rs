//! Explicit operator preparation: copy one installed toolchain and print its content pin.
use review_sandbox::toolchain::{ToolchainLimits, snapshot_toolchain};
use std::path::PathBuf;
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err(
            "usage: toolchain-snapshot ABSOLUTE_INSTALLED_TOOLCHAIN ABSOLUTE_ABSENT_SEED".into(),
        );
    }
    let digest = snapshot_toolchain(
        &PathBuf::from(&args[0]),
        &PathBuf::from(&args[1]),
        ToolchainLimits {
            max_bytes: 2 * 1024 * 1024 * 1024,
            max_entries: 100_000,
            max_copy_bytes: 2 * 1024 * 1024 * 1024,
        },
        None,
    )?;
    println!("{digest}");
    Ok(())
}
