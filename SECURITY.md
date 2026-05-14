# Security Policy

Hello and thank you for your interest! :tada: :lock:

## Supported versions

This package is at v0. Only the latest commit on `main` is supported for security fixes - there are no pinned releases to backport to.

| Version             | Supported          |
| ------------------- | ------------------ |
| `main` (latest)     | :white_check_mark: |
| any pinned commit   | :x: (upgrade)      |

## Reporting a vulnerability

Please disclose any vulnerabilities by emailing [coilysiren@gmail.com](mailto:coilysiren@gmail.com). Expect a first response within 48 hours; follow-up cadence by email after that. This project is run on volunteer time, so please have patience :bow:

## What counts as a vulnerability

repo-recall is a local hydration layer over the operator's git repos, GitHub state, Claude Code session JSONL, and cli-guard audit log. The HTTP/MCP server binds loopback only and runs in user space, but its inputs are sensitive and its outputs can be quoted into other tools. Specifically interested in:

- the HTTP/MCP server accepting connections on a non-loopback interface without `REPO_RECALL_HOST` being explicitly set to one
- the 200-char session-summary truncation leaking content past the cap (secrets, credentials, identifying detail)
- ETag / `scan_version` collisions that let a stale response satisfy a `If-None-Match` from a newer client
- `gh` subprocess argv constructed from user-controlled input (repo names, branch names) producing different argv than the same string typed at a shell
- redb cache or tantivy index written outside `$REPO_RECALL_CACHE_DIR` / `$REPO_RECALL_INDEX_DIR`, or with permissions wider than the invoking user
- search index or session ingest treating one user's data as another's (cross-user mixing on a shared host)
- `gh api graphql` calls anywhere in the codebase (REST only; see AGENTS.md)

## Out of scope

- bugs in [axum](https://github.com/tokio-rs/axum), [redb](https://github.com/cberner/redb), [tantivy](https://github.com/quickwit-oss/tantivy), [pmcp](https://crates.io/crates/pmcp), or [gh](https://github.com/cli/cli) - report those upstream
- consumer misuse (binding `REPO_RECALL_HOST=0.0.0.0` on a shared box, pointing the dashboard at someone else's Claude projects dir) - those are operator choices, documented in the README
- secrets pasted into a session JSONL by the upstream Claude Code client - that is a Claude Code concern; redaction beyond the 200-char truncate is future work
- `gh` missing or unauthenticated - documented behavior, not a vulnerability
