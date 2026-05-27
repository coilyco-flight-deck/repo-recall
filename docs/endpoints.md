# Endpoints

The Rust binary serves JSON HTTP and MCP from the same process on `127.0.0.1:7777` by default.

## JSON endpoints

- `GET /` - full dashboard projection: repos ranked by composite activity score, recent sessions, recent commits, action-required signals, banner counts.
- `GET /api/action-required` - thin action-required list. `id = "<repo_id>:<signal>"`.
- `GET /api/scan-version` - single-integer poll target.
- `POST /api/refresh` - sync refresh.
- `GET /api/repos/{repo_id}/tickets/{issue_number}/history` - per-issue session + commit join.
- `GET /openapi.json` - hand-maintained OpenAPI 3.1 description of the surface.

Every JSON response carries `ETag: "<scan_version>"`. Pass `If-None-Match` for `304 Not Modified` between scans.

## MCP tools

`recall_dashboard`, `recall_repo`, `recall_session`, `recall_search`, `recall_action_required`, `recall_ticket_history`, `recall_refresh`.

## See also

- [README.md](../README.md) - human-facing intro.
- [AGENTS.md](../AGENTS.md) - agent-facing operating rules.
