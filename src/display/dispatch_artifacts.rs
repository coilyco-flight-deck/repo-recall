//! Dispatch-artifact emitter (#92, #107).
//!
//! The recall-dispatch planner calls into this module to write a
//! dispatch file in two places:
//!
//! 1. **In-repo, canonical.** `<repo>/docs/repo-dispatch/<slug>.md`.
//!    Write-once, frontmatter + prompt body. Ingested on next refresh
//!    by [`crate::ingest::docs::repo_dispatch`] and surfaced everywhere
//!    a repo-dispatch row appears. The caller is responsible for the
//!    eventual `git add && git commit` — repo-recall does not touch
//!    git on its own.
//! 2. **Pollable mirror.** `~/.repo-recall/dispatch/<repo>/<slug>.md`.
//!    A flat, OS-local layout a sub-agent runner can poll without
//!    needing to know which git repo each prompt belongs to.
//!    Identical body. Older mirrors stay where they are; no cleanup
//!    yet.
//!
//! Both writes are write-once: a 409 is returned if either path
//! already exists. Files are written via tmp + rename so a partial
//! crash never leaves a half-written dispatch on disk.

use std::hash::{DefaultHasher, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Inbound request shape (#92, #107).
#[derive(Debug, Clone, Deserialize)]
pub struct EmitDispatchRequest {
    /// `["owner/repo#N", ...]` — the tickets this dispatch addresses.
    /// At least one entry is required so the slug has something to
    /// hang the issue number on.
    pub issue_refs: Vec<String>,
    pub score: Option<i64>,
    pub autonomy_confidence: Option<i64>,
    pub autonomy_confidence_basis: Option<String>,
    /// Optional `"owner/repo#M"` for the thin tracking issue.
    pub tracking_issue: Option<String>,
    /// The emitted prompt body. Stored verbatim in the file.
    pub prompt: String,
    /// Optional slug override. When unset, derived from
    /// `<YYYY-MM-DD>-<first-issue-number>` plus a short hash.
    pub slug: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmitDispatchResponse {
    pub slug: String,
    pub in_repo_path: String,
    pub pollable_path: String,
    pub prompt_hash: String,
    pub dispatched_at: String,
}

#[derive(Debug, thiserror::Error)]
pub enum EmitError {
    #[error("issue_refs must contain at least one ref")]
    NoIssueRefs,
    #[error("invalid issue ref: {0}")]
    InvalidRef(String),
    #[error("dispatch slug already exists: {0}")]
    AlreadyExists(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Write the dispatch artifact to both target paths and return the
/// resolved metadata. `repo_path` is the on-disk repo root. `repo_slug`
/// is the human-readable name (e.g. `repo-recall`) used for the
/// pollable directory.
pub fn emit_dispatch(
    repo_path: &Path,
    repo_slug: &str,
    req: &EmitDispatchRequest,
) -> Result<EmitDispatchResponse, EmitError> {
    if req.issue_refs.is_empty() {
        return Err(EmitError::NoIssueRefs);
    }
    // Validate every issue_ref now so we don't write a half-broken file.
    for r in &req.issue_refs {
        if crate::process::join::gh_refs_with_issue_in_text(r).is_empty() {
            return Err(EmitError::InvalidRef(r.clone()));
        }
    }
    let now = chrono::Utc::now();
    let dispatched_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    // #110: scrub free-text fields before they reach any public-safe path.
    // The hash and slug are derived from the scrubbed body so the dispatch
    // ledger identifies what actually got written, not what was submitted.
    use crate::process::sanitize::{scrub, SanitizeSource};
    let scrubbed = EmitDispatchRequest {
        issue_refs: req.issue_refs.clone(),
        score: req.score,
        autonomy_confidence: req.autonomy_confidence,
        autonomy_confidence_basis: req
            .autonomy_confidence_basis
            .as_ref()
            .map(|b| scrub(b, SanitizeSource::Frontmatter)),
        tracking_issue: req.tracking_issue.clone(),
        prompt: scrub(&req.prompt, SanitizeSource::DispatchArtifact),
        slug: req.slug.clone(),
    };
    let req = &scrubbed;

    let prompt_hash = prompt_identity_hash(req.prompt.as_bytes());

    let primary_issue = first_issue_number(&req.issue_refs);
    let slug = req.slug.clone().unwrap_or_else(|| {
        let date = now.format("%Y-%m-%d");
        let n = primary_issue.unwrap_or(0);
        let short = &prompt_hash[..prompt_hash.len().min(7)];
        format!("{date}-{n}-{short}")
    });

    let in_repo = repo_path
        .join("docs/repo-dispatch")
        .join(format!("{slug}.md"));
    let pollable_root = pollable_root();
    let pollable = pollable_root.join(repo_slug).join(format!("{slug}.md"));

    if in_repo.exists() {
        return Err(EmitError::AlreadyExists(in_repo.to_string_lossy().into()));
    }
    if pollable.exists() {
        return Err(EmitError::AlreadyExists(pollable.to_string_lossy().into()));
    }

    let body = render_dispatch_file(req, &prompt_hash, &dispatched_at);
    write_atomic(&in_repo, &body)?;
    write_atomic(&pollable, &body)?;

    Ok(EmitDispatchResponse {
        slug,
        in_repo_path: in_repo.to_string_lossy().into(),
        pollable_path: pollable.to_string_lossy().into(),
        prompt_hash,
        dispatched_at,
    })
}

/// Render the markdown body. Mirrors the parser in
/// `ingest::docs::repo_dispatch` exactly so a round-trip is stable.
fn render_dispatch_file(
    req: &EmitDispatchRequest,
    prompt_hash: &str,
    dispatched_at: &str,
) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    s.push_str("---\n");
    let refs = req.issue_refs.join(", ");
    let _ = writeln!(s, "issue_refs: [{refs}]");
    if let Some(score) = req.score {
        let _ = writeln!(s, "score: {score}");
    }
    if let Some(c) = req.autonomy_confidence {
        let _ = writeln!(s, "autonomy_confidence: {c}");
    }
    if let Some(basis) = &req.autonomy_confidence_basis {
        let _ = writeln!(s, "autonomy_confidence_basis: {basis}");
    }
    let _ = writeln!(s, "prompt_hash: {prompt_hash}");
    let _ = writeln!(s, "dispatched_at: {dispatched_at}");
    if let Some(t) = &req.tracking_issue {
        let _ = writeln!(s, "tracking_issue: {t}");
    }
    s.push_str("---\n");
    s.push_str(&req.prompt);
    if !req.prompt.ends_with('\n') {
        s.push('\n');
    }
    s
}

fn first_issue_number(refs: &[String]) -> Option<u32> {
    refs.iter()
        .filter_map(|r| crate::process::join::gh_refs_with_issue_in_text(r).pop())
        .map(|r| r.issue)
        .next()
}

/// Stable 64-bit identity hash over the prompt body. Used to detect
/// "is this still the same prompt I read?" in the dispatch ledger.
/// SipHash-1-3 isn't a cryptographic primitive, but the use case here
/// is identity inside a single-operator repo, not adversarial
/// collision resistance. Hex-encoded so it's grep-friendly in the
/// frontmatter.
fn prompt_identity_hash(bytes: &[u8]) -> String {
    let mut h = DefaultHasher::new();
    h.write(bytes);
    format!("{:016x}", h.finish())
}

/// `~/.repo-recall/dispatch/`. Override with `REPO_RECALL_DISPATCH_ROOT`
/// for tests and out-of-home installations.
pub fn pollable_root() -> PathBuf {
    if let Ok(root) = std::env::var("REPO_RECALL_DISPATCH_ROOT") {
        return PathBuf::from(root);
    }
    let home = dirs_home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".repo-recall").join("dispatch")
}

fn dirs_home_dir() -> Option<PathBuf> {
    // Resolve $HOME on Unix / %USERPROFILE% on Windows without
    // pulling in a `dirs` crate just for one lookup.
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
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

    fn scratch_dir() -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "repo-recall-emit-{nanos}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn emit_writes_both_files_and_round_trips_through_parser() {
        let scratch = scratch_dir();
        let repo = scratch.join("repo");
        let pollable_root = scratch.join("pollable");
        std::env::set_var("REPO_RECALL_DISPATCH_ROOT", &pollable_root);
        std::fs::create_dir_all(&repo).unwrap();

        let req = EmitDispatchRequest {
            issue_refs: vec!["coilysiren/repo-recall#92".into()],
            score: Some(5),
            autonomy_confidence: Some(4),
            autonomy_confidence_basis: Some("substrate is indexed".into()),
            tracking_issue: Some("coilysiren/repo-recall#999".into()),
            prompt: "do the thing\n".into(),
            slug: Some("test-slug".into()),
        };
        let resp = emit_dispatch(&repo, "repo-recall", &req).expect("emit ok");
        assert_eq!(resp.slug, "test-slug");
        let in_repo = std::path::PathBuf::from(&resp.in_repo_path);
        assert!(in_repo.exists());
        let pollable = std::path::PathBuf::from(&resp.pollable_path);
        assert!(pollable.exists());

        // Round-trip via the parser to verify the schema matches the
        // ingest contract.
        let parsed = crate::ingest::docs::repo_dispatch::parse_dispatch_file(&in_repo, &repo)
            .expect("parse");
        assert_eq!(parsed.slug, "test-slug");
        assert_eq!(parsed.score, Some(5));
        assert_eq!(parsed.autonomy_confidence, Some(4));
        assert_eq!(
            parsed.issue_refs,
            vec![("coilysiren".into(), "repo-recall".into(), 92u32)]
        );
        assert_eq!(
            parsed.tracking_issue,
            Some(("coilysiren".into(), "repo-recall".into(), 999u32))
        );
        assert!(parsed.dispatched_at.unwrap() > 1_700_000_000);
    }

    #[test]
    fn refuses_to_overwrite() {
        let scratch = scratch_dir();
        let repo = scratch.join("repo");
        let pollable_root = scratch.join("pollable");
        std::env::set_var("REPO_RECALL_DISPATCH_ROOT", &pollable_root);
        std::fs::create_dir_all(&repo).unwrap();

        let req = EmitDispatchRequest {
            issue_refs: vec!["foo/bar#1".into()],
            score: None,
            autonomy_confidence: None,
            autonomy_confidence_basis: None,
            tracking_issue: None,
            prompt: "body".into(),
            slug: Some("dup".into()),
        };
        emit_dispatch(&repo, "repo-recall", &req).expect("first ok");
        let err = emit_dispatch(&repo, "repo-recall", &req).expect_err("dup err");
        assert!(matches!(err, EmitError::AlreadyExists(_)));
    }

    /// #110: every public-write path goes through `process::sanitize::scrub`.
    /// The dispatched body must not carry through known-bad terms even if a
    /// caller submits them.
    #[test]
    fn emit_scrubs_body_and_basis_before_writing() {
        let scratch = scratch_dir();
        let repo = scratch.join("repo");
        let pollable_root = scratch.join("pollable");
        std::env::set_var("REPO_RECALL_DISPATCH_ROOT", &pollable_root);
        std::fs::create_dir_all(&repo).unwrap();

        let req = EmitDispatchRequest {
            issue_refs: vec!["coilysiren/repo-recall#110".into()],
            score: None,
            autonomy_confidence: None,
            autonomy_confidence_basis: Some("read coilyco-vault for context".into()),
            tracking_issue: None,
            prompt: "ssh kai-server with ghp_AAAABBBBCCCCDDDDEEEEFFFFGGGGHHHHIIII please".into(),
            slug: Some("scrub-proof".into()),
        };
        let resp = emit_dispatch(&repo, "repo-recall", &req).expect("emit ok");
        let in_repo_body = std::fs::read_to_string(&resp.in_repo_path).unwrap();
        let pollable_body = std::fs::read_to_string(&resp.pollable_path).unwrap();
        for body in [&in_repo_body, &pollable_body] {
            assert!(!body.contains("kai-server"), "{body}");
            assert!(!body.contains("ghp_AAAA"), "{body}");
            assert!(!body.contains("coilyco-vault"), "{body}");
            assert!(body.contains("[REDACTED:internal-host]"), "{body}");
            assert!(body.contains("[REDACTED:github-token]"), "{body}");
            assert!(body.contains("[REDACTED:vault-path]"), "{body}");
        }
    }
}
