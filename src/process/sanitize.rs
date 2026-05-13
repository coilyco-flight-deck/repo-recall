//! Sanitization gate for session-derived content (#110).
//!
//! Every public-safe emitter routes free text through [`scrub`] before
//! it lands on disk or in a remote issue body. The pass is best-effort
//! for a single-operator install — a known-bad list, not a real DLP
//! solution. Two categories:
//!
//! * **Fixed terms** matched via `aho-corasick` (vault paths, hostnames,
//!   anything caller-configured via `REPO_RECALL_SANITIZE_TERMS`).
//! * **Token prefixes** (`ghp_`, `sk-ant-`, `AKIA`, ...) where the
//!   prefix is fixed but the trailing characters are the secret. We
//!   find the prefix with `aho-corasick`, then consume the run of
//!   token characters that follow.
//!
//! Matches are replaced with `[REDACTED:<category>]` so a reviewer can
//! see which gate fired without leaking the original text.

use std::sync::OnceLock;

use aho_corasick::AhoCorasick;

/// Where the text came from. Currently only used for telemetry / future
/// per-source policy. Carry it through every emitter even though the
/// scrub itself is uniform today.
#[derive(Debug, Clone, Copy)]
pub enum SanitizeSource {
    /// Body text destined for `docs/repo-dispatch/<slug>.md` and the
    /// pollable mirror.
    DispatchArtifact,
    /// Body text destined for a GitHub issue created by the planner
    /// (#105 structural-ask drafts, #106 drift PRs).
    GithubIssueBody,
    /// Frontmatter free-text fields embedded in any of the above.
    Frontmatter,
}

const FIXED_TERMS: &[(&str, &str)] = &[
    ("coilyco-vault", "vault-path"),
    ("Obsidian Vault", "vault-path"),
    ("kai-server", "internal-host"),
    ("KAI-SERVER", "internal-host"),
];

const TOKEN_PREFIXES: &[(&str, &str)] = &[
    ("ghp_", "github-token"),
    ("github_pat_", "github-token"),
    ("gho_", "github-token"),
    ("ghs_", "github-token"),
    ("sk-ant-", "anthropic-key"),
    ("xoxb-", "slack-token"),
    ("xoxp-", "slack-token"),
    ("xoxa-", "slack-token"),
    ("AKIA", "aws-access-key"),
    ("ASIA", "aws-access-key"),
];

struct Matcher {
    ac: AhoCorasick,
    categories: Vec<&'static str>,
}

fn term_matcher() -> &'static Matcher {
    static M: OnceLock<Matcher> = OnceLock::new();
    M.get_or_init(|| {
        let mut patterns: Vec<String> = FIXED_TERMS.iter().map(|(p, _)| (*p).to_string()).collect();
        let mut categories: Vec<&'static str> = FIXED_TERMS.iter().map(|(_, c)| *c).collect();
        for extra in extra_terms() {
            patterns.push(extra.clone());
            categories.push("configured");
        }
        Matcher {
            ac: AhoCorasick::new(&patterns).expect("term matcher"),
            categories,
        }
    })
}

fn prefix_matcher() -> &'static Matcher {
    static M: OnceLock<Matcher> = OnceLock::new();
    M.get_or_init(|| Matcher {
        ac: AhoCorasick::new(TOKEN_PREFIXES.iter().map(|(p, _)| *p)).expect("prefix matcher"),
        categories: TOKEN_PREFIXES.iter().map(|(_, c)| *c).collect(),
    })
}

fn extra_terms() -> &'static Vec<String> {
    static V: OnceLock<Vec<String>> = OnceLock::new();
    V.get_or_init(|| {
        std::env::var("REPO_RECALL_SANITIZE_TERMS")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Best-effort scrub. Returns the input with known-bad terms and
/// secret-shaped tokens replaced by `[REDACTED:<category>]`.
pub fn scrub(raw: &str, _source: SanitizeSource) -> String {
    let terms = term_matcher();
    let prefixes = prefix_matcher();

    let mut spans: Vec<(usize, usize, &'static str)> = Vec::new();
    for m in terms.ac.find_iter(raw) {
        spans.push((m.start(), m.end(), terms.categories[m.pattern().as_usize()]));
    }
    let bytes = raw.as_bytes();
    for m in prefixes.ac.find_iter(raw) {
        let end = consume_token_tail(bytes, m.end());
        spans.push((m.start(), end, prefixes.categories[m.pattern().as_usize()]));
    }
    if spans.is_empty() {
        return raw.to_string();
    }

    spans.sort_by_key(|s| s.0);
    let merged = merge_overlapping(&spans);

    let mut out = String::with_capacity(raw.len());
    let mut cursor = 0;
    for (start, end, cat) in merged {
        if start > cursor {
            out.push_str(&raw[cursor..start]);
        }
        out.push_str("[REDACTED:");
        out.push_str(cat);
        out.push(']');
        cursor = end;
    }
    if cursor < raw.len() {
        out.push_str(&raw[cursor..]);
    }
    out
}

fn consume_token_tail(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() {
        let b = bytes[i];
        if !(b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            break;
        }
        i += 1;
    }
    i
}

fn merge_overlapping(spans: &[(usize, usize, &'static str)]) -> Vec<(usize, usize, &'static str)> {
    let mut out: Vec<(usize, usize, &'static str)> = Vec::with_capacity(spans.len());
    for &(s, e, c) in spans {
        if let Some(last) = out.last_mut() {
            if s < last.1 {
                if e > last.1 {
                    last.1 = e;
                }
                continue;
            }
        }
        out.push((s, e, c));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_strips_known_bad_terms_from_session_excerpt() {
        let excerpt = "Walking ~/projects/coilysiren/coilyco-vault/Obsidian Vault/Notes \
                       on kai-server with token ghp_AAAABBBBCCCCDDDDEEEEFFFFGGGGHHHHIIII \
                       and sk-ant-api03-xyz123.";
        let scrubbed = scrub(excerpt, SanitizeSource::DispatchArtifact);
        assert!(!scrubbed.contains("coilyco-vault"), "{scrubbed}");
        assert!(!scrubbed.contains("Obsidian Vault"), "{scrubbed}");
        assert!(!scrubbed.contains("kai-server"), "{scrubbed}");
        assert!(!scrubbed.contains("ghp_AAAA"), "{scrubbed}");
        assert!(!scrubbed.contains("sk-ant-api03"), "{scrubbed}");
        assert!(scrubbed.contains("[REDACTED:vault-path]"), "{scrubbed}");
        assert!(scrubbed.contains("[REDACTED:internal-host]"), "{scrubbed}");
        assert!(scrubbed.contains("[REDACTED:github-token]"), "{scrubbed}");
        assert!(scrubbed.contains("[REDACTED:anthropic-key]"), "{scrubbed}");
    }

    #[test]
    fn scrub_passes_clean_text_through_unchanged() {
        let clean = "do the thing in src/foo.rs and link #92.";
        assert_eq!(scrub(clean, SanitizeSource::GithubIssueBody), clean);
    }

    #[test]
    fn token_tail_stops_at_punctuation() {
        let s = "key=ghp_abcDEF123, next";
        let out = scrub(s, SanitizeSource::Frontmatter);
        assert!(out.starts_with("key=[REDACTED:github-token]"), "{out}");
        assert!(out.ends_with(", next"), "{out}");
    }
}
