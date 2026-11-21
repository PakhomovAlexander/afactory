"""A scripted HTTP provider with independent usage and transport evidence.

Each workload selects a named script with X-Fixture-Operation. The script's
entries describe successive requests, with no implicit retries or fallback.
Response bodies are literal UTF-8 chunks. Private evidence is written to a
SQLite file, so no HTTP route exposes the provider's ground truth.
"""
import argparse
from contextlib import closing
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import hashlib
import hmac
import json
import math
import os
from pathlib import Path
import socket
import sqlite3
import time


def encode(value):
    return json.dumps(value, separators=(",", ":"))


class Ledger:
    def __init__(self, path):
        self.path = path
        with closing(sqlite3.connect(path)) as db, db:
            db.execute("PRAGMA journal_mode=WAL")
            db.execute("""CREATE TABLE IF NOT EXISTS calls (
                id INTEGER PRIMARY KEY, operation TEXT NOT NULL,
                ordinal INTEGER NOT NULL, request_sha256 TEXT NOT NULL,
                UNIQUE(operation, ordinal))""")
            db.execute("""CREATE TABLE IF NOT EXISTS events (
                sequence INTEGER PRIMARY KEY, call INTEGER NOT NULL,
                kind TEXT NOT NULL, detail TEXT NOT NULL, unix_ns INTEGER NOT NULL)""")

    def receive(self, operation, body):
        with closing(sqlite3.connect(self.path)) as db, db:
            db.execute("BEGIN IMMEDIATE")
            ordinal = db.execute(
                "SELECT count(*) FROM calls WHERE operation=?", (operation,)
            ).fetchone()[0]
            call = db.execute(
                "INSERT INTO calls(operation,ordinal,request_sha256) VALUES (?,?,?)",
                (operation, ordinal, hashlib.sha256(body).hexdigest()),
            ).lastrowid
            self.record(db, call, "received", {})
            return call, ordinal

    @staticmethod
    def record(db, call, kind, detail):
        db.execute("INSERT INTO events(call,kind,detail,unix_ns) VALUES (?,?,?,?)",
                   (call, kind, encode(detail), time.time_ns()))

    def event(self, call, kind, detail=None):
        with closing(sqlite3.connect(self.path)) as db, db:
            self.record(db, call, kind, detail or {})

    def export(self):
        with closing(sqlite3.connect(self.path)) as db:
            db.row_factory = sqlite3.Row
            db.execute("BEGIN")
            calls = [dict(row) for row in db.execute("SELECT * FROM calls ORDER BY id")]
            events = [dict(row) for row in db.execute("SELECT * FROM events ORDER BY sequence")]
            for event in events:
                event["detail"] = json.loads(event["detail"])
            return {"calls": calls, "events": events}


def load_script(path):
    document = json.loads(path.read_text())
    if document.get("schema") != "af-durability/provider-script@1":
        raise ValueError("unsupported provider script version")
    scripts = document["operations"]
    for name, entries in scripts.items():
        if not name or not isinstance(entries, list) or not entries:
            raise ValueError("an operation requires at least one response")
        for entry in entries:
            fields = {"accepted", "chunks", "content_type", "delay_s", "chunk_delay_s",
                      "disconnect", "retry_after", "status", "usage"}
            if not isinstance(entry, dict) or entry.keys() - fields:
                raise ValueError("invalid provider response field")
            cuts = {None, "before_accept", "after_charge", "during_stream"}
            if entry.get("disconnect") not in cuts:
                raise ValueError("invalid disconnect boundary")
            chunks = entry.get("chunks", [])
            if not isinstance(chunks, list) or not all(isinstance(chunk, str) for chunk in chunks):
                raise ValueError("response chunks must be an array of strings")
            duration = entry.get("delay_s", 0) + entry.get("chunk_delay_s", 0) * len(chunks)
            if not math.isfinite(duration) or not all(
                0 <= value <= 120 for value in
                (entry.get("delay_s", 0), entry.get("chunk_delay_s", 0), duration)
            ):
                raise ValueError("response delay exceeds fixture limit")
            status = entry.get("status", 200)
            if type(status) is not int or not 200 <= status <= 599:
                raise ValueError("invalid HTTP status")
            accepted = entry.get("accepted", True)
            if type(accepted) is not bool:
                raise ValueError("accepted must be boolean")
            if "usage" in entry:
                usage = entry["usage"]
                if not accepted or set(usage) != {"input_tokens", "output_tokens"} or any(
                    type(value) is not int or value < 0 for value in usage.values()
                ):
                    raise ValueError("usage must count nonnegative tokens from an accepted call")
            if entry.get("disconnect") == "after_charge" and "usage" not in entry:
                raise ValueError("after_charge requires ground-truth usage")
            if entry.get("disconnect") == "during_stream" and len(chunks) < 2:
                raise ValueError("during_stream requires at least two response chunks")
            for header in (entry.get("content_type", "application/json"),
                           str(entry.get("retry_after", ""))):
                if not isinstance(header, str) or any(ord(c) < 32 or ord(c) >= 127 for c in header):
                    raise ValueError("invalid fixture response header")
    return scripts


