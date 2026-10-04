//! Private host lifecycle tests. All challenges and CLI processes are deterministic fixtures.
#![cfg(target_os = "linux")]
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};

use serde_json::{Value, json};

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let executable = bin.join("codex");
        // No model dispatch method exists: any accidental inference call fails the fixture.
        std::fs::write(&executable, r#"#!/usr/bin/python3
import os,sys,time
from pathlib import Path
root=Path(os.environ['CODEX_HOME'])
if sys.argv[1:]==['login','status']:
    with (root/'status-probes').open('a') as f: f.write('status\n')
    if (root/'ready').exists():
        print('Logged in using ChatGPT',file=sys.stderr);sys.exit(0)
    print('Not logged in',file=sys.stderr);sys.exit(1)
if sys.argv[1:]==['--version']:
    print('codex-cli 0.159.2');sys.exit(0)
if sys.argv[1:]!=['login','--device-auth']: sys.exit(64)
assert os.environ.get('HTTP_PROXY')=='http://fixture.invalid:8080'
assert os.environ.get('SSL_CERT_FILE')=='/fixture/certificate.pem'
assert all(name not in os.environ for name in ['OPENAI_API_KEY','ANTHROPIC_API_KEY','NODE_OPTIONS','NODE_TLS_REJECT_UNAUTHORIZED','BROWSER','DISPLAY'])
(root/'native.pid').write_text(str(os.getpid()))
with (root/'starts').open('a') as f: f.write('start\n')
# Exact 0.159.2 device_code_prompt stdout, including unconditional SGR and println newline.
prompt=("\nWelcome to Codex [v\x1b[90m0.159.2\x1b[0m]\n"
    "\x1b[90mOpenAI's command-line coding agent\x1b[0m\n"
    "\nFollow these steps to sign in with ChatGPT using device code authorization:\n"
    "\n1. Open this link in your browser and sign in to your account\n"
    "   \x1b[94mhttps://auth.openai.com/codex/device\x1b[0m\n"
    "\n2. Enter this one-time code \x1b[90m(expires in 15 minutes)\x1b[0m\n   \x1b[94mAF-FAKE-SECRET\x1b[0m\n"
    "\n\x1b[90mContinue only if you started this login in Codex. If a website or another person gave you this code, cancel.\x1b[0m\n")
def emit_diagnostic():
    if (root/'stderr-fixture').exists():
        sys.stderr.buffer.write((root/'stderr-fixture').read_bytes());sys.stderr.flush()
if (root/'failure-before-prompt').exists():
    emit_diagnostic();sys.exit(1)
if (root/'empty-output').exists(): sys.exit(0)
if (root/'invalid-prompt').exists(): prompt=prompt.replace('0.159.2','0.159.3')
print(prompt,flush=True)
while not (root/'complete').exists(): time.sleep(.01)
if (root/'late-output').exists(): print('access_token=AF-FAKE-SECRET',flush=True)
emit_diagnostic()
if (root/'nonzero-exit').exists(): sys.exit(1)
if not (root/'status-fails').exists(): (root/'ready').touch()
print('Successfully logged in',file=sys.stderr)
"#).unwrap();
        std::fs::set_permissions(executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(root.path().join("host.py"), r#"import os,sys,subprocess,threading
from pathlib import Path
root=Path(os.environ['HOME'])
a,b=os.pipe()
c,d=os.pipe()
args=sys.argv[1:]+['--host-read-fd',str(a),'--host-write-fd',str(d)]
p=subprocess.Popen(args,pass_fds=(a,d),stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
(root/'owner.pid').write_text(str(p.pid))
os.close(a);os.close(d)
def responses():
    try:
        while True:
            data=os.read(0,16384)
            if not data: break
            os.write(b,data)
    except BrokenPipeError: pass
    finally: os.close(b)
threading.Thread(target=responses,daemon=True).start()
def events():
    while True:
        data=os.read(c,16384)
        if not data: break
        os.write(1,data)
    os.close(c)
thread=threading.Thread(target=events,daemon=True);thread.start()
stdout,stderr=p.communicate()
(root/'result.stdout').write_bytes(stdout)
(root/'result.stderr').write_bytes(stderr)
thread.join(timeout=5)
sys.exit(p.returncode)
"#).unwrap();
        Self { root }
    }

    fn command(&self, action: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_af"));
        command
            .args([
                "provider",
                "auth",
                action,
                "codex-main",
                "--kind",
                "codex",
                "--auth-dir",
            ])
            .arg(self.root.path().join("auth"))
            .env_clear()
            .env("HOME", self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("PATH", self.root.path().join("bin"))
            .env("AF_SELF_OFFLINE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn begin(&self) -> (Child, Host) {
        self.begin_kind("codex")
    }

    fn begin_kind(&self, kind: &str) -> (Child, Host) {
        let mut command = Command::new("/usr/bin/python3");
        command
            .arg(self.root.path().join("host.py"))
            .arg(env!("CARGO_BIN_EXE_af"))
            .args([
                "provider",
                "auth",
                "begin",
                &format!("{kind}-main"),
                "--kind",
                kind,
                "--auth-dir",
            ])
            .arg(self.root.path().join("auth"))
            .env_clear()
            .env("HOME", self.root.path())
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("PATH", self.root.path().join("bin"))
            .env("AF_SELF_OFFLINE", "1")
            .env("HTTP_PROXY", "http://fixture.invalid:8080")
            .env("SSL_CERT_FILE", "/fixture/certificate.pem")
            .env("OPENAI_API_KEY", "AMBIENT-KEY-MUST-NOT-PASS")
            .env("ANTHROPIC_API_KEY", "AMBIENT-KEY-MUST-NOT-PASS")
            .env("NODE_OPTIONS", "--no-warnings")
            .env("NODE_TLS_REJECT_UNAUTHORIZED", "0")
            .env("BROWSER", "/fixture/forbidden-browser-hook")
            .env("DISPLAY", ":fixture")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let host = Host {
            root: self.root.path().to_path_buf(),
            reader: BufReader::new(child.stdout.take().unwrap()),
            writer: child.stdin.take().unwrap(),
        };
        (child, host)
    }

    fn result(&self, mut child: Child) -> Output {
        let status = child.wait().unwrap();
        Output {
            status,
            stdout: std::fs::read(self.root.path().join("result.stdout")).unwrap(),
            stderr: std::fs::read(self.root.path().join("result.stderr")).unwrap(),
        }
    }

    fn mode(&self, mode: &str) {
        let auth = self.root.path().join("auth");
        std::fs::create_dir_all(&auth).unwrap();
        std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(auth.join(mode), "").unwrap();
    }

    fn complete(&self) {
        std::fs::write(self.root.path().join("auth/complete"), "").unwrap();
    }

    fn no_secrets(&self, output: &Output) {
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !text.contains("AF-FAKE-SECRET"),
            "private device code leaked"
        );
        assert!(
            !text.contains("https://auth.openai.com"),
            "private challenge leaked"
        );
        let state = std::fs::read_to_string(
            self.root
                .path()
                .join("auth/.af-codex-auth-recovery/session.json"),
        )
        .unwrap();
        assert!(!state.contains("AF-FAKE-SECRET"));
        assert!(!state.contains("https://"));
        let registry = self.root.path().join("config/af/providers.toml");
        if registry.exists() {
            let text = std::fs::read_to_string(registry).unwrap();
            assert!(!text.contains("AF-FAKE-SECRET"));
            assert!(!text.contains("https://"));
        }
    }
}

struct Host {
    root: std::path::PathBuf,
    reader: BufReader<ChildStdout>,
    writer: ChildStdin,
}

fn read(stream: &mut Host) -> Value {
    let mut line = String::new();
    stream.reader.read_line(&mut line).unwrap();
    assert!(
        !line.is_empty(),
        "host closed; state={}, stdout={}, stderr={}",
        std::fs::read_to_string(
            stream
                .root
                .join("auth/.af-codex-auth-recovery/session.json")
        )
        .unwrap_or_default(),
        std::fs::read_to_string(stream.root.join("result.stdout")).unwrap_or_default(),
        std::fs::read_to_string(stream.root.join("result.stderr")).unwrap_or_default()
    );
    serde_json::from_str(&line).unwrap()
}

fn send(stream: &mut Host, value: &Value) {
    writeln!(&mut stream.writer, "{value}").unwrap();
    stream.writer.flush().unwrap();
}

fn approve(stream: &mut Host, request: &Value) {
    send(
        stream,
        &json!({
            "schema":"af/provider-auth-host@1", "recovery_id":request["recovery_id"],
            "provider":request["provider"], "context_id":request["context_id"],
            "requester_ref":"a".repeat(64), "coordinator_ref":"b".repeat(64),
            "approved":true, "private_delivery":true, "expires_at":request["host_deadline"],
        }),
    );
}

fn response(action: &str, request: &Value) -> Value {
    json!({"action":action,"recovery_id":request["recovery_id"],"requester_ref":"a".repeat(64)})
}

fn state(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn private_device_handoff_finishes_setup_without_model_calls_or_secret_output() {
    let fixture = Fixture::new();
    let (child, mut host) = fixture.begin();
    let request = read(&mut host);
    assert_eq!(request["purpose"], "provider_login_only");
    assert!(!fixture.root.path().join("auth/starts").exists());
    approve(&mut host, &request);
    let challenge = read(&mut host);
    assert_eq!(challenge["mode"], "device_code");
    assert_eq!(challenge["user_code"], "AF-FAKE-SECRET");
    send(&mut host, &response("delivered", &request));
    fixture.complete();
    let finished = read(&mut host);
    assert_eq!(finished["action"], "setup_completed");
    assert_eq!(finished["coordinator_ref"], "b".repeat(64));
    let output = fixture.result(child);
    assert!(
        output.status.success(),
        "status={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(state(&output)["state"], "authenticated_unverified");
    assert_eq!(state(&output)["verified"], false);
    assert_eq!(state(&output)["registered"], true);
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("auth/status-probes")).unwrap(),
        "status\n"
    );
    fixture.no_secrets(&output);
}

#[test]
fn codex_device_completion_requires_successful_native_exit_and_separate_status() {
    for mode in ["nonzero-exit", "status-fails", "late-output"] {
        let fixture = Fixture::new();
        fixture.mode(mode);
        let (child, mut host) = fixture.begin();
        let request = read(&mut host);
        approve(&mut host, &request);
        assert_eq!(read(&mut host)["action"], "challenge");
        assert!(!fixture.root.path().join("auth/status-probes").exists());
        send(&mut host, &response("delivered", &request));
        fixture.complete();
        let output = fixture.result(child);
        assert!(!output.status.success());
        assert_eq!(
            state(&output)["state"],
            if mode == "late-output" {
                "unsupported"
            } else {
                "authentication_failed"
            }
        );
        assert_eq!(
            fixture.root.path().join("auth/status-probes").exists(),
            mode == "status-fails"
        );
        assert!(
            !fixture
                .root
                .path()
                .join("config/af/providers.toml")
                .exists()
        );
        fixture.no_secrets(&output);
    }
}

#[test]
fn native_failure_categories_are_closed_secret_free_and_never_probe_or_register() {
    for (diagnostic, expected) in [
        (
            "OAuth token response is invalid: access_token=AF-FAKE-SECRET",
            "authentication_invalid_token_response",
        ),
        (
            "Failed to configure outbound proxy selected for auth: AF-FAKE-SECRET",
            "authentication_proxy_configuration_failed",
        ),
        (
            "Failed to build HTTP client with explicit TLS configuration: AF-FAKE-SECRET",
            "authentication_tls_configuration_failed",
        ),
        (
            "token endpoint returned status 401 Unauthorized: access_token=AF-FAKE-SECRET",
            "authentication_rejected",
        ),
        (
            "error sending request for url (https://AF-FAKE-SECRET)",
            "authentication_transport_failed",
        ),
        (
            "unknown error access_token=AF-FAKE-SECRET",
            "authentication_failed",
        ),
        (
            "OAuth token response is invalid: AF-FAKE-SECRET\nerror sending request for url (AF-FAKE-SECRET)",
            "authentication_failed",
        ),
    ] {
        for before_prompt in [false, true] {
            let fixture = Fixture::new();
            fixture.mode(if before_prompt {
                "failure-before-prompt"
            } else {
                "nonzero-exit"
            });
            std::fs::write(
                fixture.root.path().join("auth/stderr-fixture"),
                format!(
                    "Error logging in with device code: device code exchange failed: {diagnostic}\n"
                ),
            )
            .unwrap();
            let (child, mut host) = fixture.begin();
            let request = read(&mut host);
            approve(&mut host, &request);
            if !before_prompt {
                assert_eq!(read(&mut host)["action"], "challenge");
                send(&mut host, &response("delivered", &request));
                fixture.complete();
            }
            let output = fixture.result(child);
            assert_eq!(output.status.code(), Some(6));
            let result = state(&output);
            assert_eq!(result["state"], expected);
            assert_eq!(result["exit_code"], 6);
            assert_eq!(result["registered"], false);
            assert_eq!(result["verified"], false);
            assert_eq!(result["continuation"], "not_authorized_by_login");
            assert!(!fixture.root.path().join("auth/status-probes").exists());
            assert!(
                !fixture
                    .root
                    .path()
                    .join("config/af/providers.toml")
                    .exists()
            );
            let mut host_tail = Vec::new();
            host.reader.read_to_end(&mut host_tail).unwrap();
            assert!(
                host_tail.is_empty(),
                "native failure bytes reached host pipe"
            );
            fixture.no_secrets(&output);
            let status = fixture.command("status").output().unwrap();
            assert_eq!(state(&status)["state"], expected);
            assert_eq!(status.status.code(), Some(6));
            fixture.no_secrets(&status);
        }
    }
}

#[test]
fn truncated_and_oversized_native_stderr_remain_generic_without_echo() {
    for diagnostic in [
        "Error logging in with device code: OAuth token response is invalid: AF-FAKE-SECRET"
            .to_string(),
        format!(
            "Error logging in with device code: OAuth token response is invalid: {}\n",
            "AF-FAKE-SECRET".repeat(6000)
        ),
    ] {
        let fixture = Fixture::new();
        fixture.mode("nonzero-exit");
        std::fs::write(fixture.root.path().join("auth/stderr-fixture"), diagnostic).unwrap();
        let (child, mut host) = fixture.begin();
        let request = read(&mut host);
        approve(&mut host, &request);
        assert_eq!(read(&mut host)["action"], "challenge");
        send(&mut host, &response("delivered", &request));
        fixture.complete();
        let output = fixture.result(child);
        assert_eq!(output.status.code(), Some(6));
        assert_eq!(state(&output)["state"], "authentication_failed");
        assert!(!fixture.root.path().join("auth/status-probes").exists());
        fixture.no_secrets(&output);
    }
}

#[test]
fn codex_unknown_version_or_success_without_a_challenge_cannot_register() {
    for mode in ["invalid-prompt", "empty-output"] {
        let fixture = Fixture::new();
        fixture.mode(mode);
        let (child, mut host) = fixture.begin();
        let request = read(&mut host);
        approve(&mut host, &request);
        let output = fixture.result(child);
        assert!(!output.status.success());
        assert_eq!(
            state(&output)["state"],
            if mode == "invalid-prompt" {
                "unsupported"
            } else {
                "authentication_failed"
            }
        );
        assert!(!fixture.root.path().join("auth/status-probes").exists());
        assert!(
            !fixture
                .root
                .path()
                .join("config/af/providers.toml")
                .exists()
        );
        fixture.no_secrets(&output);
    }
}

#[test]
fn concurrent_begin_reuses_session_and_never_sends_a_second_challenge() {
    let fixture = Fixture::new();
    let (child, mut host) = fixture.begin();
    let request = read(&mut host);
    approve(&mut host, &request);
    let _challenge = read(&mut host);
    let output = fixture
        .command("begin")
        .args(["--host-read-fd", "0", "--host-write-fd", "1"])
        .output()
        .unwrap();
    assert_eq!(state(&output)["recovery_id"], request["recovery_id"]);
    assert_eq!(state(&output)["state"], "awaiting_user");
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("auth/starts")).unwrap(),
        "start\n"
    );
    send(&mut host, &response("cancel", &request));
    let output = fixture.result(child);
    assert_eq!(state(&output)["state"], "cancelled");
    fixture.no_secrets(&output);
}

#[test]
fn permission_denial_starts_no_official_login() {
    let fixture = Fixture::new();
    let (child, mut host) = fixture.begin();
    let request = read(&mut host);
    send(
        &mut host,
        &json!({
            "schema":"af/provider-auth-host@1", "recovery_id":request["recovery_id"],
            "provider":request["provider"],"context_id":request["context_id"],
            "requester_ref":"a".repeat(64), "coordinator_ref":"b".repeat(64),
            "approved":false,"private_delivery":true,"expires_at":request["host_deadline"],
        }),
    );
    let output = fixture.result(child);
    assert_eq!(state(&output)["state"], "permission_denied");
    assert!(!fixture.root.path().join("auth/starts").exists());
    fixture.no_secrets(&output);
}

#[test]
fn ordinary_streams_cannot_be_used_as_private_host_pipes() {
    let fixture = Fixture::new();
    let output = fixture
        .command("begin")
        .args(["--host-read-fd", "0", "--host-write-fd", "1"])
        .output()
        .unwrap();
    assert_eq!(state(&output)["state"], "private_route_unavailable");
    assert!(!fixture.root.path().join("auth/starts").exists());
    fixture.no_secrets(&output);
}

#[test]
fn wrong_recipient_and_device_code_response_fail_closed() {
    for wrong_recipient in [true, false] {
        let fixture = Fixture::new();
        let (child, mut host) = fixture.begin();
        let request = read(&mut host);
        approve(&mut host, &request);
        let _challenge = read(&mut host);
        let mut delivered = response("delivered", &request);
        if wrong_recipient {
            delivered["requester_ref"] = json!("c".repeat(64));
        }
        send(&mut host, &delivered);
        if !wrong_recipient {
            let mut code = response("code", &request);
            code["code"] = json!("SHOULD-NOT-REACH-DEVICE-CLI");
            send(&mut host, &code);
        }
        let output = fixture.result(child);
        assert_eq!(state(&output)["state"], "invalid_response");
        fixture.no_secrets(&output);
    }
}

#[test]
fn cancellation_is_scoped_to_current_session_and_status_survives_owner_loss() {
    let fixture = Fixture::new();
    let (child, mut host) = fixture.begin();
    let request = read(&mut host);
    let stale = fixture
        .command("cancel")
        .arg("--recovery-id")
        .arg("f".repeat(64))
        .output()
        .unwrap();
    assert!(!stale.status.success());
    let live = fixture.command("status").output().unwrap();
    assert_eq!(state(&live)["state"], "awaiting_permission");
    // Closing the host input interrupts the owner and leaves a durable blocked result.
    drop(host);
    let result = fixture.result(child);
    assert_eq!(state(&result)["state"], "private_route_unavailable");
    let lost = fixture.command("status").output().unwrap();
    assert_eq!(state(&lost)["state"], "private_route_unavailable");
    assert_eq!(state(&lost)["recovery_id"], request["recovery_id"]);
    fixture.no_secrets(&lost);
}

#[test]
fn explicit_reauthentication_bypasses_stale_authenticated_status() {
    let fixture = Fixture::new();
    let auth = fixture.root.path().join("auth");
    std::fs::create_dir(&auth).unwrap();
    std::fs::write(auth.join("ready"), "").unwrap();
    let (child, mut host) = fixture.begin();
    let request = read(&mut host);
    approve(&mut host, &request);
    assert_eq!(read(&mut host)["action"], "challenge");
    send(&mut host, &response("cancel", &request));
    let output = fixture.result(child);
    assert_eq!(state(&output)["state"], "cancelled");
    assert!(auth.join("starts").exists());
    fixture.no_secrets(&output);
}

#[test]
fn private_status_matches_published_schema() {
    let fixture = Fixture::new();
    let output = fixture
        .command("begin")
        .args(["--host-read-fd", "0", "--host-write-fd", "1"])
        .output()
        .unwrap();
    let root = std::env::var_os("AF_WORKSPACE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .join("schemas/provider-auth-v1.json");
    let schema: Value = serde_json::from_slice(&std::fs::read(root).unwrap()).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&state(&output)));
}

