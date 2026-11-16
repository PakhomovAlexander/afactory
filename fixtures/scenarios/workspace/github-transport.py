
# Appended after the shared HTTPS fixture, before the unmodified installed application.
# Only HTTP responses are modeled. Head identities come from the actual bare remote,
# updated by real git push through the separately pinned destination-only Git wrapper.
import subprocess
import time

_workspace_http_reply = github_fixture_reply
_workspace_actor_observations = 0


def github_fixture_reply(state, method, path, body):
    global _workspace_actor_observations
    state["requests"].append([method, path])
    if method == "GET" and path == "/user":
        _workspace_actor_observations += 1
        # Each installed adapter invocation has a fresh process/transport clock. Move
        # only the fixture's wall clock forward at the final guard's blocking identity
        # observation; never change a retained native deadline or acceptance receipt.
        boundary = 2 if state["mode"].endswith("push") else 3
        if _workspace_actor_observations >= boundary:
            if state["mode"].startswith("expired-"):
                state["guard_fault"] = dict(mode=state["mode"], observation=_workspace_actor_observations)
                time.time_ns = lambda: 8_000_000_000_000_000_000
            elif state["mode"].startswith("foreign-"):
                state["guard_fault"] = dict(mode=state["mode"], observation=_workspace_actor_observations)
                return 200, dict(id=999, login="foreign-bot")
    heads = subprocess.check_output(
        ["/usr/bin/git", "--git-dir", state["bare_remote"], "for-each-ref",
         "--format=%(refname:short) %(objectname)", "refs/heads/accepted/"],
        env={"PATH": "/usr/bin:/bin", "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null"},
    ).decode().splitlines()
    if heads:
        assert len(heads) == 1
        branch, revision = heads[0].split()
        state.update(head=branch, revision=revision)
        for pr in state["prs"].values():
            assert pr["head"]["ref"] == branch
            pr["head"]["sha"] = revision
        for key in ("checks", "suites"):
            for row in state[key]:
                row["head_sha"] = "0" * 40 if state["mode"] == "stale" else revision
                row["status"] = "in_progress" if state["mode"] == "pending" else "completed"
                row["conclusion"] = (None if state["mode"] == "pending" else
                                     "failure" if state["mode"] == "repairable" else "success")
    return _workspace_http_reply(state, method, path, body)
