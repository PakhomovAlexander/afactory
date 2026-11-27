"""Fixed LSP client. The host supplies only an explicit project, position and operation."""
from contextlib import suppress
import json
import os
from pathlib import Path
import re
import selectors
import subprocess
import sys
import time
from urllib.parse import unquote, urlsplit

SOURCE = Path("/source")
BINARY = "/analyzer/rust-analyzer"
SHUTDOWN_SECONDS = 2
MAX_MESSAGE = 2 * 1024 * 1024
METHODS = {
    "definition": "textDocument/definition",
    "references": "textDocument/references",
    "implementation": "textDocument/implementation",
}


def send(server, message):
    data = json.dumps({"jsonrpc": "2.0", **message}).encode()
    server.stdin.write(f"Content-Length: {len(data)}\r\n\r\n".encode() + data)
    server.stdin.flush()


def receive(server):
    length = None
    for _ in range(16):
        header = server.stdout.readline(1024)
        if header == b"\r\n":
            break
        if not header.endswith(b"\r\n"):
            raise ValueError("invalid LSP header")
        name, _, value = header.partition(b":")
        if name.lower() == b"content-length":
            if length is not None:
                raise ValueError("repeated LSP length")
            length = int(value)
    else:
        raise ValueError("too many LSP headers")
    if not isinstance(length, int) or not 0 < length <= MAX_MESSAGE:
        raise ValueError("LSP message exceeds limit")
    data = server.stdout.read(length)
    if len(data) != length:
        raise ValueError("incomplete LSP message")
    return json.loads(data)


def await_message(server, predicate):
    for _ in range(2048):
        message = receive(server)
        if predicate(message):
            if "error" in message:
                raise ValueError("LSP request failed")
            return message
        if "id" in message and "method" in message:
            send(server, {"id": message["id"], "error": {"code": -32601, "message": "not supported"}})
    raise ValueError("LSP message limit reached")


def anchors(result, limit):
    unique = set()
    source_prefix = str(SOURCE) + "/"
    for item in result if isinstance(result, list) else [result] if result else []:
        uri = urlsplit(item.get("targetUri", item.get("uri")))
        path = unquote(uri.path)
        if (uri.scheme != "file" or uri.netloc or uri.query or uri.fragment
                or not path.startswith(source_prefix)
                or any(not re.fullmatch(r"[A-Za-z0-9._-]+", c) or c in {".", ".."}
                       for c in path.removeprefix(source_prefix).split("/"))
                or not Path(path).is_file()):
            raise ValueError("LSP location outside the source manifest")
        line = item.get("targetSelectionRange", item.get("range"))["start"]["line"]
        if type(line) is not int or not 0 <= line < 2**32 - 1:
            raise ValueError("invalid LSP location")
        unique.add((path.removeprefix(source_prefix), line + 1))
    return {"anchors": [{"path": p, "line": n} for p, n in sorted(unique)[:limit]],
            "truncated": len(unique) > limit}


def finish(server):
    """Keep the receive pipe open through exit, with bounded time and output."""
    deadline = time.monotonic() + SHUTDOWN_SECONDS
    server.stdin.close()
    os.set_blocking(server.stdout.fileno(), False)
    drained = 0
    with selectors.DefaultSelector() as selector:
        selector.register(server.stdout, selectors.EVENT_READ)
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise ValueError("rust-analyzer exit timed out")
            chunk = server.stdout.read1(65536)
            if not chunk:
                if not selector.select(remaining):
                    raise ValueError("rust-analyzer exit timed out")
                # The reader may have buffered bytes before shutdown; read those before polling.
                chunk = server.stdout.read1(65536)
                if not chunk:
                    break
            drained += len(chunk)
            if drained > MAX_MESSAGE:
                raise ValueError("rust-analyzer exit output exceeds limit")
    if server.wait(timeout=max(0, deadline - time.monotonic())) != 0:
        raise ValueError("rust-analyzer did not exit cleanly")


def main():
    request = json.load(sys.stdin)
    env = {"HOME": "/nonexistent", "PATH": "/nonexistent", "CARGO_NET_OFFLINE": "true",
           "LD_LIBRARY_PATH": "/analyzer",
           "CARGO_HOME": "/nonexistent", "RUSTUP_HOME": "/nonexistent",
           "RUSTUP_TOOLCHAIN": request["version"], "RAYON_NUM_THREADS": "2"}
    version = subprocess.check_output([BINARY, "--version"], env=env, text=True).split()
    if version[:2] != ["rust-analyzer", request["version"]]:
        raise ValueError("rust-analyzer version differs")
    crates = [
        {"root_module": str(SOURCE / crate["root"]), "edition": crate["edition"],
         "deps": [{"crate": dep["index"], "name": dep["name"]} for dep in crate["dependencies"]],
         "is_workspace_member": True}
        for crate in request["project"]
    ]
    config = {
        "linkedProjects": [{"crates": crates}], "cargo": {"buildScripts": {"enable": False},
                                                           "sysroot": None, "autoload": False},
        "procMacro": {"enable": False}, "checkOnSave": False, "numThreads": 2,
        "cachePriming": {"enable": True, "numThreads": 2},
    }
    position = request["position"]
    server = subprocess.Popen([BINARY], stdin=subprocess.PIPE, stdout=subprocess.PIPE, env=env)
    try:
        send(server, {"id": 1, "method": "initialize", "params": {
            "processId": os.getpid(), "rootUri": SOURCE.as_uri(),
            "capabilities": {"experimental": {"serverStatusNotification": True}},
            "initializationOptions": config}})
        await_message(server, lambda m: m.get("id") == 1 and "method" not in m)
        send(server, {"method": "initialized", "params": {}})
        await_message(server, lambda m: m.get("method") == "experimental/serverStatus"
                      and m["params"].get("quiescent") is True
                      and m["params"].get("health") == "ok")
        path = SOURCE / position["path"]
        text = path.read_text(encoding="utf-8")
        lines = text.split("\n")
        line = position["line"]
        if line >= len(lines) or position["character"] >= len(lines[line].encode("utf-16-le")) // 2:
            raise ValueError("position outside Rust source")
        send(server, {"method": "textDocument/didOpen", "params": {
            "textDocument": {"uri": path.as_uri(), "languageId": "rust", "version": 1,
                             "text": text}}})
        params = {"textDocument": {"uri": path.as_uri()},
                  "position": {"line": position["line"], "character": position["character"]}}
        if request["operation"] == "references":
            params["context"] = {"includeDeclaration": True}
        send(server, {"id": 2, "method": METHODS[request["operation"]], "params": params})
        result = anchors(await_message(server, lambda m: m.get("id") == 2
                                       and "method" not in m)["result"], request["limit"])
        send(server, {"id": 3, "method": "shutdown", "params": None})
        await_message(server, lambda m: m.get("id") == 3 and "method" not in m)
        send(server, {"method": "exit", "params": None})
        finish(server)
    finally:
        try:
            if server.poll() is None:
                server.kill()
                server.wait(timeout=SHUTDOWN_SECONDS)
        finally:
            server.stdout.close()
            with suppress(BrokenPipeError):
                server.stdin.close()
    print(json.dumps(result))


if __name__ == "__main__":
    main()
