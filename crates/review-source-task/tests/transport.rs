use review_source_task::{jira::*, *};
use std::{
    os::unix::fs::PermissionsExt,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
fn select() -> JiraSelector {
    JiraSelector {
        site: "example.atlassian.net".into(),
        key: "AF-42".into(),
        acceptance_fields: vec![],
    }
}
fn transport(root: &std::path::Path, body: &str) -> CurlJiraTransport {
    let program = root.join("curl-fixture");
    // Resolve the test interpreter before isolation; Apple's /usr/bin shim adds SDK settings.
    let launcher = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("python3"))
        .find(|path| path.is_file())
        .unwrap();
    let resolved = std::process::Command::new(launcher)
        .args(["-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(resolved.status.success(), "resolve the fixture interpreter");
    let python = std::path::PathBuf::from(String::from_utf8(resolved.stdout).unwrap().trim());
    assert!(python.is_absolute() && python.is_file());
    std::fs::write(&program, format!("#!{}\n{body}\n", python.display())).unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
    CurlJiraTransport {
        program,
        credentials: JiraCredentials::new(
            "fixture@example.invalid".into(),
            "private-fixture-token".into(),
        )
        .unwrap(),
    }
}
#[test]
fn native_transport_owns_protocol_flags_and_keeps_credentials_off_argv() {
    let root = tempfile::tempdir().unwrap();
    let transport = transport(
        root.path(),
        r#"import os,sys
args=sys.argv[1:]
assert args[0]=='--disable'
assert '--location' not in args and '--insecure' not in args
assert args[args.index('--request')+1]=='GET'
assert args[args.index('--proto')+1]=='=https'
assert args[args.index('--proxy')+1]==''
assert args[args.index('--max-filesize')+1]=='1048576'
assert args[args.index('--url')+1]=='https://example.atlassian.net/rest/api/3/issue/AF-42?fields=summary,description,updated'
assert not any('private-fixture-token' in arg for arg in args)
assert set(os.environ).issubset({'HOME','LC_ALL','LC_CTYPE','__CF_USER_TEXT_ENCODING'})
assert not os.path.exists(os.path.join(os.environ['HOME'],'.curlrc'))
assert sys.stdin.read()=='user = "fixture@example.invalid:private-fixture-token"\n'
sys.stdout.write('{"fields":{}}\n200')"#,
    );
    let cancelled = AtomicBool::new(false);
    let control = SourceControl {
        deadline: Instant::now() + Duration::from_secs(5),
        cancelled: &cancelled,
    };
    let response = transport.get_issue(&select(), &control).unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.body, b"{\"fields\":{}}");
}
#[test]
fn native_source_cancels_inflight_process_and_bounds_output() {
    let root = tempfile::tempdir().unwrap();
    let ready = root.path().join("ready");
    let transport = transport(
        root.path(),
        &format!(
            r#"import os,sys,time,subprocess
sys.stdin.read()
child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'])
with open({ready:?},'w') as f: f.write(str(os.getpid())+' '+str(child.pid))
time.sleep(30)"#,
            ready = ready.to_str().unwrap()
        ),
    );
    let cancelled = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let observer = scope.spawn(|| {
            let limit = Instant::now() + Duration::from_secs(4);
            let pids = loop {
                if let Ok(text) = std::fs::read_to_string(&ready) {
                    let pids: Vec<u32> = text
                        .split_whitespace()
                        .filter_map(|s| s.parse().ok())
                        .collect();
                    if pids.len() == 2 {
                        break Some(pids);
                    }
                }
                if Instant::now() >= limit {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            let live = pids
                .as_ref()
                .is_some_and(|pids| pids.iter().all(|pid| process_live(*pid)));
            // Always release the owned operation before making assertions in the calling thread.
            cancelled.store(true, Ordering::Release);
            (pids, live, Instant::now())
        });
        let control = SourceControl {
            deadline: Instant::now() + Duration::from_secs(5),
            cancelled: &cancelled,
        };
        let result = transport.get_issue(&select(), &control);
        let (pids, live, cancellation_time) = observer.join().unwrap();
        assert!(
            cancellation_time.elapsed() < Duration::from_secs(2),
            "source cancellation was not prompt"
        );
        assert!(
            live,
            "source leader and descendant must be alive before cancellation: {pids:?}"
        );
        assert!(
            matches!(&result, Err(SourceError::Cancelled)),
            "{:?}",
            result.err()
        );
        let pids = pids.unwrap();
        assert!(
            !process_exists(pids[0]),
            "direct source child was not reaped"
        );
        let limit = Instant::now() + Duration::from_secs(2);
        while process_live(pids[1]) && Instant::now() < limit {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !process_live(pids[1]),
            "source descendant survived cancellation"
        );
    });
    cancelled.store(false, Ordering::Release);
    let control = SourceControl {
        deadline: Instant::now() + Duration::from_millis(100),
        cancelled: &cancelled,
    };
    assert!(matches!(
        transport.get_issue(&select(), &control),
        Err(SourceError::TimedOut)
    ));
    let oversized = super_transport(root.path());
    let control = SourceControl {
        deadline: Instant::now() + Duration::from_secs(5),
        cancelled: &cancelled,
    };
    assert!(matches!(
        oversized.get_issue(&select(), &control),
        Err(SourceError::TooLarge)
    ));
}
fn super_transport(root: &std::path::Path) -> CurlJiraTransport {
    transport(
        root,
        "import sys\nsys.stdin.read()\nsys.stdout.write('x'*1048580+'\\n200')",
    )
}

fn process_exists(pid: u32) -> bool {
    std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "kill -0 \"$1\" 2>/dev/null",
            "source-fixture",
            &pid.to_string(),
        ])
        .status()
        .unwrap()
        .success()
}
fn process_live(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    if std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| s.rsplit_once(") ").map(|(_, tail)| tail.starts_with('Z')))
        == Some(true)
    {
        return false;
    }
    process_exists(pid)
}
