use std::path::{Path, PathBuf};

/// Given a session cwd and a slice of known repos, return the index of the
/// longest repo path that is an ancestor of (or equal to) `cwd`. Returns
/// `None` if no repo matches.
pub fn best_repo_for_cwd(cwd: &str, repos: &[(i64, PathBuf)]) -> Option<i64> {
    let cwd_canon = canonicalize(Path::new(cwd));
    let mut best: Option<(usize, i64)> = None;
    for (id, repo_path) in repos {
        if is_ancestor_or_equal(repo_path, &cwd_canon) {
            let depth = repo_path.components().count();
            match best {
                Some((best_depth, _)) if best_depth >= depth => {}
                _ => best = Some((depth, *id)),
            }
        }
    }
    best.map(|(_, id)| id)
}

fn canonicalize(p: &Path) -> PathBuf {
    dunce::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Scan `text` for GitHub-style references and yield `(owner, repo)` pairs
/// (lower-cased). Matches two shapes:
///
/// - `<owner>/<repo>#<n>` — the `gh issue list` / commit-message shorthand.
/// - `github.com/<owner>/<repo>/(pull|issues)/<n>` — pasted PR or issue URLs.
///
/// Returns one entry per match site (callers can dedup). Owner/repo are
/// validated against GitHub's name rules: 1-39 chars, alphanumerics plus
/// `-`, `_`, `.`. Cheap, allocation-light single pass; no regex dep.
pub fn gh_refs_in_text(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    // Pass 1: github.com/<owner>/<repo>/(pull|issues)/<n>. Split on
    // "github.com/" so we can ignore protocol/host/leading path.
    for tail in text.split("github.com/").skip(1) {
        if let Some((owner, rest)) = split_name(tail) {
            let after_owner = &tail[owner.len()..];
            if !after_owner.starts_with('/') {
                continue;
            }
            let _ = rest;
            let repo_tail = &after_owner[1..];
            if let Some((repo, after_repo)) = split_name(repo_tail) {
                if !after_repo.starts_with('/') {
                    continue;
                }
                let after = &after_repo[1..];
                let kind_ok = after.starts_with("pull/") || after.starts_with("issues/");
                if kind_ok && is_valid_name(owner) && is_valid_name(repo) {
                    out.push((owner.to_ascii_lowercase(), repo.to_ascii_lowercase()));
                }
            }
        }
    }
    // Pass 2: <owner>/<repo>#<n>. Walk byte-by-byte, anchor on `#`, walk
    // backwards to collect `<repo>` and `<owner>`. Cheap given typical text
    // sizes (a few MB of session JSONL at most).
    let bytes = text.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'#' {
            continue;
        }
        // Need at least one digit after the `#`.
        let after = &bytes[i + 1..];
        if after.first().is_none_or(|c| !c.is_ascii_digit()) {
            continue;
        }
        // Walk backwards over the repo name.
        let mut j = i;
        while j > 0 && is_name_byte(bytes[j - 1]) {
            j -= 1;
        }
        let repo_bytes = &bytes[j..i];
        if repo_bytes.is_empty() || j == 0 || bytes[j - 1] != b'/' {
            continue;
        }
        // Walk backwards over the owner name.
        let slash = j - 1;
        let mut k = slash;
        while k > 0 && is_name_byte(bytes[k - 1]) {
            k -= 1;
        }
        let owner_bytes = &bytes[k..slash];
        if owner_bytes.is_empty() {
            continue;
        }
        // The byte before the owner must not be a name byte (boundary).
        if k > 0 && is_name_byte(bytes[k - 1]) {
            continue;
        }
        let (Ok(owner), Ok(repo)) = (
            std::str::from_utf8(owner_bytes),
            std::str::from_utf8(repo_bytes),
        ) else {
            continue;
        };
        if is_valid_name(owner) && is_valid_name(repo) {
            out.push((owner.to_ascii_lowercase(), repo.to_ascii_lowercase()));
        }
    }
    out
}

fn split_name(s: &str) -> Option<(&str, &str)> {
    let end = s
        .as_bytes()
        .iter()
        .position(|b| !is_name_byte(*b))
        .unwrap_or(s.len());
    if end == 0 {
        None
    } else {
        Some((&s[..end], &s[end..]))
    }
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.'
}

fn is_valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 39
        && !s.starts_with('.')
        && !s.starts_with('-')
        && !s.ends_with('.')
}

fn is_ancestor_or_equal(ancestor: &Path, descendant: &Path) -> bool {
    // Compare on macOS (case-insensitive by default) and Linux (case-sensitive)
    // consistently by lowercasing on macOS-style targets. Simpler: compare
    // components as-is; if that fails, fall back to case-insensitive.
    if descendant.starts_with(ancestor) {
        return true;
    }
    let a = ancestor.to_string_lossy().to_lowercase();
    let d = descendant.to_string_lossy().to_lowercase();
    d == a || d.starts_with(&format!("{a}/")) || d.starts_with(&format!("{a}\\"))
}

#[cfg(test)]
mod tests {
    use super::gh_refs_in_text;

    #[test]
    fn shorthand_owner_repo_hash_n() {
        let hits = gh_refs_in_text("see coilysiren/repo-recall#42 for context");
        assert_eq!(hits, vec![("coilysiren".into(), "repo-recall".into())]);
    }

    #[test]
    fn pull_url() {
        let hits =
            gh_refs_in_text("https://github.com/coilysiren/repo-recall/pull/56 changed files");
        assert_eq!(hits, vec![("coilysiren".into(), "repo-recall".into())]);
    }

    #[test]
    fn issue_url() {
        let hits = gh_refs_in_text("filed at https://github.com/Anthropics/Anth-API/issues/1");
        assert_eq!(hits, vec![("anthropics".into(), "anth-api".into())]);
    }

    #[test]
    fn shorthand_requires_digit_after_hash() {
        // `#main` is a markdown anchor, not an issue ref.
        let hits = gh_refs_in_text("see foo/bar#main and foo/bar#1");
        assert_eq!(hits, vec![("foo".into(), "bar".into())]);
    }

    #[test]
    fn shorthand_overmatches_path_fragments() {
        // `path/to/file#1` matches `to/file` here. That's by design: we
        // can't tell apart "path-like text" from "owner/repo" without a
        // GitHub-side check, and the refresh pass filters every hit
        // against discovered repo remotes anyway. Documenting the
        // behavior so a future change doesn't silently tighten it.
        let hits = gh_refs_in_text("path/to/file#1");
        assert_eq!(hits, vec![("to".into(), "file".into())]);
    }

    #[test]
    fn url_rejects_wrong_kind() {
        let hits = gh_refs_in_text("https://github.com/owner/repo/tree/main");
        assert!(hits.is_empty(), "unexpected hits: {hits:?}");
    }

    #[test]
    fn url_rejects_short_path() {
        let hits = gh_refs_in_text("https://github.com/owner");
        assert!(hits.is_empty());
    }

    #[test]
    fn dedup_left_to_caller() {
        // The caller dedups; this layer returns one entry per match site.
        let hits = gh_refs_in_text("foo/bar#1 foo/bar#2");
        assert_eq!(hits.len(), 2);
    }
}
