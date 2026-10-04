#!/usr/bin/env python3
"""One-session adapter for an externally verified private-chat coordinator.

This command is NOT an authorization or chat-delivery service. Its caller owns
those capabilities (see docs/provider-auth-host.md). Only the explicitly selected
private-chat output may carry validated, short-lived challenges. Native output,
credentials and response codes are never reflected. No network listener or files.
"""
import argparse
import json
import os
from pathlib import Path
import re
import select
import selectors
import subprocess
import sys
import termios
import time
from urllib.parse import parse_qsl, urlsplit


HOST = "af/provider-auth-host@1"
CHAT = "af/provider-auth-chat@1"
LIMIT = 16 * 1024
STATES = {
    "awaiting_permission", "starting", "awaiting_user", "completing",
    "authenticated_unverified", "cancelled", "cancellation_requested",
    "expired", "interrupted", "permission_denied", "private_route_unavailable",
    "unsupported", "authentication_failed", "invalid_challenge", "invalid_response",
    "context_changed", "registry_conflict",
    "authentication_invalid_token_response", "authentication_proxy_configuration_failed",
    "authentication_tls_configuration_failed", "authentication_rejected",
    "authentication_transport_failed",
}


class Rejected(Exception):
    """Closed failure: never include untrusted input or an exception's text."""


def require(condition):
    if not condition:
        raise Rejected()


