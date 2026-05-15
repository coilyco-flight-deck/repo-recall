//! Categorized failure states for GitHub `gh` subprocess calls.
//!
//! Until #168 every fetcher returned `Option<Vec<T>>`, collapsing four
//! distinct failure modes (genuinely-empty / 404 / 401-403-auth /
//! rate-limited / other-error) into a single silent blank. The
//! dashboard had no way to render "rate limited, paused" vs "unauth"
//! vs "no data," and #167's backoff loop had no way to know whether a
//! failure was transient and worth backing off from.
//!
//! Classification reads `gh`'s exit status + stderr. The strings here
//! match the body that GitHub returns through `gh api`'s error
//! formatter (HTTP status + JSON `message` field passthrough). They are
//! checked from most-specific to least-specific so a single match wins.

use std::process::Output;

/// Categorized result of one `gh` subprocess invocation. Generic over
/// the success payload so a fetcher can return `RemoteFetchState<Vec<T>>`
/// without losing the typed data.
#[derive(Debug, Clone)]
pub enum RemoteFetchState<T> {
    /// Call succeeded and the payload parsed.
    Ok(T),
    /// 404. The endpoint or repo does not exist (or we cannot see it).
    Missing,
    /// 401 or 403 without a rate-limit header. `gh` not authenticated,
    /// token lacks scope, or the repo is private and the token can't
    /// see it.
    Unauthorized,
    /// 403 with `x-ratelimit-remaining: 0` or a "secondary rate limit"
    /// hit. `retry_after_secs` is parsed from the `Retry-After` header
    /// when present; `None` means "we know we're throttled but `gh`
    /// didn't surface a deadline."
    RateLimited { retry_after_secs: Option<u64> },
    /// Any other failure: subprocess spawn error, non-403/404 HTTP
    /// status, JSON parse failure. The string is the trimmed stderr (or
    /// a synthetic description) for one debug log line.
    Error(String),
}

impl<T> RemoteFetchState<T> {
    /// Discard the payload while keeping the categorization. Useful for
    /// hoisting state up to a per-pass aggregator without dragging the
    /// typed data along.
    pub fn discard_payload(self) -> RemoteFetchState<()> {
        match self {
            RemoteFetchState::Ok(_) => RemoteFetchState::Ok(()),
            RemoteFetchState::Missing => RemoteFetchState::Missing,
            RemoteFetchState::Unauthorized => RemoteFetchState::Unauthorized,
            RemoteFetchState::RateLimited { retry_after_secs } => {
                RemoteFetchState::RateLimited { retry_after_secs }
            }
            RemoteFetchState::Error(s) => RemoteFetchState::Error(s),
        }
    }

    /// Convenience for the existing call sites that still want the old
    /// `Option<T>` shape. Drops the categorization.
    pub fn into_option(self) -> Option<T> {
        match self {
            RemoteFetchState::Ok(v) => Some(v),
            _ => None,
        }
    }

    /// True for `RateLimited`. #167 uses this to drive the per-pass
    /// short-circuit and the next-pass backoff.
    pub fn is_rate_limited(&self) -> bool {
        matches!(self, RemoteFetchState::RateLimited { .. })
    }
}

/// Classify a failed `gh` `Output`. Caller has already checked that
/// `output.status.success()` is false.
pub fn classify_gh_failure(output: &Output) -> RemoteFetchState<()> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    classify_gh_stderr(&stderr)
}

/// String-only entry point for testing. Inspects stderr for the
/// signatures GitHub's REST error formatter and `gh`'s wrapper produce.
pub fn classify_gh_stderr(stderr: &str) -> RemoteFetchState<()> {
    let lower = stderr.to_lowercase();

    // Rate-limit takes priority over "403" because both can appear in
    // the same stderr blob.
    if lower.contains("api rate limit exceeded")
        || lower.contains("secondary rate limit")
        || lower.contains("you have exceeded a secondary rate limit")
    {
        return RemoteFetchState::RateLimited {
            retry_after_secs: parse_retry_after(stderr),
        };
    }

    // `gh` formats HTTP errors as `(HTTP NNN)` at the end of the line.
    if stderr.contains("(HTTP 404)") || lower.contains("not found") {
        return RemoteFetchState::Missing;
    }
    if stderr.contains("(HTTP 401)")
        || stderr.contains("(HTTP 403)")
        || lower.contains("bad credentials")
        || lower.contains("requires authentication")
    {
        return RemoteFetchState::Unauthorized;
    }

    RemoteFetchState::Error(stderr.trim().to_string())
}

/// Pull a `Retry-After: <secs>` value out of a stderr blob. Returns
/// `None` when the header isn't there or the value isn't a positive
/// integer.
fn parse_retry_after(stderr: &str) -> Option<u64> {
    for line in stderr.lines() {
        let trimmed = line.trim();
        let lower = trimmed.to_lowercase();
        if let Some(rest) = lower.strip_prefix("retry-after:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_rate_limit_classifies() {
        let s = "API rate limit exceeded for user ID 5768468. (HTTP 403)";
        assert!(matches!(
            classify_gh_stderr(s),
            RemoteFetchState::RateLimited { .. }
        ));
    }

    #[test]
    fn secondary_rate_limit_classifies() {
        let s = "You have exceeded a secondary rate limit. (HTTP 403)";
        assert!(matches!(
            classify_gh_stderr(s),
            RemoteFetchState::RateLimited { .. }
        ));
    }

    #[test]
    fn retry_after_is_parsed_when_present() {
        let s = "Retry-After: 42\nAPI rate limit exceeded.";
        match classify_gh_stderr(s) {
            RemoteFetchState::RateLimited { retry_after_secs } => {
                assert_eq!(retry_after_secs, Some(42));
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn retry_after_absent_when_not_in_stderr() {
        let s = "API rate limit exceeded for user ID 5768468.";
        match classify_gh_stderr(s) {
            RemoteFetchState::RateLimited { retry_after_secs } => {
                assert_eq!(retry_after_secs, None);
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn http_404_classifies_as_missing() {
        let s = "Not Found (HTTP 404)";
        assert!(matches!(classify_gh_stderr(s), RemoteFetchState::Missing));
    }

    #[test]
    fn http_401_classifies_as_unauthorized() {
        let s = "Bad credentials (HTTP 401)";
        assert!(matches!(
            classify_gh_stderr(s),
            RemoteFetchState::Unauthorized
        ));
    }

    #[test]
    fn http_403_without_rate_limit_classifies_as_unauthorized() {
        let s = "Resource not accessible by integration (HTTP 403)";
        assert!(matches!(
            classify_gh_stderr(s),
            RemoteFetchState::Unauthorized
        ));
    }

    #[test]
    fn other_failure_lands_in_error() {
        let s = "gh: connection refused";
        match classify_gh_stderr(s) {
            RemoteFetchState::Error(msg) => assert!(msg.contains("connection refused")),
            other => panic!("expected Error, got {other:?}"),
        }
    }
}
