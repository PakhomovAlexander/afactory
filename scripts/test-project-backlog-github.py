#!/usr/bin/env python3
"""Offline contract tests for the Project backlog GitHub Issues entry point."""
import copy
import importlib.util
from pathlib import Path
import sys
import unittest
from urllib.parse import parse_qs, urlsplit

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "project_backlog_github", Path(__file__).with_name("project-backlog-github.py"))
destination = importlib.util.module_from_spec(spec)
spec.loader.exec_module(destination)

REPO = "/repos/Example/Backlog"
ISSUES = REPO + "/issues"
API = "https://api.github.com"
PUBLISHER = {"id": 7, "login": "machine-bot"}
AUTHORITY = {"destination": "github:Example/Backlog", "repository_id": 42,
             "publisher": PUBLISHER}


def digest(char):
    return "sha256:" + char * 64


INTENT = {**AUTHORITY, "intent_id": digest("a"), "item_id": digest("b"),
          "revision_id": digest("c"), "title": "Approved title", "body": "Approved body"}


class FakeTransport:
    def __init__(self):
        self.actor = PUBLISHER.copy()
        self.repo = {"id": 42, "full_name": "Example/Backlog", "url": API + REPO}
        self.rows = []
        self.events = []
        self.posts = []
        self.visible = True
        self.break_before_send = False
        self.lose_response = False
        self.create_override = None
        self.link_override = None
        self.detail_override = None

    def row(self, number, *, title="Unrelated", body=None):
        return {"id": 1000 + number, "number": number, "state": "open",
                "user": PUBLISHER.copy(), "url": f"{API}{ISSUES}/{number}",
                "repository_url": API + REPO,
                "html_url": f"https://github.com/Example/Backlog/issues/{number}",
                "title": title, "body": body}

    def request(self, method, path, body=None, *, before_send=None):
        self.events.append((method, path))
        if method == "POST" and path == ISSUES:
            if self.break_before_send:
                raise destination.github.TransportUnavailable("offline before send")
            before_send()
            self.posts.append(copy.deepcopy(body))
            row = self.row(len(self.rows) + 1, **body)
            self.rows.append(row)
            if self.lose_response:
                raise destination.github.TransportUnavailable("response lost")
            return copy.deepcopy(self.create_override if self.create_override is not None else row)
        if method == "GET" and path == "/user":
            return self.actor
        if method == "GET" and path == REPO:
            return self.repo
        if method == "GET" and path.startswith(ISSUES + "/"):
            return copy.deepcopy(self.detail_override or self.rows[int(path.rsplit("/", 1)[1]) - 1])
        raise AssertionError(f"unexpected request {method} {path}")

    def issue_page(self, path):
        parsed = urlsplit(path)
        query = parse_qs(parsed.query)
        page = int(query.pop("page")[0])
        assert parsed.path == ISSUES
        assert query == {"per_page": ["100"], "state": ["all"], "sort": ["created"], "direction": ["asc"]}
        self.events.append(("LIST", page))
        rows = self.rows if self.visible else []
        link = ""
        if len(rows) > page * 100:
            link = (f'<{API}/repositories/42/issues?state=all&sort=created'
                    f'&direction=asc&per_page=100&page={page + 1}>; rel="next"')
        if self.link_override is not None:
            link = self.link_override(page, link)
        return copy.deepcopy(rows[(page - 1) * 100:page * 100]), link


