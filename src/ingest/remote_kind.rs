//! Per-host probe for GitHub vs Forgejo. See docs/forgejo-dispatch.md.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteKind {
    Github,
    Forgejo,
}

const CACHE_TTL: Duration = Duration::from_secs(3600);
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
struct CacheEntry {
    kind: Option<RemoteKind>,
    captured: Instant,
}

#[derive(Default, Clone)]
pub struct RemoteKindCache {
    inner: Arc<Mutex<HashMap<String, CacheEntry>>>,
}

impl RemoteKindCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Detect host kind with TTL cache; `None` = not dispatchable.
    pub async fn detect(&self, host: &str) -> Option<RemoteKind> {
        if host.eq_ignore_ascii_case("github.com") {
            return Some(RemoteKind::Github);
        }
        let key = host.to_ascii_lowercase();
        {
            let guard = self.inner.lock().await;
            if let Some(entry) = guard.get(&key) {
                if entry.captured.elapsed() < CACHE_TTL {
                    return entry.kind;
                }
            }
        }
        let kind = probe_forgejo(&key).await;
        let mut guard = self.inner.lock().await;
        guard.insert(
            key,
            CacheEntry {
                kind,
                captured: Instant::now(),
            },
        );
        kind
    }
}

async fn probe_forgejo(host: &str) -> Option<RemoteKind> {
    let url = format!("https://{host}/api/v1/version");
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .build()
        .ok()?;
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    body.get("version")
        .and_then(|v| v.as_str())
        .map(|_| RemoteKind::Forgejo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn github_com_short_circuits() {
        let cache = RemoteKindCache::new();
        assert_eq!(cache.detect("github.com").await, Some(RemoteKind::Github));
        assert_eq!(cache.detect("GitHub.COM").await, Some(RemoteKind::Github));
    }
}
