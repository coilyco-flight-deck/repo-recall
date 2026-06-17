//! Forgejo HTTP client. See docs/forgejo-dispatch.md.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;

use crate::ingest::git::log::{ActiveRepo, CommitRecord, DeployHealth};
use crate::ingest::github::client::parse_active_repos_json;
use crate::ingest::github::{
    parse_commits_json, parse_issues_json, parse_milestones_json, parse_prs_json, AuthedUser,
    GithubClient, IssueRecordInput, MilestoneInput, PrRecordInput, RemoteFetchState,
};

const FORGEJO_TOKEN_ENV: &str = "REPO_RECALL_FORGEJO_TOKEN";
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Build a Forgejo client; missing token → `Unconfigured` on every fetch.
pub fn build_client(host: &str) -> Arc<dyn GithubClient> {
    Arc::new(ReqwestForgejoClient::new(host))
}

pub struct ReqwestForgejoClient {
    base: String,
    client: Client,
    unconfigured: bool,
}

impl ReqwestForgejoClient {
    pub fn new(host: &str) -> Self {
        let base = format!("https://{}/api/v1", host.trim_end_matches('/'));
        let token = std::env::var(FORGEJO_TOKEN_ENV)
            .ok()
            .filter(|s| !s.is_empty());
        let unconfigured = token.is_none();
        let mut builder = Client::builder()
            .timeout(HTTP_TIMEOUT)
            .user_agent("repo-recall");
        if let Some(t) = token {
            let mut headers = reqwest::header::HeaderMap::new();
            match reqwest::header::HeaderValue::from_str(&format!("token {t}")) {
                Ok(v) => {
                    headers.insert(reqwest::header::AUTHORIZATION, v);
                    builder = builder.default_headers(headers);
                }
                Err(_) => {
                    tracing::warn!(
                        "{FORGEJO_TOKEN_ENV} contained invalid header bytes; Forgejo calls will be anonymous"
                    );
                }
            }
        } else {
            tracing::warn!(
                "Forgejo: {FORGEJO_TOKEN_ENV} unset; Forgejo-hosted repos will render as `not configured`."
            );
        }
        let client = builder.build().unwrap_or_else(|e| {
            tracing::warn!("reqwest build failed ({e}); falling back to default client");
            Client::new()
        });
        Self {
            base,
            client,
            unconfigured,
        }
    }

    async fn get_json<T>(
        &self,
        path: &str,
        parse: impl FnOnce(&serde_json::Value) -> T,
    ) -> RemoteFetchState<T> {
        if self.unconfigured {
            return RemoteFetchState::Unconfigured;
        }
        let url = format!("{}{path}", self.base);
        let resp = match self.client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => return RemoteFetchState::Error(format!("forgejo {path}: {e}")),
        };
        let status = resp.status().as_u16();
        let headers: Vec<(String, String)> = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_ascii_lowercase(),
                    v.to_str().unwrap_or("").to_string(),
                )
            })
            .collect();
        if let Some(state) =
            crate::ingest::github::fetch_state::classify_http_status::<T>(status, &headers)
        {
            return state;
        }
        let value: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => return RemoteFetchState::Error(format!("forgejo {path}: {e}")),
        };
        RemoteFetchState::Ok(parse(&value))
    }
}

#[async_trait]
impl GithubClient for ReqwestForgejoClient {
    async fn fetch_user(&self) -> RemoteFetchState<AuthedUser> {
        if self.unconfigured {
            return RemoteFetchState::Unconfigured;
        }
        self.get_json("/user", |v| {
            let login = v
                .get("login")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            AuthedUser { login }
        })
        .await
    }

    async fn fetch_open_issues(&self, owner_repo: &str) -> RemoteFetchState<Vec<IssueRecordInput>> {
        // `type=issues` filters PRs server-side (Forgejo /issues mixes both).
        let path = format!("/repos/{owner_repo}/issues?state=open&type=issues&limit=50");
        self.get_json(&path, parse_issues_json).await
    }

    async fn fetch_open_prs(&self, owner_repo: &str) -> RemoteFetchState<Vec<PrRecordInput>> {
        let path = format!("/repos/{owner_repo}/pulls?state=open&limit=50");
        self.get_json(&path, parse_prs_json).await
    }

    async fn fetch_open_milestones(
        &self,
        owner_repo: &str,
    ) -> RemoteFetchState<Vec<MilestoneInput>> {
        let path = format!("/repos/{owner_repo}/milestones?state=open&limit=50");
        self.get_json(&path, parse_milestones_json).await
    }

    async fn fetch_deploy_health(
        &self,
        _owner_repo: &str,
        _workflow: &str,
        _branch: &str,
    ) -> RemoteFetchState<DeployHealth> {
        // Forgejo Actions ingest deferred; see docs/forgejo-dispatch.md.
        RemoteFetchState::Unconfigured
    }

    async fn fetch_active_repos(&self, limit: usize) -> RemoteFetchState<Vec<ActiveRepo>> {
        let capped = limit.clamp(1, 50);
        let path = format!("/user/repos?page=1&limit={capped}");
        self.get_json(&path, parse_active_repos_json).await
    }

    async fn fetch_commits(
        &self,
        owner_repo: &str,
        branch: &str,
        limit: usize,
    ) -> RemoteFetchState<Vec<CommitRecord>> {
        let capped = limit.clamp(1, 100);
        let path = format!("/repos/{owner_repo}/commits?sha={branch}&limit={capped}");
        self.get_json(&path, parse_commits_json).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unconfigured_returns_unconfigured_for_every_endpoint() {
        std::env::remove_var(FORGEJO_TOKEN_ENV);
        let client = ReqwestForgejoClient::new("forgejo.coilysiren.me");
        assert!(matches!(
            client.fetch_user().await,
            RemoteFetchState::Unconfigured
        ));
        assert!(matches!(
            client.fetch_open_issues("a/b").await,
            RemoteFetchState::Unconfigured
        ));
        assert!(matches!(
            client.fetch_open_prs("a/b").await,
            RemoteFetchState::Unconfigured
        ));
        assert!(matches!(
            client.fetch_open_milestones("a/b").await,
            RemoteFetchState::Unconfigured
        ));
        assert!(matches!(
            client.fetch_active_repos(50).await,
            RemoteFetchState::Unconfigured
        ));
        assert!(matches!(
            client.fetch_deploy_health("a/b", "ci.yml", "main").await,
            RemoteFetchState::Unconfigured
        ));
        assert!(matches!(
            client.fetch_commits("a/b", "main", 50).await,
            RemoteFetchState::Unconfigured
        ));
    }

    #[test]
    fn base_url_construction_strips_trailing_slash() {
        std::env::remove_var(FORGEJO_TOKEN_ENV);
        let c = ReqwestForgejoClient::new("forgejo.coilysiren.me/");
        assert_eq!(c.base, "https://forgejo.coilysiren.me/api/v1");
    }
}
