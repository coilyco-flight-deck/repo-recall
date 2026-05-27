//! GitHub client abstraction for the octocrab rewrite (#173).
//!

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use octocrab::Octocrab;

use super::fetch_state::RemoteFetchState;
use super::issues::{parse_issues_json, IssueRecordInput};
use super::milestones::{parse_milestones_json, MilestoneInput};
use super::pulls::{parse_prs_json, PrRecordInput};
use crate::ingest::git::log::{ActiveRepo, DeployHealth};

/// Authenticated user as exposed to repo-recall. Trimmed shape: only
/// the fields the dashboard / refresh path actually reads. Octocrab's
#[derive(Debug, Clone)]
pub struct AuthedUser {
    pub login: String,
}

/// The single seam between repo-recall and GitHub. Implementors return
/// [`RemoteFetchState`]-wrapped payloads so the existing classifier in
#[async_trait]
pub trait GithubClient: Send + Sync {
    /// `GET /user` - viewer's GitHub identity. Single source of truth
    /// for the authenticated login + the dashboard's `gh_health` banner.
    async fn fetch_user(&self) -> RemoteFetchState<AuthedUser>;

    /// `GET /repos/{owner}/{repo}/issues?state=open&per_page=100` -
    /// open issues for one repo. PR-tagged rows are filtered by the
    async fn fetch_open_issues(&self, owner_repo: &str) -> RemoteFetchState<Vec<IssueRecordInput>>;

    /// `GET /repos/{owner}/{repo}/pulls?state=open&per_page=100` -
    /// open pull requests for one repo. Replaces
    async fn fetch_open_prs(&self, owner_repo: &str) -> RemoteFetchState<Vec<PrRecordInput>>;

    /// `GET /repos/{owner}/{repo}/milestones?state=open&per_page=100` (#88).
    async fn fetch_open_milestones(
        &self,
        owner_repo: &str,
    ) -> RemoteFetchState<Vec<MilestoneInput>>;

    /// `GET /repos/{owner}/{repo}/actions/workflows/{wf}/runs?branch=B&per_page=30`
    /// plus a `last_success_ts` derived from the same response.
    async fn fetch_deploy_health(
        &self,
        owner_repo: &str,
        workflow: &str,
        branch: &str,
    ) -> RemoteFetchState<DeployHealth>;

    /// `GET /user/repos?sort=pushed&type=owner&per_page=N` - viewer's
    /// recently-pushed repos. Replaces
    async fn fetch_active_repos(&self, limit: usize) -> RemoteFetchState<Vec<ActiveRepo>>;
}

/// Build the right client for the current process based on env state:
///
pub fn build_client() -> Arc<dyn GithubClient> {
    if let Ok(dir) = std::env::var("REPO_RECALL_GITHUB_FIXTURES_DIR") {
        if !dir.is_empty() {
            let path = PathBuf::from(&dir);
            tracing::warn!(
                "FIXTURE MODE active, dir={}, no real GitHub calls will be made",
                path.display()
            );
            return Arc::new(FixturesClient { dir: path });
        }
    }
    Arc::new(OctocrabClient::from_gh_auth_token())
}

/// Production GitHub client. Wraps an [`Octocrab`] built from the
/// user's `gh auth token`. Empty token → `unconfigured = true` and
pub struct OctocrabClient {
    inner: Octocrab,
    unconfigured: bool,
}

impl OctocrabClient {
    /// Read `gh auth token` (one subprocess at startup), build the
    /// inner client. Logs a `WARN` if the token is missing so the
    pub fn from_gh_auth_token() -> Self {
        let token = read_gh_auth_token();
        match token {
            Some(t) if !t.is_empty() => {
                let inner = Octocrab::builder()
                    .personal_token(t)
                    .build()
                    .unwrap_or_else(|e| {
                        tracing::warn!("octocrab build with token failed ({e}); falling back to anonymous client");
                        Octocrab::builder().build().expect("anonymous octocrab build")
                    });
                Self {
                    inner,
                    unconfigured: false,
                }
            }
            _ => {
                tracing::warn!(
                    "GitHub: no `gh auth token` available; remote columns will render as `not configured`. \
                     Run `gh auth login` to enable."
                );
                let inner = Octocrab::builder()
                    .build()
                    .expect("anonymous octocrab build");
                Self {
                    inner,
                    unconfigured: true,
                }
            }
        }
    }
}

