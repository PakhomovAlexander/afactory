#![cfg(unix)]
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
    let python = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("python3"))
        .find(|path| path.is_file())
        .unwrap();
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
    let transport = transport(
        root.path(),
        "import sys,time\nsys.stdin.read()\ntime.sleep(30)",
    );
    let cancelled = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(150));
            cancelled.store(true, Ordering::Release);
        });
        let started = Instant::now();
        let control = SourceControl {
            deadline: started + Duration::from_secs(5),
            cancelled: &cancelled,
        };
        assert!(matches!(
            transport.get_issue(&select(), &control),
            Err(SourceError::Cancelled)
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
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
