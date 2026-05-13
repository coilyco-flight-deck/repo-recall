//! Structural-context-ask drafter (#92 phase 5, #105).
//!
//! Mirrors [`crate::display::dispatch_artifacts`] in shape: the planner
//! calls in with an ask body plus a `lifts` list (the tickets the
//! answer would unblock); the drafter writes a write-once markdown
//! file under `~/.repo-recall/structural-asks/<slug>.md` for Kai to
//! review and post as a `structural-ask`-labeled GitHub issue. There
//! is no in-repo write target — structural asks live above any single
//! repo. The pollable mirror is the only artifact path.
//!
//! All free text is routed through [`crate::process::sanitize::scrub`]
//! before it lands on disk (#110). Slugs are derived from the primary
//! lift's issue number plus a short content hash, so two drafts that
//! lift the same primary ticket collide on slug and the second write
//! is rejected.
//!
//! Refusing to re-emit asks already posted to GitHub is a follow-up
//! and intentionally out of scope here. The cache exposes
//! `labeled_issues_by_state("structural-ask", "open")` for the planner
//! to read first via the `recall_open_structural_asks` MCP tool.

use std::hash::{DefaultHasher, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EmitStructuralAskRequest {
    /// Short, imperative title for the GitHub issue (e.g. "Decide
    /// whether the dispatch ledger lives in its own redb file").
    pub title: String,
    /// Markdown body. Sanitized before write.
    pub ask_text: String,
    /// `["owner/repo#N", ...]` — tickets that would be unblocked by
    /// answering this ask. At least one entry required so the slug has
    /// a primary lift.
    pub lifts: Vec<String>,
    /// Optional override; otherwise `<YYYY-MM-DD>-ask-<lift-n>-<short>`.
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmitStructuralAskResponse {
    pub slug: String,
    pub pollable_path: String,
    pub ask_hash: String,
    pub drafted_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum EmitError {
    #[error("title must not be empty")]
    EmptyTitle,
    #[error("ask_text must not be empty")]
    EmptyAskText,
    #[error("lifts must contain at least one ref")]
    NoLifts,
    #[error("invalid lift ref: {0}")]
    InvalidRef(String),
    #[error("structural-ask draft already exists: {0}")]
    AlreadyExists(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn emit_structural_ask(
    req: &EmitStructuralAskRequest,
) -> Result<EmitStructuralAskResponse, EmitError> {
    if req.title.trim().is_empty() {
        return Err(EmitError::EmptyTitle);
    }
    if req.ask_text.trim().is_empty() {
        return Err(EmitError::EmptyAskText);
    }
    if req.lifts.is_empty() {
        return Err(EmitError::NoLifts);
    }
    for r in &req.lifts {
        if crate::process::join::gh_refs_with_issue_in_text(r).is_empty() {
            return Err(EmitError::InvalidRef(r.clone()));
        }
    }

    use crate::process::sanitize::{scrub, SanitizeSource};
    let title = scrub(&req.title, SanitizeSource::GithubIssueBody);
    let ask_text = scrub(&req.ask_text, SanitizeSource::GithubIssueBody);

    let now = chrono::Utc::now();
    let drafted_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let ask_hash = identity_hash(ask_text.as_bytes());

    let primary_lift = first_issue_number(&req.lifts);
    let slug = req.slug.clone().unwrap_or_else(|| {
        let date = now.format("%Y-%m-%d");
        let n = primary_lift.unwrap_or(0);
        let short = &ask_hash[..ask_hash.len().min(7)];
        format!("{date}-ask-{n}-{short}")
    });

    let pollable = pollable_root().join(format!("{slug}.md"));
    if pollable.exists() {
        return Err(EmitError::AlreadyExists(pollable.to_string_lossy().into()));
    }

    let body = render_ask_file(&title, &ask_text, &req.lifts, &ask_hash, &drafted_at);
    write_atomic(&pollable, &body)?;

    Ok(EmitStructuralAskResponse {
        slug,
        pollable_path: pollable.to_string_lossy().into(),
        ask_hash,
        drafted_at,
    })
}

fn render_ask_file(
    title: &str,
    ask_text: &str,
    lifts: &[String],
    ask_hash: &str,
    drafted_at: &str,
) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str("---\n");
    let _ = writeln!(s, "title: {title}");
    let _ = writeln!(s, "labels: [structural-ask]");
    let lifts_csv = lifts.join(", ");
    let _ = writeln!(s, "lifts: [{lifts_csv}]");
    let _ = writeln!(s, "ask_hash: {ask_hash}");
    let _ = writeln!(s, "drafted_at: {drafted_at}");
    s.push_str("---\n");
    s.push_str(ask_text);
    if !ask_text.ends_with('\n') {
        s.push('\n');
    }
    s.push_str("\n## Lifts\n\n");
    for l in lifts {
        let _ = writeln!(s, "- {l}");
    }
    s
}

fn first_issue_number(refs: &[String]) -> Option<u32> {
    refs.iter()
        .filter_map(|r| crate::process::join::gh_refs_with_issue_in_text(r).pop())
        .map(|r| r.issue)
        .next()
}

fn identity_hash(bytes: &[u8]) -> String {
    let mut h = DefaultHasher::new();
    h.write(bytes);
    format!("{:016x}", h.finish())
}

/// `~/.repo-recall/structural-asks/`. Override with
/// `REPO_RECALL_STRUCTURAL_ASKS_ROOT` for tests and out-of-home installs.
pub fn pollable_root() -> PathBuf {
    if let Ok(root) = std::env::var("REPO_RECALL_STRUCTURAL_ASKS_ROOT") {
        return PathBuf::from(root);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".repo-recall").join("structural-asks")
}

fn write_atomic(path: &Path, body: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("md.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch_root() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "repo-recall-asks-{nanos}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("REPO_RECALL_STRUCTURAL_ASKS_ROOT", &dir);
        dir
    }

    #[test]
    fn emit_writes_pollable_with_frontmatter_and_lifts() {
        let _root = scratch_root();
        let req = EmitStructuralAskRequest {
            title: "Decide dispatch ledger persistence boundary".into(),
            ask_text: "Should the dispatch ledger live in its own redb file?".into(),
            lifts: vec![
                "coilysiren/repo-recall#92".into(),
                "coilysiren/repo-recall#107".into(),
            ],
            slug: Some("test-ask".into()),
        };
        let resp = emit_structural_ask(&req).expect("emit ok");
        let body = std::fs::read_to_string(&resp.pollable_path).unwrap();
        assert!(body.contains("title: Decide dispatch ledger"), "{body}");
        assert!(body.contains("labels: [structural-ask]"), "{body}");
        assert!(body.contains("coilysiren/repo-recall#92"), "{body}");
        assert!(body.contains("coilysiren/repo-recall#107"), "{body}");
        assert!(body.contains("## Lifts"), "{body}");
    }

    #[test]
    fn emit_refuses_duplicate_slug() {
        let _root = scratch_root();
        let req = EmitStructuralAskRequest {
            title: "T".into(),
            ask_text: "Q?".into(),
            lifts: vec!["foo/bar#1".into()],
            slug: Some("dup".into()),
        };
        emit_structural_ask(&req).expect("first ok");
        let err = emit_structural_ask(&req).expect_err("dup err");
        assert!(matches!(err, EmitError::AlreadyExists(_)));
    }

    #[test]
    fn emit_scrubs_title_and_body_before_write() {
        let _root = scratch_root();
        let req = EmitStructuralAskRequest {
            title: "Question about kai-server".into(),
            ask_text: "Should we ssh kai-server with ghp_AAAABBBBCCCCDDDDEEEEFFFFGGGGHHHHIIII?"
                .into(),
            lifts: vec!["foo/bar#2".into()],
            slug: Some("scrub-ask".into()),
        };
        let resp = emit_structural_ask(&req).expect("emit ok");
        let body = std::fs::read_to_string(&resp.pollable_path).unwrap();
        assert!(!body.contains("kai-server"), "{body}");
        assert!(!body.contains("ghp_AAAA"), "{body}");
        assert!(body.contains("[REDACTED:internal-host]"), "{body}");
        assert!(body.contains("[REDACTED:github-token]"), "{body}");
    }

    #[test]
    fn emit_rejects_invalid_inputs() {
        let _root = scratch_root();
        let base = EmitStructuralAskRequest {
            title: "T".into(),
            ask_text: "Q?".into(),
            lifts: vec!["foo/bar#1".into()],
            slug: None,
        };
        assert!(matches!(
            emit_structural_ask(&EmitStructuralAskRequest {
                title: "  ".into(),
                ..base.clone()
            }),
            Err(EmitError::EmptyTitle)
        ));
        assert!(matches!(
            emit_structural_ask(&EmitStructuralAskRequest {
                ask_text: "".into(),
                ..base.clone()
            }),
            Err(EmitError::EmptyAskText)
        ));
        assert!(matches!(
            emit_structural_ask(&EmitStructuralAskRequest {
                lifts: vec![],
                ..base.clone()
            }),
            Err(EmitError::NoLifts)
        ));
        assert!(matches!(
            emit_structural_ask(&EmitStructuralAskRequest {
                lifts: vec!["nothing-shaped-like-a-ref".into()],
                ..base
            }),
            Err(EmitError::InvalidRef(_))
        ));
    }
}