#[async_trait]
impl GithubClient for OctocrabClient {
    async fn fetch_user(&self) -> RemoteFetchState<AuthedUser> {
        if self.unconfigured {
            return RemoteFetchState::Unconfigured;
        }
        match self.inner.current().user().await {
            Ok(u) => RemoteFetchState::Ok(AuthedUser { login: u.login }),
            Err(e) => super::fetch_state::classify_octocrab_error(&e),
        }
    }

    async fn fetch_open_issues(&self, owner_repo: &str) -> RemoteFetchState<Vec<IssueRecordInput>> {
        if self.unconfigured {
            return RemoteFetchState::Unconfigured;
        }
        // Raw-JSON path keeps parsing identical to the gh-subprocess
        // shape and lets the shared `parse_issues_json` own the field
        let path = format!("/repos/{owner_repo}/issues?state=open&per_page=100");
        let value: serde_json::Value = match self.inner.get(&path, None::<&()>).await {
            Ok(v) => v,
            Err(e) => return super::fetch_state::classify_octocrab_error(&e),
        };
        RemoteFetchState::Ok(parse_issues_json(&value))
    }

    async fn fetch_open_prs(&self, owner_repo: &str) -> RemoteFetchState<Vec<PrRecordInput>> {
        if self.unconfigured {
            return RemoteFetchState::Unconfigured;
        }
        let path = format!("/repos/{owner_repo}/pulls?state=open&per_page=100");
        let value: serde_json::Value = match self.inner.get(&path, None::<&()>).await {
            Ok(v) => v,
            Err(e) => return super::fetch_state::classify_octocrab_error(&e),
        };
        RemoteFetchState::Ok(parse_prs_json(&value))
    }

    async fn fetch_open_milestones(
        &self,
        owner_repo: &str,
    ) -> RemoteFetchState<Vec<MilestoneInput>> {
        if self.unconfigured {
            return RemoteFetchState::Unconfigured;
        }
        let path = format!("/repos/{owner_repo}/milestones?state=open&per_page=100");
        let value: serde_json::Value = match self.inner.get(&path, None::<&()>).await {
            Ok(v) => v,
            Err(e) => return super::fetch_state::classify_octocrab_error(&e),
        };
        RemoteFetchState::Ok(parse_milestones_json(&value))
    }

    async fn fetch_deploy_health(
        &self,
        owner_repo: &str,
        workflow: &str,
        branch: &str,
    ) -> RemoteFetchState<DeployHealth> {
        if self.unconfigured {
            return RemoteFetchState::Unconfigured;
        }
        let path = format!(
            "/repos/{owner_repo}/actions/workflows/{workflow}/runs?branch={branch}&per_page=30"
        );
        let value: serde_json::Value = match self.inner.get(&path, None::<&()>).await {
            Ok(v) => v,
            Err(e) => return super::fetch_state::classify_octocrab_error(&e),
        };
        RemoteFetchState::Ok(parse_deploy_health_json(&value))
    }

    async fn fetch_active_repos(&self, limit: usize) -> RemoteFetchState<Vec<ActiveRepo>> {
        if self.unconfigured {
            return RemoteFetchState::Unconfigured;
        }
        let path = format!("/user/repos?sort=pushed&type=owner&per_page={limit}");
        let value: serde_json::Value = match self.inner.get(&path, None::<&()>).await {
            Ok(v) => v,
            Err(e) => return super::fetch_state::classify_octocrab_error(&e),
        };
        RemoteFetchState::Ok(parse_active_repos_json(&value))
    }
}

/// Normalize a single REST workflow-run object's `(status, conclusion)`
/// into the small status vocabulary the dashboard renders. Used by
fn normalize_run_status(status: &str, conclusion: &str) -> Option<&'static str> {
    match (status, conclusion) {
        ("completed", "success") => Some("success"),
        ("completed", "failure" | "startup_failure" | "timed_out") => Some("failure"),
        ("completed", _) => Some("success"), // cancelled / skipped / neutral: not urgent
        ("in_progress", _) => Some("running"),
        ("queued" | "pending" | "requested" | "waiting", _) => Some("pending"),
        _ => None,
    }
}

/// Pure parser. Builds a `DeployHealth` from the REST workflow-runs
/// response (latest run's status + most-recent successful run's
fn parse_deploy_health_json(value: &serde_json::Value) -> DeployHealth {
    let Some(arr) = value.get("workflow_runs").and_then(|v| v.as_array()) else {
        return DeployHealth::default();
    };
    if arr.is_empty() {
        return DeployHealth::default();
    }
    let first = &arr[0];
    let first_status = first.get("status").and_then(|v| v.as_str()).unwrap_or("");
    let first_conclusion = first
        .get("conclusion")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let status = normalize_run_status(first_status, first_conclusion).map(str::to_string);
    let last_success_ts = arr.iter().find_map(|r| {
        let conclusion = r.get("conclusion").and_then(|v| v.as_str()).unwrap_or("");
        if conclusion != "success" {
            return None;
        }
        let created = r.get("created_at").and_then(|v| v.as_str())?;
        chrono::DateTime::parse_from_rfc3339(created)
            .ok()
            .map(|dt| dt.timestamp())
    });
    DeployHealth {
        status,
        last_success_ts,
    }
}