def opaque(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def reference(value):
    return isinstance(value, str) and re.fullmatch(r"[A-Za-z0-9_:./-]{1,256}", value) is not None


def unique_object(pairs):
    obj = {}
    for key, value in pairs:
        require(key not in obj)
        obj[key] = value
    return obj


def frame(raw):
    require(len(raw) <= LIMIT)
    value = json.loads(raw, object_pairs_hook=unique_object)
    require(isinstance(value, dict))
    return value


def exact(value, keys):
    require(set(value) == set(keys.split()))


def challenge_url(kind, value):
    require(isinstance(value, str) and 0 < len(value) <= 12 * 1024)
    require(value.isascii() and not any(c.isspace() for c in value))
    require(re.fullmatch(r"[A-Za-z0-9:/?%&=._~!$()*+,;@-]+", value) is not None)
    parsed = urlsplit(value)
    require(parsed.scheme == "https" and not parsed.fragment and not parsed.username
            and not parsed.password and parsed.port is None)
    if kind == "codex":
        require(value == "https://auth.openai.com/codex/device")
        return None
    require((parsed.netloc, parsed.path) in {
        ("claude.com", "/cai/oauth/authorize"),
        ("claude.ai", "/oauth/authorize"),
        ("platform.claude.com", "/oauth/authorize"),
    })
    pairs = parse_qsl(parsed.query, keep_blank_values=True, strict_parsing=True)
    query = unique_object(pairs)
    # An authorization request is allowed; token-bearing URLs and arbitrary
    # same-origin navigation are not. Never repair or synthesize the native URL.
    require(set(query) <= {
        "code", "client_id", "response_type", "redirect_uri", "scope", "state",
        "code_challenge", "code_challenge_method", "orgUUID",
    })
    require(query.get("response_type") == "code"
            and query.get("code_challenge_method") == "S256"
            and re.fullmatch(r"[A-Za-z0-9_-]{43}", query.get("code_challenge", ""))
            and re.fullmatch(r"[A-Za-z0-9_-]{43}", query.get("state", ""))
            and query.get("client_id") == "9d1c250a-e61b-44d9-88ed-5944d1962f5e")
    require(query.get("code") == "true")
    scopes = query.get("scope", "").split()
    require(bool(scopes) and " ".join(scopes) == query.get("scope")
            and len(scopes) == len(set(scopes)) and set(scopes) <= {
        "org:create_api_key", "user:profile", "user:inference", "user:sessions:claude_code",
        "user:mcp_servers", "user:file_upload", "user:plugins",
    } and {"user:profile", "user:inference"} <= set(scopes))
    if "orgUUID" in query:
        require(re.fullmatch(r"[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}", query["orgUUID"]))
    redirect = urlsplit(query.get("redirect_uri", ""))
    require(not redirect.username and not redirect.password and not redirect.fragment
            and not redirect.query)
    require(redirect.scheme == "https" and redirect.netloc == "platform.claude.com"
            and redirect.path == "/oauth/code/callback")
    return query["state"]


def public_status(value, provider, kind, recovery=None):
    exact(value, "schema recovery_id provider kind state created_at host_deadline "
          "challenge_delivered registered verified continuation exit_code")
    require(value["schema"] == "af/provider-auth@1" and value["provider"] == provider
            and value["kind"] == kind and opaque(value["recovery_id"])
            and (recovery is None or value["recovery_id"] == recovery)
            and value["state"] in STATES and value["verified"] is False
            and value["continuation"] == "not_authorized_by_login")
    require(all(type(value[k]) is bool for k in ["challenge_delivered", "registered"]))
    require(all(type(value[k]) is int and value[k] >= 0 for k in [
        "created_at", "host_deadline", "exit_code"]))
    expected = (0 if value["state"] in {"authenticated_unverified", "cancellation_requested"}
                else 5 if value["state"] == "registry_conflict"
                else 6 if value["state"].startswith("authentication_")
                or value["state"] in {"invalid_challenge", "invalid_response", "context_changed"}
                else 3)
    require(value["exit_code"] == expected)
    return value


class Bridge:
    def __init__(self, provider, kind, requester, coordinator, emit, send):
        self.provider = provider
        self.kind = kind
        self.requester = requester
        self.coordinator = coordinator
        self.emit = emit
        self.send = send
        self.request = None
        self.approved = False
        self.presented = False
        self.delivered = False
        self.consumed = False
        self.completed = False
        self.cancelled = False
        self.oauth_state = None

    def event(self, value):
        action = value.get("action")
        require(value.get("schema") == HOST)
        if self.cancelled:
            # The private channel may already contain an in-flight challenge.
            # A user's cancellation revokes presentation authority immediately.
            return
        if action == "request_permission":
            exact(value, "schema action recovery_id provider kind context_id host_deadline purpose")
            require(self.request is None and value["provider"] == self.provider
                    and value["kind"] == self.kind and opaque(value["recovery_id"])
                    and opaque(value["context_id"]) and value["purpose"] == "provider_login_only"
                    and type(value["host_deadline"]) is int
                    and time.time() < value["host_deadline"] <= time.time() + 901)
            self.request = value
            self.emit({**value, "schema": CHAT, "requester_ref": self.requester,
                       "coordinator_ref": self.coordinator})
            return
        require(self.request is not None and self.approved
                and value.get("recovery_id") == self.request["recovery_id"])
        if action == "challenge":
            exact(value, "schema action recovery_id provider requester_ref mode url user_code host_deadline")
            require(not self.presented and value["provider"] == self.provider
                    and value["requester_ref"] == self.requester
                    and value["host_deadline"] == self.request["host_deadline"]
                    and time.time() < value["host_deadline"])
            self.oauth_state = challenge_url(self.kind, value["url"])
            if self.kind == "codex":
                require(value["mode"] == "device_code" and isinstance(value["user_code"], str)
                        and re.fullmatch(r"[A-Z0-9-]{4,128}", value["user_code"]))
            else:
                require(value["mode"] == "code_or_callback" and value["user_code"] is None)
            self.presented = True
            self.emit({**value, "schema": CHAT})
            return
        if action == "setup_completed":
            exact(value, "schema action recovery_id coordinator_ref status")
            require(self.delivered and not self.completed and value["coordinator_ref"] == self.coordinator)
            status = public_status(value["status"], self.provider, self.kind, self.request["recovery_id"])
            require(status["state"] == "authenticated_unverified")
            self.completed = True
            self.emit({"schema": CHAT, "action": "setup_completed", "status": status})
            return
        raise Rejected()

    def command(self, value):
        require(self.request is not None and not self.completed and not self.cancelled
                and value.get("recovery_id") == self.request["recovery_id"]
                and value.get("requester_ref") == self.requester
                and time.time() < self.request["host_deadline"])
        action = value.get("action")
        if action == "approve":
            exact(value, "action recovery_id requester_ref context_id approval_ref")
            require(not self.approved and value["context_id"] == self.request["context_id"]
                    and reference(value["approval_ref"]))
            self.send({"schema": HOST, "recovery_id": self.request["recovery_id"],
                       "provider": self.provider, "context_id": self.request["context_id"],
                       "requester_ref": self.requester, "coordinator_ref": self.coordinator,
                       "approved": True, "private_delivery": True,
                       "expires_at": self.request["host_deadline"]})
            self.approved = True
            return
        require(self.approved)
        response = {"action": action, "recovery_id": self.request["recovery_id"],
                    "requester_ref": self.requester}
        if action == "delivered":
            exact(value, "action recovery_id requester_ref delivery_ref")
            require(self.presented and not self.delivered and reference(value["delivery_ref"]))
            self.delivered = True
        elif action == "code":
            exact(value, "action recovery_id requester_ref code")
            require(self.kind == "claude" and self.delivered and not self.consumed
                    and isinstance(value["code"], str))
            # Claude's manual browser response is code#state, never an access token.
            parts = value["code"].split("#")
            require(len(parts) == 2 and parts[1] == self.oauth_state
                    and 1 <= len(parts[0]) <= 1024
                    and parts[0].isascii() and all(33 <= ord(c) <= 126 for c in parts[0])
                    and not parts[0].startswith(("sk-", "eyJ")))
            response["code"] = value["code"]
            self.consumed = True
        elif action == "cancel":
            exact(value, "action recovery_id requester_ref")
            self.cancelled = True
        else:
            raise Rejected()
        self.send(response)


def emit(value, fd=1, timeout=1):
    data = json.dumps(value, separators=(",", ":")).encode() + b"\n"
    require(len(data) <= LIMIT)
    previous = os.get_blocking(fd)
    deadline = time.monotonic() + timeout
    written = 0
    try:
        os.set_blocking(fd, False)
        while written < len(data):
            require(time.monotonic() < deadline)
            try:
                count = os.write(fd, data[written:])
                require(count > 0)
                written += count
            except BlockingIOError:
                select.select([], [fd], [], min(0.025, max(0, deadline - time.monotonic())))
    finally:
        os.set_blocking(fd, previous)


def run(args):
    require(sys.platform == "linux" and Path(args.auth_dir).is_absolute())
    require(opaque(args.requester_ref) and opaque(args.coordinator_ref))
    require(re.fullmatch(r"[A-Za-z0-9_-]{1,128}", args.provider) is not None)
    require(30 <= args.timeout_secs <= 900)
    request_read, request_write = os.pipe()
    event_read, event_write = os.pipe()
    process = None
    selector = selectors.DefaultSelector()
    settings = None
    try:
        # A PTY may be used by the coordinator to retain stdin across turns. Do not
        # echo approve/code messages back into its captured output.
        if os.isatty(0):
            settings = termios.tcgetattr(0)
            quiet = termios.tcgetattr(0)
            quiet[3] &= ~(termios.ECHO | termios.ECHONL)
            termios.tcsetattr(0, termios.TCSANOW, quiet)
        for fd in [request_read, request_write, event_read, event_write]:
            os.fchmod(fd, 0o600)
        process = subprocess.Popen(
            [args.af, "provider", "auth", "begin", args.provider, "--kind", args.kind,
             "--auth-dir", args.auth_dir, "--timeout-secs", str(args.timeout_secs),
             "--host-read-fd", str(request_read), "--host-write-fd", str(event_write)],
            pass_fds=(request_read, event_write), stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, start_new_session=True)
        os.close(request_read)
        request_read = None
        os.close(event_write)
        event_write = None
        for fd in [0, request_write, event_read, process.stdout.fileno()]:
            os.set_blocking(fd, False)
        pending = bytearray()

        def send(value):
            data = json.dumps(value, separators=(",", ":")).encode() + b"\n"
            require(len(data) <= LIMIT and len(pending) + len(data) <= LIMIT)
            pending.extend(data)

        bridge = Bridge(args.provider, args.kind, args.requester_ref, args.coordinator_ref, emit, send)
        buffers = {"command": bytearray(), "event": bytearray(), "status": bytearray()}
        totals = {name: 0 for name in buffers}
        selector.register(0, selectors.EVENT_READ, "command")
        selector.register(event_read, selectors.EVENT_READ, "event")
        selector.register(process.stdout, selectors.EVENT_READ, "status")
        deadline = time.monotonic() + args.timeout_secs + 5
        status = None
        while True:
            require(time.monotonic() < deadline)
            if pending:
                try:
                    written = os.write(request_write, pending)
                    pending[:written] = b"\0" * written
                    del pending[:written]
                except BlockingIOError:
                    pass
            for key, _ in selector.select(0.025):
                name = key.data
                data = os.read(key.fd, 2048)
                if not data:
                    selector.unregister(key.fileobj)
                    require(not buffers[name])
                    if name == "command":
                        # Closing the capability cancels af and its lifetime guard.
                        os.close(request_write)
                        request_write = None
                        pending.clear()
                    continue
                totals[name] += len(data)
                require(totals[name] <= 4 * LIMIT)
                buffers[name].extend(data)
                require(len(buffers[name]) <= LIMIT)
                while b"\n" in buffers[name]:
                    line, _, rest = buffers[name].partition(b"\n")
                    buffers[name][:] = rest
                    value = frame(line)
                    if name == "command":
                        bridge.command(value)
                    elif name == "event":
                        bridge.event(value)
                    else:
                        require(status is None)
                        status = public_status(value, args.provider, args.kind,
                                               bridge.request["recovery_id"] if bridge.request else None)
            code = process.poll()
            if code is not None and not any(k.data in {"event", "status"}
                                            for k in selector.get_map().values()):
                require(status is not None and code == status["exit_code"])
                emit({"schema": CHAT, "action": "result", "status": status})
                return code
    finally:
        # EOF, invalid input and hard owner loss all reach the existing af lifetime
        # guard. Never kill that guard before it has reaped the native login.
        for fd in [request_read, request_write, event_read, event_write]:
            if fd is not None:
                os.close(fd)
        selector.close()
        if process is not None:
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            process.stdout.close()
        if settings is not None:
            termios.tcsetattr(0, termios.TCSANOW, settings)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("provider")
    parser.add_argument("--kind", choices=["codex", "claude"], required=True)
    parser.add_argument("--auth-dir", required=True)
    parser.add_argument("--af", default="af")
    parser.add_argument("--requester-ref", required=True)
    parser.add_argument("--coordinator-ref", required=True)
    parser.add_argument("--delivery", choices=["verified-private-chat"], required=True,
                        help="caller has verified this exact original requester's private channel")
    parser.add_argument("--timeout-secs", type=int, default=600)
    args = parser.parse_args()
    try:
        return run(args)
    except (Rejected, OSError, ValueError, KeyError, TypeError, RecursionError, KeyboardInterrupt):
        try:
            emit({"schema": CHAT, "action": "bridge_error", "state": "private_route_unavailable"})
        except (Rejected, OSError):
            pass  # A closed/blocked private output is not a reason to expose a traceback.
        return 4


if __name__ == "__main__":
    sys.exit(main())
