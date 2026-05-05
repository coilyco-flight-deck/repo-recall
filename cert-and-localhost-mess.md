# Connecting repo-recall to Claude

repo-recall exposes sensitive local filesystem state, so its default authorization posture treats the network boundary as a trust boundary: it's only reachable from the same host. That constraint shapes which Claude clients can use it and how to configure them.

## Compatible clients

Claude.ai web is **not** compatible. Connectors configured there are routed through Anthropic's MCP gateway, which cannot reach `localhost` or any `.localhost` hostname.

Two clients work, both because they run on your machine and reach localhost directly:

- **Claude Code CLI** — `claude` in your terminal.
- **Claude Desktop**, Code or Cowork tabs — both run Claude Code under the hood.

A third option, **stdio transport**, is the lowest-friction path for v1: Claude spawns repo-recall as a subprocess and communicates over stdin/stdout, with no network exposure or TLS. See [stdio alternative](#stdio-alternative) below.

## Project config: `.mcp.json`

repo-recall is configured via a project-scoped `.mcp.json` at the repo root, committed to version control. Both Claude Code CLI and the Code/Cowork tabs in Claude Desktop pick it up automatically.

### HTTP mode (production-shaped)

```json
{
  "mcpServers": {
    "repo-recall": {
      "type": "http",
      "url": "https://repo-recall.localhost:7443/mcp"
    }
  }
}
```

### stdio mode (v1 default)

```json
{
  "mcpServers": {
    "repo-recall": {
      "command": "repo-recall",
      "args": ["--stdio"]
    }
  }
}
```

Pick one. The rest of this doc focuses on HTTP mode because it has more setup; stdio mode requires nothing beyond the binary being on `PATH`.

## HTTP mode setup

Three layers need to line up: hostname resolution, TLS trust, and the runtime environment of whichever Claude client you're using.

### 1. Hostname resolution

`repo-recall.localhost` doesn't resolve via libc on macOS by default — only browsers and some other tools handle `*.localhost` specially. Add explicit hosts entries:

```
echo "127.0.0.1 repo-recall.localhost" | sudo tee -a /etc/hosts
echo "::1 repo-recall.localhost"      | sudo tee -a /etc/hosts
```

Verify:

```
node -e "require('dns').lookup('repo-recall.localhost', console.log)"
```

Should print `null '127.0.0.1' 4`.

### 2. TLS trust

Caddy generates a local CA and installs it into the macOS system Keychain on first run, which is why browsers and curl trust the cert. Node.js does **not** read the system trust store by default — it ships its own CA bundle — so Claude Code (built on Node) won't validate the cert without help.

Find Caddy's root cert:

```
ls "$HOME/Library/Application Support/Caddy/pki/authorities/local/root.crt"
```

If that path doesn't exist, Caddy may be installed elsewhere; try:

```
sudo find / -name root.crt -path "*caddy*" 2>/dev/null
```

Set `NODE_EXTRA_CA_CERTS` and verify the full TLS path with a fetch:

```
export NODE_EXTRA_CA_CERTS="$HOME/Library/Application Support/Caddy/pki/authorities/local/root.crt"
node -e "fetch('https://repo-recall.localhost:7443/mcp').then(r=>console.log('ok',r.status)).catch(e=>console.error(e.cause||e))"
```

A response like `ok 406` means the TLS handshake succeeded — the server just doesn't like a bare GET, which is expected. `UNABLE_TO_GET_ISSUER_CERT_LOCALLY` means the cert path is wrong.

### 3a. CLI: persist the env var in your shell rc

```
echo 'export NODE_EXTRA_CA_CERTS="$HOME/Library/Application Support/Caddy/pki/authorities/local/root.crt"' >> ~/.zshrc
```

Open a fresh terminal, then `claude` in the repo. On first launch you'll get a trust prompt for the project-scoped MCP server — approve it. Verify with `/mcp`; you should see repo-recall as ✓ connected with its tools listed.

### 3b. Desktop: launchctl setenv + LaunchAgent

GUI apps on macOS don't read your shell rc — they inherit launchd's environment. Set the env var via launchctl so Claude Desktop sees it:

```
launchctl setenv NODE_EXTRA_CA_CERTS "$HOME/Library/Application Support/Caddy/pki/authorities/local/root.crt"
```

Then fully quit Claude Desktop (closing the window isn't enough on macOS) and relaunch:

```
osascript -e 'quit app "Claude"'
open -a Claude
```

`launchctl setenv` doesn't survive a reboot. To persist, drop a LaunchAgent at `~/Library/LaunchAgents/dev.repo-recall.node-extra-ca.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>dev.repo-recall.node-extra-ca</string>
  <key>ProgramArguments</key>
  <array>
    <string>launchctl</string>
    <string>setenv</string>
    <string>NODE_EXTRA_CA_CERTS</string>
    <string>/Users/YOU/Library/Application Support/Caddy/pki/authorities/local/root.crt</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
```

Replace `YOU` with your username — LaunchAgents don't expand `$HOME`. Load it:

```
launchctl load ~/Library/LaunchAgents/dev.repo-recall.node-extra-ca.plist
```

Then in Claude Desktop, click **+ New session** in the sidebar, pick **Local**, and select the repo-recall folder. On first run you'll get a trust prompt for the project-scoped server — approve it.

The CLI and Desktop share `~/.claude.json`, so if you already approved via CLI, the prompt won't reappear in Desktop.

## Verification

In an active session, ask Claude:

```
What tools do you have available from the repo-recall server?
```

If it lists tools, repo-recall is connected. If it says it has no such server, see [troubleshooting](#troubleshooting).

In the CLI specifically, `/mcp` shows per-server status with reconnect controls. Desktop doesn't expose this yet — see [known gaps](#known-gaps).

## stdio alternative

For v1 testing, stdio mode sidesteps the entire DNS/TLS/launchd chain. Update `.mcp.json`:

```json
{
  "mcpServers": {
    "repo-recall": {
      "command": "repo-recall",
      "args": ["--stdio"]
    }
  }
}
```

The Claude client spawns repo-recall as a subprocess and talks to it over stdin/stdout. No `/etc/hosts` edits, no Caddy CA trust, no launchctl. The OS process boundary becomes the trust boundary, which actually enforces "must be on same host" by construction rather than by policy.

Tradeoff: each Claude client launches its own subprocess, so any in-process state (caches, open connections, accumulated context) isn't shared across CLI and Desktop sessions the way an HTTP server's state would be.

## Troubleshooting

**`getaddrinfo ENOTFOUND repo-recall.localhost`** — hosts file is missing the entry, or the DNS cache has a stale negative answer. Add the entry per [step 1](#1-hostname-resolution), then flush:

```
sudo dscacheutil -flushcache
sudo killall -HUP mDNSResponder
```

**`UNABLE_TO_GET_ISSUER_CERT_LOCALLY` / `unable to get local issuer certificate`** — `NODE_EXTRA_CA_CERTS` is unset or pointing at the wrong file. Re-run the verify step under [TLS trust](#2-tls-trust).

**Desktop session shows no repo-recall tools** — the env var wasn't in scope when Claude launched. Check with `launchctl getenv NODE_EXTRA_CA_CERTS` from a terminal; if blank, `launchctl setenv` hasn't run in the current launchd session. Re-run it and fully quit/relaunch Claude.

**Trust prompt didn't appear on first session** — you likely approved the server already on a previous run, since CLI and Desktop share `~/.claude.json`. To re-trigger the prompt, run `claude mcp reset-project-choices` from the repo, then restart your Claude session.

**`SDK auth failed: Unable to connect`** — misleading wording; this usually means the TLS connection couldn't be established at all (before any auth handshake). Run the Node fetch verify step to find the actual layer that's failing.

## Known gaps

**Desktop has no `/mcp` UI.** No status indicator, no per-server reconnect, no view of OAuth state. Tracked in [anthropics/claude-code#54136](https://github.com/anthropics/claude-code/issues/54136). Workaround: use CLI for status, or fully quit and relaunch Desktop to force a reconnect of all servers.

**`launchctl setenv` doesn't survive reboot.** Use the LaunchAgent in step 3b for persistence.

**`*.localhost` resolution varies by tool.** Browsers and some curl builds resolve it without `/etc/hosts`; Node and most other libc-backed tools don't. The hosts entry is the portable fix.
