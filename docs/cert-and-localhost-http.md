# HTTP mode setup: hostname + TLS

See [overview](cert-and-localhost-overview.md) for context.

## 1. Hostname resolution

`repo-recall.localhost` doesn't resolve via libc on macOS by default. Only browsers and some other tools handle `*.localhost` specially. Add explicit hosts entries:

```
echo "127.0.0.1 repo-recall.localhost" | sudo tee -a /etc/hosts
echo "::1 repo-recall.localhost"      | sudo tee -a /etc/hosts
```

Verify:

```
node -e "require('dns').lookup('repo-recall.localhost', console.log)"
```

Should print `null '127.0.0.1' 4`.

## 2. TLS trust

Caddy generates a local CA and installs it into the macOS system Keychain on first run, which is why browsers and curl trust the cert. Node.js does **not** read the system trust store by default. It ships its own CA bundle. So Claude Code (built on Node) won't validate the cert without help.

Find Caddy's root cert:

```
ls "$HOME/Library/Application Support/Caddy/pki/authorities/local/root.crt"
```

If that path doesn't exist, Caddy may be installed elsewhere. Try:

```
sudo find / -name root.crt -path "*caddy*" 2>/dev/null
```

Set `NODE_EXTRA_CA_CERTS` and verify the full TLS path with a fetch:

```
export NODE_EXTRA_CA_CERTS="$HOME/Library/Application Support/Caddy/pki/authorities/local/root.crt"
node -e "fetch('https://repo-recall.localhost:7443/mcp').then(r=>console.log('ok',r.status)).catch(e=>console.error(e.cause||e))"
```

A response like `ok 406` means the TLS handshake succeeded. The server just doesn't like a bare GET, which is expected. `UNABLE_TO_GET_ISSUER_CERT_LOCALLY` means the cert path is wrong.

## See also

- [docs/cert-and-localhost-overview.md](cert-and-localhost-overview.md) - intro + stdio alternative.
- [docs/cert-and-localhost-clients.md](cert-and-localhost-clients.md) - CLI and Desktop env wiring.
- [docs/cert-and-localhost-troubleshooting.md](cert-and-localhost-troubleshooting.md) - error recovery.
