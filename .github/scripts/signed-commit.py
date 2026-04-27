#!/usr/bin/env python3
"""Push a multi-file commit through GitHub's createCommitOnBranch GraphQL
mutation. Commits made this way are signed by GitHub Actions' own key
and satisfy `required_signatures` branch protection. The signature is
attributed to the workflow's identity (github-actions[bot] for
GITHUB_TOKEN, the PAT owner for a personal token).

Usage:
    signed-commit.py REPO BRANCH MESSAGE PARENT_OID FILE [FILE ...]

REPO is `owner/name`. PARENT_OID is the SHA the new commit will descend
from (createCommitOnBranch fails fast if the branch has moved past it,
which is the safety we want here). FILEs are paths on disk; their
on-disk contents are sent as additions.

Requires GH_TOKEN in the environment with Contents:write on REPO.
Prints the new commit OID on stdout for downstream steps (e.g. tag
creation) to consume.
"""

from __future__ import annotations

import base64
import json
import os
import sys
import urllib.error
import urllib.request


MUTATION = """
mutation($input: CreateCommitOnBranchInput!) {
  createCommitOnBranch(input: $input) {
    commit { oid url }
  }
}
"""


def main(argv: list[str]) -> int:
    if len(argv) < 6:
        print(__doc__, file=sys.stderr)
        return 2
    repo, branch, message, parent_oid, *files = argv[1:]

    additions = []
    for path in files:
        with open(path, "rb") as fh:
            additions.append(
                {
                    "path": path,
                    "contents": base64.b64encode(fh.read()).decode("ascii"),
                }
            )

    payload = {
        "query": MUTATION,
        "variables": {
            "input": {
                "branch": {
                    "repositoryNameWithOwner": repo,
                    "branchName": branch,
                },
                "message": {"headline": message},
                "expectedHeadOid": parent_oid,
                "fileChanges": {"additions": additions},
            }
        },
    }

    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if not token:
        print("GH_TOKEN or GITHUB_TOKEN must be set", file=sys.stderr)
        return 2

    req = urllib.request.Request(
        "https://api.github.com/graphql",
        data=json.dumps(payload).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "Content-Type": "application/json",
            "User-Agent": "repo-recall-release-workflow",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req) as resp:
            body = json.loads(resp.read())
    except urllib.error.HTTPError as e:
        print(f"HTTP {e.code}: {e.read().decode('utf-8', 'replace')}", file=sys.stderr)
        return 1

    if "errors" in body:
        print(json.dumps(body["errors"], indent=2), file=sys.stderr)
        return 1

    oid = body["data"]["createCommitOnBranch"]["commit"]["oid"]
    print(oid)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
