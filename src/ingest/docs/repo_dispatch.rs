//! `docs/repo-dispatch/` ingest source. Designed in #92.
//!
//! Each dispatch the recall-dispatch planner emits is written as a
//! write-once markdown file under `docs/repo-dispatch/`. Status updates
//! live on a thin GitHub tracking issue (`label: repo-dispatch`),
//! never on the file itself.
//!
//! Frontmatter shape:
//!
//! ```yaml
//! ---
//! issue_refs: [owner/repo#N]
//! score: 4
//! autonomy_confidence: 3
//! autonomy_confidence_basis: <one sentence>
//! prompt_hash: <sha256>
//! dispatched_at: <RFC3339>
//! tracking_issue: owner/repo#M
//! ---
//! <prompt body, verbatim>
//! ```
//!
//! This module parses one such file, plus the health source that
//! reports presence/quality of `docs/repo-dispatch/` for a given repo.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ingest::health::{Health, IngestSource, Report};

const DISPATCH_DIR: &str = "docs/repo-dispatch";

/// One dispatch record, parsed from a single `.md` file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchRecord {
    /// File path relative to the repo root (e.g.
    /// `docs/repo-dispatch/2026-05-12-92-magnum-opus.md`).
    pub file_path: String,
    /// Filename stem, used as the natural key against the tracking issue.
    pub slug: String,
    /// Cited issue refs as `(owner, repo, issue_number)`.
    pub issue_refs: Vec<(String, String, u32)>,
    /// Optional triage score (1-5, but not validated here).
    pub score: Option<i64>,
    /// Optional substrate-derived AFK confidence (1-5).
    pub autonomy_confidence: Option<i64>,
    /// One-sentence justification of the confidence score.
    pub autonomy_confidence_basis: Option<String>,
    /// sha256 of the emitted prompt body, for "is this still the
    /// dispatch I read?" checks.
    pub prompt_hash: Option<String>,
    /// RFC3339 dispatch time as unix seconds, when parseable.
    pub dispatched_at: Option<i64>,
    /// `(owner, repo, issue_number)` of the thin tracking issue.
    pub tracking_issue: Option<(String, String, u32)>,
}

/// Walk a repo's `docs/repo-dispatch/` directory and return every
/// successfully-parsed dispatch record. Files that fail to parse are
/// logged at `debug!` and skipped — the source's health report
/// downgrades to Yellow when at least one file failed, but a bad file
/// must not hide the good ones.
pub fn dispatches_for_repo(repo_path: &Path) -> (Vec<DispatchRecord>, usize) {
    let dir = repo_path.join(DISPATCH_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return (Vec::new(), 0),
    };
    let mut out = Vec::new();
    let mut errors = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        match parse_dispatch_file(&path, repo_path) {
            Some(rec) => out.push(rec),
            None => {
                tracing::debug!("repo-dispatch parse failed: {}", path.display());
                errors += 1;
            }
        }
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.dispatched_at));
    (out, errors)
}

/// Parse a single dispatch file. `repo_path` is used to compute the
/// stored relative path. Returns `None` when the file is unreadable or
/// missing required fields (slug derived from filename is the only
/// truly required field; everything else is optional).
pub fn parse_dispatch_file(path: &Path, repo_path: &Path) -> Option<DispatchRecord> {
    let content = std::fs::read_to_string(path).ok()?;
    let frontmatter = extract_frontmatter(&content)?;
    let slug = path.file_stem()?.to_str()?.to_string();
    let relative = relative_path(path, repo_path);
    let mut rec = DispatchRecord {
        file_path: relative,
        slug,
        issue_refs: Vec::new(),
        score: None,
        autonomy_confidence: None,
        autonomy_confidence_basis: None,
        prompt_hash: None,
        dispatched_at: None,
        tracking_issue: None,
    };
    for (key, value) in frontmatter {
        match key.as_str() {
            "issue_refs" => rec.issue_refs = parse_ref_list(&value),
            "score" => rec.score = value.parse().ok(),
            "autonomy_confidence" => rec.autonomy_confidence = value.parse().ok(),
            "autonomy_confidence_basis" => rec.autonomy_confidence_basis = Some(value),
            "prompt_hash" => rec.prompt_hash = Some(value),
            "dispatched_at" => {
                rec.dispatched_at = chrono::DateTime::parse_from_rfc3339(&value)
                    .ok()
                    .map(|d| d.timestamp());
            }
            "tracking_issue" => rec.tracking_issue = parse_single_ref(&value),
            _ => {}
        }
    }
    Some(rec)
}

fn relative_path(path: &Path, repo_path: &Path) -> String {
    path.strip_prefix(repo_path)
        .map(PathBuf::from)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .replace('\\', "/")
}

/// Pull the `key: value` lines between `---` markers at the top of the
/// file. Strips quotes from scalar values. Multi-line values are not
/// supported — the dispatch frontmatter is intentionally one-line-per-field.
fn extract_frontmatter(content: &str) -> Option<Vec<(String, String)>> {
    let rest = content.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let block = &rest[..end];
    let mut out = Vec::new();
    for line in block.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (k, v) = line.split_once(':')?;
        let key = k.trim().to_string();
        let value = strip_quotes(v.trim()).to_string();
        out.push((key, value));
    }
    Some(out)
}

