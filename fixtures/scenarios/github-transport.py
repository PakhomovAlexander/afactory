"""Test transport only. Tests prepend these bytes to a private installed production adapter.

There is no environment variable, config option, URL override or plugin in production code.
All application bytes and these mock bytes are covered by the Scenario executable digest.
"""
import copy
import http.client
import json
import os
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlsplit


def github_fixture_reply(state, method, path, body):
    parsed = urlsplit(path)
    route = unquote(parsed.path)
    prefix = "/repos/" + state["repository"]
    if state.get("http_error"):
        return state["http_error"], {"message": "REMOTE-SECRET-DO-NOT-PRINT"}
    if method == "GET":
        if route == "/user":
            return 200, state["actor"]
        if route.startswith("/users/"):
            user = next((u for u in state["reviewers"] if u["login"] == route.split("/")[-1]), {})
            result = copy.deepcopy(user)
            if user and state.pop("rename_reviewer_after_lookup", False):
                user["login"] = "renamed-owner"
                state["reviewers"].append({"id": 99, "login": result["login"], "node_id": "U_99", "type": "User"})
            return 200, result
        if route == prefix:
            return 200, {"id": state["repository_id"], "full_name": state["repository"]}
        if route.endswith("/protection"):
            return 200, state["protection"]
        if "/branches/" in route:
            return 200, {"name": state["head"], "protected": True, "commit": {"sha": state["revision"]}}
        if route.endswith(("/check-runs", "/check-suites", "/statuses")):
            key = "checks" if route.endswith("/check-runs") else "suites" if route.endswith("/check-suites") else "statuses"
            rows = state[key]
            page = int(parse_qs(parsed.query).get("page", ["1"])[0])
            rows_page = rows[(page - 1) * 100:page * 100]
            if key == "statuses":
                return 200, rows_page
            return 200, {"total_count": len(rows), "check_runs" if key == "checks" else "check_suites": rows_page}
        for name, key in (("pulls", "prs"), ("issues", "issues")):
            if route == prefix + "/" + name:
                rows = [] if state.get("hide_listings") else list(state[key].values())
                page = int(parse_qs(parsed.query).get("page", ["1"])[0])
                return 200, rows[(page - 1) * 100:page * 100]
            if route.startswith(prefix + "/" + name + "/"):
                return (200, state[key][route.split("/")[-1]]) if route.split("/")[-1] in state[key] else (404, {})
        return 404, {}
    assert method == "POST", "fixture never mutates through another verb"
    if state.get("post_error"):
        return state["post_error"], {}
    if route == "/graphql":
        assert body["query"] == ("mutation AfRequestReview($pull:ID!,$users:[ID!]!){"
                                 "requestReviews(input:{pullRequestId:$pull,userIds:$users,union:true}){pullRequest{id}}}")
        if state.get("graphql_error"):
            return 200, {"errors": [{"message": "REMOTE-SECRET-DO-NOT-PRINT"}]}
        remote = next(pr for pr in state["prs"].values() if pr["node_id"] == body["variables"]["pull"])
        remote["requested_reviewers"] = [u for u in state["reviewers"] if u["node_id"] in body["variables"]["users"]]
        state["effects"].append("notify:" + str(remote["number"]))
        return 200, {"data": {"requestReviews": {"pullRequest": {"id": remote["node_id"]}}}}
    pr = route == prefix + "/pulls"
    assert pr or route == prefix + "/issues", "fixture refused mutation route"
    key, kind, html = ("prs", "pulls", "pull") if pr else ("issues", "issues", "issues")
    number = len(state["prs"]) + len(state["issues"]) + 1
    remote = {"id": 1000 + number, "number": number, "title": body["title"], "body": body["body"],
              "user": copy.deepcopy(state["actor"]), "state": "open",
              "url": "https://api.github.com" + prefix + "/" + kind + "/" + str(number),
              "html_url": "https://github.com/" + state["repository"] + "/" + html + "/" + str(number)}
    if pr:
        repo = {"id": state["repository_id"], "full_name": state["repository"]}
        remote.update(node_id="PR_" + str(number), head={"ref": body["head"], "sha": state["revision"], "repo": repo},
                      base={"ref": body["base"], "repo": repo}, draft=False, merged=False,
                      merged_at=None, merge_commit_sha=None, requested_reviewers=[])
    state[key][str(number)] = remote
    state["effects"].append(("publish_pr:" if pr else "followup:") + str(number))
    if state.get("after_post_head") and pr:
        remote["head"]["sha"] = state["after_post_head"]
    return 201, remote


class GitHubFixtureResponse:
    def __init__(self, status, data, state):
        self.status, self.raw, self.state = status, json.dumps(data).encode(), state

    def getheader(self, name):
        if name == "Content-Length":
            return str(len(self.raw) + self.state.get("truncated_by", 0))
        return self.state.get("headers", {}).get(name)

    def read(self, amount):
        return self.raw[:amount]


class GitHubFixtureConnection:
    state_path = Path(__file__).parent / "mock-service.json"

    def __init__(self, host, timeout, context):
        assert host == "api.github.com" and 0 < timeout <= 5
        assert context.check_hostname

    def request(self, method, path, body, headers):
        assert headers["Authorization"] == "Bearer github_pat_TEST_CREDENTIAL_ONLY"
        self.method, self.path = method, path
        self.body = json.loads(body) if body is not None else None

    def getresponse(self):
        state = json.loads(self.state_path.read_text())
        code, data = github_fixture_reply(state, self.method, self.path, self.body)
        crash = self.method == "POST" and 200 <= code < 300 and state.pop("crash_after_effect", False)
        if crash:
            state["hide_listings"] = state.get("hide_after_crash", False)
        with self.state_path.open("w") as stream:
            json.dump(state, stream)
            stream.flush()
            os.fsync(stream.fileno())
        if self.method == "POST" and 200 <= code < 300 and state.get("hold_after_effect"):
            rendezvous = state["hold_after_effect"]
            os.write(rendezvous["ready_fd"], b"r")
            os.read(rendezvous["release_fd"], 1)
        if crash:
            os._exit(86)
        return GitHubFixtureResponse(code, data, state)

    def close(self):
        pass


http.client.HTTPSConnection = GitHubFixtureConnection
