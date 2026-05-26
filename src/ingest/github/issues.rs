//! Open-issue ingest. Source 3 of #155. The wire layer lives in
//! [`super::client::GithubClient::fetch_open_issues`]; this module

use chrono::DateTime;

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

fn parse_ts(v: &serde_json::Value, key: &str) -> i64 {
    v.get(key)
        .and_then(|x| x.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp())
        .unwrap_or(0)
}
