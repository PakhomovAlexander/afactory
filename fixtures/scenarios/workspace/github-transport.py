
# Appended after the shared HTTPS fixture, before the unmodified installed application.
# Only HTTP responses are modeled. Head identities come from the actual bare remote,
# updated by real git push through the separately pinned destination-only Git wrapper.
import subprocess

_workspace_http_reply = github_fixture_reply


def github_fixture_reply(state, method, path, body):
    state["requests"].append([method, path])
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
