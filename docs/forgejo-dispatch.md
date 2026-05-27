# Forgejo per-repo dispatch

Per-repo dispatch routes each tracked repo's remote-state fetch (issues, PRs, milestones, active-repos) to the right provider client. Kai moved her source-of-truth to Forgejo (`forgejo.coilysiren.me`) to escape GitHub rate limits; repo-recall mirrors that move via #91.

## How dispatch picks a client

`ingest::remote_kind::RemoteKindCache::detect(host)` returns `Github`, `Forgejo`, or `None`:

- `github.com` short-circuits to `Github` with no probe.
- Any other host gets one `GET https://<host>/api/v1/version` per TTL window (1h). Forgejo/Gitea answers with `{"version":"..."}`; anything else returns `None` and the repo is skipped from the remote pass.

`display::routes::refresh::ingest_remote_state` walks `cache_db.remote_targets`, calls `git::log::remote_host_and_slug` to get `(host, owner/repo)`, then picks `state.github_client` or `state.forgejo_client` based on the probe result. Milestones get the `source` tag (`milestone_source::GITHUB` or `FORGEJO`) at upsert time.

## Auth + config

- `REPO_RECALL_FORGEJO_TOKEN` — Forgejo API token (header: `Authorization: token <T>`). Missing token → `RemoteFetchState::Unconfigured` from every fetcher, dashboard renders Forgejo columns as "not configured."
- `REPO_RECALL_FORGEJO_HOST` — host for the singleton Forgejo client built at startup (default `forgejo.coilysiren.me`). Per-repo dispatch uses this client for every Forgejo-detected repo; multi-instance Forgejo support is not in scope.

## Endpoint mapping

| Trait method | GitHub | Forgejo |
|---|---|---|
| `fetch_user` | `/user` | `/user` |
| `fetch_open_issues` | `/repos/{X}/issues?state=open` | `/repos/{X}/issues?state=open&type=issues` |
| `fetch_open_prs` | `/repos/{X}/pulls?state=open` | `/repos/{X}/pulls?state=open` |
| `fetch_open_milestones` | `/repos/{X}/milestones?state=open` | `/repos/{X}/milestones?state=open` |
| `fetch_active_repos` | `/user/repos?sort=pushed&type=owner` | `/user/repos?page=1&limit=N` |
| `fetch_deploy_health` | `/repos/{X}/actions/workflows/{wf}/runs` | `Unconfigured` (deferred) |

Payload shapes are field-compatible across the four parsed endpoints, so `parse_issues_json` / `parse_prs_json` / `parse_milestones_json` / `parse_active_repos_json` are reused as-is.

## Out of scope

- Forgejo Actions deploy-health ingest. The `fetch_deploy_health` Forgejo impl returns `Unconfigured`; track as a follow-up if needed.
- Cross-source de-duplication (a project mirrored to both providers gets two milestone rows; the daily skill decides whether to merge).
- GraphQL on either provider.
