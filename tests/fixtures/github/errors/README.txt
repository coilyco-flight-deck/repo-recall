# Synthetic GitHub error fixtures

Hand-authored from [GitHub REST docs](https://docs.github.com/en/rest/overview/troubleshooting-the-rest-api) and the [rate-limit guide](https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api). These cover failure modes we can't reliably trigger against the live API:

- `unauthorized.http` - 401 from a revoked / bad token
- `rate_limited_primary.http` - 403 with `x-ratelimit-remaining: 0` + reset epoch
- `rate_limited_secondary.http` - 403 + `retry-after: 60` + secondary-limit body
- `server_error.http` - 502 from a transient backend failure
- `malformed_body.http` - 200 + truncated JSON (drives the parser-failure path)
- `issues_empty.http` - 200 + `[]` (genuinely zero open issues)

Real-server captures live in `../rest/`, captured by `../capture.sh`.