#[test]
fn duplicate_stdout_and_named_fifos_are_not_private_capabilities() {
    for setup in [
        "r,keep=os.pipe();w=os.dup(1)",
        "os.mkfifo(root+'/input',0o600);os.mkfifo(root+'/output',0o600);r=os.open(root+'/input',os.O_RDWR);w=os.open(root+'/output',os.O_RDWR)",
    ] {
        let fixture = Fixture::new();
        let script = format!(
            r#"import os,sys,subprocess
root=sys.argv[1]
{setup}
p=subprocess.run([sys.argv[2],'provider','auth','begin','codex-main','--kind','codex','--auth-dir',root+'/auth','--host-read-fd',str(r),'--host-write-fd',str(w)],pass_fds=(r,w))
sys.exit(p.returncode)
"#
        );
        let output = Command::new("/usr/bin/python3")
            .args(["-c", &script])
            .arg(fixture.root.path())
            .arg(env!("CARGO_BIN_EXE_af"))
            .env_clear()
            .env("HOME", fixture.root.path())
            .env("XDG_CONFIG_HOME", fixture.root.path().join("config"))
            .env("PATH", fixture.root.path().join("bin"))
            .env("AF_SELF_OFFLINE", "1")
            .output()
            .unwrap();
        assert_eq!(state(&output)["state"], "private_route_unavailable");
        assert!(!fixture.root.path().join("auth/starts").exists());
        fixture.no_secrets(&output);
    }
}

