//! JSON response helper with ETag short-circuit.
//!
//! Every JSON response carries `ETag: "<scan_version>"` keyed on the
//! monotonic `AppState::scan_version` counter, bumped at the end of every
//! successful refresh. A polling consumer that passes `If-None-Match` gets
//! `304 Not Modified` between scans.

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Build a JSON response carrying the given body, with `ETag` and
/// `Cache-Control` set so a poller can short-circuit unchanged scans.
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
    // Loopback-only service, no auth. `private` is the most accurate hint
    // even though no shared cache will ever see this. `must-revalidate`
    // makes well-behaved clients re-check rather than reuse blindly.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, must-revalidate"),
    );
}