/// Pure parser. Reads `/user/repos` and maps to `ActiveRepo`. REST
/// field names: `full_name`, `clone_url`, `ssh_url`, `default_branch`,
fn parse_active_repos_json(value: &serde_json::Value) -> Vec<ActiveRepo> {
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|r| {
            let full_name = r.get("full_name").and_then(|v| v.as_str())?.to_string();
            let https_url = r
                .get("clone_url")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let ssh_url = r
                .get("ssh_url")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let default_branch = r
                .get("default_branch")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let pushed_at = r
                .get("pushed_at")
                .and_then(|v| v.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.timestamp());
            let description = r
                .get("description")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let is_fork = r.get("fork").and_then(|v| v.as_bool()).unwrap_or(false);
            let is_archived = r.get("archived").and_then(|v| v.as_bool()).unwrap_or(false);
            Some(ActiveRepo {
                full_name,
                https_url,
                ssh_url,
                default_branch,
                pushed_at,
                description,
                is_fork,
                is_archived,
            })
        })
        .collect()
}

/// Replays `.http` fixture files from a directory. Each trait method
/// reads a known filename (e.g. `fetch_user` → `user.http`), parses
pub struct FixturesClient {
    dir: PathBuf,
}

impl FixturesClient {
    /// Construct a client over `dir`. The dir is not validated at
    /// construction time; missing files surface as
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn read_fixture(&self, filename: &str) -> Result<ParsedHttp, String> {
        let path = self.dir.join(filename);
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("fixture read {}: {e}", path.display()))?;
        ParsedHttp::parse(&raw).map_err(|e| format!("fixture parse {}: {e}", path.display()))
    }
}

#[async_trait]
impl GithubClient for FixturesClient {
    async fn fetch_user(&self) -> RemoteFetchState<AuthedUser> {
        let parsed = match self.read_fixture("user.http") {
            Ok(p) => p,
            Err(e) => return RemoteFetchState::Error(e),
        };
        if let Some(state) =
            super::fetch_state::classify_http_status(parsed.status, &parsed.headers)
        {
            return state;
        }
        let value: serde_json::Value = match serde_json::from_str(&parsed.body) {
            Ok(v) => v,
            Err(e) => return RemoteFetchState::Error(format!("user.http body: {e}")),
        };
        let Some(login) = value.get("login").and_then(|v| v.as_str()) else {
            return RemoteFetchState::Error("user.http: missing `login`".into());
        };
        RemoteFetchState::Ok(AuthedUser {
            login: login.to_string(),
        })
    }

    async fn fetch_open_issues(
        &self,
        _owner_repo: &str,
    ) -> RemoteFetchState<Vec<IssueRecordInput>> {
        let parsed = match self.read_fixture("issues_open.http") {
            Ok(p) => p,
            Err(e) => return RemoteFetchState::Error(e),
        };
        if let Some(state) =
            super::fetch_state::classify_http_status(parsed.status, &parsed.headers)
        {
            return state;
        }
        let value: serde_json::Value = match serde_json::from_str(&parsed.body) {
            Ok(v) => v,
            Err(e) => return RemoteFetchState::Error(format!("issues_open.http body: {e}")),
        };
        RemoteFetchState::Ok(parse_issues_json(&value))
    }

    async fn fetch_open_prs(&self, _owner_repo: &str) -> RemoteFetchState<Vec<PrRecordInput>> {
        let parsed = match self.read_fixture("pulls_all.http") {
            Ok(p) => p,
            Err(e) => return RemoteFetchState::Error(e),
        };
        if let Some(state) =
            super::fetch_state::classify_http_status(parsed.status, &parsed.headers)
        {
            return state;
        }
        let value: serde_json::Value = match serde_json::from_str(&parsed.body) {
            Ok(v) => v,
            Err(e) => return RemoteFetchState::Error(format!("pulls_all.http body: {e}")),
        };
        RemoteFetchState::Ok(parse_prs_json(&value))
    }

