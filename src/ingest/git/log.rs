//! Git-subprocess helpers. `scan()` pulls `git log`; `remote_info()` pulls
//! the default-branch + origin URL. We shell out rather than linking libgit2:

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

/// One commit — enough to power a recent-activity list plus a join key
/// surface (parents, refs, committer identity, full body).
#[derive(Debug, Clone)]
pub struct CommitRecord {
    pub sha: String,
    pub author_name: String,
    pub author_email: String,
    pub timestamp: i64,
    pub subject: String,
    /// Committer name (%cn). Often equal to the author for solo work, but
    /// diverges on rebased, cherry-picked, or partner-pushed history.
    pub committer_name: String,
    /// Committer email (%ce).
    pub committer_email: String,
    /// Committer date as strict ISO-8601 (%cI). Strings, not unix seconds,
    /// because committer date is what GitHub displays and ISO survives
    pub committer_date_iso: String,
    /// Parent SHAs, space-separated as git emits them (%P). Empty for
    /// root commits, two-or-more for merges.
    pub parents: String,
    /// Decorated ref names (%D). Tag and branch tips that point at this
    /// commit, comma+space separated. Empty when undecorated.
    pub refs: String,
    /// Full commit body (%B). Includes the subject line and any trailing
    /// paragraphs. Stored verbatim so closes/refs trailers stay parseable.
    pub body: String,
}