class ProviderHandler(BaseHTTPRequestHandler):
    server_version = "DurabilityProvider/1"

    def setup(self):
        self.request.settimeout(15)
        super().setup()

    def do_POST(self):
        if self.path != self.server.route:
            self.send_error(404)
            return
        token = self.headers.get("Authorization", "").encode()
        if not hmac.compare_digest(token, self.server.authorization):
            self.send_error(401)
            return
        try:
            size = int(self.headers.get("Content-Length", ""))
        except ValueError:
            self.send_error(411)
            return
        if not 0 <= size <= 1_048_576:
            self.send_error(413)
            return
        body = self.rfile.read(size)
        if len(body) != size:
            return
        operation = self.headers.get("X-Fixture-Operation", "")
        call, ordinal = self.server.ledger.receive(operation, body)
        entries = self.server.scripts.get(operation, [])
        if ordinal >= len(entries):
            self.server.ledger.event(call, "unexpected")
            self.send_error(422, "Unexpected fixture call")
            return
        try:
            self.reply(call, entries[ordinal])
        except OSError:
            self.server.ledger.event(call, "peer_disconnected")

    def cut(self, call, boundary):
        self.server.ledger.event(call, "fault_fired", {"boundary": boundary})
        self.request.shutdown(socket.SHUT_RDWR)
        self.close_connection = True

    def reply(self, call, entry):
        boundary = entry.get("disconnect")
        if boundary == "before_accept":
            self.cut(call, boundary)
            return
        if entry.get("accepted", True):
            self.server.ledger.event(call, "accepted")
        time.sleep(entry.get("delay_s", 0))
        chunks = [chunk.encode() for chunk in entry.get("chunks", [])]
        self.server.ledger.event(call, "output_produced", {"bytes": sum(map(len, chunks))})
        if "usage" in entry:
            self.server.ledger.event(call, "charged", entry["usage"])
        if boundary == "after_charge":
            self.cut(call, boundary)
            return
        self.send_response(entry.get("status", 200))
        self.send_header("Content-Type", entry.get("content_type", "application/json"))
        self.send_header("Content-Length", str(sum(map(len, chunks))))
        if "retry_after" in entry:
            self.send_header("Retry-After", str(entry["retry_after"]))
        self.end_headers()
        for index, chunk in enumerate(chunks):
            time.sleep(entry.get("chunk_delay_s", 0))
            self.wfile.write(chunk)
            self.wfile.flush()
            self.server.ledger.event(call, "chunk_written", {"index": index})
            if index == 0 and boundary == "during_stream":
                self.cut(call, boundary)
                return
        self.server.ledger.event(call, "response_written")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ledger", required=True, type=Path)
    parser.add_argument("--export", action="store_true")
    parser.add_argument("--script", type=Path)
    parser.add_argument("--token-file", type=Path)
    parser.add_argument("--bind", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=0)
    parser.add_argument("--route", default="/v1/chat/completions")
    args = parser.parse_args()
    if args.export and not args.ledger.is_file():
        parser.error("ledger does not exist")
    ledger = Ledger(args.ledger)
    if args.export:
        print(json.dumps(ledger.export(), indent=2))
        return
    if args.script is None or args.token_file is None:
        parser.error("serving requires --script and --token-file")
    token = args.token_file.read_bytes().strip()
    if not token:
        parser.error("empty fixture token")
    with ThreadingHTTPServer((args.bind, args.port), ProviderHandler) as server:
        server.authorization = b"Bearer " + token
        server.ledger = ledger
        server.route = args.route
        server.scripts = load_script(args.script)
        print(encode({"listening": server.server_address}), flush=True)
        server.serve_forever()


if __name__ == "__main__":
    os.umask(0o077)
    main()
