//! Open-issue ingest. Source 3 of #155. The wire layer lives in
//! [`super::client::GithubClient::fetch_open_issues`]; this module
//! owns the typed input shape and the pure JSON-to-record parser
//! that both the octocrab and fixtures impls call into.
//!
//! `pull_request`-tagged rows from the REST `/issues` endpoint are
//! filtered out (the endpoint mixes PRs in with issues - PRs go
//! through `pulls.rs`).

use chrono::DateTime;

use super::fetch_state::RemoteFetchState;
use super::pulls::cap_body;

#[derive(Debug, Clone, Default)]
pub struct IssueRecordInput {
    pub number: i64,
    pub title: String,
    pub html_url: String,
    pub body: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub closed_at: Option<i64>,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub author_login: String,
    pub milestone: Option<String>,
    pub comments_count: i64,
    pub state_reason: Option<String>,
    pub locked: bool,
    /// Raw reactions block, stored as JSON.
    pub reactions_json: String,
}

/// Pure parser. Takes the GitHub REST `GET /repos/X/issues` response
/// body (a JSON array) and returns the typed records. Rows tagged
/// `pull_request` are skipped. Both the octocrab path and the fixtures
/// path call this so the parsing rules live in one place.
pub fn parse_issues_json(value: &serde_json::Value) -> Vec<IssueRecordInput> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for issue in arr {
        if issue.get("pull_request").is_some() {
            continue;
        }
        let number = issue.get("number").and_then(|v| v.as_i64()).unwrap_or(0);
        if number == 0 {
            continue;
        }
        let reactions_json = issue
            .get("reactions")
            .map(|r| serde_json::to_string(r).unwrap_or_else(|_| "{}".into()))
            .unwrap_or_else(|| "{}".into());
        out.push(IssueRecordInput {
            number,
            title: issue
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            html_url: issue
                .get("html_url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            body: cap_body(issue.get("body").and_then(|v| v.as_str()).unwrap_or("")),
            created_at: parse_ts(issue, "created_at"),
            updated_at: parse_ts(issue, "updated_at"),
            closed_at: issue
                .get("closed_at")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.timestamp()),
            labels: issue
                .get("labels")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|l| l.get("name").and_then(|n| n.as_str()))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            assignees: issue
                .get("assignees")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|a| a.get("login").and_then(|l| l.as_str()))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            author_login: issue
                .get("user")
                .and_then(|u| u.get("login"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            milestone: issue
                .get("milestone")
                .and_then(|m| m.get("title"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
            comments_count: issue.get("comments").and_then(|v| v.as_i64()).unwrap_or(0),
            state_reason: issue
                .get("state_reason")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            locked: issue
                .get("locked")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            reactions_json,
        });
    }
    out
}

/// Shared helper: log a categorized failure at the appropriate level.
/// `RateLimited` is loud (warn) so the user sees the rate-limit
/// situation immediately rather than wondering why the dashboard went
/// blank. Other categories stay at debug since they are routine
/// (404s for transferred repos, auth misses for archived orgs).
pub(crate) fn log_categorized_failure(
    call: &str,
    owner_repo: &str,
    state: &RemoteFetchState<()>,
    detail: &str,
) {
    match state {
        RemoteFetchState::RateLimited { retry_after_secs } => {
            let retry = match retry_after_secs {
                Some(s) => format!(", retry-after {s}s"),
                None => String::new(),
            };
            tracing::warn!(
                "{call} rate-limited for {owner_repo}{retry}: {}",
                detail.trim()
            );
        }
        RemoteFetchState::Missing => {
            tracing::debug!("{call} 404 for {owner_repo}");
        }
        RemoteFetchState::Unauthorized => {
            tracing::debug!("{call} unauthorized for {owner_repo}");
        }
        RemoteFetchState::Unconfigured => {
            tracing::debug!("{call} skipped for {owner_repo}: GitHub client unconfigured");
        }
        RemoteFetchState::Error(_) | RemoteFetchState::Ok(()) => {
            tracing::debug!("{call} failed for {owner_repo}: {}", detail.trim());
        }
    }
}

fn parse_ts(v: &serde_json::Value, key: &str) -> i64 {
    v.get(key)
        .and_then(|x| x.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp())
        .unwrap_or(0)
}
