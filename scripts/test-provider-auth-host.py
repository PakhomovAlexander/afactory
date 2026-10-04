#!/usr/bin/env python3
"""Credential-free checks for the concrete private-chat bridge. Never starts OAuth."""
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import pty
import select
import subprocess
import sys
import tempfile
import time
import unittest
from urllib.parse import urlencode

HERE = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("provider_auth_host", HERE / "provider-auth-host.py")
HOST = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(HOST)
RECOVERY = "a" * 64
REQUESTER = "b" * 64
COORDINATOR = "c" * 64
CONTEXT = "d" * 64
OAUTH_STATE = "s" * 43


def claude_url(**overrides):
    query = {"code": "true", "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
             "response_type": "code", "redirect_uri": "https://platform.claude.com/oauth/code/callback",
             "scope": "user:profile user:inference user:sessions:claude_code",
             "code_challenge": "p" * 43, "code_challenge_method": "S256", "state": OAUTH_STATE}
    query.update(overrides)
    return "https://claude.com/cai/oauth/authorize?" + urlencode(query)


def permission(kind="codex"):
    return {"schema": HOST.HOST, "action": "request_permission", "recovery_id": RECOVERY,
            "provider": kind + "-main", "kind": kind, "context_id": CONTEXT,
            "host_deadline": int(time.time()) + 90, "purpose": "provider_login_only"}


def approval():
    return {"action": "approve", "recovery_id": RECOVERY, "requester_ref": REQUESTER,
            "context_id": CONTEXT, "approval_ref": "real-private-consent-message"}


def response(action, **extra):
    return {"action": action, "recovery_id": RECOVERY, "requester_ref": REQUESTER, **extra}


def challenge(request):
    codex = request["kind"] == "codex"
    return {"schema": HOST.HOST, "action": "challenge", "recovery_id": RECOVERY,
            "provider": request["provider"], "requester_ref": REQUESTER,
            "mode": "device_code" if codex else "code_or_callback",
            "url": "https://auth.openai.com/codex/device" if codex else claude_url(),
            "user_code": "ABCD-EFGHJ" if codex else None, "host_deadline": request["host_deadline"]}


def status(kind="codex", state="authenticated_unverified", code=0):
    return {"schema": "af/provider-auth@1", "recovery_id": RECOVERY,
            "provider": kind + "-main", "kind": kind, "state": state,
            "created_at": int(time.time()), "host_deadline": int(time.time()) + 90,
            "challenge_delivered": state == "authenticated_unverified",
            "registered": state == "authenticated_unverified", "verified": False,
            "continuation": "not_authorized_by_login", "exit_code": code}


