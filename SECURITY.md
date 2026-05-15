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

## Intended deployment

repo-recall is built to run on a single operator's machine, bound to `127.0.0.1`. When an operator wants the dashboard from another device, the intended posture is to keep the loopback bind and front it with a VPN like [Tailscale](https://tailscale.com) (`tailscale serve` works well), or an SSH tunnel, or a similar tailnet-only fronting layer. Setting `REPO_RECALL_HOST` to a non-loopback address on a host that isn't already gated at that other layer is operator misuse, not a supported configuration.

## What counts as a vulnerability

repo-recall is a local hydration layer over the operator's git repos, GitHub state, Claude Code session JSONL, and cli-guard audit log. The HTTP/MCP server binds loopback only and runs in user space, but its inputs are sensitive and its outputs can be quoted into other tools. Specifically interested in:

- the HTTP/MCP server accepting connections on a non-loopback interface without `REPO_RECALL_HOST` being explicitly set to one
- the 200-char session-summary truncation leaking content past the cap (secrets, credentials, identifying detail)
- ETag / `scan_version` collisions that let a stale response satisfy a `If-None-Match` from a newer client
- `gh` subprocess argv constructed from user-controlled input (repo names, branch names) producing different argv than the same string typed at a shell
- redb cache or tantivy index written outside `$REPO_RECALL_CACHE_DIR` / `$REPO_RECALL_INDEX_DIR`, or with permissions wider than the invoking user
- search index or session ingest treating one user's data as another's (cross-user mixing on a shared host)
- `gh api graphql` calls anywhere in the codebase outside the single sanctioned site in `src/ingest/github/labeled.rs` (REST only; see AGENTS.md "No GraphQL" exception)
- the labeled-issue GraphQL query widening beyond the explicitly-listed `repo:owner/name` filters built from repos we discovered on disk

## Scope discipline: repos on disk only

repo-recall only searches repos it discovered on disk. No org-wide queries, no probing for repos the operator hasn't told it about. The labeled-issue GraphQL ingest builds its `repo:owner/name` filter list from the local repo set; the GitHub search endpoint cannot widen the result set beyond that list. There is no "scan the org" or "scan everything I have access to" path.

## Out of scope

- bugs in [axum](https://github.com/tokio-rs/axum), [redb](https://github.com/cberner/redb), [tantivy](https://github.com/quickwit-oss/tantivy), [pmcp](https://crates.io/crates/pmcp), or [gh](https://github.com/cli/cli) - report those upstream
- consumer misuse (binding `REPO_RECALL_HOST=0.0.0.0` on a shared box, pointing the dashboard at someone else's Claude projects dir) - those are operator choices, documented in the README
- secrets pasted into a session JSONL by the upstream Claude Code client - that is a Claude Code concern; redaction beyond the 200-char truncate is future work
- `gh` missing or unauthenticated - documented behavior, not a vulnerability
