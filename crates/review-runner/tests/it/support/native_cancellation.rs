//! Actual native framing plus cooperative cancellation; no model or network calls.
use review_runner::task::{WorkerAccess, WorkerModelAdapter};
use review_store::Cas;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn state(pid: u32) -> String {
    #[cfg(target_os = "linux")]
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        && stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z'))
    {
        return "Z".into();
    }
    // The shell builtin performs only kill(pid, 0); unlike ps it needs no process-list
    // entitlement on sandboxed macOS. Native test crates need no additional dependency.
    let present = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "kill -0 \"$1\" 2>/dev/null",
            "fixture",
            &pid.to_string(),
        ])
        .status()
        .unwrap()
        .success();
    if present {
        "running".into()
    } else {
        String::new()
    }
}

pub fn check(
    make: impl Fn(&str) -> Box<dyn WorkerModelAdapter>,
    output: &[u8],
    expected_usage: review_core::task::usage::TaskTokenUsageV3,
) {
    for cas_failure in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let cas_path = directory.path().join("cas");
        let cas = Cas::open(&cas_path).unwrap();
        let program = directory.path().join("native-fixture");
        let quoted = std::str::from_utf8(output).unwrap().replace('\'', "'\\''");
        std::fs::write(&program, format!("#!/bin/sh\ncat >input\nprintf '%s\\n' \"$@\" >args\nout=; prev=; for arg in \"$@\"; do [ \"$prev\" = -o ] && out=$arg; prev=$arg; done\n[ -z \"$out\" ] || printf OK >\"$out\"\nsleep 30 & child=$!\nprintf '%s' '{quoted}'\nprintf diagnostic >&2\nprintf '%s %s' $$ $child >ready\nwait\n")).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = make(program.to_str().unwrap());
        let flag = AtomicBool::new(true);
        let pre = adapter.invoke(
            &cas,
            directory.path(),
            b"exact context".to_vec(),
            Duration::from_secs(10),
            WorkerAccess::ReadOnly,
            Some(&flag),
            &[],
        );
        assert!(pre.message.is_err());
        assert_eq!(pre.usage.unwrap().chargeable_tokens.get(), 0);
        assert!(
            !directory.path().join("ready").exists(),
            "pre-cancellation cannot spawn"
        );
        flag.store(false, Ordering::Release);
        let (returned, pids, stopped) = std::thread::scope(|scope| {
            let cancel = scope.spawn(|| {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut pids = Vec::new();
                while Instant::now() < deadline {
                    if let Ok(text) = std::fs::read_to_string(directory.path().join("ready")) {
                        pids = text
                            .split_whitespace()
                            .filter_map(|n| n.parse::<u32>().ok())
                            .collect();
                        if pids.len() == 2 {
                            break;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                let observed_live =
                    pids.len() == 2 && state(pids[0]) == "running" && state(pids[1]) == "running";
                let destroyed = if cas_failure {
                    std::fs::remove_dir_all(&cas_path)
                        .and_then(|()| std::fs::write(&cas_path, b"CAS unavailable"))
                } else {
                    Ok(())
                };
                let stopped = Instant::now();
                flag.store(true, Ordering::Release);
                destroyed.unwrap();
                assert_eq!(pids.len(), 2, "cancel only after native usage was flushed");
                assert!(
                    observed_live,
                    "the cleanup observer must see both live fixture processes first"
                );
                (pids, stopped)
            });
            let returned = adapter.invoke(
                &cas,
                directory.path(),
                b"exact context".to_vec(),
                Duration::from_secs(10),
                WorkerAccess::ReadOnly,
                Some(&flag),
                &[],
            );
            let (pids, stopped) = cancel.join().unwrap();
            (returned, pids, stopped)
        });
        assert!(
            returned.message.is_err(),
            "complete printed message cannot overcome cancellation"
        );
        assert_eq!(returned.usage.unwrap(), expected_usage);
        assert!(stopped.elapsed() < Duration::from_secs(2));
        assert_eq!(
            std::fs::read(directory.path().join("input")).unwrap(),
            b"exact context"
        );
        if cas_failure {
            assert!(returned.raw_artifact_ids.is_empty());
        } else {
            assert_eq!(returned.raw_artifact_ids.len(), 2);
            assert_eq!(cas.get(&returned.raw_artifact_ids[0]).unwrap(), output);
            assert_eq!(
                cas.get(&returned.raw_artifact_ids[1]).unwrap(),
                b"diagnostic"
            );
        }
        assert!(
            state(pids[0]).is_empty(),
            "direct native child must be reaped"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let child = state(pids[1]);
            // init owns orphan reaping; a zombie is dead and holds no descriptors.
            if child.is_empty() || child.starts_with('Z') {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "native descendant survived group cancellation"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let args = std::fs::read_to_string(directory.path().join("args")).unwrap();
        assert!(!args.contains("dangerously-bypass"));
        assert!(!args.contains("Edit,Write"));
        assert!(!args.contains("workspace-write"));
        assert!(args.contains("fixture-model-1"));
        assert!(args.contains("high"));
    }
}