class ProtocolTests(unittest.TestCase):
    def fixture(self, kind="codex", approve=True):
        self.output, self.sent = [], []
        self.bridge = HOST.Bridge(kind + "-main", kind, REQUESTER, COORDINATOR,
                                  self.output.append, self.sent.append)
        self.request = permission(kind)
        self.bridge.event(self.request)
        if approve:
            self.bridge.command(approval())
        return self.bridge

    def test_no_implicit_approval(self):
        bridge = self.fixture(approve=False)
        self.assertEqual(self.sent, [])
        self.assertNotIn("url", self.output[0])
        with self.assertRaises(HOST.Rejected):
            bridge.event(challenge(self.request))

    def test_grant_bound_to_exact_context_and_original_recipient(self):
        for key in ["context_id", "recovery_id", "requester_ref"]:
            bridge = self.fixture(approve=False)
            value = approval()
            value[key] = "e" * 64
            with self.assertRaises(HOST.Rejected):
                bridge.command(value)
            self.assertEqual(self.sent, [])

    def test_success_requires_real_delivery_ack(self):
        bridge = self.fixture()
        bridge.event(challenge(self.request))
        completed = {"schema": HOST.HOST, "action": "setup_completed", "recovery_id": RECOVERY,
                     "coordinator_ref": COORDINATOR, "status": status()}
        with self.assertRaises(HOST.Rejected):
            bridge.event(completed)
        bridge.command(response("delivered", delivery_ref="sent-message-id"))
        bridge.event(completed)
        self.assertEqual(self.output[-1]["status"]["state"], "authenticated_unverified")
        self.assertFalse(self.output[-1]["status"]["verified"])

    def test_unknown_fields_and_duplicate_json_keys_fail_closed(self):
        with self.assertRaises(HOST.Rejected):
            HOST.frame(b'{"action":"approve","action":"cancel"}')
        bridge = self.fixture(approve=False)
        with self.assertRaises(HOST.Rejected):
            bridge.command({**approval(), "secret": "DO-NOT-REFLECT"})

    def test_wrong_challenge_recipient_and_session_rejected_before_output(self):
        for key in ["provider", "recovery_id", "requester_ref", "host_deadline"]:
            bridge = self.fixture()
            value = challenge(self.request)
            value[key] = "wrong"
            with self.assertRaises(HOST.Rejected):
                bridge.event(value)
            self.assertEqual(len(self.output), 1)

    def test_replayed_grant_challenge_and_delivery_rejected(self):
        bridge = self.fixture()
        with self.assertRaises(HOST.Rejected):
            bridge.command(approval())
        bridge.event(challenge(self.request))
        with self.assertRaises(HOST.Rejected):
            bridge.event(challenge(self.request))
        bridge.command(response("delivered", delivery_ref="sent-message"))
        with self.assertRaises(HOST.Rejected):
            bridge.command(response("delivered", delivery_ref="sent-message"))

    def test_expired_approval_does_not_start_login(self):
        bridge = self.fixture(approve=False)
        bridge.request["host_deadline"] = int(time.time()) - 1
        with self.assertRaises(HOST.Rejected):
            bridge.command(approval())
        self.assertEqual(self.sent, [])

    def test_codex_no_response_code_and_strict_challenge(self):
        bridge = self.fixture()
        bridge.event(challenge(self.request))
        bridge.command(response("delivered", delivery_ref="sent-message"))
        with self.assertRaises(HOST.Rejected):
            bridge.command(response("code", code="ABCD-EFGHJ"))
        for invalid in ["sk-secret", "ABCD\nEFGHI", "https://bad.invalid", "ABCD-EFGHJ-extra"]:
            bridge = self.fixture()
            value = challenge(self.request)
            value["user_code"] = invalid
            with self.assertRaises(HOST.Rejected):
                bridge.event(value)

    def test_claude_returned_code_bound_to_url_state_not_echoed(self):
        bridge = self.fixture("claude")
        bridge.event(challenge(self.request))
        bridge.command(response("delivered", delivery_ref="sent-message"))
        code = "ONE-TIME-CODE#" + OAUTH_STATE
        bridge.command(response("code", code=code))
        self.assertEqual(self.sent[-1]["code"], code)
        self.assertNotIn(code, json.dumps(self.output))
        with self.assertRaises(HOST.Rejected):
            bridge.command(response("code", code=code))

    def test_claude_mismatched_state_token_and_multiline_rejected(self):
        for code in ["CODE#wrong", "CODE", "CODE#" + OAUTH_STATE + "#EXTRA",
                     "CODE\n#" + OAUTH_STATE, "sk-ant-secret#" + OAUTH_STATE,
                     "eyJ.jwt#" + OAUTH_STATE]:
            bridge = self.fixture("claude")
            bridge.event(challenge(self.request))
            bridge.command(response("delivered", delivery_ref="sent-message"))
            with self.assertRaises(HOST.Rejected):
                bridge.command(response("code", code=code))

    def test_claude_url_rejects_token_redirect_extra_scope_and_user_info(self):
        for url in [claude_url(access_token="secret"), claude_url(scope="secret"),
                    claude_url(redirect_uri="https://evil.invalid/callback"),
                    claude_url(client_id="other-client"), claude_url(login_hint="private@example.com"),
                    claude_url().replace("claude.com/cai", "claude.com@evil.invalid/cai"),
                    claude_url().replace("/cai/oauth/authorize", "/v1/oauth/token"),
                    claude_url() + "&state=duplicate", claude_url() + "#secret"]:
            with self.assertRaises((HOST.Rejected, ValueError)):
                HOST.challenge_url("claude", url)
        self.assertEqual(HOST.challenge_url("claude", claude_url()), OAUTH_STATE)

    def test_status_drops_no_unknown_native_fields(self):
        value = status()
        value["access_token"] = "secret"
        with self.assertRaises(HOST.Rejected):
            HOST.public_status(value, "codex-main", "codex")

    def test_closed_native_failure_states_do_not_authorize_continuation(self):
        for state in ["authentication_invalid_token_response", "authentication_proxy_configuration_failed",
                      "authentication_tls_configuration_failed", "authentication_rejected",
                      "authentication_transport_failed"]:
            value = HOST.public_status(status(state=state, code=6), "codex-main", "codex")
            self.assertFalse(value["verified"])
            self.assertEqual(value["continuation"], "not_authorized_by_login")
        for state, code in [("authenticated_unverified", 6), ("authentication_failed", 0),
                            ("arbitrary-native-secret", 6)]:
            with self.assertRaises(HOST.Rejected):
                HOST.public_status(status(state=state, code=code), "codex-main", "codex")

    def test_frames_bounded(self):
        with self.assertRaises(HOST.Rejected):
            HOST.frame(b"x" * (HOST.LIMIT + 1))

    def test_cancel_revokes_presentation_and_code_authority_immediately(self):
        bridge = self.fixture("claude")
        bridge.command(response("cancel"))
        bridge.event(challenge(self.request))
        self.assertEqual(len(self.output), 1)
        with self.assertRaises(HOST.Rejected):
            bridge.command(response("code", code="CODE#" + OAUTH_STATE))

    def test_expired_challenge_is_never_presented(self):
        bridge = self.fixture()
        bridge.request["host_deadline"] = int(time.time()) - 1
        with self.assertRaises(HOST.Rejected):
            bridge.event(challenge(self.request))
        self.assertEqual(len(self.output), 1)

    @unittest.skipUnless(sys.platform == "linux", "Linux pipe-capacity fixture")
    def test_blocked_presentation_pipe_has_a_bounded_write_deadline(self):
        read, write = os.pipe()
        try:
            fcntl.fcntl(write, fcntl.F_SETPIPE_SZ, 4096)
            os.set_blocking(write, False)
            os.write(write, b"x" * 4096)
            start = time.monotonic()
            with self.assertRaises(HOST.Rejected):
                HOST.emit({"schema": HOST.CHAT, "action": "challenge"}, fd=write, timeout=0.05)
            self.assertLess(time.monotonic() - start, 0.5)
        finally:
            os.close(read)
            os.close(write)


