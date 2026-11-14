#!/usr/bin/env python3
"""Render merged PRs, excluding commits already included in the previous release."""
import json
import subprocess
import sys


def main():
    tag = sys.argv[1]
    prs = json.load(sys.stdin)
    for pr in sorted(prs, key=lambda item: item['mergedAt']):
        if tag:
            oid = (pr.get('mergeCommit') or {}).get('oid')
            if not oid:
                sys.exit(f"release: PR #{pr['number']} has no merge commit")
            result = subprocess.run(['git', 'merge-base', '--is-ancestor', oid, tag])
            if result.returncode == 0:
                continue
            if result.returncode != 1:
                sys.exit(f"release: cannot compare PR #{pr['number']} with {tag}")
        print(f"- {pr['title']} (#{pr['number']})")


if __name__ == '__main__':
    main()
