//! CI run ingest. Source 4 of #155. The wire layer lives in
//! [`super::client::GithubClient::fetch_recent_runs`]; this module
//! owns the typed input shape and the pure JSON-to-record parser
//! that both the octocrab and fixtures impls call into.
//!
//! Field naming follows GitHub REST snake_case (`id`, `display_title`,
//! `head_sha`, ...) rather than the camelCase that `gh run list --json`
//! emitted, because the rewrite (#173) sources directly from REST.

use chrono::DateTime;

#[derive(Debug, Clone, Default)]
pub struct CiRunRecordInput {
    pub run_id: i64,
    pub name: String,
    pub display_title: String,
    pub head_sha: String,
    pub head_branch: String,
    pub run_number: i64,
    pub event: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub run_started_at: Option<i64>,
    pub html_url: String,
    pub run_attempt: i64,
    pub status: String,
    pub conclusion: Option<String>,
    /// Sorted, deduplicated list of job names from `jobs[].name`.
    pub jobs: Vec<String>,
}

/// Pure parser. Takes the GitHub REST `GET /repos/X/actions/runs`
/// response body (an object with `workflow_runs` array) and returns
/// the typed records. `jobs` is left empty - the caller fills it
/// per-run via a separate endpoint.
pub fn parse_runs_json(value: &serde_json::Value) -> Vec<CiRunRecordInput> {
    let arr = match value.get("workflow_runs").and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    let mut out = Vec::with_capacity(arr.len());
    for run in arr {
        let run_id = run.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        if run_id == 0 {
            continue;
        }
        out.push(CiRunRecordInput {
            run_id,
            name: pull_str(run, "name"),
            display_title: pull_str(run, "display_title"),
            head_sha: pull_str(run, "head_sha"),
            head_branch: pull_str(run, "head_branch"),
            run_number: run.get("run_number").and_then(|v| v.as_i64()).unwrap_or(0),
            event: pull_str(run, "event"),
            created_at: pull_ts(run, "created_at"),
            updated_at: pull_ts(run, "updated_at"),
            run_started_at: run
                .get("run_started_at")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.timestamp()),
            html_url: pull_str(run, "html_url"),
            run_attempt: run.get("run_attempt").and_then(|v| v.as_i64()).unwrap_or(0),
            status: pull_str(run, "status"),
            conclusion: run
                .get("conclusion")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            jobs: Vec::new(),
        });
    }
    out
}

/// Pure parser. Takes the GitHub REST `GET /repos/X/actions/runs/<id>/jobs`
/// response body (an object with `jobs` array) and returns a
/// deduped, sorted list of job names.
pub fn parse_job_names_json(value: &serde_json::Value) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut names: BTreeSet<String> = BTreeSet::new();
    if let Some(arr) = value.get("jobs").and_then(|j| j.as_array()) {
        for j in arr {
            if let Some(n) = j.get("name").and_then(|x| x.as_str()) {
                names.insert(n.to_string());
            }
        }
    }
    names.into_iter().collect()
}

fn pull_str(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string()
}

fn pull_ts(v: &serde_json::Value, key: &str) -> i64 {
    v.get(key)
        .and_then(|x| x.as_str())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp())
        .unwrap_or(0)
}