    async fn fetch_open_milestones(
        &self,
        _owner_repo: &str,
    ) -> RemoteFetchState<Vec<MilestoneInput>> {
        let parsed = match self.read_fixture("milestones_open.http") {
            Ok(p) => p,
            Err(e) => return RemoteFetchState::Error(e),
        };
        if let Some(state) =
            super::fetch_state::classify_http_status(parsed.status, &parsed.headers)
        {
            return state;
        }
        let value: serde_json::Value = match serde_json::from_str(&parsed.body) {
            Ok(v) => v,
            Err(e) => return RemoteFetchState::Error(format!("milestones_open.http body: {e}")),
        };
        RemoteFetchState::Ok(parse_milestones_json(&value))
    }

    async fn fetch_deploy_health(
        &self,
        _owner_repo: &str,
        _workflow: &str,
        _branch: &str,
    ) -> RemoteFetchState<DeployHealth> {
        // No captured-real-server fixture for the workflow-runs
        // endpoint today; the branch-scoped runs fixture has the
        let parsed = match self.read_fixture("actions_runs_branch.http") {
            Ok(p) => p,
            Err(e) => return RemoteFetchState::Error(e),
        };
        if let Some(state) =
            super::fetch_state::classify_http_status(parsed.status, &parsed.headers)
        {
            return state;
        }
        let value: serde_json::Value = match serde_json::from_str(&parsed.body) {
            Ok(v) => v,
            Err(e) => {
                return RemoteFetchState::Error(format!("actions_runs_branch.http body: {e}"))
            }
        };
        RemoteFetchState::Ok(parse_deploy_health_json(&value))
    }

    async fn fetch_active_repos(&self, _limit: usize) -> RemoteFetchState<Vec<ActiveRepo>> {
        let parsed = match self.read_fixture("user_repos.http") {
            Ok(p) => p,
            Err(e) => return RemoteFetchState::Error(e),
        };
        if let Some(state) =
            super::fetch_state::classify_http_status(parsed.status, &parsed.headers)
        {
            return state;
        }
        let value: serde_json::Value = match serde_json::from_str(&parsed.body) {
            Ok(v) => v,
            Err(e) => return RemoteFetchState::Error(format!("user_repos.http body: {e}")),
        };
        RemoteFetchState::Ok(parse_active_repos_json(&value))
    }
}

/// Read the user's `gh auth token`. Returns `None` if `gh` is missing,
/// not authenticated, or returns an empty token. One subprocess at
fn read_gh_auth_token() -> Option<String> {
    let output = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Lightweight HTTP-response parser for fixture files. Only what we
/// need for replay: status code, headers, body. Tolerates `\r\n` or
struct ParsedHttp {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl ParsedHttp {
    fn parse(raw: &str) -> Result<Self, String> {
        let normalized = raw.replace("\r\n", "\n");
        let (head, body) = normalized
            .split_once("\n\n")
            .ok_or("missing header/body separator")?;
        let mut lines = head.lines();
        let status_line = lines.next().ok_or("empty fixture")?;
        let status = status_line
            .split_whitespace()
            .nth(1)
            .ok_or("malformed status line")?
            .parse::<u16>()
            .map_err(|e| format!("status code: {e}"))?;
        let headers = lines
            .filter_map(|l| {
                let (k, v) = l.split_once(':')?;
                Some((k.trim().to_ascii_lowercase(), v.trim().to_string()))
            })
            .collect();
        Ok(Self {
            status,
            headers,
            body: body.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fixtures_client_reads_user_login() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/github/rest");
        let client = FixturesClient::new(dir);
        let state = client.fetch_user().await;
        match state {
            RemoteFetchState::Ok(u) => assert_eq!(u.login, "coilysiren"),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn fixtures_client_classifies_unauthorized_when_pointed_at_errors_dir() {
        // Symlink-equivalent: same client, different fixture file.
        // We rename inside the test's view by reading from errors/ directly.
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/github/errors");
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::copy(dir.join("unauthorized.http"), temp.path().join("user.http"))
            .expect("copy fixture");
        let client = FixturesClient::new(temp.path());
        let state = client.fetch_user().await;
        assert!(
            matches!(state, RemoteFetchState::Unauthorized),
            "expected Unauthorized, got {state:?}"
        );
    }

    #[test]
    fn parsed_http_handles_crlf() {
        let raw = "HTTP/2.0 200 OK\r\nContent-Type: application/json\r\n\r\n{\"x\":1}\n";
        let parsed = ParsedHttp::parse(raw).expect("parse");
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.body.trim(), "{\"x\":1}");
        assert!(parsed
            .headers
            .iter()
            .any(|(k, v)| k == "content-type" && v == "application/json"));
    }
}
