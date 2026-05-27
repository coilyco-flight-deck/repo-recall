# Claude client env wiring

After completing [hostname + TLS](cert-and-localhost-http.md) setup, wire the env var into your Claude client.

## CLI: persist the env var in your shell rc

```
echo 'export NODE_EXTRA_CA_CERTS="$HOME/Library/Application Support/Caddy/pki/authorities/local/root.crt"' >> ~/.zshrc
```

Open a fresh terminal, then `claude` in the repo. On first launch you'll get a trust prompt for the project-scoped MCP server. Approve it. Verify with `/mcp`. You should see repo-recall as connected with its tools listed.

## Desktop: launchctl setenv + LaunchAgent

GUI apps on macOS don't read your shell rc. They inherit launchd's environment. Set the env var via launchctl so Claude Desktop sees it:

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

Replace `YOU` with your username. LaunchAgents don't expand `$HOME`. Load it:

```
launchctl load ~/Library/LaunchAgents/dev.repo-recall.node-extra-ca.plist
```

Then in Claude Desktop, click **+ New session** in the sidebar, pick **Local**, and select the repo-recall folder. On first run you'll get a trust prompt for the project-scoped server. Approve it.

The CLI and Desktop share `~/.claude.json`, so if you already approved via CLI, the prompt won't reappear in Desktop.

## Verification

In an active session, ask Claude:

```
What tools do you have available from the repo-recall server?
```

If it lists tools, repo-recall is connected. In the CLI, `/mcp` shows per-server status with reconnect controls.

## See also

- [docs/cert-and-localhost-troubleshooting.md](cert-and-localhost-troubleshooting.md) - error recovery + known gaps.