fn strip_quotes(s: &str) -> &str {
    let (open, close) = (s.starts_with('"'), s.ends_with('"'));
    if open && close && s.len() >= 2 {
        return &s[1..s.len() - 1];
    }
    let (open, close) = (s.starts_with('\''), s.ends_with('\''));
    if open && close && s.len() >= 2 {
        return &s[1..s.len() - 1];
    }
    s
}

/// Parse a YAML-style list of `owner/repo#N` refs. Tolerates both the
/// inline form `[a/b#1, c/d#2]` and the block form (one entry per line).
fn parse_ref_list(value: &str) -> Vec<(String, String, u32)> {
    let inner = value
        .trim()
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(value);
    let mut refs = Vec::new();
    for part in inner.split(',') {
        if let Some(r) = parse_single_ref(part.trim()) {
            refs.push(r);
        }
    }
    refs
}

fn parse_single_ref(value: &str) -> Option<(String, String, u32)> {
    let mut hits = crate::process::join::gh_refs_with_issue_in_text(value);
    let first = hits.pop()?;
    Some((first.owner, first.repo, first.issue))
}

/// IngestSource entry for the dispatch-file directory. Health is:
/// Green when files exist and all parsed; Yellow when at least one
/// failed to parse but others succeeded; Red when the directory is
/// missing. An empty directory is Yellow ("present but no records").
pub struct RepoDispatchSource;

impl IngestSource for RepoDispatchSource {
    fn id(&self) -> &'static str {
        "docs.repo_dispatch"
    }

    fn label(&self) -> &'static str {
        "docs/repo-dispatch/"
    }

    fn report(&self, repo_path: &Path) -> Option<Report> {
        let dir = repo_path.join(DISPATCH_DIR);
        if !dir.exists() {
            return Some(Report {
                source_id: self.id(),
                health: Health::Red,
                reason: "no docs/repo-dispatch/ directory".into(),
            });
        }
        let (records, errors) = dispatches_for_repo(repo_path);
        let report = if records.is_empty() && errors == 0 {
            Report {
                source_id: self.id(),
                health: Health::Yellow,
                reason: "docs/repo-dispatch/ exists but empty".into(),
            }
        } else if errors > 0 {
            Report {
                source_id: self.id(),
                health: Health::Yellow,
                reason: format!("{} parsed, {errors} failed to parse", records.len()),
            }
        } else {
            Report {
                source_id: self.id(),
                health: Health::Green,
                reason: format!("{} dispatch record(s)", records.len()),
            }
        };
        Some(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
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
            "repo-recall-dispatch-{nanos}-{}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(p: &Path, body: &str) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let mut f = fs::File::create(p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn parses_full_frontmatter() {
        let repo = scratch_dir();
        let path = repo.join("docs/repo-dispatch/2026-05-12-92-magnum-opus.md");
        write(
            &path,
            "---\n\
             issue_refs: [coilysiren/repo-recall#92]\n\
             score: 5\n\
             autonomy_confidence: 4\n\
             autonomy_confidence_basis: substrate is now indexed end-to-end\n\
             prompt_hash: deadbeef\n\
             dispatched_at: 2026-05-12T21:14:00Z\n\
             tracking_issue: coilysiren/repo-recall#999\n\
             ---\n\
             prompt body goes here\n",
        );
        let rec = parse_dispatch_file(&path, &repo).expect("parse");
        assert_eq!(rec.slug, "2026-05-12-92-magnum-opus");
        assert_eq!(rec.score, Some(5));
        assert_eq!(rec.autonomy_confidence, Some(4));
        assert_eq!(rec.prompt_hash.as_deref(), Some("deadbeef"));
        assert_eq!(
            rec.issue_refs,
            vec![("coilysiren".into(), "repo-recall".into(), 92u32)]
        );
        assert_eq!(
            rec.tracking_issue,
            Some(("coilysiren".into(), "repo-recall".into(), 999u32))
        );
        assert!(rec.dispatched_at.unwrap() > 1_700_000_000);
    }

    #[test]
    fn red_when_directory_missing() {
        let repo = scratch_dir();
        let r = RepoDispatchSource.report(&repo).expect("applies");
        assert_eq!(r.health, Health::Red);
    }

    #[test]
    fn green_when_one_valid_file() {
        let repo = scratch_dir();
        write(
            &repo.join("docs/repo-dispatch/x.md"),
            "---\nissue_refs: [foo/bar#1]\n---\nbody\n",
        );
        let r = RepoDispatchSource.report(&repo).expect("applies");
        assert_eq!(r.health, Health::Green);
    }

    #[test]
    fn yellow_when_present_but_empty() {
        let repo = scratch_dir();
        fs::create_dir_all(repo.join("docs/repo-dispatch")).unwrap();
        let r = RepoDispatchSource.report(&repo).expect("applies");
        assert_eq!(r.health, Health::Yellow);
    }
}