#[test]
fn provider_kinds_sharing_directory_keep_independent_recovery_state() {
    let fixture = Fixture::new();
    let (mut codex, mut codex_host) = fixture.begin();
    let codex_request = read(&mut codex_host);
    let (mut claude, mut claude_host) = fixture.begin_kind("claude");
    let claude_request = read(&mut claude_host);
    assert_ne!(codex_request["recovery_id"], claude_request["recovery_id"]);
    for (kind, request) in [("codex", &codex_request), ("claude", &claude_request)] {
        let path = fixture
            .root
            .path()
            .join(format!("auth/.af-{kind}-auth-recovery/session.json"));
        let saved: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved["kind"], kind);
        assert_eq!(saved["recovery_id"], request["recovery_id"]);
    }
    drop(codex_host);
    drop(claude_host);
    codex.wait().unwrap();
    claude.wait().unwrap();
}

#[test]
fn expired_permission_never_starts_login() {
    let fixture = Fixture::new();
    let (child, mut host) = fixture.begin();
    let request = read(&mut host);
    send(
        &mut host,
        &json!({
            "schema":"af/provider-auth-host@1", "recovery_id":request["recovery_id"],
            "provider":request["provider"],"context_id":request["context_id"],
            "requester_ref":"a".repeat(64), "coordinator_ref":"b".repeat(64),
            "approved":true,"private_delivery":true,"expires_at":1,
        }),
    );
    let output = fixture.result(child);
    assert_eq!(state(&output)["state"], "permission_denied");
    assert!(!fixture.root.path().join("auth/starts").exists());
    fixture.no_secrets(&output);
}

