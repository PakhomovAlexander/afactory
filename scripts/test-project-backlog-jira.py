#!/usr/bin/env python3
"""Hermetic contract checks for Jira backlog scope and reconciliation."""

import copy
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import Mock, patch

SPEC = importlib.util.spec_from_file_location("backlog_jira", Path(__file__).with_name("project-backlog-jira.py"))
J = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(J)


class JiraContract(unittest.TestCase):
    def setUp(self):
        self.scope = dict(schema="af/backlog-jira-destination@1", project_id="sha256:" + "1" * 64,
                          site="fixture.atlassian.net", jira_project_id="10000", issue_type_id="10001",
                          publisher_account_id="fixture:publisher", executable_digest="sha256:" + "2" * 64)
        self.request = dict(schema="af/backlog-jira-request@1", request_id="sha256:" + "3" * 64,
                            scope=self.scope, operation="create", item_id="sha256:" + "4" * 64,
                            issue_id=None, fields={"title": "Boundary", "body": "Alpha\n\nβ"})
        self.marker = "af-backlog-" + "1" * 64 + "-" + "4" * 64
        self.row = dict(id="10002", key="TEAM-1", fields={
            "project": {"id": "10000"}, "issuetype": {"id": "10001"},
            "creator": {"accountId": "fixture:publisher"}, "labels": [self.marker],
            "status": {"statusCategory": {"key": "new"}}, "summary": "Boundary",
            "description": J.description("Alpha\n\nβ"),
        })
        self.service = Mock()

    def test_request_is_bound_to_local_scope_and_read_only_recovery(self):
        J.validate(self.request, self.scope)
        for field, bad in [("site", "fixture.atlassian.net.invalid"), ("jira_project_id", "01"),
                           ("issue_type_id", "10001 OR project = 3"), ("publisher_account_id", "")]:
            changed = {**self.scope, field: bad}
            with self.assertRaises(ValueError):
                J.validate({**self.request, "scope": changed}, changed)
        with self.assertRaises(ValueError):
            J.validate({**self.request, "operation": "reconcile"}, self.scope)
        with self.assertRaises(ValueError):
            J.validate({**self.request, "scope": {**self.scope, "jira_project_id": "10099"}}, self.scope)

    def test_only_approved_fields_and_pinned_ids_enter_the_create_request(self):
        self.service.request.side_effect = [
            {"accountId": "fixture:publisher"}, {"id": "10000"}, {"issues": [], "isLast": True},
            {"accountId": "fixture:publisher"}, {"id": "10000"}, {"id": "10002"}, self.row,
        ]
        self.assertEqual(J.dispatch(self.service, self.request)["kind"], "observed")
        create = self.service.request.call_args_list[5].args
        self.assertEqual(create[:2], ("POST", "/issue"))
        self.assertEqual(create[2], {"fields": {
            "project": {"id": "10000"}, "issuetype": {"id": "10001"},
            "summary": "Boundary", "description": J.description("Alpha\n\nβ"), "labels": [self.marker],
        }})
        self.assertEqual(J.observed_body(create[2]["fields"]["description"]), "Alpha\n\nβ")

    def test_mapped_reads_keep_stable_id_and_record_closed_human_edit(self):
        self.row["key"] = "TEAM-3"
        self.row["fields"]["status"]["statusCategory"]["key"] = "done"
        self.row["fields"]["summary"] = "Edited"
        self.service.request.side_effect = [{"accountId": "fixture:publisher"}, {"id": "10000"}, self.row]
        result = J.dispatch(self.service, {**self.request, "operation": "reconcile",
                                            "issue_id": "10002", "fields": None})
        self.assertEqual((result["id"], result["key"], result["closed"]), ("10002", "TEAM-3", True))
        self.assertTrue(all(c.args[0] == "GET" for c in self.service.request.call_args_list))

    def test_empty_delayed_or_conflicting_search_never_retries_creation(self):
        request = {**self.request, "operation": "reconcile", "fields": None}
        for search, kind in [({"issues": [], "isLast": True}, "absent"),
                             ({"issues": [], "isLast": False}, "unavailable"),
                             ({"issues": [{"id": "10002"}, {"id": "10300"}]}, "conflict")]:
            self.service.reset_mock()
            self.service.request.side_effect = [{"accountId": "fixture:publisher"}, {"id": "10000"}, search]
            self.assertEqual(J.dispatch(self.service, request), {"kind": kind})
            self.assertTrue(all(c.args[0] == "GET" for c in self.service.request.call_args_list))
        for key in ("project", "creator", "issuetype", "labels"):
            changed = copy.deepcopy(self.row)
            changed["fields"][key] = [] if key == "labels" else {}
            with self.assertRaises(J.Conflict):
                J.observed(changed, self.scope, self.marker, "10002")

    def test_transport_stays_on_pinned_host_and_refuses_redirect(self):
        with patch.object(J.http.client, "HTTPSConnection") as connect:
            response = connect.return_value.getresponse.return_value
            response.status = 302
            jira = J.Jira(self.scope["site"], {"email": "test@example.invalid", "api_token": "fake"})
            with self.assertRaises(ValueError):
                jira.request("POST", "/issue", {})
            self.assertEqual(connect.call_count, 1)
            self.assertEqual(connect.call_args.args, ("fixture.atlassian.net",))
            self.assertEqual(connect.return_value.request.call_args.args, ("POST", "/rest/api/3/issue"))


if __name__ == "__main__":
    unittest.main()
