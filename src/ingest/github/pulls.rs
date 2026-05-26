//! Open-PR ingest. Source 2 of #155. The wire layer lives in
//! [`super::client::GithubClient::fetch_open_prs`]; this module owns

use chrono::DateTime;

use crate::process::sanitize::{scrub, SanitizeSource};

/// Body cap for stored PR/issue bodies. Per #155, we cap at first ~500
/// chars after gitleaks scrub before persistence.
pub const BODY_CAP: usize = 500;

/// Input to `CacheWriter::upsert_pr_record`. Owned strings so the caller
/// can drop the parsed JSON before writing.
#[derive(Debug, Clone, Default)]
pub struct PrRecordInput {
    pub number: i64,
    pub title: String,
    pub html_url: String,
    pub body: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub head_ref: String,
    pub base_ref: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub milestone: Option<String>,
    pub comments_count: i64,
    pub review_comments_count: i64,
    pub additions: i64,
    pub deletions: i64,
    pub changed_files: i64,
    pub mergeable_state: Option<String>,
    pub requested_teams: Vec<String>,
}

/// Pure parser. Takes the GitHub REST `GET /repos/X/pulls` response
/// body (a JSON array) and returns the typed records. Both the
pub fn parse_prs_json(value: &serde_json::Value) -> Vec<PrRecordInput> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for pr in arr {
        let number = pr.get("number").and_then(|v| v.as_i64()).unwrap_or(0);
        if number == 0 {
            continue;
        }
        out.push(PrRecordInput {
            number,
            title: pull_str(pr, "title"),
            html_url: pull_str(pr, "html_url"),
            body: cap_body(&pull_str(pr, "body")),
            created_at: pull_ts(pr, "created_at"),
            updated_at: pull_ts(pr, "updated_at"),
            head_ref: pr
                .get("head")
                .and_then(|h| h.get("ref"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            base_ref: pr
                .get("base")
                .and_then(|b| b.get("ref"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            labels: pull_label_names(pr),
            assignees: pull_login_list(pr, "assignees"),
            milestone: pr
                .get("milestone")
                .and_then(|m| m.get("title"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
            comments_count: pull_i64(pr, "comments"),
            review_comments_count: pull_i64(pr, "review_comments"),
            additions: pull_i64(pr, "additions"),
            deletions: pull_i64(pr, "deletions"),
            changed_files: pull_i64(pr, "changed_files"),
            mergeable_state: pr
                .get("mergeable_state")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            requested_teams: pr
                .get("requested_teams")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|t| t.get("slug").and_then(|s| s.as_str()))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        });
    }
    out
}

fn pull_str(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn pull_i64(v: &serde_json::Value, key: &str) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(0)
}

fn pull_ts(v: &serde_json::Value, key: &str) -> i64 {
    v.get(key)
        .and_then(|x| x.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp())
        .unwrap_or(0)
}

fn pull_label_names(v: &serde_json::Value) -> Vec<String> {
    v.get("labels")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l.get("name").and_then(|n| n.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn pull_login_list(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| a.get("login").and_then(|l| l.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn cap_body(raw: &str) -> String {
    let scrubbed = scrub(raw, SanitizeSource::GithubIssueBody);
    if scrubbed.chars().count() <= BODY_CAP {
        return scrubbed;
    }
    let mut out: String = scrubbed.chars().take(BODY_CAP).collect();
    out.push('…');
    out
}
