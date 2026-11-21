#!/usr/bin/python3
"""Translate the AF Codex CLI boundary to a scripted provider HTTP request."""
import json
import os
from pathlib import Path
import sys
from urllib import request


def emit(value):
    print(json.dumps(value), flush=True)


args = sys.argv[1:]
if args == ["--version"]:
    print("codex-cli 0.162.1")
elif args == ["exec", "--help"]:
    print("""Usage: codex exec [OPTIONS] [PROMPT]
Options:
  -c, --config <key=value>
  -m, --model <MODEL>
  -s, --sandbox <SANDBOX_MODE>
  -C, --cd <DIR>
      --skip-git-repo-check
      --ephemeral
      --json
  -o, --output-last-message <FILE>
      --output-schema <FILE>
  -h, --help
""")
elif args[:2] == ["login", "status"]:
    print("Logged in using ChatGPT", file=sys.stderr)
elif args == ["app-server"]:
    for line in sys.stdin:
        message = json.loads(line)
        if message.get("method") == "initialize":
            emit({"id": message["id"], "result": {"userAgent": "codex_cli_rs/0.162.1"}})
        elif message.get("method") == "account/read":
            emit({"id": message["id"], "result": {"account": {
                "type": "chatgpt", "email": "fixture@example.invalid",
            }}})
        elif message.get("method") == "model/list":
            emit({"id": message["id"], "result": {"data": [{
                "id": "codex-fixture-1", "model": "codex-fixture-1", "displayName": "Fixture",
                "description": "Deterministic test provider", "isDefault": True, "hidden": False,
                "defaultReasoningEffort": "high",
                "supportedReasoningEfforts": [{"reasoningEffort": "high", "description": "Fixture"}],
            }]}})
elif args[:1] == ["exec"]:
    home = Path(os.environ["CODEX_HOME"])
    config = json.loads((home / "fixture.json").read_text())
    body = sys.stdin.buffer.read()
    if not body:
        raise SystemExit("fixture received no prompt")
    stage = "author" if body.lstrip().startswith(b"{") else "admission"
    operation = config["task"] + "-" + stage
    emit({"type": "thread.started", "thread_id": operation})
    opener = request.build_opener(request.ProxyHandler({}))
    with opener.open(request.Request(
        config["url"], data=body, method="POST", headers={
            "Authorization": "Bearer " + (home / "fixture-token").read_text().strip(),
            "X-Fixture-Operation": operation,
        },
    ), timeout=5) as response:
        result = json.load(response)
    message = result["message"]
    if "-o" in args:
        Path(args[args.index("-o") + 1]).write_text(message)
    emit({"type": "item.completed", "item": {"type": "agent_message", "text": message}})
    emit({"type": "turn.completed", "usage": result["usage"]})
else:
    raise SystemExit("unexpected Codex fixture invocation")
