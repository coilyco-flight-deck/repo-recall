use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// One row from coily's audit JSONL, normalized for storage. Mirrors the
/// shape produced by cli-guard's `audit` writer. Unknown fields on the
/// wire are tolerated via serde's default behavior.
#[derive(Debug, Clone)]
pub struct AuditRecord {
    pub event_id: String,
    pub ts: i64,
    pub decision: String,
    pub verb: String,
    pub argv: Vec<String>,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub commit_scope: Option<String>,
    pub audit_override: bool,
    pub source_file: String,
    /// LUCA join key. Populated by cli-guard #2a (landed); converts the
    /// audit↔session join from a timestamp-window heuristic to an exact key.
    pub session_id: Option<String>,
    /// cli-guard binary version that wrote the row.
    pub version: Option<String>,
    /// Error message captured for `decision: "deny"` or upstream-failed rows.
    pub error: Option<String>,
    /// Last chunk of subprocess stderr on failure rows.
    pub stderr_tail: Option<String>,
    /// Git toplevel cli-guard resolved for the row. May differ from
    /// `commit_scope` when scope is explicit.
    pub repo_root: Option<String>,
    /// Working directory of the spawned subprocess.
    pub cwd_subprocess: Option<String>,
    /// Working directory at cli-guard invocation (before any internal cd).
    pub cwd_at_invocation: Option<String>,
    /// Outbound network attempts captured by the egress wedge.
    pub egress: Vec<EgressEntry>,
    /// Profile-decision block: which profile applied, why, and the
    /// resolved capability coordinate.
    pub profile_decision: Option<ProfileDecision>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EgressEntry {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub decision: String,
    #[serde(default)]
    pub bytes_up: i64,
    #[serde(default)]
    pub bytes_down: i64,
    #[serde(default)]
    pub duration_ms: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProfileDecision {
    #[serde(default)]
    pub allowed: bool,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub reason: String,
    /// Resolved capability coordinate. Shape is owned by cli-guard and may
    /// gain fields; stored as raw JSON so additions land without a schema
    /// migration.
    #[serde(default)]
    pub coordinate: serde_json::Value,
}

/// Resolve the audit directory. Honors `REPO_RECALL_AUDIT_DIR` (point at
/// a fixture tree for tests) and otherwise falls back to `~/.coily/audit`,
/// where cli-guard writes one JSONL shard per git toplevel.
pub fn default_audit_dir() -> Option<PathBuf> {
    if let Some(over) = std::env::var_os("REPO_RECALL_AUDIT_DIR") {
        let dir = PathBuf::from(over);
        if dir.is_dir() {
            return Some(dir);
        }
        tracing::warn!(
            "REPO_RECALL_AUDIT_DIR set to {:?} but is not a directory; falling back",
            dir
        );
    }
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home).join(".coily").join("audit");
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

/// Enumerate every `.jsonl` file directly under the audit directory.
/// cli-guard shards one file per git toplevel, so no recursion needed.
pub fn list_audit_files(audit_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(audit_dir)? {
        let entry = entry?;
        let p = entry.path();
        if p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            out.push(p);
        }
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
struct RawRow {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    ts: Option<i64>,
    #[serde(default)]
    decision: Option<String>,
    #[serde(default)]
    verb: Option<String>,
    #[serde(default)]
    argv: Vec<String>,
    #[serde(default)]
    exit_code: Option<i64>,
    #[serde(default)]
    duration_ms: Option<i64>,
    #[serde(default)]
    commit_scope: Option<String>,
    #[serde(default)]
    audit_override: bool,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    stderr_tail: Option<String>,
    #[serde(default)]
    repo_root: Option<String>,
    #[serde(default)]
    cwd_subprocess: Option<String>,
    #[serde(default)]
    cwd_at_invocation: Option<String>,
    #[serde(default)]
    egress: Vec<EgressEntry>,
    #[serde(default)]
    profile_decision: Option<ProfileDecision>,
}

/// Parse every line of a single JSONL shard into `AuditRecord`. Malformed
/// or empty lines are logged at `debug!` and skipped - one bad row should
/// never sink a whole file.
pub fn parse_audit_file(path: &Path) -> Result<Vec<AuditRecord>> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let source_file = path.to_string_lossy().into_owned();
    let mut out = Vec::new();
    for (lineno, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!("audit read error {}:{}: {e}", path.display(), lineno + 1);
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let raw: RawRow = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("audit parse error {}:{}: {e}", path.display(), lineno + 1);
                continue;
            }
        };
        let (Some(event_id), Some(ts), Some(verb)) = (raw.id, raw.ts, raw.verb) else {
            continue;
        };
        out.push(AuditRecord {
            event_id,
            ts,
            decision: raw.decision.unwrap_or_default(),
            verb,
            argv: raw.argv,
            exit_code: raw.exit_code,
            duration_ms: raw.duration_ms,
            commit_scope: raw.commit_scope,
            audit_override: raw.audit_override,
            source_file: source_file.clone(),
            session_id: raw.session_id,
            version: raw.version,
            error: raw.error,
            stderr_tail: raw.stderr_tail,
            repo_root: raw.repo_root,
            cwd_subprocess: raw.cwd_subprocess,
            cwd_at_invocation: raw.cwd_at_invocation,
            egress: raw.egress,
            profile_decision: raw.profile_decision,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(rows: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "repo-recall-audit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scope.jsonl");
        std::fs::write(&path, rows.join("\n")).unwrap();
        path
    }

    #[test]
    fn parses_canonical_row() {
        let path = fixture(&[
            r#"{"id":"019e288b-280c-7fde-85fd-f323b3086b13","ts":1778796668,"decision":"accept","verb":"ops.gh","argv":["coily","ops","gh","whoami"],"exit_code":0,"duration_ms":1000,"commit_scope":"/Users/kai/projects/coilysiren/repo-recall"}"#,
        ]);
        let recs = parse_audit_file(&path).unwrap();
        assert_eq!(recs.len(), 1);
        let r = &recs[0];
        assert_eq!(r.event_id, "019e288b-280c-7fde-85fd-f323b3086b13");
        assert_eq!(r.ts, 1778796668);
        assert_eq!(r.verb, "ops.gh");
        assert_eq!(r.decision, "accept");
        assert_eq!(
            r.commit_scope.as_deref(),
            Some("/Users/kai/projects/coilysiren/repo-recall")
        );
        assert!(!r.audit_override);
    }

    #[test]
    fn skips_blank_and_malformed_lines() {
        let path = fixture(&[
            r#"{"id":"a","ts":1,"verb":"v"}"#,
            "",
            "not json",
            r#"{"missing":"required"}"#,
            r#"{"id":"b","ts":2,"verb":"v2"}"#,
        ]);
        let recs = parse_audit_file(&path).unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].event_id, "a");
        assert_eq!(recs[1].event_id, "b");
    }

    #[test]
    fn tolerates_audit_override_flag() {
        let path = fixture(&[r#"{"id":"x","ts":1,"verb":"repo.ci","audit_override":true}"#]);
        let recs = parse_audit_file(&path).unwrap();
        assert_eq!(recs.len(), 1);
        assert!(recs[0].audit_override);
    }
}
