//! Explicit operator smoke: two fresh native runtimes, with no rustup download command.
use review_check::{CheckDefinition, CheckRunner, CheckStatus};
use review_core::{Arg, Command};
use review_pipeline::task::code::{RustToolchainRequest, prepare_native_rust_toolchain};
use review_store::Cas;
use std::collections::BTreeSet;
use std::path::PathBuf;
fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if !(3..=4).contains(&args.len()) {
        return Err("usage: native-toolchain-probe MAPPING VERSION HOST [REPOSITORY]".into());
    }
    let mapping = PathBuf::from(&args[0]);
    let request = RustToolchainRequest {
        version: args[1].clone(),
        host: args[2].clone(),
        components: BTreeSet::from(["clippy".into(), "rustfmt".into()]),
        checks: BTreeSet::from(["probe".into()]),
    };
    let candidate = tempfile::tempdir().map_err(|e| e.to_string())?;
    let candidate_root = candidate.path().canonicalize().map_err(|e| e.to_string())?;
    let cas_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let cas = Cas::open(cas_dir.path()).map_err(|e| e.to_string())?;
    for iteration in 0..2 {
        let runtime = tempfile::tempdir().map_err(|e| e.to_string())?;
        let runtime_root = runtime.path().canonicalize().map_err(|e| e.to_string())?;
        let env = prepare_native_rust_toolchain(
            &candidate_root,
            &runtime_root,
            &request,
            Some(&mapping),
        )?
        .ok_or("explicit mapping unexpectedly absent")?;
        let mut runner = CheckRunner::new(&cas, &candidate_root)
            .with_env("HOME", runtime_root.display().to_string())
            .with_env("CARGO_NET_OFFLINE", "true")
            .with_env("RUSTUP_DIST_SERVER", "http://127.0.0.1:9")
            .with_timeout(std::time::Duration::from_secs(15));
        for (key, value) in &env.environment {
            runner = runner.with_env(key, value);
        }
        for (program, args) in [
            ("rustc", vec!["--version"]),
            ("cargo", vec!["--version"]),
            ("cargo", vec!["fmt", "--version"]),
            ("cargo", vec!["clippy", "--version"]),
        ] {
            let result = runner.run(&CheckDefinition::new(
                "probe",
                Command::new(program, args.iter().map(|arg| Arg::literal(*arg)).collect()),
            ));
            if result.status != CheckStatus::Passed {
                let detail = result
                    .stderr
                    .as_ref()
                    .and_then(|id| cas.get(id).ok())
                    .unwrap_or_default();
                return Err(format!(
                    "probe failed: {program} {args:?}: {}",
                    String::from_utf8_lossy(&detail)
                ));
            }
            println!("fresh HOME {iteration}: {program} {args:?} passed");
        }
        if let Some(repository) = args.get(3) {
            // Real repository preflight plus real nextest discovery on a dependency-free
            // fixture: an empty private Cargo home intentionally has no registry cache.
            // This is not a full workspace gate or captured Task acceptance receipt.
            let smoke = runtime_root.join("nextest-smoke");
            std::fs::create_dir_all(smoke.join("src")).map_err(|e| e.to_string())?;
            std::fs::write(smoke.join("Cargo.toml"), "[package]\nname = \"native-toolchain-smoke\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").map_err(|e| e.to_string())?;
            std::fs::write(
                smoke.join("src/lib.rs"),
                "#[test] fn private_toolchain_works() {}\n",
            )
            .map_err(|e| e.to_string())?;
            let smoke_manifest = smoke.join("Cargo.toml");
            let smoke_manifest = smoke_manifest.to_str().ok_or("non-UTF8 smoke path")?;
            let repository = PathBuf::from(repository)
                .canonicalize()
                .map_err(|e| e.to_string())?;
            let mut preflight = CheckRunner::new(&cas, &repository)
                .with_env("HOME", runtime_root.display().to_string())
                .with_env("CARGO_NET_OFFLINE", "true")
                .with_env("RUSTUP_DIST_SERVER", "http://127.0.0.1:9")
                .with_env(
                    "CARGO_TARGET_DIR",
                    runtime_root.join("target").display().to_string(),
                )
                .with_timeout(std::time::Duration::from_secs(120));
            for (key, value) in &env.environment {
                preflight = preflight.with_env(key, value);
            }
            for (program, arguments) in [
                ("make", vec!["preflight-check"]),
                (
                    "cargo",
                    vec![
                        "nextest",
                        "list",
                        "--list-type",
                        "binaries-only",
                        "--manifest-path",
                        smoke_manifest,
                        "--offline",
                    ],
                ),
            ] {
                let result = preflight.run(&CheckDefinition::new(
                    "preflight",
                    Command::new(
                        program,
                        arguments.iter().map(|arg| Arg::literal(*arg)).collect(),
                    ),
                ));
                println!(
                    "fresh HOME {iteration}: {program} {arguments:?}: {:?}",
                    result.status
                );
                if result.status != CheckStatus::Passed {
                    let detail = result
                        .stderr
                        .as_ref()
                        .and_then(|id| cas.get(id).ok())
                        .unwrap_or_default();
                    return Err(format!(
                        "repository smoke failed: {}",
                        String::from_utf8_lossy(&detail)
                    ));
                }
            }
        }
        if runtime_root.join("cargo/credentials.toml").exists() {
            return Err("credential file in private HOME".into());
        }
        std::fs::write(
            runtime_root.join("toolchain/bin/rustc"),
            b"candidate-local mutation",
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
