// Persistent state for things that must outlive the wipe-on-restart
// cache: the VAPID keypair we sign push deliveries with, the set of
// browser push subscriptions, and the deduplication set of action-required
// signal ids we have already notified for.
//
// Lives at $REPO_RECALL_STATE_DIR (override) or $XDG_DATA_HOME/repo-recall
// or ~/.local/share/repo-recall. Created mode 0700.
//
// Backed by redb (pure-Rust embedded KV, ACID, single file). Replaces the
// prior rusqlite-backed state DB. The on-disk file is `state.redb`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use chrono::Utc;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use web_push::VapidSignatureBuilder;

// All durable state is keyed by &str -> JSON bytes for forward compatibility.
// The state surface is dozens of rows; the JSON overhead is invisible and
// adding a field never requires a migration.
const VAPID: TableDefinition<&str, &[u8]> = TableDefinition::new("vapid");
const SUBSCRIPTIONS: TableDefinition<&str, &[u8]> = TableDefinition::new("subscriptions");
const SUB_META: TableDefinition<&str, u64> = TableDefinition::new("sub_meta");
const SEEN_SIGNALS: TableDefinition<&str, (i64, i64)> = TableDefinition::new("seen_signals");

const VAPID_KEY: &str = "current";
const SUB_NEXT_ID_KEY: &str = "next_id";

#[derive(Clone)]
pub struct StateDb {
    db: Arc<Database>,
}