class IssueDestinationTests(unittest.TestCase):
    def setUp(self):
        self.transport = FakeTransport()
        self.claims = set()

    def commit(self, intent_id):
        self.transport.events.append(("claim", intent_id))
        if intent_id in self.claims:
            return False
        self.claims.add(intent_id)
        return True

    def run_intent(self, intent=INTENT, *, send=False, authority=AUTHORITY):
        return destination.publish_issue(intent, authority, self.transport,
                                         commit_send=self.commit if send else None)

    def test_default_observes_and_committed_create_returns_receipt(self):
        self.assertEqual(self.run_intent()["status"], "pending")
        self.assertEqual(self.transport.posts, [])
        result = self.run_intent(send=True)
        self.assertEqual(result, {"status": "done", "receipt": {
            "schema": destination.RECEIPT_SCHEMA, **AUTHORITY,
            "intent_id": INTENT["intent_id"], "item_id": INTENT["item_id"],
            "revision_id": INTENT["revision_id"], "issue_id": 1001, "issue_number": 1,
            "state": "open", "content_matches": True}})
        self.assertEqual(self.transport.events[-2:], [("POST", ISSUES), ("claim", INTENT["intent_id"])])
        posted, = self.transport.posts
        self.assertEqual(set(posted), {"title", "body"})
        self.assertEqual(posted["title"], INTENT["title"])
        self.assertTrue(posted["body"].startswith(INTENT["body"] + "\n\n<!-- af-backlog-issue/v1 "))
        self.assertIn(INTENT["revision_id"], posted["body"])
        self.assertIn(INTENT["item_id"], posted["body"])
        self.assertNotIn(INTENT["intent_id"], posted["body"])
        self.assertEqual(self.run_intent(send=True), result)
        self.assertEqual(len(self.transport.posts), 1)

    def test_safe_pre_send_failure_differs_from_lost_response_and_delayed_visibility(self):
        self.transport.break_before_send = True
        self.assertEqual(self.run_intent(send=True)["status"], "retry")
        self.assertEqual(self.claims, set())
        self.transport.break_before_send = False
        self.transport.lose_response = True
        self.assertEqual(self.run_intent(send=True)["status"], "pending")
        self.assertEqual(self.claims, {INTENT["intent_id"]})
        self.transport.visible = False
        self.assertEqual(self.run_intent()["status"], "pending")
        self.assertEqual(self.run_intent(send=True)["status"], "pending")
        self.transport.visible = True
        self.assertEqual(self.run_intent()["receipt"]["issue_number"], 1)
        self.assertEqual(len(self.transport.posts), 1)

    def test_failed_claim_never_sends_and_unknown_claim_stays_pending(self):
        def failed_commit(intent_id):
            self.commit(intent_id)
            raise RuntimeError("private diagnostic")
        result = destination.publish_issue(INTENT, AUTHORITY, self.transport, commit_send=failed_commit)
        self.assertEqual(result["status"], "pending")
        self.assertNotIn("private diagnostic", str(result))
        self.assertEqual(self.run_intent(send=True)["status"], "pending")
        self.assertEqual(self.transport.posts, [])

    def test_malformed_create_receipt_requires_reconciliation(self):
        self.transport.create_override = {"id": 1001}
        self.assertEqual(self.run_intent(send=True)["status"], "pending")
        self.assertEqual(self.run_intent()["receipt"]["issue_id"], 1001)
        self.assertEqual(len(self.transport.posts), 1)

    def test_authority_and_input_shape_are_checked_before_transport(self):
        changes = ({"destination": "github:Wrong/Backlog"}, {"repository_id": True},
                   {"publisher": {"id": 8, "login": "machine-bot"}}, {"report": "private"},
                   {"body": "<!-- af-backlog-issue/v1 injected -->"})
        for change in changes:
            with self.subTest(change=change):
                self.assertEqual(self.run_intent({**INTENT, **change}, send=True)["status"], "refused")
        self.assertEqual(self.transport.events, [])
        self.transport.actor = {"id": 8, "login": "machine-bot"}
        self.assertEqual(self.run_intent(send=True)["status"], "conflict")
        self.transport.actor = PUBLISHER
        self.transport.repo["id"] = 43
        self.assertEqual(self.run_intent(send=True)["status"], "conflict")
        self.assertEqual(self.transport.posts, [])

    def test_conflicting_publisher_marker_and_duplicate_are_never_adopted(self):
        self.assertEqual(self.run_intent(send=True)["status"], "done")
        original = self.transport.rows[0]
        variants = ({"user": {"id": 8, "login": "machine-bot"}},
                    {"body": original["body"].replace(" -->", " modified -->")},
                    {"body": original["body"].replace("\n\n<!--", "\n\ntext <!--")},
                    {"pull_request": {}}, {"html_url": "https://github.com/Other/Backlog/issues/1"})
        for change in variants:
            with self.subTest(change=change):
                self.transport.rows[0] = {**original, **change}
                self.assertEqual(self.run_intent()["status"], "conflict")
        self.transport.rows[0] = original
        changed = {**INTENT, "intent_id": digest("d"), "body": "Different body for the same revision"}
        self.assertEqual(self.run_intent(changed, send=True)["status"], "conflict")
        self.transport.rows.append(self.transport.row(2, body=original["body"]))
        self.assertEqual(self.run_intent()["status"], "conflict")
        self.assertEqual(len(self.transport.posts), 1)

    def test_human_edits_and_closure_survive_an_independent_later_revision(self):
        self.run_intent(send=True)
        row = self.transport.rows[0]
        row.update(title="Human edit", body=row["body"].replace("Approved body", "Human edit"),
                   state="closed")
        edited = row.copy()
        receipt = self.run_intent()["receipt"]
        self.assertEqual((receipt["issue_number"], receipt["state"], receipt["content_matches"]),
                         (1, "closed", False))
        later = {**INTENT, "intent_id": digest("d"), "revision_id": digest("e"),
                 "title": "Separately approved revision"}
        self.assertEqual(self.run_intent(later, send=True)["receipt"]["issue_number"], 2)
        self.assertEqual(self.transport.rows[0], edited)
        row["body"] = "Human removed the marker"
        self.assertEqual(self.run_intent()["status"], "pending")
        self.assertEqual(len(self.transport.posts), 2)

    def test_reconciliation_finishes_page_walk_and_rechecks_exact_issue(self):
        self.transport.rows = [self.transport.row(number) for number in range(1, 101)]
        self.assertEqual(self.run_intent(send=True)["receipt"]["issue_number"], 101)
        self.assertEqual(self.run_intent()["receipt"]["issue_number"], 101)
        self.assertIn(("LIST", 2), self.transport.events)
        self.transport.detail_override = {**self.transport.rows[-1], "id": 12345}
        self.assertEqual(self.run_intent()["status"], "conflict")
        self.transport.detail_override = None
        self.transport.rows[-1]["id"] = self.transport.rows[0]["id"]
        self.assertEqual(self.run_intent()["status"], "pending")
        self.assertEqual(len(self.transport.posts), 1)

    def test_inconclusive_pagination_does_not_dispatch(self):
        self.transport.rows = [self.transport.row(number) for number in range(1, 102)]
        for transform in (lambda _, link: link.replace("page=2", "page=3"),
                          lambda _, link: link.replace("state=all", "state=open"),
                          lambda _, link: link.replace("api.github.com", "example.org"),
                          lambda _, link: link + ", " + link,
                          lambda _, link: ""):
            self.transport.link_override = transform
            self.assertEqual(self.run_intent(send=True)["status"], "pending")
        self.assertEqual(self.transport.posts, [])
        self.assertEqual(self.claims, set())


if __name__ == "__main__":
    unittest.main()
