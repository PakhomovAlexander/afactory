#!/usr/bin/env python3
"""One bounded collection cycle; safe to schedule hourly on a trusted archive host."""
import argparse
import os
import shutil
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from perflab import archive, collection, contracts, history, transport  # noqa: E402
from perflab.values import ContractError, require  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--history", required=True, help="dedicated generated history directory")
    parser.add_argument("--lookback-days", type=int, default=13)
    parser.add_argument("--gh-auth", action="store_true", help="read token from gh's credential store")
    parser.add_argument("--token-env", default=transport.TOKEN_ENV)
    args = parser.parse_args()
    try:
        if args.gh_auth:
            result = subprocess.run(["gh", "auth", "token", "--hostname", "github.com"],
                                    capture_output=True, timeout=15, check=False)
            require(result.returncode == 0, "gh could not read the collector credential")
            token = result.stdout.decode().strip()
        else:
            token = transport.token_from(os.environ, args.token_env)
        api = transport.Collector(transport.HttpsTransport(), args.repository, token)
        runs = collection.completed(api, args.lookback_days)
        with archive.Archive(args.root) as store:
            ok = collection.collect(store, api, runs)
            store.retain()
            health = store.status()
        # No token is passed to a product process; history renders validated archive data.
        destination = os.path.abspath(args.history)
        require(not os.path.islink(destination), "history directory may not be a symlink")
        require(not os.path.exists(destination)
                or os.path.isfile(os.path.join(destination, "history.json")),
                "history output must be absent or a generated history directory")
        parent = os.path.dirname(destination)
        os.makedirs(parent, exist_ok=True)
        temporary = tempfile.mkdtemp(prefix=".af-perf-history-", dir=parent)
        fresh = os.path.join(temporary, "new")
        try:
            history.write([args.root], fresh)
            with open(os.path.join(fresh, "archive-status.json"), "wb") as stream:
                stream.write(contracts.write_bytes(health, "archive-status"))
            if os.path.exists(destination):
                os.rename(destination, os.path.join(temporary, "old"))
            try:
                os.rename(fresh, destination)
            except OSError:
                if os.path.exists(os.path.join(temporary, "old")):
                    os.rename(os.path.join(temporary, "old"), destination)
                raise
        finally:
            shutil.rmtree(temporary)
        print("Archive: %d runs, %d bytes; health %s" % (
            health["runs"], health["used_bytes"], health["health"]["status"]))
        return 0 if ok and health["health"]["status"] == "ok" else 4
    except (ContractError, OSError, ValueError, subprocess.TimeoutExpired) as error:
        sys.stderr.write("performance collection failed: %s\n" % error)
        return 4


if __name__ == "__main__":
    sys.exit(main())
