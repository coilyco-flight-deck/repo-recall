//! AGENTS.md drift-PR drafter (#92 phase 5+, #106).
//!
//! When recall-dispatch sees a convention emerging across successful
//! dispatches in a repo (e.g. "every successful dispatch in eco-mods
//! uses the EcoModKit reference path X"), the planner calls in here
//! with the proposed rule plus the supporting dispatches, and the
//! drafter writes a write-once markdown file under
//! `~/.repo-recall/agents-drift/<repo>/<slug>.md` for Kai to review
//! and post as a PR against `<repo>/AGENTS.md`.
//!
//! Mirrors [`crate::process::structural_asks`] in shape: write-once
//! pollable mirror only, free text routed through the #110 sanitize
//! gate, slug derived from the rule hash so two drafts proposing the
//! same rule collide.
//!
//! Two follow-ups intentionally out of scope here, both filed against
//! #106 if pursued:
//! 1. Pattern detection itself (which dispatches imply which rule).
//!    This drafter is the sink; the planner is the source.
//! 2. Actual `gh pr create` invocation. The pollable markdown is
//!    enough for Kai to post manually and gives the planner a stable
//!    artifact to anchor against.

use std::hash::{DefaultHasher, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EmitDriftProposalRequest {
    /// Human-readable repo slug (e.g. `eco-mods`). Used for the
    /// pollable subdirectory and rendered into the proposed-PR title.
    pub repo_slug: String,
    /// One-line PR-title-shaped summary (e.g. "AGENTS.md: pin
    /// EcoModKit reference path").
    pub title: String,
    /// The proposed rule body, rendered into the AGENTS.md proposal
    /// section. Markdown allowed.
    pub proposed_rule: String,
    /// `["owner/repo#N", ...]` — the dispatches whose convergence
    /// motivates the rule. At least one entry required so the
    /// proposal has provenance.
    pub supporting_dispatches: Vec<String>,
    /// Optional override; otherwise `<YYYY-MM-DD>-drift-<short>`.
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmitDriftProposalResponse {
    pub slug: String,
    pub pollable_path: String,
    pub rule_hash: String,
    pub drafted_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum EmitError {
    #[error("repo_slug must not be empty")]
    EmptyRepoSlug,
    #[error("title must not be empty")]
    EmptyTitle,
    #[error("proposed_rule must not be empty")]
    EmptyRule,
    #[error("supporting_dispatches must contain at least one ref")]
    NoSupportingDispatches,
    #[error("invalid dispatch ref: {0}")]
    InvalidRef(String),
    #[error("agents-drift draft already exists: {0}")]
    AlreadyExists(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn emit_drift_proposal(
    req: &EmitDriftProposalRequest,
) -> Result<EmitDriftProposalResponse, EmitError> {
    if req.repo_slug.trim().is_empty() {
        return Err(EmitError::EmptyRepoSlug);
    }
    if req.title.trim().is_empty() {
        return Err(EmitError::EmptyTitle);
    }
    if req.proposed_rule.trim().is_empty() {
        return Err(EmitError::EmptyRule);
    }
    if req.supporting_dispatches.is_empty() {
        return Err(EmitError::NoSupportingDispatches);
    }
    for r in &req.supporting_dispatches {
        if crate::process::join::gh_refs_with_issue_in_text(r).is_empty() {
            return Err(EmitError::InvalidRef(r.clone()));
        }
    }

    use crate::process::sanitize::{scrub, SanitizeSource};
    let title = scrub(&req.title, SanitizeSource::GithubIssueBody);
    let proposed_rule = scrub(&req.proposed_rule, SanitizeSource::GithubIssueBody);

    let now = chrono::Utc::now();
    let drafted_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let rule_hash = identity_hash(proposed_rule.as_bytes());

    let slug = req.slug.clone().unwrap_or_else(|| {
        let date = now.format("%Y-%m-%d");
        let short = &rule_hash[..rule_hash.len().min(7)];
        format!("{date}-drift-{short}")
    });

    let pollable = pollable_root()
        .join(&req.repo_slug)
        .join(format!("{slug}.md"));
    if pollable.exists() {
        return Err(EmitError::AlreadyExists(pollable.to_string_lossy().into()));
    }

    let body = render_proposal_file(
        &title,
        &req.repo_slug,
        &proposed_rule,
        &req.supporting_dispatches,
        &rule_hash,
        &drafted_at,
    );
    write_atomic(&pollable, &body)?;

    Ok(EmitDriftProposalResponse {
        slug,
        pollable_path: pollable.to_string_lossy().into(),
        rule_hash,
        drafted_at,
    })
}

fn render_proposal_file(
    title: &str,
    repo_slug: &str,
    proposed_rule: &str,
    supporting: &[String],
    rule_hash: &str,
    drafted_at: &str,
) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str("---\n");
    let _ = writeln!(s, "title: {title}");
    let _ = writeln!(s, "repo: {repo_slug}");
    let _ = writeln!(s, "target_file: AGENTS.md");
    let supporting_csv = supporting.join(", ");
    let _ = writeln!(s, "supporting_dispatches: [{supporting_csv}]");
    let _ = writeln!(s, "rule_hash: {rule_hash}");
    let _ = writeln!(s, "drafted_at: {drafted_at}");
    s.push_str("---\n\n");
    s.push_str("## Proposed AGENTS.md addition\n\n");
    s.push_str(proposed_rule);
    if !proposed_rule.ends_with('\n') {
        s.push('\n');
    }
    s.push_str("\n## Supporting dispatches\n\n");
    for d in supporting {
        let _ = writeln!(s, "- {d}");
    }
    s
}

fn identity_hash(bytes: &[u8]) -> String {
    let mut h = DefaultHasher::new();
    h.write(bytes);
    format!("{:016x}", h.finish())
}

/// `~/.repo-recall/agents-drift/`. Override with
/// `REPO_RECALL_AGENTS_DRIFT_ROOT` for tests and out-of-home installs.
pub fn pollable_root() -> PathBuf {
    if let Ok(root) = std::env::var("REPO_RECALL_AGENTS_DRIFT_ROOT") {
        return PathBuf::from(root);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".repo-recall").join("agents-drift")
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

    /// Same isolation pattern as structural_asks: pollable_root reads a
    /// process-wide env var, so parallel tests in this module must
    /// serialize. See #121 for the same risk in dispatch_artifacts.
    fn test_lock() -> std::sync::MutexGuard<'static, ()> {
        static M: std::sync::Mutex<()> = std::sync::Mutex::new(());
        M.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn scratch_root() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "repo-recall-drift-{nanos}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("REPO_RECALL_AGENTS_DRIFT_ROOT", &dir);
        dir
    }

    #[test]
    fn emit_writes_pollable_with_proposal_and_provenance() {
        let _g = test_lock();
        let _root = scratch_root();
        let req = EmitDriftProposalRequest {
            repo_slug: "eco-mods".into(),
            title: "AGENTS.md: pin EcoModKit reference path".into(),
            proposed_rule: "EcoModKit references must use the canonical path X.".into(),
            supporting_dispatches: vec![
                "coilysiren/eco-mods#41".into(),
                "coilysiren/eco-mods#48".into(),
            ],
            slug: Some("test-drift".into()),
        };
        let resp = emit_drift_proposal(&req).expect("emit ok");
        assert!(resp.pollable_path.contains("eco-mods/test-drift.md"));
        let body = std::fs::read_to_string(&resp.pollable_path).unwrap();
        assert!(body.contains("title: AGENTS.md: pin"), "{body}");
        assert!(body.contains("repo: eco-mods"), "{body}");
        assert!(body.contains("target_file: AGENTS.md"), "{body}");
        assert!(body.contains("## Proposed AGENTS.md addition"), "{body}");
        assert!(body.contains("EcoModKit references must use"), "{body}");
        assert!(body.contains("coilysiren/eco-mods#41"), "{body}");
    }

    #[test]
    fn emit_refuses_duplicate_slug() {
        let _g = test_lock();
        let _root = scratch_root();
        let req = EmitDriftProposalRequest {
            repo_slug: "r".into(),
            title: "T".into(),
            proposed_rule: "rule".into(),
            supporting_dispatches: vec!["foo/bar#1".into()],
            slug: Some("dup".into()),
        };
        emit_drift_proposal(&req).expect("first ok");
        let err = emit_drift_proposal(&req).expect_err("dup err");
        assert!(matches!(err, EmitError::AlreadyExists(_)));
    }

    #[test]
    fn emit_scrubs_title_and_rule_before_write() {
        let _g = test_lock();
        let _root = scratch_root();
        let req = EmitDriftProposalRequest {
            repo_slug: "r".into(),
            title: "Rule about kai-server access".into(),
            proposed_rule: "Always ssh kai-server with ghp_AAAABBBBCCCCDDDDEEEEFFFFGGGGHHHHIIII." // trufflehog:ignore
                .into(),
            supporting_dispatches: vec!["foo/bar#2".into()],
            slug: Some("scrub-drift".into()),
        };
        let resp = emit_drift_proposal(&req).expect("emit ok");
        let body = std::fs::read_to_string(&resp.pollable_path).unwrap();
        assert!(!body.contains("kai-server"), "{body}");
        assert!(!body.contains("ghp_AAAA"), "{body}");
        assert!(body.contains("[REDACTED:internal-host]"), "{body}");
        assert!(body.contains("[REDACTED:github-token]"), "{body}");
    }

    #[test]
    fn emit_rejects_invalid_inputs() {
        let _g = test_lock();
        let _root = scratch_root();
        let base = EmitDriftProposalRequest {
            repo_slug: "r".into(),
            title: "T".into(),
            proposed_rule: "rule".into(),
            supporting_dispatches: vec!["foo/bar#1".into()],
            slug: None,
        };
        assert!(matches!(
            emit_drift_proposal(&EmitDriftProposalRequest {
                repo_slug: "  ".into(),
                ..base.clone()
            }),
            Err(EmitError::EmptyRepoSlug)
        ));
        assert!(matches!(
            emit_drift_proposal(&EmitDriftProposalRequest {
                title: "".into(),
                ..base.clone()
            }),
            Err(EmitError::EmptyTitle)
        ));
        assert!(matches!(
            emit_drift_proposal(&EmitDriftProposalRequest {
                proposed_rule: " ".into(),
                ..base.clone()
            }),
            Err(EmitError::EmptyRule)
        ));
        assert!(matches!(
            emit_drift_proposal(&EmitDriftProposalRequest {
                supporting_dispatches: vec![],
                ..base.clone()
            }),
            Err(EmitError::NoSupportingDispatches)
        ));
        assert!(matches!(
            emit_drift_proposal(&EmitDriftProposalRequest {
                supporting_dispatches: vec!["nothing-shaped-like-a-ref".into()],
                ..base
            }),
            Err(EmitError::InvalidRef(_))
        ));
    }
}
