#!/usr/bin/env python3
"""Credential-free production adapter tests, including actual installed stdin/CLI recovery."""
import copy
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
sys.dont_write_bytecode = True
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "scripts/scenario-github-adapter.py"


def module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


A = module("github_adapter", SOURCE)
REAL_HTTPS = A.http.client.HTTPSConnection
MOCK = module("github_fixture", ROOT / "fixtures/scenarios/github-transport.py")
A.http.client.HTTPSConnection = REAL_HTTPS
SHA = "1" * 40


def payload(operation="publish_pr", publication=None):
    if operation == "publish_pr":
        return {"schema": A.PAYLOAD_SCHEMAS[operation], "head": "approved/change", "base": "main",
                "title": "Approved change", "body": "Exact approved publication text"}
    if operation == "followup":
        return {"schema": A.PAYLOAD_SCHEMAS[operation], "publication": publication,
                "title": "Owner follow-up", "body": "Exact approved follow-up text"}
    return {"schema": A.PAYLOAD_SCHEMAS[operation], "publication": publication,
            "reviewers": [{"id": 8, "login": "owner"}]}


def request(operation="publish_pr", intent="a", publication=None):
    p = payload(operation, publication)
    subject = A.subject(operation, p)
    decision = {"schema": "af/scenario-signed-decision@1", "signature": "kernel-verifies-minisign",
                "request": {"schema": "af/scenario-decision@1", "run_id": "test", "execution_id": "sha256:" + "c" * 64,
                            "pin_id": "sha256:" + "d" * 64, "step": "root/owner", "subject": subject,
                            "revision": SHA, "owner": "owner", "approve": True, "expires_unix_ms": 9999999999999}}
    inputs = {"subject": {"type": "text", "value": subject}, "revision": {"type": "text", "value": SHA},
              "body": {"type": "text", "value": A.canonical(p).decode()},
              "decision": {"type": "decision", "value": decision}}
    return {"schema": "af/scenario-adapter-request@2", "method": "reconcile", "intent_id": "sha256:" + intent * 64,
            "input_digest": "sha256:" + "b" * 64, "operation": operation, "destination": "github:octo/project", "inputs": inputs}


def initial_service():
    return json.loads((ROOT / "fixtures/scenarios/github/mock-service.json").read_text())


class AdapterTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.root.chmod(0o700)
        self.config_path = self.root / "config.json"
        self.output = self.root / "github-adapter"
        self.token = self.root / "token"
        self.token.write_text("github_pat_TEST_CREDENTIAL_ONLY\n")
        self.token.chmod(0o600)
        self.config = {"schema": A.CONFIG_SCHEMA, "repository": "octo/project", "repository_id": 17,
                       "actor": {"id": 7, "login": "machine-bot"}, "token_file": str(self.token),
                       "state_dir": str(self.root / "receipts"), "heads": {"approved/change": "main"},
                       "required_checks": [{"kind": "check", "name": "CI", "app_id": 4}],
                       "reviewers": [{"id": 8, "login": "owner"}],
                       "operations": ["publish_pr", "followup", "notify", "observe"]}
        self.config_path.write_bytes(A.canonical(self.config))
        self.config_path.chmod(0o600)
        self.config_digest = A.digest(self.config_path.read_bytes())
        self.service_path = self.root / "mock-service.json"
        self.service(initial_service())
        self.install()
        MOCK.GitHubFixtureConnection.state_path = self.service_path
        self.patcher = patch.object(A.http.client, "HTTPSConnection", MOCK.GitHubFixtureConnection)
        self.patcher.start()
        self.addCleanup(self.patcher.stop)

    def installer(self, config_path, output):
        return subprocess.run(["/usr/bin/python3", "-IS", str(ROOT / "scripts/install-scenario-github-adapter.py"),
                               "--config", str(config_path), "--output", str(output)],
                              capture_output=True,
                              env={"HOME": str(self.root), "XDG_CONFIG_HOME": str(self.root)})

    def install(self):
        completed = self.installer(self.config_path, self.output)
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(completed.stdout.decode().strip(), A.digest(self.output.read_bytes()))
        production = self.output.read_bytes()
        mock = (ROOT / "fixtures/scenarios/github-transport.py").read_bytes()
        # Every byte of the installed production application is retained and pinned. Only the
        # service transport is mocked in this separate executable; production has no test hook.
        self.output.write_bytes(b"#!/usr/bin/python3 -IS\n" + mock + b"\n" + production)
        self.assertIn(production, self.output.read_bytes())

    def service(self, value=None):
        if value is not None:
            self.service_path.write_text(json.dumps(value))
        return json.loads(self.service_path.read_text())

    def adapter(self):
        return A.Adapter(self.config, self.config_digest, A.GitHub(self.config))

    def call(self, req, method=None):
        req = copy.deepcopy(req)
        if method:
            req["method"] = method
        return self.adapter().handle(req)

    def wire(self, req, code=0, **kwargs):
        p = subprocess.run([str(self.output)], input=json.dumps(req).encode(), capture_output=True,
                           env={"HOME": str(self.root), "XDG_CONFIG_HOME": str(self.root),
                                "HTTPS_PROXY": "https://steal.invalid", "PYTHONPATH": str(self.root)}, **kwargs)
        self.assertEqual(p.returncode, code, (p.stdout, p.stderr))
        self.assertNotIn(b"TEST_CREDENTIAL", p.stdout + p.stderr)
        self.assertNotIn(b"REMOTE-SECRET", p.stdout + p.stderr)
        return json.loads(p.stdout) if code == 0 else p

    def publish(self):
        req = request()
        self.assertEqual(self.call(req)["outcome"], "absent")
        result = self.call(req, "execute")
        self.assertEqual(result["outcome"], "done", result)
        return req["inputs"]["subject"]["value"]

    def observe(self, publication, condition="checks_passed"):
        req = {"schema": "af/scenario-adapter-request@2", "method": "observe", "operation": "observe",
               "intent_id": "sha256:" + "e" * 64, "input_digest": "sha256:" + "f" * 64,
               "destination": "github:octo/project", "inputs": {
                   "subject": {"type": "text", "value": publication}, "revision": {"type": "text", "value": SHA},
                   "condition": {"type": "text", "value": condition}}}
        return self.call(req)

    def test_production_wire_success_publish_green_notify_merged_and_followup(self):
        req = request()
        self.assertEqual(self.wire(req)["outcome"], "absent")
        req["method"] = "execute"
        done = self.wire(req)
        self.assertEqual(done["outcome"], "done", done)
        pub = req["inputs"]["subject"]["value"]
        self.assertTrue(self.observe(pub)["observation"]["satisfied"])
        for operation, intent in [("notify", "c"), ("followup", "d")]:
            action = request(operation, intent, pub)
            self.assertEqual(self.wire(action)["outcome"], "absent")
            action["method"] = "execute"
            result = self.wire(action)
            self.assertEqual(result["outcome"], "done", result)
            self.assertEqual(self.wire(action)["outcome"], "done")
        state = self.service()
        state["prs"]["1"].update(state="closed", merged=True, merged_at="2026-11-02T00:00:00Z", merge_commit_sha="2" * 40)
        self.service(state)
        merged = self.observe(pub, "merged")
        self.assertTrue(merged["observation"]["satisfied"], merged)
        self.assertEqual(self.service()["effects"], ["publish_pr:1", "notify:1", "followup:2"])

    def test_restart_after_remote_effect_before_response_reconciles_without_duplicate(self):
        for operation in ("publish_pr", "followup"):
            with self.subTest(operation=operation):
                publication = None if operation == "publish_pr" else request()["inputs"]["subject"]["value"]
                req = request(operation, "a" if operation == "publish_pr" else "d", publication)
                self.assertEqual(self.wire(req)["outcome"], "absent")
                state = self.service()
                state["crash_after_effect"] = True
                self.service(state)
                req["method"] = "execute"
                self.wire(req, code=86)
                record = A.decode((self.root / "receipts/state.json").read_bytes(), A.STATE_LIMIT)["records"][req["intent_id"]]
                self.assertEqual(record["phase"], "dispatched")
                req["method"] = "reconcile"
                self.assertEqual(self.wire(req)["outcome"], "done")
                req["method"] = "execute"
                self.assertEqual(self.wire(req)["outcome"], "done")
        self.assertEqual(self.service()["effects"], ["publish_pr:1", "followup:2"])

    def test_uncertain_empty_eventual_listing_never_permits_redispatch(self):
        req = request()
        state = self.service()
        state.update(crash_after_effect=True, hide_after_crash=True)
        self.service(state)
        req["method"] = "execute"
        self.wire(req, code=86)
        for method in ("reconcile", "execute", "execute"):
            req["method"] = method
            self.assertEqual(self.wire(req)["outcome"], "pending")
        self.assertEqual(self.service()["effects"], ["publish_pr:1"])
        state = self.service()
        state["hide_listings"] = False
        self.service(state)
        self.assertEqual(self.wire(req)["outcome"], "done")

    def test_review_dispatch_uses_stable_user_and_pr_ids_despite_login_rename_and_reuse(self):
        pub = self.publish()
        state = self.service()
        state["rename_reviewer_after_lookup"] = True
        self.service(state)
        response = self.call(request("notify", "c", pub), "execute")
        self.assertEqual(response["outcome"], "done", response)
        self.assertEqual([u["id"] for u in self.service()["prs"]["1"]["requested_reviewers"]], [8])
        self.assertEqual(self.service()["effects"], ["publish_pr:1", "notify:1"])

    def test_graphql_error_cannot_complete_or_redispatch_review_request(self):
        pub = self.publish()
        state = self.service()
        state["graphql_error"] = True
        self.service(state)
        req = request("notify", "c", pub)
        self.assertEqual(self.call(req, "execute")["outcome"], "pending")
        state = self.service()
        state.pop("graphql_error")
        self.service(state)
        self.assertEqual(self.call(req, "execute")["outcome"], "pending")
        self.assertEqual(self.service()["effects"], ["publish_pr:1"])

    def test_uncertain_review_removed_never_repeats_request(self):
        pub = self.publish()
        req = request("notify", "c", pub)
        state = self.service()
        state["crash_after_effect"] = True
        self.service(state)
        req["method"] = "execute"
        self.wire(req, code=86)
        state = self.service()
        state["prs"]["1"]["requested_reviewers"] = []
        self.service(state)
        for method in ("reconcile", "execute"):
            req["method"] = method
            response = self.wire(req)
            self.assertEqual(response["outcome"], "pending", response)
        self.assertEqual(self.service()["effects"], ["publish_pr:1", "notify:1"])

    def test_concurrent_dispatch_lock_and_changed_digest_never_duplicate(self):
        req = request()
        lock = self.root / "receipts/lock"
        with lock.open("r+b") as stream:
            fcntl.flock(stream.fileno(), fcntl.LOCK_EX)
            req["method"] = "execute"
            response = self.wire(req)
            self.assertEqual(response["outcome"], "pending")
            self.assertIn("busy", response["evidence"])
        self.assertEqual(self.wire(req)["outcome"], "done")
        changed = copy.deepcopy(req)
        changed["input_digest"] = "sha256:" + "c" * 64
        self.assertEqual(self.wire(changed)["outcome"], "pending")
        changed = copy.deepcopy(req)
        changed["intent_id"] = "sha256:" + "c" * 64
        self.assertEqual(self.wire(changed)["outcome"], "pending")
        self.assertEqual(self.service()["effects"], ["publish_pr:1"])

    def test_two_live_executors_serialize_while_first_remote_effect_is_in_flight(self):
        ready_read, ready_write = os.pipe()
        release_read, release_write = os.pipe()
        self.addCleanup(os.close, ready_read)
        self.addCleanup(os.close, release_write)
        state = self.service()
        state["hold_after_effect"] = {"ready_fd": ready_write, "release_fd": release_read}
        self.service(state)
        req = request()
        req["method"] = "execute"
        first = subprocess.Popen([str(self.output)], stdin=subprocess.PIPE,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 pass_fds=(ready_write, release_read),
                                 env={"HOME": str(self.root), "XDG_CONFIG_HOME": str(self.root)})
        os.close(ready_write)
        os.close(release_read)
        def reap():
            if first.poll() is None:
                first.kill()
            first.wait()
            for stream in (first.stdin, first.stdout, first.stderr):
                if stream:
                    stream.close()
        self.addCleanup(reap)
        first.stdin.write(json.dumps(req).encode())
        first.stdin.close()
        first.stdin = None
        # A pipe rendezvous waits for the exact effect, with EOF if the child fails early.
        self.assertEqual(os.read(ready_read, 1), b"r")
        try:
            second = self.wire(req)
            self.assertEqual(second["outcome"], "pending")
            self.assertIn("busy", second["evidence"])
        finally:
            os.write(release_write, b"g")
        stdout, stderr = first.communicate()
        self.assertEqual(first.returncode, 0, (stdout, stderr))
        self.assertEqual(json.loads(stdout)["outcome"], "done")
        self.assertEqual(self.wire(req)["outcome"], "done")
        self.assertEqual(self.service()["effects"], ["publish_pr:1"])

    def test_closed_unmerged_fails_and_stale_pr_never_green(self):
        pub = self.publish()
        state = self.service()
        state["prs"]["1"]["state"] = "closed"
        self.service(state)
        self.assertEqual(self.observe(pub, "merged")["outcome"], "failed")
        state["prs"]["1"].update(state="open", head={**state["prs"]["1"]["head"], "sha": "2" * 40})
        self.service(state)
        result = self.observe(pub)
        self.assertEqual(result["outcome"], "pending")
        self.assertIsNone(result["observation"])

    def test_missing_pending_failed_skipped_neutral_stale_and_newer_attempts_never_green(self):
        pub = self.publish()
        original = self.service()
        variants = []
        for conclusion in ("failure", "skipped", "neutral", "cancelled", "timed_out", None):
            state = copy.deepcopy(original)
            state["checks"][0]["conclusion"] = conclusion
            variants.append(state)
        state = copy.deepcopy(original)
        state["checks"] = []
        variants.append(state)
        state = copy.deepcopy(original)
        state["checks"][0]["head_sha"] = "2" * 40
        variants.append(state)
        state = copy.deepcopy(original)
        state["checks"][0]["app"]["id"] = 99
        variants.append(state)
        state = copy.deepcopy(original)
        state["checks"].append({**state["checks"][0], "id": 32, "status": "in_progress", "conclusion": None})
        variants.append(state)
        state = copy.deepcopy(original)
        state["suites"].append({**state["suites"][0], "id": 22, "status": "queued", "conclusion": None})
        variants.append(state)
        for state in variants:
            with self.subTest(state=state["checks"]):
                self.service(state)
                result = self.observe(pub)
                self.assertFalse((result["observation"] or {}).get("satisfied", False), result)
        state = copy.deepcopy(original)
        state["checks"].insert(0, {**state["checks"][0], "id": 30, "conclusion": "failure"})
        self.service(state)
        self.assertTrue(self.observe(pub)["observation"]["satisfied"])

    def test_status_identity_and_latest_context_are_explicit(self):
        pub = self.publish()
        self.config["required_checks"] = [{"kind": "status", "context": "ci/status", "creator_id": 42}]
        original = self.service()
        for rows, green in [([], False), ([{"id": 40, "context": "ci/status", "creator": {"id": 42}, "state": "success"}], True),
                            ([{"id": 40, "context": "ci/status", "creator": {"id": 42}, "state": "success"},
                              {"id": 41, "context": "ci/status", "creator": {"id": 42}, "state": "pending"}], False),
                            ([{"id": 42, "context": "ci/status", "creator": {"id": 99}, "state": "success"}], False)]:
            state = copy.deepcopy(original)
            state["statuses"] = rows
            self.service(state)
            if rows and rows[0]["creator"]["id"] == 99:
                with self.assertRaises(A.Refused):
                    self.adapter().green(SHA)
            else:
                self.assertEqual(self.adapter().green(SHA), green)

    def test_check_pagination_is_complete_bounded_and_rejects_untrusted_links(self):
        pub = self.publish()
        state = self.service()
        state["checks"] = [{**state["checks"][0], "id": n + 100, "name": "unrelated"} for n in range(100)] + state["checks"]
        self.service(state)
        self.assertTrue(self.observe(pub)["observation"]["satisfied"])
        state["headers"] = {"Link": '<https://steal.invalid/token>; rel="next"'}
        self.service(state)
        self.assertEqual(self.observe(pub)["outcome"], "pending")
        state.pop("headers")
        state["checks"] *= 2  # duplicate IDs mean pagination shifted; cannot prove completeness.
        self.service(state)
        self.assertEqual(self.observe(pub)["outcome"], "pending")

    def test_auth_rate_limit_redirect_truncation_and_post_errors_remain_unknown(self):
        pub = self.publish()
        original = self.service()
        for code in (301, 302, 401, 403, 429, 500):
            state = copy.deepcopy(original)
            state["http_error"] = code
            self.service(state)
            result = self.observe(pub)
            self.assertEqual(result["outcome"], "pending", result)
            self.assertIsNone(result["observation"])
        for extra in ({"truncated_by": 1}, {"headers": {"Content-Encoding": "gzip"}}):
            state = copy.deepcopy(original)
            state.update(extra)
            self.service(state)
            self.assertEqual(self.observe(pub)["outcome"], "pending")
        self.service(original)
        req = request("followup", "d", pub)
        state = self.service()
        state["post_error"] = 422
        self.service(state)
        self.assertEqual(self.call(req, "execute")["outcome"], "pending")
        self.assertEqual(self.call(req, "execute")["outcome"], "pending")
        self.assertEqual(self.service()["effects"], ["publish_pr:1"])

    def test_unsafe_or_unsupported_branch_protections_never_publish(self):
        original = self.service()
        attempt = 0
        for name in original["protection"]:
            for change in (None, {"enabled": not original["protection"][name]["enabled"]}):
                state = copy.deepcopy(original)
                state["protection"][name] = change
                self.service(state)
                req = request(intent=format(attempt, "x"))
                attempt += 1
                result = self.call(req, "execute")
                self.assertEqual(result["outcome"], "failed", result)
                self.assertEqual(self.service()["effects"], [])
        state = copy.deepcopy(original)
        state["revision"] = "2" * 40
        self.service(state)
        self.assertEqual(self.call(request(intent="f"), "execute")["outcome"], "failed")

    def test_definitive_pre_dispatch_failure_is_durable_and_new_bounded_intent_can_try(self):
        req = request()
        state = self.service()
        state["protection"]["lock_branch"]["enabled"] = False
        self.service(state)
        self.assertEqual(self.call(req, "execute")["outcome"], "failed")
        self.service(initial_service())
        self.assertEqual(self.call(req, "execute")["outcome"], "failed")
        self.assertEqual(self.call(request(intent="c"), "execute")["outcome"], "done")
        self.assertEqual(self.service()["effects"], ["publish_pr:1"])
        self.assertTrue(self.observe(req["inputs"]["subject"]["value"])["observation"]["satisfied"])

    def test_head_race_after_possible_effect_never_returns_failed_or_done(self):
        state = self.service()
        state["after_post_head"] = "2" * 40
        self.service(state)
        req = request()
        self.assertEqual(self.call(req, "execute")["outcome"], "pending")
        self.assertEqual(self.call(req, "execute")["outcome"], "pending")
        self.assertEqual(self.service()["effects"], ["publish_pr:1"])

    def test_tampered_foreign_receipts_spoofed_actor_and_unrelated_objects_fail_closed(self):
        pub = self.publish()
        original = self.service()
        for change in ({"id": 9999}, {"user": {"id": 99, "login": "attacker"}},
                       {"url": "https://api.github.com/repos/foreign/repo/pulls/1"},
                       {"body": "spoofed marker"}):
            state = copy.deepcopy(original)
            state["prs"]["1"].update(change)
            self.service(state)
            self.assertEqual(self.observe(pub)["outcome"], "pending")
        self.service(original)
        path = self.root / "receipts/state.json"
        saved = path.read_bytes()
        value = A.decode(saved, A.STATE_LIMIT)
        value["records"][request()["intent_id"]]["object"]["id"] = 9999
        path.write_bytes(A.canonical(value))
        self.assertEqual(self.call(request())["outcome"], "pending")
        value.pop("checksum")
        path.write_bytes(A.state_bytes(value))
        self.assertEqual(self.call(request())["outcome"], "pending")
        value["config_digest"] = "sha256:" + "f" * 64
        path.write_bytes(A.state_bytes(value))
        self.assertEqual(self.call(request())["outcome"], "pending")
        path.unlink()
        self.assertEqual(self.call(request())["outcome"], "pending")

    def test_crash_recovery_refuses_foreign_actor_with_copied_exact_marker(self):
        req = request()
        state = self.service()
        state["crash_after_effect"] = True
        self.service(state)
        req["method"] = "execute"
        self.wire(req, code=86)
        state = self.service()
        state["prs"]["1"]["user"] = {"id": 99, "login": "attacker"}
        self.service(state)
        req["method"] = "reconcile"
        self.assertEqual(self.wire(req)["outcome"], "pending")
        req["method"] = "execute"
        self.assertEqual(self.wire(req)["outcome"], "pending")
        self.assertEqual(self.service()["effects"], ["publish_pr:1"])

    def test_completed_notification_receipt_cannot_be_redirected_to_another_pr(self):
        pub = self.publish()
        req = request("notify", "c", pub)
        self.assertEqual(self.call(req, "execute")["outcome"], "done")
        state = self.service()
        other = copy.deepcopy(state["prs"]["1"])
        other.update(id=9999, number=2, url=other["url"].replace("/1", "/2"),
                     html_url=other["html_url"].replace("/1", "/2"))
        state["prs"]["2"] = other
        self.service(state)
        path = self.root / "receipts/state.json"
        journal = A.decode(path.read_bytes(), A.STATE_LIMIT)
        journal.pop("checksum")
        journal["records"][req["intent_id"]]["object"] = {"id": 9999, "number": 2}
        path.write_bytes(A.state_bytes(journal))
        self.assertEqual(self.call(req)["outcome"], "pending")
        self.assertEqual(self.service()["effects"], ["publish_pr:1", "notify:1"])

    def test_unknown_publication_cannot_observe_arbitrary_pr(self):
        self.assertEqual(self.observe("github-pr:sha256:" + "a" * 64)["outcome"], "pending")
        self.assertEqual(self.service()["effects"], [])

    def test_rejected_authority_payload_digest_malformed_maps_and_secret_paths(self):
        original = request()
        bad = []
        for field, value in [("destination", "github:attacker/project"), ("operation", "shell"), ("method", "GET"),
                             ("intent_id", "../../file"), ("schema", "af/scenario-adapter-request@0")]:
            req = copy.deepcopy(original)
            req[field] = value
            bad.append(req)
        for field in ("token", "url", "argv", "executable", "condition"):
            req = copy.deepcopy(original)
            req["inputs"][field] = {"type": "text", "value": "secret"}
            bad.append(req)
        req = copy.deepcopy(original)
        req["inputs"]["body"]["value"] = req["inputs"]["body"]["value"].replace("main", "other")
        bad.append(req)
        req = copy.deepcopy(original)
        req["inputs"]["decision"]["value"]["request"]["revision"] = "2" * 40
        bad.append(req)
        req = copy.deepcopy(original)
        del req["inputs"]["decision"]
        bad.append(req)
        for req in bad:
            with self.subTest(req=req):
                self.wire(req, code=1)
        self.assertEqual(self.service()["effects"], [])
        for branch in ("../secret", "a/../secret", "a//b", "x.lock", "a/.hidden", "https://host/a", "x%2fy"):
            with self.assertRaises(A.Refused):
                A.branch(branch)
        with self.assertRaises(A.Refused):
            A.decode(b'{"x":1,"x":2}')
        with self.assertRaises(A.Refused):
            A.decode(b"x" * (A.LIMIT + 1))
        with self.assertRaises(A.Refused):
            A.validate_config({**self.config, "required_checks": []})

    def test_installer_refuses_aliases_and_unsafe_ancestors_before_creating_state(self):
        alias = self.root / "alias"
        alias.symlink_to(self.root, target_is_directory=True)
        for unsafe in ("alias", "ancestor"):
            with self.subTest(unsafe=unsafe):
                state = self.root / (unsafe + "-receipts")
                output = self.root / (unsafe + "-adapter")
                config_path = self.root / (unsafe + "-config.json")
                config_path.write_bytes(A.canonical({**self.config, "state_dir": str(state)}))
                config_path.chmod(0o600)
                if unsafe == "ancestor":
                    self.root.chmod(0o770)
                try:
                    supplied = alias / config_path.name if unsafe == "alias" else config_path
                    refused = self.installer(supplied, output)
                    self.assertEqual(refused.returncode, 1)
                    self.assertIn(b"Installation refused", refused.stderr)
                    self.assertFalse(state.exists())
                    self.assertFalse(output.exists())
                finally:
                    self.root.chmod(0o700)
                # The same fresh authority installs when spelled canonically under safe
                # ancestors. Refusal was not caused by an existing executable or journal.
                installed = self.installer(config_path, output)
                self.assertEqual(installed.returncode, 0, installed.stderr)
                self.assertEqual(installed.stdout.decode().strip(), A.digest(output.read_bytes()))

    def test_machine_config_digest_and_file_permissions_and_symlinks_fail_closed(self):
        # Runtime authority is pinned in the executable, not supplied by request or environment.
        for name in ("json.py", "http.py"):
            (self.root / name).write_text('raise RuntimeError("UNTRUSTED_IMPORT_RAN")')
        self.assertEqual(self.wire(request())["outcome"], "absent")
        self.config_path.write_bytes(A.canonical({**self.config, "operations": ["observe"]}))
        self.wire(request(), code=1)
        self.config_path.write_bytes(A.canonical(self.config))
        self.token.chmod(0o644)
        self.wire(request(), code=1)
        self.token.chmod(0o600)
        real = self.token.with_name("real-token")
        self.token.rename(real)
        self.token.symlink_to(real)
        self.wire(request(), code=1)
        repository = self.root / "checkout"
        repository.mkdir()
        (repository / ".git").write_text("gitdir: /untrusted")
        with self.assertRaises(A.Refused):
            A.outside_checkout(repository / "token")
        with self.assertRaises(A.Refused):
            A.outside_checkout(Path("relative/token"))

    def test_receipt_retention_budget_refuses_new_intents_without_discarding_existing(self):
        pub = self.publish()
        before = (self.root / "receipts/state.json").read_bytes()
        with patch.object(A, "MAX_RECORDS", 1):
            self.assertEqual(self.call(request("followup", "d", pub), "execute")["outcome"], "pending")
        self.assertEqual((self.root / "receipts/state.json").read_bytes(), before)
        self.assertEqual(self.call(request())["outcome"], "done")

    def test_schema_parity_and_old_versions_fail_closed(self):
        for name, schema in A.schema_documents().items():
            rendered = json.dumps(schema, indent=2, sort_keys=True) + "\n"
            self.assertEqual((ROOT / "schemas" / name).read_text(), rendered)
        with self.assertRaises(A.Refused):
            A.validate_config({**self.config, "schema": "af/github-adapter-config@0"})
        earlier = request()
        earlier["schema"] = "af/scenario-adapter-request@1"
        with self.assertRaises(A.Refused):
            A.validate_request(earlier, self.config)
        for version in (0, 1):
            state = A.empty_state(self.config_digest)
            state["schema"] = "af/github-adapter-state@%d" % version
            A.atomic_write(self.root / "receipts/state.json", A.state_bytes(state))
            self.assertEqual(self.call(request())["outcome"], "pending")


if __name__ == "__main__":
    unittest.main()
