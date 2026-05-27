# Connecting repo-recall to Claude: overview

repo-recall exposes sensitive local filesystem state, so its default authorization treats the network boundary as a trust boundary: only reachable from the same host. That constraint shapes which Claude clients can use it and how to configure them.

## Compatible clients

Claude.ai web is **not** compatible. Connectors there route through Anthropic's MCP gateway, which cannot reach `localhost` or any `.localhost` hostname.

Two clients work, both because they run on your machine and reach localhost directly:

- **Claude Code CLI** - `claude` in your terminal.
- **Claude Desktop**, Code or Cowork tabs - both run Claude Code under the hood.

A third option, **stdio transport**, is the lowest-friction path for v1: Claude spawns repo-recall as a subprocess and communicates over stdin/stdout, with no network exposure or TLS.

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

Pick one. stdio mode requires nothing beyond the binary being on `PATH`. The OS process boundary becomes the trust boundary, which actually enforces "must be on same host" by construction rather than by policy. Tradeoff: each Claude client launches its own subprocess, so any in-process state (caches, open connections, accumulated context) isn't shared across CLI and Desktop sessions the way an HTTP server's state would be.

For HTTP mode, three layers need to line up: hostname resolution, TLS trust, and the runtime environment of whichever Claude client you're using.

## See also

- [docs/cert-and-localhost-http.md](cert-and-localhost-http.md) - hostname + TLS setup.
- [docs/cert-and-localhost-clients.md](cert-and-localhost-clients.md) - CLI and Desktop env wiring.
- [docs/cert-and-localhost-troubleshooting.md](cert-and-localhost-troubleshooting.md) - error recovery + known gaps.
