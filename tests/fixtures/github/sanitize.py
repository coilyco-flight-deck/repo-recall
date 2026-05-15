#!/usr/bin/env python3
"""Sanitize a `gh api -i` response for use as a test fixture.

Reads stdin, writes stdout. Drops noisy / auth-revealing headers, trims
arrays in the JSON body to at most 2 elements, and replaces the Date
header with a stable placeholder so fixtures don't churn on every run.
"""

from __future__ import annotations

import json
import sys

DROP_HEADERS = {
    "x-github-request-id",
    "x-oauth-client-id",
    "x-oauth-scopes",
    "x-accepted-oauth-scopes",
    "x-github-sso",
    "etag",
    "content-security-policy",
    "strict-transport-security",
    "vary",
    "server",
    "referrer-policy",
    "access-control-allow-origin",
    "access-control-expose-headers",
    "x-content-type-options",
    "x-frame-options",
    "x-xss-protection",
    "set-cookie",
    "cache-control",
}


def split_response(raw: bytes) -> tuple[list[str], bytes]:
    sep = b"\r\n\r\n"
    if sep not in raw:
        sep = b"\n\n"
    head, _, body = raw.partition(sep)
    headers = head.decode("utf-8", errors="replace").splitlines()
    return headers, body


def sanitize_headers(lines: list[str]) -> list[str]:
    if not lines:
        return lines
    out = [lines[0]]
    for line in lines[1:]:
        if ":" not in line:
            out.append(line)
            continue
        name, _, _ = line.partition(":")
        key = name.strip().lower()
        if key in DROP_HEADERS:
            continue
        if key == "date":
            out.append("Date: Fri, 01 Jan 2027 00:00:00 GMT")
            continue
        out.append(line)
    return out


def trim_array(value, depth=0):
    if isinstance(value, list):
        return [trim_array(v, depth + 1) for v in value[:2]]
    if isinstance(value, dict):
        return {k: trim_array(v, depth + 1) for k, v in value.items()}
    return value


def sanitize_body(body: bytes) -> bytes:
    if not body.strip():
        return body
    try:
        parsed = json.loads(body)
    except json.JSONDecodeError:
        return body
    trimmed = trim_array(parsed)
    return json.dumps(trimmed, indent=2).encode("utf-8") + b"\n"


def main() -> None:
    raw = sys.stdin.buffer.read()
    headers, body = split_response(raw)
    headers = sanitize_headers(headers)
    body = sanitize_body(body)
    out = ("\r\n".join(headers) + "\r\n\r\n").encode("utf-8") + body
    sys.stdout.buffer.write(out)


if __name__ == "__main__":
    main()
