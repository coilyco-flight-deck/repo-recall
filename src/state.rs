// Persistent state for things that must outlive the wipe-on-restart
// cache DB: the VAPID keypair we sign push deliveries with, the set of
// browser push subscriptions, and the deduplication set of action-required
// signal ids we have already notified for.
//
// Lives at $REPO_RECALL_STATE_DIR (override) or $XDG_DATA_HOME/repo-recall
// or ~/.local/share/repo-recall. Created mode 0700.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use web_push::VapidSignatureBuilder;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS vapid (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  private_pem TEXT NOT NULL,
  public_b64url TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS subscriptions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  endpoint TEXT NOT NULL UNIQUE,
  p256dh TEXT NOT NULL,
  auth TEXT NOT NULL,
  user_agent TEXT,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS seen_signals (
  id TEXT PRIMARY KEY,
  first_seen INTEGER NOT NULL,
  last_seen INTEGER NOT NULL
);
"#;

#[derive(Clone, Debug)]
pub struct StateDb {
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Vapid {
    pub private_pem: String,
    pub public_b64url: String,
}

#[derive(Clone, Debug)]
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

impl StateDb {
    pub fn open_default() -> Result<Self> {
        let dir = state_dir()?;
        std::fs::create_dir_all(&dir).with_context(|| format!("create state dir: {dir:?}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        Self::open_at(dir.join("state.sqlite"))
    }

    pub fn open_at(path: PathBuf) -> Result<Self> {
        let db = Self { path };
        let conn = db.conn()?;
        conn.execute_batch(SCHEMA)?;
        drop(conn);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&db.path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(db)
    }

    fn conn(&self) -> Result<Connection> {
        Connection::open(&self.path).map_err(Into::into)
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
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO vapid (id, private_pem, public_b64url, created_at) VALUES (1, ?1, ?2, ?3)",
            params![&priv_pem, &pub_b64url, now],
        )?;
        Ok(Vapid {
            private_pem: priv_pem,
            public_b64url: pub_b64url,
        })
    }

    fn read_vapid(&self) -> Result<Option<Vapid>> {
        let conn = self.conn()?;
        let row = conn
            .query_row(
                "SELECT private_pem, public_b64url FROM vapid WHERE id = 1",
                [],
                |r| {
                    Ok(Vapid {
                        private_pem: r.get(0)?,
                        public_b64url: r.get(1)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn list_subscriptions(&self) -> Result<Vec<Subscription>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, endpoint, p256dh, auth, user_agent FROM subscriptions ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(Subscription {
                    id: r.get(0)?,
                    endpoint: r.get(1)?,
                    p256dh: r.get(2)?,
                    auth: r.get(3)?,
                    user_agent: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn upsert_subscription(&self, sub: &NewSubscription) -> Result<()> {
        let conn = self.conn()?;
        let now = Utc::now().timestamp();
        conn.execute(
            "INSERT INTO subscriptions (endpoint, p256dh, auth, user_agent, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(endpoint) DO UPDATE SET
                p256dh = excluded.p256dh,
                auth = excluded.auth,
                user_agent = excluded.user_agent",
            params![&sub.endpoint, &sub.p256dh, &sub.auth, &sub.user_agent, now],
        )?;
        Ok(())
    }

    pub fn remove_subscription(&self, endpoint: &str) -> Result<usize> {
        let conn = self.conn()?;
        Ok(conn.execute(
            "DELETE FROM subscriptions WHERE endpoint = ?1",
            params![endpoint],
        )?)
    }

    /// Reconcile the persistent seen-signals set with this scan's
    /// action-required ids. Returns the subset of `current_ids` that are
    /// new since the last call, which is exactly the set the dispatcher
    /// should fire push notifications for. Ids no longer present are
    /// pruned so that the same signal re-armed (broken, fixed, broken
    /// again) re-notifies on the next failure.
    pub fn diff_and_record_signals(&self, current_ids: &[String]) -> Result<Vec<String>> {
        let mut conn = self.conn()?;
        let now = Utc::now().timestamp();
        let tx = conn.transaction()?;

        let existing: HashSet<String> = {
            let mut stmt = tx.prepare("SELECT id FROM seen_signals")?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<HashSet<String>>>()?;
            rows
        };
        let current_set: HashSet<&String> = current_ids.iter().collect();

        for id in existing.iter().filter(|id| !current_set.contains(id)) {
            tx.execute("DELETE FROM seen_signals WHERE id = ?1", params![id])?;
        }

        let mut new_ids = Vec::new();
        for id in current_ids {
            if existing.contains(id) {
                tx.execute(
                    "UPDATE seen_signals SET last_seen = ?1 WHERE id = ?2",
                    params![now, id],
                )?;
            } else {
                tx.execute(
                    "INSERT INTO seen_signals (id, first_seen, last_seen) VALUES (?1, ?2, ?2)",
                    params![id, now],
                )?;
                new_ids.push(id.clone());
            }
        }

        tx.commit()?;
        Ok(new_ids)
    }
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
        let dir = std::env::temp_dir().join(format!(
            "repo-recall-state-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        StateDb::open_at(dir.join("state.sqlite")).unwrap()
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
}