#[test]
fn killing_the_owner_reaps_native_login_before_context_becomes_reusable() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let fixture = Fixture::new();
    let (child, mut host) = fixture.begin();
    let request = read(&mut host);
    approve(&mut host, &request);
    let _challenge = read(&mut host);
    let pid = |name: &str| -> Pid {
        Pid::from_raw(
            std::fs::read_to_string(fixture.root.path().join(name))
                .unwrap()
                .parse()
                .unwrap(),
        )
    };
    let native = pid("auth/native.pid");
    let owner = pid("owner.pid");
    // The synthetic provider deliberately ignores stdin while polling its completion marker.
    kill(owner, Signal::SIGKILL).unwrap();
    let _owner_result = fixture.result(child);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let output = fixture.command("status").output().unwrap();
        if state(&output)["state"] == "interrupted" {
            assert!(
                matches!(kill(native, None), Err(nix::errno::Errno::ESRCH)),
                "native login outlived its auth context lock"
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "orphan guard did not settle"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!fixture.root.path().join("auth/ready").exists());
}

#[test]
fn claude_private_code_and_callback_paths_complete_without_task_dispatch() {
    for callback in [false, true] {
        let fixture = Fixture::new();
        let path = fixture.root.path().join("bin/claude");
        std::fs::write(&path, r#"#!/usr/bin/python3
import json,os,sys,time
from pathlib import Path
root=Path(os.environ['CLAUDE_CONFIG_DIR'])
if sys.argv[1:]==['auth','status','--json']:
    ready=(root/'ready').exists()
    print(json.dumps({'loggedIn':ready,'authMethod':'claude.ai' if ready else 'none','apiProvider':'firstParty'}))
    sys.exit(0 if ready else 1)
if sys.argv[1:]!=['auth','login','--claudeai']:sys.exit(64)
assert os.environ.get('HTTP_PROXY')=='http://fixture.invalid:8080'
assert os.environ.get('SSL_CERT_FILE')=='/fixture/certificate.pem'
assert all(name not in os.environ for name in ['OPENAI_API_KEY','ANTHROPIC_API_KEY','NODE_OPTIONS','NODE_TLS_REJECT_UNAUTHORIZED','BROWSER','DISPLAY'])
url='https://claude.ai/oauth/authorize?state=AF-PRIVATE-FIXTURE'
print('Opening browser to sign in…',flush=True)
print("If the browser didn't open, visit: "+url,flush=True)
print('Paste code here if prompted > ',end='',flush=True)
if (root/'callback').exists():
    while not (root/'complete').exists():time.sleep(.01)
else:
    if sys.stdin.readline().strip()!='AF-ONE-TIME-FIXTURE':sys.exit(65)
(root/'ready').touch()
print('Login successful.',flush=True)
"#).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        if callback {
            std::fs::create_dir(fixture.root.path().join("auth")).unwrap();
            std::fs::write(fixture.root.path().join("auth/callback"), "").unwrap();
        }
        let (child, mut host) = fixture.begin_kind("claude");
        let request = read(&mut host);
        approve(&mut host, &request);
        let challenge = read(&mut host);
        assert_eq!(challenge["mode"], "code_or_callback");
        assert_eq!(challenge["user_code"], Value::Null);
        send(&mut host, &response("delivered", &request));
        if callback {
            fixture.complete();
        } else {
            let mut code = response("code", &request);
            code["code"] = json!("AF-ONE-TIME-FIXTURE");
            send(&mut host, &code);
        }
        assert_eq!(read(&mut host)["action"], "setup_completed");
        let output = fixture.result(child);
        assert!(output.status.success());
        assert_eq!(state(&output)["state"], "authenticated_unverified");
        let saved = std::fs::read_to_string(
            fixture
                .root
                .path()
                .join("auth/.af-claude-auth-recovery/session.json"),
        )
        .unwrap();
        for text in [
            saved,
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ] {
            assert!(!text.contains("AF-PRIVATE-FIXTURE"));
            assert!(!text.contains("AF-ONE-TIME-FIXTURE"));
            assert!(!text.contains("oauth/authorize"));
        }
    }
}

