#!/usr/bin/env python3
"""tooling-autonomous-pickup: scan repo-recall dispatch artifacts and mark them handled.

Subcommands:
    scan          Walk ~/.repo-recall/dispatch/<repo>/ and emit JSON for every
                  unhandled artifact. The skill body consumes this and calls
                  mcp__scheduled-tasks__create_scheduled_task per item.
    mark-handled  Write a `<path>.handled` sidecar so future scans skip the
                  artifact. Atomic. Idempotent.

The frontmatter shape mirrors repo-recall's `dispatch_artifacts.rs`:

    ---
    issue_refs: [coilysiren/repo-recall#92]
    score: 5
    autonomy_confidence: 4
    autonomy_confidence_basis: substrate is indexed
    prompt_hash: abcdef0123456789
    dispatched_at: 2026-05-13T12:34:56Z
    tracking_issue: coilysiren/repo-recall#99
    ---
    <prompt body>
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
from pathlib import Path

ROOT_ENV = "REPO_RECALL_DISPATCH_ROOT"
DEFAULT_ROOT = Path.home() / ".repo-recall" / "dispatch"
HANDLED_SUFFIX = ".handled"


def dispatch_root() -> Path:
    override = os.environ.get(ROOT_ENV)
    return Path(override) if override else DEFAULT_ROOT


def is_handled(md_path: Path) -> bool:
    return md_path.with_name(md_path.name + HANDLED_SUFFIX).exists()


def parse_frontmatter(text: str) -> tuple[dict, str]:
    """Minimal YAML-ish frontmatter parser. Matches what `render_dispatch_file`
    actually emits: scalar key: value pairs and a single bracketed-list
    `issue_refs`. Anything richer is not produced today, so a full YAML
    dependency would be overkill."""
    if not text.startswith("---\n"):
        return {}, text
    end = text.find("\n---\n", 4)
    if end < 0:
        return {}, text
    front = text[4:end]
    body = text[end + len("\n---\n"):]
    meta: dict = {}
    for line in front.splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if ":" not in line:
            continue
        key, _, val = line.partition(":")
        key = key.strip()
        val = val.strip()
        if val.startswith("[") and val.endswith("]"):
            inner = val[1:-1].strip()
            meta[key] = [s.strip() for s in inner.split(",") if s.strip()] if inner else []
        else:
            meta[key] = val
    return meta, body


def find_artifacts(root: Path) -> list[Path]:
    if not root.exists():
        return []
    out: list[Path] = []
    for repo_dir in sorted(root.iterdir()):
        if not repo_dir.is_dir() or repo_dir.name.startswith("."):
            continue
        for f in sorted(repo_dir.glob("*.md")):
            if f.name.endswith(HANDLED_SUFFIX):
                continue
            if is_handled(f):
                continue
            out.append(f)
    return out


SLUG_OK = re.compile(r"[^a-z0-9-]+")


def task_id_from(slug: str, repo: str) -> str:
    """Compose a kebab-case taskId. `create_scheduled_task` auto-sanitizes but
    we still want something predictable so the user can recognize it in the
    scheduled-tasks list."""
    base = f"pickup-{repo}-{slug}"
    base = base.lower()
    base = SLUG_OK.sub("-", base)
    base = re.sub(r"-+", "-", base).strip("-")
    return base[:80] or "pickup"


def cmd_scan(args: argparse.Namespace) -> int:
    del args  # unused; argparse dispatch supplies it
    root = dispatch_root()
    artifacts = find_artifacts(root)
    out = []
    for path in artifacts:
        try:
            text = path.read_text(encoding="utf-8")
        except OSError as e:
            print(f"# skip {path}: {e}", file=sys.stderr)
            continue
        meta, body = parse_frontmatter(text)
        repo_slug = path.parent.name
        slug = path.stem
        issue_refs = meta.get("issue_refs", []) or []
        primary_ref = issue_refs[0] if issue_refs else None
        description = (
            f"autonomous-pickup: {primary_ref}" if primary_ref else f"autonomous-pickup: {repo_slug}/{slug}"
        )
        out.append({
            "path": str(path),
            "repo_slug": repo_slug,
            "slug": slug,
            "task_id": task_id_from(slug, repo_slug),
            "description": description[:120],
            "issue_refs": issue_refs,
            "tracking_issue": meta.get("tracking_issue") or None,
            "prompt": body.rstrip() + "\n",
        })
    json.dump({"root": str(root), "count": len(out), "items": out}, sys.stdout, indent=2)
    sys.stdout.write("\n")
    return 0


def cmd_mark_handled(args: argparse.Namespace) -> int:
    target = Path(args.path)
    if not target.exists():
        print(f"error: not a file: {target}", file=sys.stderr)
        return 2
    sidecar = target.with_name(target.name + HANDLED_SUFFIX)
    tmp = sidecar.with_suffix(sidecar.suffix + ".tmp")
    payload = (args.note or "").strip()
    if payload:
        payload += "\n"
    tmp.write_text(payload, encoding="utf-8")
    os.replace(tmp, sidecar)
    print(str(sidecar))
    return 0


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(prog="autonomous-pickup")
    sub = p.add_subparsers(dest="cmd", required=True)

    scan = sub.add_parser("scan", help="emit JSON for every unhandled dispatch artifact")
    scan.set_defaults(func=cmd_scan)

    mark = sub.add_parser("mark-handled", help="write a .handled sidecar next to the artifact")
    mark.add_argument("path", help="path to the .md artifact (NOT the sidecar)")
    mark.add_argument("--note", help="optional one-line note recorded in the sidecar", default="")
    mark.set_defaults(func=cmd_mark_handled)

    ns = p.parse_args(argv)
    return ns.func(ns)


if __name__ == "__main__":
    raise SystemExit(main())