@unittest.skipUnless(sys.platform == "linux", "Private auth host is Linux-only")
class ProcessTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.fake = self.root / "af"
        # A deliberately small AF protocol fixture, not a provider or live login.
        self.fake.write_text('''#!/usr/bin/env python3
import json,os,sys,time,stat
args=sys.argv
r=int(args[args.index('--host-read-fd')+1]); w=int(args[args.index('--host-write-fd')+1])
assert r>2 and w>2 and r!=w and stat.S_ISFIFO(os.fstat(r).st_mode)
kind=args[args.index('--kind')+1]; root=args[args.index('--auth-dir')+1]
request=json.loads(os.environ['FIXTURE_REQUEST']); challenge=json.loads(os.environ['FIXTURE_CHALLENGE'])
status=json.loads(os.environ['FIXTURE_STATUS'])
stream=os.fdopen(r,'r')
def emit(value): os.write(w,json.dumps(value).encode()+b'\\n')
emit(request)
line=stream.readline()
if not line: sys.exit(4)
grant=json.loads(line); assert grant['approved'] and grant['private_delivery']
open(root+'/started','w').close()
emit(challenge)
line=stream.readline()
if not line: sys.exit(4)
assert json.loads(line)['action']=='delivered'
if kind=='claude':
    reply=json.loads(stream.readline()); assert reply['action']=='code'
print('NATIVE-SECRET-MUST-NOT-LEAK',file=sys.stderr,flush=True)
emit({'schema':'af/provider-auth-host@1','action':'setup_completed',
      'recovery_id':request['recovery_id'],'coordinator_ref':'c'*64,'status':status})
print(json.dumps(status),flush=True)
''')
        self.fake.chmod(0o755)
        self.process = None
        self.master = None

    def tearDown(self):
        if self.process:
            if self.process.poll() is None:
                self.process.kill()
            self.process.wait()
            for stream in [self.process.stdin, self.process.stdout, self.process.stderr]:
                if stream:
                    stream.close()
        if self.master is not None:
            os.close(self.master)
        self.temp.cleanup()

    def spawn(self, kind="codex", use_pty=False):
        request = permission(kind)
        env = {**os.environ, "FIXTURE_REQUEST": json.dumps(request),
               "FIXTURE_CHALLENGE": json.dumps(challenge(request)),
               "FIXTURE_STATUS": json.dumps(status(kind))}
        command = [sys.executable, str(HERE / "provider-auth-host.py"), kind + "-main",
                   "--kind", kind, "--auth-dir", str(self.root), "--af", str(self.fake),
                   "--requester-ref", REQUESTER, "--coordinator-ref", COORDINATOR,
                   "--delivery", "verified-private-chat"]
        slave = None
        if use_pty:
            self.master, slave = pty.openpty()
        self.process = subprocess.Popen(command, stdin=slave if use_pty else subprocess.PIPE,
                                        stdout=slave if use_pty else subprocess.PIPE,
                                        stderr=subprocess.PIPE, env=env)
        if slave is not None:
            os.close(slave)
        self.fd = self.master if use_pty else self.process.stdout.fileno()
        self.buffer = b""
        return self.read()

    def read(self):
        deadline = time.monotonic() + 5
        while b"\n" not in self.buffer:
            remaining = deadline - time.monotonic()
            self.assertGreater(remaining, 0, "bridge did not produce a complete frame")
            self.assertTrue(select.select([self.fd], [], [], remaining)[0])
            self.buffer += os.read(self.fd, 8192)
        line, _, self.buffer = self.buffer.partition(b"\n")
        return json.loads(line)

    def send(self, value):
        raw = json.dumps(value).encode() + b"\n"
        if self.master is not None:
            os.write(self.master, raw)
        else:
            self.process.stdin.write(raw)
            self.process.stdin.flush()

    def test_actual_anonymous_pipes_survive_idle_then_finish(self):
        self.assertEqual(self.spawn()["action"], "request_permission")
        self.assertFalse((self.root / "started").exists())
        self.assertFalse(select.select([self.fd], [], [], 0.05)[0])
        self.send(approval())
        self.assertEqual(self.read()["user_code"], "ABCD-EFGHJ")
        self.send(response("delivered", delivery_ref="private-message-receipt"))
        self.assertEqual(self.read()["action"], "setup_completed")
        self.assertEqual(self.read()["action"], "result")
        self.assertEqual(self.process.wait(timeout=5), 0)
        self.assertEqual(self.process.stderr.read(), b"")

    def test_pty_response_is_not_echoed(self):
        self.spawn("claude", use_pty=True)
        self.send(approval())
        self.assertEqual(self.read()["action"], "challenge")
        self.send(response("delivered", delivery_ref="private-message-receipt"))
        self.send(response("code", code="UNIQUE-RESPONSE#" + OAUTH_STATE))
        self.assertEqual(self.read()["action"], "setup_completed")
        self.assertEqual(self.read()["action"], "result")
        self.assertEqual(self.process.wait(timeout=5), 0)
        self.assertNotIn(b"UNIQUE-RESPONSE", self.buffer)

    def test_unknown_input_never_reflected_and_no_login(self):
        self.spawn()
        self.send({**approval(), "access_token": "SECRET-INPUT"})
        result = self.read()
        self.assertEqual(result["action"], "bridge_error")
        self.assertNotIn("SECRET-INPUT", json.dumps(result))
        self.assertEqual(self.process.wait(timeout=5), 4)
        self.assertFalse((self.root / "started").exists())

    def test_deep_json_has_closed_error_without_traceback(self):
        self.spawn()
        self.process.stdin.write(b'{"a":' + b"[" * 1100 + b"0" + b"]" * 1100 + b"}\n")
        self.process.stdin.flush()
        self.assertEqual(self.read()["action"], "bridge_error")
        self.assertEqual(self.process.wait(timeout=5), 4)
        self.assertEqual(self.process.stderr.read(), b"")


if __name__ == "__main__":
    unittest.main()