#[test]
fn cancellation_while_registry_is_busy_cannot_publish_setup_success() {
    let fixture = Fixture::new();
    let (mut child, mut host) = fixture.begin();
    let request = read(&mut host);
    approve(&mut host, &request);
    let _challenge = read(&mut host);
    let registry_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.root.path().join("config/af/providers.toml.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&registry_lock).unwrap();
    send(&mut host, &response("delivered", &request));
    fixture.complete();
    // Keep the competing lock held while issuing scoped cancellation. The owner must stop
    // independently of whether it has reached the token-free probe or the registry wait yet.
    let cancel = fixture
        .command("cancel")
        .arg("--recovery-id")
        .arg(request["recovery_id"].as_str().unwrap())
        .output()
        .unwrap();
    assert!(cancel.status.success());
    assert_eq!(state(&cancel)["state"], "cancellation_requested");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let stopped_while_locked = child.try_wait().unwrap().is_some();
    drop(registry_lock);
    let output = fixture.result(child);
    assert!(
        stopped_while_locked,
        "cancellation waited on the registry lock"
    );
    assert_eq!(state(&output)["state"], "cancelled");
    assert!(
        !fixture
            .root
            .path()
            .join("config/af/providers.toml")
            .exists()
    );
    fixture.no_secrets(&output);
}

#[test]
fn reauthentication_never_recreates_an_existing_binding_removed_during_login() {
    let fixture = Fixture::new();
    let auth = fixture.root.path().join("auth");
    std::fs::create_dir(&auth).unwrap();
    std::fs::write(auth.join("ready"), "").unwrap();
    let config = fixture.root.path().join("config/af");
    std::fs::create_dir_all(&config).unwrap();
    let registry = config.join("providers.toml");
    std::fs::write(
        &registry,
        format!(
            "version = 1\n[[providers]]\nid = \"codex-main\"\nkind = \"codex\"\nauth_dir = {}\n",
            serde_json::to_string(auth.to_str().unwrap()).unwrap()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&registry, std::fs::Permissions::from_mode(0o600)).unwrap();
    let (child, mut host) = fixture.begin();
    let request = read(&mut host);
    approve(&mut host, &request);
    let _challenge = read(&mut host);
    send(&mut host, &response("delivered", &request));
    // Represent another authorized registry writer; credentials and this login are untouched.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(config.join("providers.toml.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    std::fs::write(&registry, "version = 1\nproviders = []\n").unwrap();
    drop(lock);
    fixture.complete();
    let output = fixture.result(child);
    assert_eq!(state(&output)["state"], "registry_conflict");
    assert_eq!(state(&output)["registered"], false);
    assert_eq!(
        std::fs::read_to_string(registry).unwrap(),
        "version = 1\nproviders = []\n"
    );
    fixture.no_secrets(&output);
}
