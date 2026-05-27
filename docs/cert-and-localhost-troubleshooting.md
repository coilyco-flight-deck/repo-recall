# Troubleshooting + known gaps

## Troubleshooting

**`getaddrinfo ENOTFOUND repo-recall.localhost`** - hosts file is missing the entry, or the DNS cache has a stale negative answer. Add the entry per [hostname setup](cert-and-localhost-http.md), then flush:

```
sudo dscacheutil -flushcache
sudo killall -HUP mDNSResponder
```

**`UNABLE_TO_GET_ISSUER_CERT_LOCALLY` / `unable to get local issuer certificate`** - `NODE_EXTRA_CA_CERTS` is unset or pointing at the wrong file. Re-run the verify step under [TLS trust](cert-and-localhost-http.md).

**Desktop session shows no repo-recall tools** - the env var wasn't in scope when Claude launched. Check with `launchctl getenv NODE_EXTRA_CA_CERTS` from a terminal. If blank, `launchctl setenv` hasn't run in the current launchd session. Re-run it and fully quit / relaunch Claude.

**Trust prompt didn't appear on first session** - you likely approved the server already on a previous run, since CLI and Desktop share `~/.claude.json`. To re-trigger the prompt, run `claude mcp reset-project-choices` from the repo, then restart your Claude session.

**`SDK auth failed: Unable to connect`** - misleading wording. This usually means the TLS connection couldn't be established at all (before any auth handshake). Run the Node fetch verify step to find the actual layer that's failing.

## Known gaps

**Desktop has no `/mcp` UI.** No status indicator, no per-server reconnect, no view of OAuth state. Tracked in [anthropics/claude-code#54136](https://github.com/anthropics/claude-code/issues/54136). Workaround: use CLI for status, or fully quit and relaunch Desktop to force a reconnect of all servers.

**`launchctl setenv` doesn't survive reboot.** Use the LaunchAgent in [clients](cert-and-localhost-clients.md) for persistence.

**`*.localhost` resolution varies by tool.** Browsers and some curl builds resolve it without `/etc/hosts`. Node and most other libc-backed tools don't. The hosts entry is the portable fix.

## See also

- [docs/cert-and-localhost-overview.md](cert-and-localhost-overview.md) - intro + stdio alternative.
- [docs/cert-and-localhost-http.md](cert-and-localhost-http.md) - hostname + TLS.
- [docs/cert-and-localhost-clients.md](cert-and-localhost-clients.md) - CLI and Desktop env.