impl std::fmt::Debug for StateDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StateDb").finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Vapid {
    pub private_pem: String,
    pub public_b64url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Subscription {
    pub id: i64,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub user_agent: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NewSubscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub user_agent: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct VapidRecord {
    private_pem: String,
    public_b64url: String,
    created_at: i64,
}

#[derive(Serialize, Deserialize)]
struct SubscriptionRecord {
    id: i64,
    endpoint: String,
    p256dh: String,
    auth: String,
    user_agent: Option<String>,
    created_at: i64,
}

impl StateDb {
    pub fn open_default() -> Result<Self> {
        let dir = state_dir()?;
        std::fs::create_dir_all(&dir).with_context(|| format!("create state dir: {dir:?}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        Self::open_at(dir.join("state.redb"))
    }

    pub fn open_at(path: PathBuf) -> Result<Self> {
        let db = Database::create(&path).with_context(|| format!("open redb at {path:?}"))?;
        // Ensure tables exist so first reads do not error on a fresh file.
        let write = db.begin_write()?;
        {
            let _ = write.open_table(VAPID)?;
            let _ = write.open_table(SUBSCRIPTIONS)?;
            let _ = write.open_table(SUB_META)?;
            let _ = write.open_table(SEEN_SIGNALS)?;
        }
        write.commit()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(Self { db: Arc::new(db) })
    }

    pub fn get_or_init_vapid(&self) -> Result<Vapid> {
        if let Some(v) = self.read_vapid()? {
            return Ok(v);
        }
        let priv_pem = openssl_generate_p256_pem()
            .context("generate VAPID keypair (is `openssl` on PATH?)")?;
        let pub_b64url = derive_vapid_public_b64url(&priv_pem)
            .context("derive VAPID public key from private PEM")?;
        let now = Utc::now().timestamp();
        let record = VapidRecord {
            private_pem: priv_pem.clone(),
            public_b64url: pub_b64url.clone(),
            created_at: now,
        };
        let bytes = serde_json::to_vec(&record)?;
        let write = self.db.begin_write()?;
        {
            let mut t = write.open_table(VAPID)?;
            t.insert(VAPID_KEY, bytes.as_slice())?;
        }
        write.commit()?;
        Ok(Vapid {
            private_pem: priv_pem,
            public_b64url: pub_b64url,
        })
    }

    fn read_vapid(&self) -> Result<Option<Vapid>> {
        let read = self.db.begin_read()?;
        let t = read.open_table(VAPID)?;
        let Some(g) = t.get(VAPID_KEY)? else {
            return Ok(None);
        };
        let r: VapidRecord = serde_json::from_slice(g.value())?;
        Ok(Some(Vapid {
            private_pem: r.private_pem,
            public_b64url: r.public_b64url,
        }))
    }

    pub fn list_subscriptions(&self) -> Result<Vec<Subscription>> {
        let read = self.db.begin_read()?;
        let t = read.open_table(SUBSCRIPTIONS)?;
        let mut out: Vec<Subscription> = Vec::new();
        for entry in t.iter()? {
            let (_k, v) = entry?;
            let r: SubscriptionRecord = serde_json::from_slice(v.value())?;
            out.push(Subscription {
                id: r.id,
                endpoint: r.endpoint,
                p256dh: r.p256dh,
                auth: r.auth,
                user_agent: r.user_agent,
            });
        }
        // Match the prior SQLite ORDER BY id contract.
        out.sort_by_key(|s| s.id);
        Ok(out)
    }

    pub fn upsert_subscription(&self, sub: &NewSubscription) -> Result<()> {
        let now = Utc::now().timestamp();
        let write = self.db.begin_write()?;
        {
            let existing_id = {
                let t = write.open_table(SUBSCRIPTIONS)?;
                let g = t.get(sub.endpoint.as_str())?;
                let parsed = match g {
                    Some(g) => {
                        let r: SubscriptionRecord = serde_json::from_slice(g.value())?;
                        Some((r.id, r.created_at))
                    }
                    None => None,
                };
                parsed
            };
            let (id, created_at) = match existing_id {
                Some((id, ca)) => (id, ca),
                None => (next_subscription_id(&write)?, now),
            };
            let record = SubscriptionRecord {
                id,
                endpoint: sub.endpoint.clone(),
                p256dh: sub.p256dh.clone(),
                auth: sub.auth.clone(),
                user_agent: sub.user_agent.clone(),
                created_at,
            };
            let bytes = serde_json::to_vec(&record)?;
            let mut t = write.open_table(SUBSCRIPTIONS)?;
            t.insert(sub.endpoint.as_str(), bytes.as_slice())?;
        }
        write.commit()?;
        Ok(())
    }

    pub fn remove_subscription(&self, endpoint: &str) -> Result<usize> {
        let write = self.db.begin_write()?;
        let removed = {
            let mut t = write.open_table(SUBSCRIPTIONS)?;
            let prior = t.remove(endpoint)?;
            prior.is_some()
        };
        write.commit()?;
        Ok(if removed { 1 } else { 0 })
    }

    /// Reconcile the persistent seen-signals set with this scan's
    /// action-required ids. Returns the subset of `current_ids` that are
    /// new since the last call, which is exactly the set the dispatcher
    /// should fire push notifications for. Ids no longer present are
    /// pruned so that the same signal re-armed (broken, fixed, broken
    /// again) re-notifies on the next failure.
    pub fn diff_and_record_signals(&self, current_ids: &[String]) -> Result<Vec<String>> {
        let now = Utc::now().timestamp();
        let write = self.db.begin_write()?;
        let mut new_ids: Vec<String> = Vec::new();
        {
            let mut t = write.open_table(SEEN_SIGNALS)?;
            let existing: HashSet<String> = t
                .iter()?
                .map(|e| e.map(|(k, _)| k.value().to_string()))
                .collect::<std::result::Result<_, _>>()?;
            let current_set: HashSet<&String> = current_ids.iter().collect();

            for id in existing.iter().filter(|id| !current_set.contains(id)) {
                t.remove(id.as_str())?;
            }
            for id in current_ids {
                if existing.contains(id) {
                    let first_seen = t.get(id.as_str())?.map(|g| g.value().0).unwrap_or(now);
                    t.insert(id.as_str(), (first_seen, now))?;
                } else {
                    t.insert(id.as_str(), (now, now))?;
                    new_ids.push(id.clone());
                }
            }
        }
        write.commit()?;
        Ok(new_ids)
    }
}

fn next_subscription_id(write: &redb::WriteTransaction) -> Result<i64> {
    let mut meta = write.open_table(SUB_META)?;
    let next = meta.get(SUB_NEXT_ID_KEY)?.map(|g| g.value()).unwrap_or(1);
    meta.insert(SUB_NEXT_ID_KEY, next + 1)?;
    Ok(next as i64)
}

fn state_dir() -> Result<PathBuf> {
    if let Ok(override_path) = std::env::var("REPO_RECALL_STATE_DIR") {
        if !override_path.is_empty() {
            return Ok(PathBuf::from(override_path));
        }
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return Ok(PathBuf::from(xdg).join("repo-recall"));
        }
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot find home dir"))?;
    Ok(home.join(".local").join("share").join("repo-recall"))
}

fn openssl_generate_p256_pem() -> Result<String> {
    let out = Command::new("openssl")
        .args(["ecparam", "-name", "prime256v1", "-genkey", "-noout"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("spawn openssl ecparam")?;
    if !out.status.success() {
        bail!(
            "openssl ecparam failed (status {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let pem = String::from_utf8(out.stdout).context("openssl ecparam returned non-utf8")?;
    if !pem.contains("EC PRIVATE KEY") && !pem.contains("PRIVATE KEY") {
        bail!("openssl output does not look like a PEM private key");
    }
    Ok(pem)
}

fn derive_vapid_public_b64url(priv_pem: &str) -> Result<String> {
    let builder = VapidSignatureBuilder::from_pem_no_sub(priv_pem.as_bytes())
        .map_err(|e| anyhow!("from_pem_no_sub: {e}"))?;
    let pub_bytes = builder.get_public_key();
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&pub_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> StateDb {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "repo-recall-state-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQ.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        StateDb::open_at(dir.join("state.redb")).unwrap()
    }

    #[test]
    fn vapid_is_persisted_and_idempotent() {
        let db = temp_db();
        let v1 = db.get_or_init_vapid().unwrap();
        let v2 = db.get_or_init_vapid().unwrap();
        assert_eq!(v1.private_pem, v2.private_pem);
        assert_eq!(v1.public_b64url, v2.public_b64url);
        assert!(!v1.public_b64url.is_empty());
    }

    #[test]
    fn subscription_upsert_dedups_by_endpoint() {
        let db = temp_db();
        let sub = NewSubscription {
            endpoint: "https://fcm.googleapis.com/fcm/send/abc".into(),
            p256dh: "p256dh-1".into(),
            auth: "auth-1".into(),
            user_agent: Some("test-1".into()),
        };
        db.upsert_subscription(&sub).unwrap();
        let updated = NewSubscription {
            p256dh: "p256dh-2".into(),
            auth: "auth-2".into(),
            user_agent: Some("test-2".into()),
            ..sub.clone()
        };
        db.upsert_subscription(&updated).unwrap();
        let all = db.list_subscriptions().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].p256dh, "p256dh-2");
        assert_eq!(all[0].auth, "auth-2");
    }

    #[test]
    fn subscription_ids_are_stable_across_upsert_and_increment_for_new_endpoints() {
        let db = temp_db();
        let a = NewSubscription {
            endpoint: "https://a.example/".into(),
            p256dh: "p1".into(),
            auth: "a1".into(),
            user_agent: None,
        };
        let b = NewSubscription {
            endpoint: "https://b.example/".into(),
            p256dh: "p2".into(),
            auth: "a2".into(),
            user_agent: None,
        };
        db.upsert_subscription(&a).unwrap();
        db.upsert_subscription(&b).unwrap();
        let initial = db.list_subscriptions().unwrap();
        assert_eq!(initial.len(), 2);
        let a_id = initial
            .iter()
            .find(|s| s.endpoint.contains("a.example"))
            .unwrap()
            .id;
        let b_id = initial
            .iter()
            .find(|s| s.endpoint.contains("b.example"))
            .unwrap()
            .id;
        assert!(a_id < b_id, "ids reflect insertion order");

        // Re-upsert a; id stays.
        db.upsert_subscription(&NewSubscription {
            p256dh: "p1-new".into(),
            ..a.clone()
        })
        .unwrap();
        let after = db.list_subscriptions().unwrap();
        let a_id_after = after
            .iter()
            .find(|s| s.endpoint.contains("a.example"))
            .unwrap()
            .id;
        assert_eq!(a_id, a_id_after);
    }

    #[test]
    fn diff_and_record_returns_only_new_ids_and_prunes_gone_ones() {
        let db = temp_db();
        let first = db
            .diff_and_record_signals(&["a:ci_failing".into(), "b:dirty_tree".into()])
            .unwrap();
        assert_eq!(first.len(), 2);

        let second = db
            .diff_and_record_signals(&["a:ci_failing".into(), "b:dirty_tree".into()])
            .unwrap();
        assert!(second.is_empty(), "no new ids on identical second call");

        // b clears, c appears.
        let third = db
            .diff_and_record_signals(&["a:ci_failing".into(), "c:detached_head".into()])
            .unwrap();
        assert_eq!(third, vec!["c:detached_head"]);

        // b reappears: should re-fire because it was pruned.
        let fourth = db
            .diff_and_record_signals(&[
                "a:ci_failing".into(),
                "b:dirty_tree".into(),
                "c:detached_head".into(),
            ])
            .unwrap();
        assert_eq!(fourth, vec!["b:dirty_tree"]);
    }

    #[test]
    fn remove_subscription_returns_count_and_is_idempotent() {
        let db = temp_db();
        let sub = NewSubscription {
            endpoint: "https://x.example/".into(),
            p256dh: "p".into(),
            auth: "a".into(),
            user_agent: None,
        };
        db.upsert_subscription(&sub).unwrap();
        assert_eq!(db.remove_subscription(&sub.endpoint).unwrap(), 1);
        assert_eq!(db.remove_subscription(&sub.endpoint).unwrap(), 0);
        assert!(db.list_subscriptions().unwrap().is_empty());
    }
}
