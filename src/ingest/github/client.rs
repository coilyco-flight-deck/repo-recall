//! GitHub client abstraction for the octocrab rewrite (#173).
//!
//! Two implementations behind one trait:
//!
//! - [`OctocrabClient`] is the production path. It builds an
//!   `octocrab::Octocrab` from the user's `gh auth token` at startup;
//!   missing/empty token degrades to an anonymous client and every
//!   method short-circuits to [`RemoteFetchState::Unconfigured`] with a
//!   single startup `WARN` banner.
//! - [`FixturesClient`] reads `.http` files from
//!   `REPO_RECALL_GITHUB_FIXTURES_DIR` and replays them. Selected when
//!   the env var is set; emits a one-line `WARN` banner so leaving
//!   fixture mode on is loud, not silent. Drives both `make
//!   watch-fixtures` (for manual UI verification) and the unit tests
//!   (a test helper builds a `FixturesClient` against the same dir).
//!
//! Step 1 of #173 only adds the trait + the auth probe ([`fetch_user`]).
//! Subsequent steps grow the trait and migrate one ingest call site each.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use octocrab::Octocrab;

use super::ci_runs::{parse_job_names_json, parse_runs_json, CiRunRecordInput};
use super::fetch_state::RemoteFetchState;
use super::issues::{parse_issues_json, IssueRecordInput};
use super::pulls::{parse_prs_json, PrRecordInput};

/// Authenticated user as exposed to repo-recall. Trimmed shape: only
/// the fields the dashboard / refresh path actually reads. Octocrab's
/// `models::Author` has more, but tying the trait to octocrab's types
/// would force the fixtures path to depend on octocrab too.
#[derive(Debug, Clone)]
pub struct AuthedUser {
    pub login: String,
}

/// The single seam between repo-recall and GitHub. Implementors return
/// [`RemoteFetchState`]-wrapped payloads so the existing classifier in
/// [`super::fetch_state`] keeps owning categorization.
#[async_trait]
pub trait GithubClient: Send + Sync {
    /// `GET /user` - viewer's GitHub identity. Replaces
    /// `crate::ingest::git::log::my_gh_login`.
    async fn fetch_user(&self) -> RemoteFetchState<AuthedUser>;

    /// `GET /repos/{owner}/{repo}/issues?state=open&per_page=100` -
    /// open issues for one repo. PR-tagged rows are filtered by the
    /// shared parser. Replaces `crate::ingest::github::issues::fetch_open_issues`.
    async fn fetch_open_issues(&self, owner_repo: &str) -> RemoteFetchState<Vec<IssueRecordInput>>;

    /// `GET /repos/{owner}/{repo}/pulls?state=open&per_page=100` -
    /// open pull requests for one repo. Replaces
    /// `crate::ingest::github::pulls::fetch_open_prs`.
    async fn fetch_open_prs(&self, owner_repo: &str) -> RemoteFetchState<Vec<PrRecordInput>>;

    /// `GET /repos/{owner}/{repo}/actions/runs?per_page=N` plus a
    /// per-run `GET /repos/{owner}/{repo}/actions/runs/<id>/jobs` for
    /// deduped job names. Replaces the gh-subprocess `fetch_recent_runs`
    /// + `fetch_job_names` pair (also closes the AGENTS.md "no
    /// `gh run list`" violation in this module).
    async fn fetch_recent_runs(
        &self,
        owner_repo: &str,
        limit: usize,
    ) -> RemoteFetchState<Vec<CiRunRecordInput>>;
}

/// Build the right client for the current process based on env state:
///
/// - `REPO_RECALL_GITHUB_FIXTURES_DIR` set + readable → [`FixturesClient`].
/// - Otherwise → [`OctocrabClient`] sourced from `gh auth token`.
///
/// Emits the appropriate startup banner. Never fails; in the worst case
/// returns an `OctocrabClient` whose every call is `Unconfigured`.
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
/// every method returns [`RemoteFetchState::Unconfigured`].
pub struct OctocrabClient {
    inner: Octocrab,
    unconfigured: bool,
}

impl OctocrabClient {
    /// Read `gh auth token` (one subprocess at startup), build the
    /// inner client. Logs a `WARN` if the token is missing so the
    /// dashboard's "GitHub not configured" pill has a paired log line.
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
        // coercion. Per-page cap matches the prior subprocess call.
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

    async fn fetch_recent_runs(
        &self,
        owner_repo: &str,
        limit: usize,
    ) -> RemoteFetchState<Vec<CiRunRecordInput>> {
        if self.unconfigured {
            return RemoteFetchState::Unconfigured;
        }
        let path = format!("/repos/{owner_repo}/actions/runs?per_page={limit}");
        let value: serde_json::Value = match self.inner.get(&path, None::<&()>).await {
            Ok(v) => v,
            Err(e) => return super::fetch_state::classify_octocrab_error(&e),
        };
        let mut runs = parse_runs_json(&value);
        // Per-run job names. Sequential to keep the per-(repo, run)
        // budget tractable - jobs are bounded by `limit` (default 20).
        // A single per-run failure leaves jobs empty rather than
        // failing the whole list.
        for run in runs.iter_mut() {
            let jobs_path = format!("/repos/{owner_repo}/actions/runs/{}/jobs", run.run_id);
            if let Ok(v) = self
                .inner
                .get::<serde_json::Value, _, _>(&jobs_path, None::<&()>)
                .await
            {
                run.jobs = parse_job_names_json(&v);
            }
        }
        RemoteFetchState::Ok(runs)
    }
}

/// Replays `.http` fixture files from a directory. Each trait method
/// reads a known filename (e.g. `fetch_user` → `user.http`), parses
/// status + headers + body, runs them through the same classifier the
/// production path uses.
pub struct FixturesClient {
    dir: PathBuf,
}

impl FixturesClient {
    /// Construct a client over `dir`. The dir is not validated at
    /// construction time; missing files surface as
    /// [`RemoteFetchState::Error`] at the per-method call.
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

    async fn fetch_recent_runs(
        &self,
        _owner_repo: &str,
        _limit: usize,
    ) -> RemoteFetchState<Vec<CiRunRecordInput>> {
        let parsed = match self.read_fixture("actions_runs.http") {
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
            Err(e) => return RemoteFetchState::Error(format!("actions_runs.http body: {e}")),
        };
        // Job names left empty - no captured-real-server fixture for
        // the per-run /jobs endpoint, and the dashboard tolerates an
        // empty `jobs` vec (it just hides the per-job pills).
        RemoteFetchState::Ok(parse_runs_json(&value))
    }
}

/// Read the user's `gh auth token`. Returns `None` if `gh` is missing,
/// not authenticated, or returns an empty token. One subprocess at
/// startup; never called again.
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
/// `\n` line separators since both forms occur in captured fixtures.
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
