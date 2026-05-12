//! Content negotiation + ETag helpers shared across routes.
//!
//! The pitch in [issue #2](https://github.com/coilysiren/repo-recall/issues/2):
//! the same URL a browser hits should also serve JSON to an agent. We pick
//! the response shape from `Accept: application/json` or `?format=json`, and
//! tag every JSON response with `ETag: "<scan_version>"` so a polling
//! orchestrator (issue #3) gets `304 Not Modified` between scans.

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// True when the caller wants JSON. Either an explicit `?format=json` (wins,
/// because it's deliberate) or an `Accept` header that prefers
/// `application/json` over `text/html`. We do *not* default to JSON for
/// `Accept: */*` — browsers send that, agents tend to be specific.
pub fn wants_json(headers: &HeaderMap, format_param: Option<&str>) -> bool {
    if matches!(format_param, Some("json")) {
        return true;
    }
    let Some(accept) = headers.get(header::ACCEPT) else {
        return false;
    };
    let Ok(s) = accept.to_str() else {
        return false;
    };
    // Cheap parse: tokenize on `,`, look for `application/json`. Don't
    // bother with q-values — agents that want JSON say so directly.
    s.split(',').any(|tok| {
        let t = tok.split(';').next().unwrap_or("").trim();
        t.eq_ignore_ascii_case("application/json")
    })
}

/// Build a JSON response carrying the given body, with `ETag` and
/// `Cache-Control` set so a poller can short-circuit unchanged scans. The
/// version is the monotonic `AppState::scan_version` counter — bumped at the
/// end of every successful refresh — so the tag changes exactly when the
/// underlying data does.
pub fn json_with_etag<T: Serialize>(headers: &HeaderMap, version: u64, body: &T) -> Response {
    let etag = format!("\"{version}\"");
    if let Some(if_match) = headers.get(header::IF_NONE_MATCH) {
        if if_match.to_str().is_ok_and(|s| s == etag) {
            let mut res = StatusCode::NOT_MODIFIED.into_response();
            insert_cache_headers(res.headers_mut(), &etag);
            return res;
        }
    }
    let payload = serde_json::to_vec(body).unwrap_or_else(|_| b"null".to_vec());
    let mut res = (StatusCode::OK, payload).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    insert_cache_headers(h, &etag);
    res
}

fn insert_cache_headers(headers: &mut HeaderMap, etag: &str) {
    if let Ok(v) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, v);
    }
    // Loopback-only service, no auth — `private` is the most accurate hint
    // even though no shared cache will ever see this. `must-revalidate`
    // makes well-behaved clients re-check rather than reuse blindly.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, must-revalidate"),
    );
}
