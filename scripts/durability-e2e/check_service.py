"""A bounded Checks fixture with a durable ledger and an out-of-band control file."""
import argparse
from contextlib import closing
import hmac
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import re
import socket
import sqlite3


class Handler(BaseHTTPRequestHandler):
    def setup(self):
        self.request.settimeout(8)
        super().setup()

    def reply(self, status, value):
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.handle_check("read")

    def do_POST(self):
        self.handle_check("create")

    def handle_check(self, action):
        match = re.fullmatch(r"/checks/([0-9a-f]{64})", self.path)
        if not match:
            self.send_error(404)
            return
        check = match[1]
        settings = json.loads((self.server.root / "settings.json").read_text())
        token = (self.server.root / "token").read_text().strip()
        auth = self.headers.get("Authorization", "")
        revision = self.headers.get("X-Fixture-Revision", "")
        intent = self.headers.get("X-Fixture-Intent", "")
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", intent):
            self.send_error(400)
            return
        with closing(sqlite3.connect(self.server.root / "ledger.sqlite")) as db:
            with db:
                response = self.dispatch(db, action, check, settings, revision, auth, token)
                db.execute("""INSERT INTO calls(action,check_id,intent,status,served_revision)
                    VALUES (?,?,?,?,?)""", (action, check, intent, response[0],
                                           response[1].get("revision")))
        status, value = response
        if status == 0:
            self.request.shutdown(socket.SHUT_RDWR)
            self.close_connection = True
            return
        self.reply(status, value)

    @staticmethod
    def dispatch(db, action, check, settings, revision, auth, token):
        if settings.get("expired") or not hmac.compare_digest(auth, "Bearer " + token):
            return 401, {"error": "expired_fixture_credential"}
        row = db.execute("SELECT revision FROM checks WHERE id=?", (check,)).fetchone()
        if action == "read":
            if row and settings.get("expire_after_create") and not settings.get("restored"):
                return 401, {"error": "expired_fixture_credential"}
            if settings.get("unavailable") or (
                row and settings.get("lose_create_reply") and not settings.get("restored")
            ):
                return 503, {"error": "fixture_outage"}
            if row is None:
                return 404, {"error": "unknown_check"}
            return 200, {"id": check, "revision": settings.get("stale_revision", row[0]),
                         "result": "green" if settings.get("stale_revision") else settings["result"]}
        if not re.fullmatch(r"(?:[0-9a-f]{40}|(?:sha256:)?[0-9a-f]{64})", revision):
            return 400, {"error": "invalid_candidate_revision"}
        if row is not None:
            if row[0] != revision:
                return 409, {"error": "intent_revision_mismatch"}
            return 200, {"id": check, "revision": row[0], "result": settings["result"]}
        db.execute("INSERT INTO checks(id, revision) VALUES (?,?)", (check, revision))
        if settings.get("lose_create_reply") and not settings.get("restored"):
            return 0, {}
        return 201, {"id": check, "revision": revision, "result": settings["result"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    parser.add_argument("--bind", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=0)
    args = parser.parse_args()
    os.umask(0o077)
    with closing(sqlite3.connect(args.root / "ledger.sqlite")) as db, db:
        db.execute("""CREATE TABLE IF NOT EXISTS checks (
            id TEXT PRIMARY KEY, revision TEXT NOT NULL)""")
        db.execute("""CREATE TABLE IF NOT EXISTS calls (
            sequence INTEGER PRIMARY KEY, action TEXT NOT NULL,
            check_id TEXT NOT NULL, intent TEXT NOT NULL, status INTEGER NOT NULL,
            served_revision TEXT)""")
    with HTTPServer((args.bind, args.port), Handler) as server:
        server.root = args.root
        print(json.dumps({"listening": list(server.server_address)}), flush=True)
        server.serve_forever()


if __name__ == "__main__":
    main()