/// Run `git log` in `repo_path` and parse the last `limit` commits across all
/// refs. Merges are excluded — they clutter the feed without adding signal.
pub fn scan(repo_path: &Path, limit: usize) -> Result<Vec<CommitRecord>> {
    let path_str = repo_path.to_str().context("repo path is not valid utf-8")?;

    // Field separator is NUL (\x00); record separator is RS (\x1e). RS
    // matters because %B (full body) is multi-line — splitting records on
    let output = Command::new("git")
        .args([
            "-C",
            path_str,
            "log",
            "--all",
            "--no-merges",
            "-n",
            &limit.to_string(),
            "--use-mailmap",
            "--format=%H%x00%at%x00%aN%x00%aE%x00%cn%x00%ce%x00%cI%x00%P%x00%D%x00%s%x00%B%x1e",
        ])
        .output()
        .with_context(|| format!("failed to invoke git in {}", repo_path.display()))?;

    if !output.status.success() {
        // Log and move on — a broken repo shouldn't kill the whole refresh.
        tracing::debug!(
            "git log failed in {}: {}",
            repo_path.display(),
            String::from_utf8_lossy(&output.stderr).trim(),
        );
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for raw_record in stdout.split('\x1e') {
        let record = raw_record.trim_start_matches('\n');
        if record.is_empty() {
            continue;
        }
        let parts: Vec<&str> = record.splitn(11, '\0').collect();
        if parts.len() != 11 {
            tracing::debug!(
                "skip malformed git log record in {}: {record:?}",
                repo_path.display()
            );
            continue;
        }
        let Ok(ts) = parts[1].parse::<i64>() else {
            continue;
        };
        out.push(CommitRecord {
            sha: parts[0].to_string(),
            timestamp: ts,
            author_name: parts[2].to_string(),
            author_email: parts[3].to_string(),
            committer_name: parts[4].to_string(),
            committer_email: parts[5].to_string(),
            committer_date_iso: parts[6].to_string(),
            parents: parts[7].to_string(),
            refs: parts[8].to_string(),
            subject: parts[9].to_string(),
            body: parts[10].to_string(),
        });
    }
    Ok(out)
}

/// Origin metadata for a repo — raw normalized base URL (suitable for
/// building `.../tree/<branch>` links) and the short default branch name.
#[derive(Debug, Clone, Default)]
pub struct RemoteInfo {
    pub url: Option<String>,
    pub default_branch: Option<String>,
}

pub fn remote_info(repo_path: &Path) -> RemoteInfo {
    let Some(path_str) = repo_path.to_str() else {
        return RemoteInfo::default();
    };

    let url = Command::new("git")
        .args(["-C", path_str, "remote", "get-url", "origin"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .and_then(|raw| normalize_remote_url(&raw));

    // `symbolic-ref refs/remotes/origin/HEAD` prints e.g. `refs/remotes/origin/main`.
    // It's purely local — no network hit — and fails cleanly when unset.
    let default_branch = Command::new("git")
        .args(["-C", path_str, "symbolic-ref", "refs/remotes/origin/HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .and_then(|s| s.strip_prefix("refs/remotes/origin/").map(str::to_string));

    RemoteInfo {
        url,
        default_branch,
    }
}

#[derive(Debug, Clone)]
pub struct FileChange {
    pub sha: String,
    pub author_email: String,
    pub timestamp: i64,
    pub file_path: String,
    pub additions: i64,
    pub deletions: i64,
    /// When git detected a rename for this row (via `-M`), the old path
    /// before the move. `None` for non-rename changes.
    pub rename_from: Option<String>,
}

/// Parse a numstat path that may carry a git rename annotation. Without
/// `-z`, git emits renames in two shapes:
pub fn parse_numstat_path(raw: &str) -> (Option<String>, String) {
    if let (Some(open), Some(close)) = (raw.find('{'), raw.find('}')) {
        if close > open {
            let inside = &raw[open + 1..close];
            if let Some(arrow) = inside.find(" => ") {
                let prefix = &raw[..open];
                let suffix = &raw[close + 1..];
                let old = format!("{prefix}{}{suffix}", &inside[..arrow]);
                let new = format!("{prefix}{}{suffix}", &inside[arrow + 4..]);
                let old = old.replace("//", "/");
                let new = new.replace("//", "/");
                return (Some(old), new);
            }
        }
    }
    if let Some(arrow) = raw.find(" => ") {
        let old = raw[..arrow].to_string();
        let new = raw[arrow + 4..].to_string();
        return (Some(old), new);
    }
    (None, raw.to_string())
}

/// Walk `git log --numstat` in a single subprocess per repo and return one
/// `FileChange` per (commit, file) pair. Merges excluded; binary rows
pub fn file_changes_since(repo_path: &Path, since_ts: i64) -> Vec<FileChange> {
    let Some(path_str) = repo_path.to_str() else {
        return Vec::new();
    };
    let output = match Command::new("git")
        .args([
            "-C",
            path_str,
            "log",
            &format!("--since=@{since_ts}"),
            "--no-merges",
            "-M",
            "--pretty=format:H|%H|%at|%ae",
            "--numstat",
        ])
        .output()
    {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            tracing::debug!(
                "git log --numstat failed in {}: {}",
                repo_path.display(),
                String::from_utf8_lossy(&o.stderr).trim(),
            );
            return Vec::new();
        }
        Err(e) => {
            tracing::debug!("git subprocess failed in {}: {e}", repo_path.display());
            return Vec::new();
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    let mut cur_sha = String::new();
    let mut cur_ts: i64 = 0;
    let mut cur_email = String::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("H|") {
            // Commit header row: H|sha|timestamp|email.
            let mut parts = rest.splitn(3, '|');
            cur_sha = parts.next().unwrap_or("").to_string();
            cur_ts = parts
                .next()
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            cur_email = parts.next().unwrap_or("").to_string();
            continue;
        }
        // Numstat row: `<adds>\t<dels>\t<path>`. Binary files = `-\t-\t…`.
        let mut parts = line.splitn(3, '\t');
        let Some(add_s) = parts.next() else { continue };
        let Some(del_s) = parts.next() else { continue };
        let Some(path) = parts.next() else { continue };
        let Ok(add) = add_s.parse::<i64>() else {
            continue;
        };
        let Ok(del) = del_s.parse::<i64>() else {
            continue;
        };
        let (rename_from, file_path) = parse_numstat_path(path);
        out.push(FileChange {
            sha: cur_sha.clone(),
            author_email: cur_email.clone(),
            timestamp: cur_ts,
            file_path,
            additions: add,
            deletions: del,
            rename_from,
        });
    }
    out
}

/// Legacy helper kept for backward compatibility with callers that just
/// want the total. Internally sums the per-file rows.
pub fn churn_since(repo_path: &Path, since_ts: i64) -> i64 {
    let Some(path_str) = repo_path.to_str() else {
        return 0;
    };
    // `--pretty=format:` suppresses the per-commit header so stdout is pure
    // numstat rows. `--since=@<unix>` is git's epoch-time form.
    let output = match Command::new("git")
        .args([
            "-C",
            path_str,
            "log",
            &format!("--since=@{since_ts}"),
            "--no-merges",
            "--pretty=format:",
            "--numstat",
        ])
        .output()
    {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            tracing::debug!(
                "git log --numstat failed in {}: {}",
                repo_path.display(),
                String::from_utf8_lossy(&o.stderr).trim(),
            );
            return 0;
        }
        Err(e) => {
            tracing::debug!("git subprocess failed in {}: {e}", repo_path.display());
            return 0;
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut total: i64 = 0;
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let add = parts
            .next()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        let del = parts
            .next()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0);
        total += add + del;
    }
    total
}

/// Local-state snapshot of a repo — everything we can learn from plain `git`
/// subprocess calls that changes between refreshes. One struct, one refresh
#[derive(Debug, Clone, Default)]
pub struct LocalState {
    pub commits_ahead: i64,
    pub commits_behind: i64,
    pub stash_count: i64,
    /// Short ref name (e.g. "main") or the literal string "detached".
    pub head_ref: Option<String>,
    /// `rebase` / `merge` / `cherry-pick` / `bisect` / `revert` when there's
    /// an interrupted operation in `.git/`. `None` when clean.
    pub in_progress_op: Option<String>,
}

pub fn local_state(repo_path: &Path) -> LocalState {
    let Some(path_str) = repo_path.to_str() else {
        return LocalState::default();
    };
    let git = |args: &[&str]| -> Option<String> {
        let mut full = vec!["-C", path_str];
        full.extend_from_slice(args);
        let out = Command::new("git").args(full).output().ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };

    // HEAD: symbolic ref gives branch name; failure means detached.
    let head_ref = git(&["symbolic-ref", "--quiet", "--short", "HEAD"]).or_else(|| {
        // Distinguish "detached" from "unborn HEAD" (brand-new empty repo):
        // the latter fails both symbolic-ref and rev-parse HEAD.
        git(&["rev-parse", "--verify", "HEAD"]).map(|_| "detached".to_string())
    });

    // ahead/behind upstream via `rev-list --left-right --count @{u}...HEAD`.
    // That prints `<behind>\t<ahead>` — count of commits upstream has that
    let (behind, ahead) = git(&["rev-list", "--left-right", "--count", "@{u}...HEAD"])
        .and_then(|s| {
            let mut parts = s.split_whitespace();
            let b: i64 = parts.next()?.parse().ok()?;
            let a: i64 = parts.next()?.parse().ok()?;
            Some((b, a))
        })
        .unwrap_or((0, 0));

    let stash_count = git(&["stash", "list"])
        .map(|s| s.lines().filter(|l| !l.is_empty()).count() as i64)
        .unwrap_or(0);

    // `.git/` state files indicate an interrupted operation. Check in order
    // of how common they are. `git_dir` handles worktrees.
    let in_progress_op = git(&["rev-parse", "--git-dir"]).and_then(|git_dir| {
        let g = std::path::Path::new(&git_dir);
        let checks: &[(&str, &str)] = &[
            ("rebase", "rebase-merge"),
            ("rebase", "rebase-apply"),
            ("merge", "MERGE_HEAD"),
            ("cherry-pick", "CHERRY_PICK_HEAD"),
            ("revert", "REVERT_HEAD"),
            ("bisect", "BISECT_LOG"),
        ];
        for (op, marker) in checks {
            if g.join(marker).exists() {
                return Some((*op).to_string());
            }
        }
        None
    });

    LocalState {
        commits_ahead: ahead,
        commits_behind: behind,
        stash_count,
        head_ref,
        in_progress_op,
    }
}

/// Local branches that look like unmerged work left sitting: their tip
/// commit is older than `older_than_secs` and they are not fully merged
pub fn stale_branches(repo_path: &Path, older_than_secs: i64) -> Vec<crate::db::StaleBranch> {
    let Some(path_str) = repo_path.to_str() else {
        return Vec::new();
    };
    let git = |args: &[&str]| -> Option<String> {
        let mut full = vec!["-C", path_str];
        full.extend_from_slice(args);
        let out = Command::new("git").args(full).output().ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    };

    // Default branch: prefer origin/HEAD, fall back to a local main/master.
    let default = git(&["symbolic-ref", "refs/remotes/origin/HEAD"])
        .and_then(|s| {
            s.trim()
                .strip_prefix("refs/remotes/origin/")
                .map(str::to_string)
        })
        .or_else(|| {
            ["main", "master"]
                .into_iter()
                .find(|cand| {
                    git(&[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("refs/heads/{cand}"),
                    ])
                    .is_some()
                })
                .map(str::to_string)
        });
    let Some(default) = default else {
        return Vec::new();
    };

    // Branches already merged into the default branch - excluded.
    let merged: std::collections::HashSet<String> = git(&[
        "for-each-ref",
        "--merged",
        &default,
        "--format=%(refname:short)",
        "refs/heads",
    ])
    .map(|s| s.lines().map(|l| l.trim().to_string()).collect())
    .unwrap_or_default();

    let now = chrono::Utc::now().timestamp();
    let Some(listing) = git(&[
        "for-each-ref",
        "--format=%(refname:short)%00%(committerdate:unix)",
        "refs/heads",
    ]) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for line in listing.lines() {
        let mut parts = line.splitn(2, '\0');
        let name = parts.next().unwrap_or("").trim();
        if name.is_empty() || name == default || merged.contains(name) {
            continue;
        }
        let Some(ts) = parts.next().and_then(|s| s.trim().parse::<i64>().ok()) else {
            continue;
        };
        let age = now - ts;
        if age > older_than_secs {
            out.push(crate::db::StaleBranch {
                name: name.to_string(),
                tip_age_secs: age,
            });
        }
    }
    // Oldest tip first - the most-stale branch is the one most worth landing
    // or deleting.
    out.sort_by_key(|b| std::cmp::Reverse(b.tip_age_secs));
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Untracked,
    Modified,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Untracked => "untracked",
            FileKind::Modified => "modified",
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorktreeFile {
    pub path: String,
    pub kind: FileKind,
}

/// Working-tree snapshot. Full counts for every dirty file in the tree, plus
/// a capped sample of the individual paths (so the dashboard can show a few
#[derive(Debug, Clone, Default)]
pub struct WorktreeSnapshot {
    pub files: Vec<WorktreeFile>,
    pub total_untracked: i64,
    pub total_modified: i64,
}

impl WorktreeSnapshot {
    pub fn total(&self) -> i64 {
        self.total_untracked + self.total_modified
    }
}

/// Run `git status --porcelain=v1 -uall` and return counts + the first
/// `paths_cap` file paths. Format (from git docs): each line is `XY <path>`
pub fn worktree_snapshot(repo_path: &Path, paths_cap: usize) -> WorktreeSnapshot {
    let Some(path_str) = repo_path.to_str() else {
        return WorktreeSnapshot::default();
    };
    let output = match Command::new("git")
        .args(["-C", path_str, "status", "--porcelain=v1", "-uall"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            tracing::debug!(
                "git status failed in {}: {}",
                repo_path.display(),
                String::from_utf8_lossy(&o.stderr).trim(),
            );
            return WorktreeSnapshot::default();
        }
        Err(e) => {
            tracing::debug!("git subprocess failed in {}: {e}", repo_path.display());
            return WorktreeSnapshot::default();
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut snap = WorktreeSnapshot::default();
    for line in stdout.lines() {
        if line.len() < 4 {
            continue;
        }
        // Porcelain v1: "XY path" — two status chars, a space, then the path.
        // Rename lines look like `R  old -> new`; take the final path.
        let kind = if line.starts_with("??") {
            FileKind::Untracked
        } else {
            FileKind::Modified
        };
        let rest = &line[3..];
        let path = rest.rsplit(" -> ").next().unwrap_or(rest).trim();
        match kind {
            FileKind::Untracked => snap.total_untracked += 1,
            FileKind::Modified => snap.total_modified += 1,
        }
        if snap.files.len() < paths_cap {
            snap.files.push(WorktreeFile {
                path: path.to_string(),
                kind,
            });
        }
    }
    // Index-stat-stale phantom-dirty: `git status` reports modified files
    // whose worktree content is byte-identical to the index, just because
    if snap.total_modified > 0 && unstaged_diff_is_empty(repo_path) {
        snap.total_modified = 0;
        snap.files.retain(|f| f.kind == FileKind::Untracked);
    }
    snap
}

/// True when `git diff --quiet` exits 0 (no unstaged differences). Any other
/// outcome — real diff, subprocess failure, weird repo state — returns false
fn unstaged_diff_is_empty(repo_path: &Path) -> bool {
    let Some(path_str) = repo_path.to_str() else {
        return false;
    };
    let Ok(status) = Command::new("git")
        .args(["-C", path_str, "diff", "--quiet"])
        .status()
    else {
        return false;
    };
    status.success()
}

/// Locate the repo's deploy workflow on disk. We sniff
/// `.github/workflows/*.{yml,yaml}` for a basename containing "deploy"
pub fn find_deploy_workflow(repo_path: &Path) -> Option<String> {
    let dir = repo_path.join(".github").join("workflows");
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut matches: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let lower = name.to_lowercase();
            if (lower.ends_with(".yml") || lower.ends_with(".yaml")) && lower.contains("deploy") {
                Some(name)
            } else {
                None
            }
        })
        .collect();
    matches.sort();
    matches.into_iter().next()
}

/// Latest-deploy health for a repo. `status` is the *last* run's outcome,
/// `last_success_ts` is the unix-seconds timestamp of the most recent
#[derive(Debug, Clone, Default)]
pub struct DeployHealth {
    pub status: Option<String>,
    pub last_success_ts: Option<i64>,
}

/// Aggregated open-PR counts for one repo. Derived client-side from a
/// single `gh pr list --json` call so we only pay one subprocess per repo
#[derive(Debug, Clone, Default)]
pub struct PrCounts {
    pub open: i64,
    pub draft: i64,
    pub awaiting_my_review: i64,
    /// Your open non-draft PRs that *do* have a reviewer requested. Ball is
    /// in the reviewer's court - informational, not action-required.
    pub mine_awaiting_review: i64,
    /// Your open non-draft PRs with zero reviewers requested. You are the
    /// blocker (request a reviewer, or self-merge on a solo repo).
    pub mine_no_reviewer: i64,
    /// Open draft PRs authored by the viewer. Subset of `draft`. Drives the
    /// "get this into a reviewable state" action-required signal.
    pub my_draft: i64,
}

/// Issue counts for one repo. `open` is the repo total; `assigned_to_me` is
/// the subset assigned to the authenticated viewer (matched on `gh` login).
#[derive(Debug, Clone, Copy, Default)]
pub struct IssueCounts {
    pub open: i64,
    pub assigned_to_me: i64,
}

/// Fetch PR counts + open-issue counts for a GitHub repo via two REST
/// `gh api` calls. Stays on REST deliberately: `gh pr list` / `gh issue list`
pub fn fetch_pr_and_issue_counts(
    owner_repo: &str,
    my_login: &str,
) -> Option<(PrCounts, IssueCounts)> {
    let pr_out = Command::new("gh")
        .args([
            "api",
            &format!("/repos/{owner_repo}/pulls?state=open&per_page=100"),
        ])
        .output()
        .ok()?;
    if !pr_out.status.success() {
        tracing::debug!(
            "gh api /pulls failed for {owner_repo}: {}",
            String::from_utf8_lossy(&pr_out.stderr).trim(),
        );
        return None;
    }
    let prs: serde_json::Value = serde_json::from_slice(&pr_out.stdout).ok()?;

    let mut counts = PrCounts::default();
    for pr in prs.as_array().into_iter().flatten() {
        counts.open += 1;
        let is_draft = pr.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);
        if is_draft {
            counts.draft += 1;
        }
        let author_login = pr
            .get("user")
            .and_then(|a| a.get("login"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let reviewers: Vec<&str> = pr
            .get("requested_reviewers")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|r| r.get("login").and_then(|l| l.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        if !my_login.is_empty() && reviewers.contains(&my_login) {
            counts.awaiting_my_review += 1;
        }
        if !my_login.is_empty() && author_login == my_login && !is_draft {
            if reviewers.is_empty() {
                counts.mine_no_reviewer += 1;
            } else {
                counts.mine_awaiting_review += 1;
            }
        }
        if !my_login.is_empty() && author_login == my_login && is_draft {
            counts.my_draft += 1;
        }
    }

    // REST /repos/X/issues includes pull requests. Filter them out via
    // the presence of a `pull_request` field on the issue object.
    let issue_out = Command::new("gh")
        .args([
            "api",
            &format!("/repos/{owner_repo}/issues?state=open&per_page=100"),
        ])
        .output()
        .ok()?;
    if !issue_out.status.success() {
        tracing::debug!(
            "gh api /issues failed for {owner_repo}: {}",
            String::from_utf8_lossy(&issue_out.stderr).trim(),
        );
        return None;
    }
    let issues_json: serde_json::Value = serde_json::from_slice(&issue_out.stdout).ok()?;
    let mut issues = IssueCounts::default();
    for issue in issues_json.as_array().into_iter().flatten() {
        if issue.get("pull_request").is_some() {
            continue;
        }
        issues.open += 1;
        if !my_login.is_empty() {
            let assigned = issue
                .get("assignees")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .any(|a| a.get("login").and_then(|l| l.as_str()) == Some(my_login))
                })
                .unwrap_or(false);
            if assigned {
                issues.assigned_to_me += 1;
            }
        }
    }

    Some((counts, issues))
}

/// One GitHub issue surfaced by `gh issue list --label LABEL`. Used by
/// the recall-dispatch substrate (#92) to surface structural-ask,
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LabeledIssue {
    pub number: i64,
    pub title: String,
    /// RFC3339 createdAt parsed to unix seconds (0 when missing).
    pub created_at: i64,
    /// Full label list from gh, included so a single fetch can serve
    /// multi-label filtering downstream without re-fetching.
    pub labels: Vec<String>,
    /// Open or closed at fetch time.
    pub state: String,
    /// RFC3339 closedAt parsed to unix seconds, when state == "CLOSED".
    pub closed_at: Option<i64>,
}

/// One entry from `gh repo list --json …` — the viewer's GitHub repos
/// regardless of whether they're cloned locally. Drives the dashboard's
#[derive(Debug, Clone)]
pub struct ActiveRepo {
    pub full_name: String,
    pub https_url: String,
    pub ssh_url: Option<String>,
    pub default_branch: Option<String>,
    pub pushed_at: Option<i64>,
    pub description: Option<String>,
    pub is_fork: bool,
    pub is_archived: bool,
}

/// Pull `OWNER/NAME` out of a normalised remote URL like
/// `https://github.com/coilyco-flight-deck/repo-recall`. Returns `None` for non-
pub fn github_owner_repo(remote_url: &str) -> Option<String> {
    let parsed = parse_owner_repo(remote_url)?;
    if parsed.host != "github.com" {
        return None;
    }
    Some(format!("{}/{}", parsed.owner, parsed.name))
}

/// Host-agnostic peer of `github_owner_repo`; drives per-repo dispatch (#91).
pub fn remote_host_and_slug(remote_url: &str) -> Option<(String, String)> {
    let parsed = parse_owner_repo(remote_url)?;
    Some((parsed.host, format!("{}/{}", parsed.owner, parsed.name)))
}

/// Turn a raw git remote URL (`git@github.com:owner/repo.git`,
/// `https://github.com/owner/repo.git`, `ssh://git@host:22/owner/repo`, …)
fn normalize_remote_url(raw: &str) -> Option<String> {
    let parsed = parse_owner_repo(raw)?;
    Some(format!(
        "https://{}/{}/{}",
        parsed.host, parsed.owner, parsed.name
    ))
}

struct OwnerRepo {
    host: String,
    owner: String,
    name: String,
}

/// Parse a remote URL via `git-url-parse` and validate that the URL's path is
/// *exactly* `owner/repo(.git)?` — no extra segments. The crate is
fn parse_owner_repo(raw: &str) -> Option<OwnerRepo> {
    use git_url_parse::Scheme;
    let parsed = git_url_parse::GitUrl::parse(raw.trim()).ok()?;
    if matches!(parsed.scheme, Scheme::File | Scheme::Unspecified) {
        return None;
    }
    let host = parsed.host.filter(|s| !s.is_empty())?;
    let owner = parsed.owner.filter(|s| !s.is_empty())?;
    if parsed.name.is_empty() {
        return None;
    }
    let canonical_path = parsed
        .path
        .trim_start_matches('/')
        .trim_end_matches('/')
        .trim_end_matches(".git");
    if canonical_path != parsed.fullname {
        return None;
    }
    Some(OwnerRepo {
        host,
        owner,
        name: parsed.name,
    })
}

#[cfg(test)]
mod tests {
    use super::{normalize_remote_url, parse_numstat_path, stale_branches};

    #[test]
    fn parse_numstat_path_no_rename() {
        let (from, to) = parse_numstat_path("src/db.rs");
        assert_eq!(from, None);
        assert_eq!(to, "src/db.rs");
    }

    #[test]
    fn stale_branches_excludes_default_merged_and_fresh() {
        use std::process::Command;

        let dir = std::env::temp_dir().join(format!(
            "repo-recall-stale-branches-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.to_str().unwrap().to_string();

        // `git -c` flags configure identity inline; `-c date` env on each
        // commit backdates it so we can drive the staleness threshold.
        let git = |args: &[&str], committer_date: Option<&str>| {
            let mut cmd = Command::new("git");
            cmd.args(["-C", &path]).args(args);
            if let Some(d) = committer_date {
                cmd.env("GIT_COMMITTER_DATE", d).env("GIT_AUTHOR_DATE", d);
            }
            let out = cmd.output().expect("git invocation");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr),
            );
        };
        let commit = |msg: &str, date: Option<&str>| {
            std::fs::write(dir.join("f"), msg).unwrap();
            git(&["add", "-A"], None);
            git(
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "-m",
                    msg,
                ],
                date,
            );
        };

        git(&["init", "-b", "main"], None);
        // 10 days ago, well past the 24h threshold.
        let old = "2020-01-01T00:00:00Z";
        commit("base", Some(old));

        // A stale unmerged branch: old tip, not on main.
        git(&["checkout", "-b", "stale-feature"], None);
        commit("stale work", Some(old));

        // A merged-but-not-deleted branch: old tip, but folded into main.
        git(&["checkout", "main"], None);
        git(&["checkout", "-b", "merged-old"], None);
        commit("merged work", Some(old));
        git(&["checkout", "main"], None);
        // The merge commit needs a committer identity too; pass it inline so
        // the test doesn't depend on a global git config (CI runners have none).
        git(
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "merge",
                "--no-ff",
                "merged-old",
                "-m",
                "merge",
            ],
            None,
        );

        // A fresh unmerged branch: tip is now, so not stale.
        git(&["checkout", "-b", "fresh-feature"], None);
        commit("fresh work", None);

        git(&["checkout", "main"], None);

        let stale = stale_branches(std::path::Path::new(&path), 24 * 3_600);
        let names: Vec<&str> = stale.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["stale-feature"],
            "only the old unmerged branch counts",
        );
        assert!(stale[0].tip_age_secs > 24 * 3_600);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_numstat_path_braced_rename() {
        let (from, to) = parse_numstat_path("src/ingest/{old => new}/file.rs");
        assert_eq!(from.as_deref(), Some("src/ingest/old/file.rs"));
        assert_eq!(to, "src/ingest/new/file.rs");
    }

    #[test]
    fn parse_numstat_path_fully_renamed() {
        let (from, to) = parse_numstat_path("a/b/c.rs => d/e/f.rs");
        assert_eq!(from.as_deref(), Some("a/b/c.rs"));
        assert_eq!(to, "d/e/f.rs");
    }

    #[test]
    fn normalizes_ssh_shorthand() {
        assert_eq!(
            normalize_remote_url("git@github.com:coilyco-flight-deck/repo-recall.git").as_deref(),
            Some("https://github.com/coilyco-flight-deck/repo-recall"),
        );
    }

    #[test]
    fn normalizes_https() {
        assert_eq!(
            normalize_remote_url("https://gitlab.com/org/proj.git/").as_deref(),
            Some("https://gitlab.com/org/proj"),
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(normalize_remote_url("not-a-url").is_none());
        assert!(normalize_remote_url("").is_none());
    }

    #[test]
    fn extracts_github_owner_repo() {
        use super::github_owner_repo;
        assert_eq!(
            github_owner_repo("https://github.com/coilyco-flight-deck/repo-recall").as_deref(),
            Some("coilyco-flight-deck/repo-recall"),
        );
        assert_eq!(
            github_owner_repo("https://github.com/coilyco-flight-deck/repo-recall/").as_deref(),
            Some("coilyco-flight-deck/repo-recall"),
        );
        assert!(github_owner_repo("https://gitlab.com/a/b").is_none());
        assert!(github_owner_repo("https://github.com/only-one").is_none());
        assert!(github_owner_repo("https://github.com/a/b/tree/main").is_none());
    }

    #[test]
    fn extracts_host_and_slug_for_any_provider() {
        use super::remote_host_and_slug;
        assert_eq!(
            remote_host_and_slug("https://github.com/coilyco-flight-deck/repo-recall"),
            Some((
                "github.com".into(),
                "coilyco-flight-deck/repo-recall".into()
            )),
        );
        assert_eq!(
            remote_host_and_slug("https://forgejo.coilysiren.me/coilysiren/repo-recall.git"),
            Some((
                "forgejo.coilysiren.me".into(),
                "coilysiren/repo-recall".into(),
            )),
        );
        assert_eq!(
            remote_host_and_slug("git@forgejo.coilysiren.me:coilysiren/repo-recall.git"),
            Some((
                "forgejo.coilysiren.me".into(),
                "coilysiren/repo-recall".into(),
            )),
        );
        assert!(remote_host_and_slug("https://github.com/only-one").is_none());
    }
}
