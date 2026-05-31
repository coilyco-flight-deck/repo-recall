use std::path::{Path, PathBuf};

/// Given a session cwd and a slice of known repos, return the index of the
/// longest repo path that is an ancestor of (or equal to) `cwd`. Returns
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

/// A reference to a GitHub issue or PR, parsed from text.
///
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GhRef {
    pub owner: String,
    pub repo: String,
    pub issue: u32,
}

/// Scan `text` for fully-qualified GitHub references and yield `GhRef`
/// values. Matches two shapes:
pub fn gh_refs_with_issue_in_text(text: &str) -> Vec<GhRef> {
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
                let kind_pull = after.strip_prefix("pull/");
                let kind_issue = after.strip_prefix("issues/");
                let digits_tail = kind_pull.or(kind_issue);
                if let Some(rest) = digits_tail {
                    if let Some(n) = parse_leading_u32(rest) {
                        if is_valid_name(owner) && is_valid_name(repo) {
                            out.push(GhRef {
                                owner: owner.to_ascii_lowercase(),
                                repo: repo.to_ascii_lowercase(),
                                issue: n,
                            });
                        }
                    }
                }
            }
        }
    }
    // Pass 2: <owner>/<repo>#<n>. Walk byte-by-byte, anchor on `#`, walk
    // backwards to collect `<repo>` and `<owner>`. Cheap given typical text
    let bytes = text.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'#' {
            continue;
        }
        // Need at least one digit after the `#`.
        let after = &bytes[i + 1..];
        let Some(n) = parse_leading_u32(std::str::from_utf8(after).unwrap_or("")) else {
            continue;
        };
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
            out.push(GhRef {
                owner: owner.to_ascii_lowercase(),
                repo: repo.to_ascii_lowercase(),
                issue: n,
            });
        }
    }
    out
}

/// Back-compat wrapper that drops issue numbers. Existing session<->repo
/// join callers only need `(owner, repo)` to decide which repo to link.
pub fn gh_refs_in_text(text: &str) -> Vec<(String, String)> {
    gh_refs_with_issue_in_text(text)
        .into_iter()
        .map(|r| (r.owner, r.repo))
        .collect()
}

/// Extract issue numbers from commit-message `closes/fixes/resolves` trailers
/// (and inline forms). These are repo-implicit — the caller knows which repo
pub fn closes_refs_in_text(text: &str) -> Vec<u32> {
    const VERBS: &[&str] = &[
        "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
    ];
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut out = Vec::new();
    'outer: for (i, _) in lower.match_indices(['c', 'f', 'r']) {
        // Require word boundary before the verb.
        if i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
            continue;
        }
        let tail = &lower[i..];
        for verb in VERBS {
            if let Some(after_verb) = tail.strip_prefix(verb) {
                // Require word boundary after the verb.
                if let Some(c) = after_verb.as_bytes().first() {
                    if c.is_ascii_alphanumeric() {
                        continue;
                    }
                }
                // Skip optional whitespace, then a colon, then more whitespace.
                let rest = after_verb.trim_start();
                let rest = rest.strip_prefix(':').unwrap_or(rest);
                let rest = rest.trim_start();
                // Must be `#<digits>`. Bare numbers without `#` are too noisy.
                if let Some(digits_tail) = rest.strip_prefix('#') {
                    if let Some(n) = parse_leading_u32(digits_tail) {
                        out.push(n);
                    }
                }
                continue 'outer;
            }
        }
    }
    out
}

fn parse_leading_u32(s: &str) -> Option<u32> {
    let end = s
        .as_bytes()
        .iter()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    s[..end].parse::<u32>().ok()
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
    if descendant.starts_with(ancestor) {
        return true;
    }
    let a = ancestor.to_string_lossy().to_lowercase();
    let d = descendant.to_string_lossy().to_lowercase();
    d == a || d.starts_with(&format!("{a}/")) || d.starts_with(&format!("{a}\\"))
}

#[cfg(test)]
mod tests {
    use super::{closes_refs_in_text, gh_refs_in_text, gh_refs_with_issue_in_text, GhRef};

    #[test]
    fn with_issue_shorthand_captures_n() {
        let hits = gh_refs_with_issue_in_text("see coilysiren/repo-recall#42 for context");
        assert_eq!(
            hits,
            vec![GhRef {
                owner: "coilysiren".into(),
                repo: "repo-recall".into(),
                issue: 42,
            }]
        );
    }

    #[test]
    fn with_issue_pull_url_captures_n() {
        let hits = gh_refs_with_issue_in_text(
            "https://github.com/coilyco-flight-deck/repo-recall/pull/56 landed",
        );
        assert_eq!(
            hits,
            vec![GhRef {
                owner: "coilysiren".into(),
                repo: "repo-recall".into(),
                issue: 56,
            }]
        );
    }

    #[test]
    fn closes_subject_line_variants() {
        let cases = [
            ("Add foo, closes #12", vec![12]),
            ("Fix authentication; fixes #7 and resolves #8", vec![7, 8]),
            ("Resolve race, Resolves: #99 \n", vec![99]),
            ("Closed: #4", vec![4]),
            ("Foreclose #1", Vec::<u32>::new()), // word boundary check.
            ("closes 12", Vec::<u32>::new()),    // requires `#`.
        ];
        for (text, want) in cases {
            assert_eq!(closes_refs_in_text(text), want, "text: {text:?}");
        }
    }

    #[test]
    fn closes_ignores_bare_hash_with_no_verb() {
        // `closes_refs_in_text` only picks up issue numbers that follow an
        // auto-close verb. Plain `#42` in a message is left to the GhRef
        assert!(closes_refs_in_text("plain #42 in passing").is_empty());
    }

    #[test]
    fn shorthand_owner_repo_hash_n() {
        let hits = gh_refs_in_text("see coilysiren/repo-recall#42 for context");
        assert_eq!(hits, vec![("coilysiren".into(), "repo-recall".into())]);
    }

    #[test]
    fn pull_url() {
        let hits = gh_refs_in_text(
            "https://github.com/coilyco-flight-deck/repo-recall/pull/56 changed files",
        );
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
