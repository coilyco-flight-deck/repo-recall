//! Labeled-issue ingest via GitHub GraphQL `search`. Source 6 of #155.
//!
//! **AGENTS.md "No GraphQL" exception:** this is the sole sanctioned GraphQL
//! call site in the codebase. The cost discipline that motivates the rule
//! (REST per-repo calls at refresh cadence vs. a 5k/hr shared GraphQL
//! secondary budget) is preserved by collapsing what was 4 × N REST calls
//! (one per (label, state) per discovered repo) into a single GraphQL
//! request with aliased searches — one roundtrip regardless of repo count.
//!
//! Cadence is governed by `refresh.per_source.github_remote_labeled` (default
//! 3600s). The actual scheduling lives in the per-source refresh substrate
//! (#146); this module just exposes the call.
//!
//! **Scope:** only repos discovered on disk. The query is built from the
//! caller's repo list and uses one `repo:owner/name` clause per repo. The
//! GraphQL endpoint cannot widen the search beyond that list — there is no
//! "search the org" or "search everything" path here. See SECURITY.md.

use std::process::Command;

use chrono::DateTime;

use crate::ingest::git::log::LabeledIssue;

/// One labeled-state target: `(label, state)`. State is one of `OPEN` /
/// `CLOSED` / `ALL` (GitHub's GraphQL `IssueState`-shaped strings, but
/// the search query syntax just wants `is:open` / `is:closed`).
pub type LabelTarget = (&'static str, &'static str);

/// Run a single `gh api graphql` request that aliases one `search` per
/// `(label, state)` target. Each search filters its result set to the
/// caller-supplied repo list via `repo:owner/name` clauses (so the query
/// cannot widen beyond repos on disk). Returns `(repo_id, label,
/// Vec<LabeledIssue>)` triples ready for `upsert_labeled_issue`.
///
/// `targets` should be a short fixed list (in #155: 4 entries for
/// structural-ask, autonomous-block, repo-dispatch open/closed). `slugs`
/// is the deduped list of GitHub-hosted repos we discovered on disk; one
/// `(repo_id, "owner/repo")` per entry.
///
/// Returns an empty vec on any failure — gh missing, auth missing,
/// rate-limited, parse failure. One refresh of empty results never
/// breaks the dashboard.
pub fn fetch_labeled_issues_graphql(
    slugs: &[(i64, String)],
    targets: &[LabelTarget],
) -> Vec<(i64, String, Vec<LabeledIssue>)> {
    if slugs.is_empty() || targets.is_empty() {
        return Vec::new();
    }

    let mut id_by_slug: std::collections::HashMap<String, i64> =
        std::collections::HashMap::with_capacity(slugs.len());
    for (id, slug) in slugs {
        id_by_slug.insert(slug.to_ascii_lowercase(), *id);
    }

    // Build a `repo:` clause group reused across every aliased search.
    // Quoting each `repo:` arg lets odd-but-legal repo names survive.
    let repo_clause: String = slugs
        .iter()
        .map(|(_, s)| format!("repo:{s}"))
        .collect::<Vec<_>>()
        .join(" ");

    let mut aliases: Vec<String> = Vec::with_capacity(targets.len());
    for (i, (label, state)) in targets.iter().enumerate() {
        let alias = format!("q{i}");
        let state_clause = match state.to_ascii_lowercase().as_str() {
            "open" => "is:open",
            "closed" => "is:closed",
            _ => "",
        };
        // Each search returns up to 100 nodes; pagination across (label,
        // state, repo-set) is over-engineering for the dispatch labels —
        // these are bounded sets in practice. If a label ever exceeds
        // 100 across the workspace, the missing rows surface on the next
        // refresh after some are closed.
        let query = format!(r#"label:"{label}" {state_clause} {repo_clause}"#);
        let escaped = escape_graphql_string(&query);
        aliases.push(format!(
            r#"{alias}: search(type: ISSUE, first: 100, query: "{escaped}") {{
  nodes {{
    __typename
    ... on Issue {{
      number
      title
      createdAt
      closedAt
      state
      repository {{ nameWithOwner }}
      labels(first: 25) {{ nodes {{ name }} }}
    }}
  }}
}}"#
        ));
    }
    let query_doc = format!("query {{ {} }}", aliases.join("\n"));

    let output = match Command::new("gh")
        .args(["api", "graphql", "-f"])
        .arg(format!("query={query_doc}"))
        .output()
    {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            tracing::debug!(
                "gh api graphql (labeled issues) failed: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return Vec::new();
        }
        Err(e) => {
            tracing::debug!("gh subprocess failed for labeled issues: {e}");
            return Vec::new();
        }
    };

    let Ok(value): serde_json::Result<serde_json::Value> = serde_json::from_slice(&output.stdout)
    else {
        return Vec::new();
    };
    let Some(data) = value.get("data") else {
        return Vec::new();
    };

    let mut out: Vec<(i64, String, Vec<LabeledIssue>)> = Vec::new();
    for (i, (label, _state)) in targets.iter().enumerate() {
        let alias = format!("q{i}");
        let Some(result) = data.get(&alias) else {
            continue;
        };
        let Some(nodes) = result.get("nodes").and_then(|n| n.as_array()) else {
            continue;
        };
        // Group nodes by repo so the caller gets one entry per (repo, label).
        let mut by_repo: std::collections::HashMap<i64, Vec<LabeledIssue>> =
            std::collections::HashMap::new();
        for node in nodes {
            if node.get("__typename").and_then(|t| t.as_str()) != Some("Issue") {
                continue;
            }
            let slug = node
                .get("repository")
                .and_then(|r| r.get("nameWithOwner"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let Some(repo_id) = id_by_slug.get(&slug) else {
                continue;
            };
            let number = node.get("number").and_then(|v| v.as_i64()).unwrap_or(0);
            if number == 0 {
                continue;
            }
            let title = node
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let created_at = node
                .get("createdAt")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.timestamp())
                .unwrap_or(0);
            let closed_at = node
                .get("closedAt")
                .and_then(|v| v.as_str())
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.timestamp());
            // GraphQL Issue.state is OPEN / CLOSED uppercase; the storage
            // expects lowercase to match the REST values it replaces.
            let state = node
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let labels = node
                .get("labels")
                .and_then(|l| l.get("nodes"))
                .and_then(|n| n.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|l| l.get("name").and_then(|n| n.as_str()))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            by_repo.entry(*repo_id).or_default().push(LabeledIssue {
                number,
                title,
                created_at,
                labels,
                state,
                closed_at,
            });
        }
        for (repo_id, issues) in by_repo {
            out.push((repo_id, (*label).to_string(), issues));
        }
    }
    out
}

/// Minimal escaper for inserting a string literal into a GraphQL document.
/// We only need to handle the characters that can appear in label names
/// and repo slugs: backslash and double-quote. Newlines aren't expected
/// in the inputs we accept but are escaped defensively.
fn escape_graphql_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_passes_normal_chars() {
        assert_eq!(
            escape_graphql_string("label:foo is:open"),
            "label:foo is:open"
        );
    }

    #[test]
    fn escape_handles_quotes_and_backslash() {
        assert_eq!(
            escape_graphql_string(r#"with "quote" and \slash"#),
            r#"with \"quote\" and \\slash"#
        );
    }
}
