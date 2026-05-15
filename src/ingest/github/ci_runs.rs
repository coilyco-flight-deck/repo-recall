//! CI run ingest from `gh run list --json …`. Source 4 of #155. We pull
//! the rich field set including `jobs[]`, but persist only deduplicated
//! job names — jobs[].steps / status / conclusion are bulky and can be
//! surfaced on demand later.

use std::collections::BTreeSet;
use std::process::Command;

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

/// Pull recent runs for one repo. `limit` becomes `gh run list -L`. Runs
/// are returned newest-first as `gh` emits them.
pub fn fetch_recent_runs(
    owner_repo: &str,
    limit: usize,
) -> super::fetch_state::RemoteFetchState<Vec<CiRunRecordInput>> {
    use super::fetch_state::{classify_gh_failure, RemoteFetchState};
    use super::issues::log_categorized_failure;
    // Single `gh run list` covers the headline run fields. Jobs come from a
    // per-run `gh run view <id> --json jobs`. We could batch jobs into a
    // single `gh api` call too, but `gh run list --json jobs` is not
    // supported. Per-run is fine at the cap we use (small N).
    let fields = "databaseId,name,displayTitle,headSha,headBranch,number,event,\
                  createdAt,updatedAt,startedAt,url,attempt,status,conclusion";
    let Ok(output) = Command::new("gh")
        .args([
            "run",
            "list",
            "-R",
            owner_repo,
            "-L",
            &limit.to_string(),
            "--json",
            fields,
        ])
        .output()
    else {
        return RemoteFetchState::Error("failed to spawn gh".into());
    };
    if !output.status.success() {
        let state = classify_gh_failure(&output);
        log_categorized_failure(
            "gh run list",
            owner_repo,
            &state,
            &String::from_utf8_lossy(&output.stderr),
        );
        return match state {
            RemoteFetchState::Missing => RemoteFetchState::Missing,
            RemoteFetchState::Unauthorized => RemoteFetchState::Unauthorized,
            RemoteFetchState::RateLimited { retry_after_secs } => {
                RemoteFetchState::RateLimited { retry_after_secs }
            }
            RemoteFetchState::Unconfigured => RemoteFetchState::Unconfigured,
            RemoteFetchState::Error(s) => RemoteFetchState::Error(s),
            RemoteFetchState::Ok(()) => {
                RemoteFetchState::Error("classifier returned Ok on failure".into())
            }
        };
    }
    let Ok(value): serde_json::Result<serde_json::Value> = serde_json::from_slice(&output.stdout)
    else {
        return RemoteFetchState::Error("ci_runs: invalid JSON".into());
    };
    let Some(arr) = value.as_array() else {
        return RemoteFetchState::Error("ci_runs: expected JSON array".into());
    };
    let mut out = Vec::with_capacity(arr.len());
    for run in arr {
        let run_id = run.get("databaseId").and_then(|v| v.as_i64()).unwrap_or(0);
        if run_id == 0 {
            continue;
        }
        let jobs = fetch_job_names(owner_repo, run_id);
        out.push(CiRunRecordInput {
            run_id,
            name: pull_str(run, "name"),
            display_title: pull_str(run, "displayTitle"),
            head_sha: pull_str(run, "headSha"),
            head_branch: pull_str(run, "headBranch"),
            run_number: run.get("number").and_then(|v| v.as_i64()).unwrap_or(0),
            event: pull_str(run, "event"),
            created_at: pull_ts(run, "createdAt"),
            updated_at: pull_ts(run, "updatedAt"),
            run_started_at: run
                .get("startedAt")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.timestamp()),
            html_url: pull_str(run, "url"),
            run_attempt: run.get("attempt").and_then(|v| v.as_i64()).unwrap_or(0),
            status: pull_str(run, "status"),
            conclusion: run
                .get("conclusion")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            jobs,
        });
    }
    RemoteFetchState::Ok(out)
}

/// `gh run view <id> --json jobs` -> deduped, sorted job names. Best
/// effort; an empty vec on any failure.
fn fetch_job_names(owner_repo: &str, run_id: i64) -> Vec<String> {
    let Ok(out) = Command::new("gh")
        .args([
            "run",
            "view",
            &run_id.to_string(),
            "-R",
            owner_repo,
            "--json",
            "jobs",
        ])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let Ok(v): serde_json::Result<serde_json::Value> = serde_json::from_slice(&out.stdout) else {
        return Vec::new();
    };
    let mut names: BTreeSet<String> = BTreeSet::new();
    if let Some(arr) = v.get("jobs").and_then(|j| j.as_array()) {
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
