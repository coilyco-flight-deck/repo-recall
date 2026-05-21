use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub session_uuid: String,
    pub cwd: Option<String>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub message_count: i64,
    pub user_message_count: i64,
    pub assistant_message_count: i64,
    pub last_prompt: Option<String>,
    pub source_file: String,
    /// Wall-clock span in milliseconds (end - start). `None` when we saw at
    /// most one timestamp. Uses ms rather than seconds so sub-second sessions
    /// don't collapse to zero.
    pub duration_ms: Option<i64>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    /// Most-recent values seen on any record line; useful as a session-level
    /// pointer for joining across the JSONL graph.
    pub parent_uuid: Option<String>,
    pub request_id: Option<String>,
    pub message_id: Option<String>,
    /// Count of lines flagged `isSidechain: true` — proxy for subagent
    /// invocations within the session.
    pub is_sidechain_count: i64,
    /// Sorted unique list of `message.model` values observed.
    pub models_used: Vec<String>,
    /// Sorted unique list of `tool_use.name` values observed.
    pub tools_used: Vec<String>,
    /// Per-tool counts: `{ "<tool>": { "calls": N, "errors": N } }`.
    /// Stored as a JSON blob because the shape is variable.
    pub tool_call_counts_json: String,
    /// `{ "<stop_reason>": N }` aggregated across assistant turns.
    pub stop_reason_counts_json: String,
}

#[derive(Debug, Default, Clone, Copy)]
struct ToolStat {
    calls: i64,
    errors: i64,
}

/// Returns the directory we'll parse session files from. Honors the
/// `REPO_RECALL_SESSIONS_DIR` env override (point it at a fixture tree for
/// tests) and otherwise falls back to the canonical Claude Code projects
/// directory at `~/.claude/projects/`.
pub fn default_projects_dir() -> Option<PathBuf> {
    if let Some(over) = std::env::var_os("REPO_RECALL_SESSIONS_DIR") {
        let dir = PathBuf::from(over);
        if dir.is_dir() {
            return Some(dir);
        }
        tracing::warn!(
            "REPO_RECALL_SESSIONS_DIR set to {:?} but is not a directory; falling back",
            dir
        );
    }
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home).join(".claude").join("projects");
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

/// Enumerate every `.jsonl` file under the Claude projects directory.
pub fn list_session_files(projects_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(projects_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let p = entry.path();
        if p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            out.push(p.to_path_buf());
        }
    }
    Ok(out)
}

#[derive(Debug, Deserialize)]
struct RawLine {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default, alias = "sessionId")]
    session_id: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    content: Option<serde_json::Value>,
    #[serde(default)]
    message: Option<RawMessage>,
    #[serde(default, alias = "parentUuid")]
    parent_uuid: Option<String>,
    #[serde(default, alias = "requestId")]
    request_id: Option<String>,
    #[serde(default, alias = "isSidechain")]
    is_sidechain: Option<bool>,
    #[serde(default, alias = "lastPrompt")]
    last_prompt: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    #[serde(default)]
    content: Option<serde_json::Value>,
    /// Claude's usage block. Real shape (verified against live JSONL):
    ///   `input_tokens`, `output_tokens`, `cache_read_input_tokens`,
    ///   `cache_creation_input_tokens`, plus richer detail we don't use.
    /// `None` for user turns and older sessions.
    #[serde(default)]
    usage: Option<serde_json::Value>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
}

/// Parse one JSONL session file into a `SessionRecord` plus the flat list
/// of renderable/indexable turns. Returns `Ok(None)` if the file doesn't
/// yield any recognisable session data (empty, malformed, etc).
///
/// Turns are collected in the same single pass as the metadata so the
/// scan never parses a session file twice — the full-text turn index
/// (#229) reuses this output rather than re-reading the JSONL.
pub fn parse_session_file(path: &Path) -> Result<Option<(SessionRecord, Vec<Turn>)>> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);

    let mut session_uuid: Option<String> = None;
    let mut cwd: Option<String> = None;
    // Milliseconds for duration; seconds for display-ts fields (historical).
    let mut first_ts_ms: Option<i64> = None;
    let mut last_ts_ms: Option<i64> = None;
    let mut first_ts: Option<i64> = None;
    let mut last_ts: Option<i64> = None;
    let mut user_message_count: i64 = 0;
    let mut assistant_message_count: i64 = 0;
    let mut last_prompt: Option<String> = None;
    let mut input_tokens: i64 = 0;
    let mut output_tokens: i64 = 0;
    let mut cache_read_tokens: i64 = 0;
    let mut cache_creation_tokens: i64 = 0;
    let mut parent_uuid: Option<String> = None;
    let mut request_id: Option<String> = None;
    let mut message_id: Option<String> = None;
    let mut is_sidechain_count: i64 = 0;
    let mut models_used: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut tools_used: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut tool_call_counts: BTreeMap<String, ToolStat> = BTreeMap::new();
    let mut stop_reason_counts: BTreeMap<String, i64> = BTreeMap::new();
    // Pending tool_use_id -> tool name, so we can attribute is_error tool
    // results back to the tool that produced them. Cleared as results arrive.
    let mut pending_tool_uses: BTreeMap<String, String> = BTreeMap::new();
    // Flat transcript, one entry per user/assistant/system line. Built in
    // this same pass for the full-text turn index (#229).
    let mut turns: Vec<Turn> = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!("read error in {}: {}", path.display(), e);
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let raw: RawLine = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("skip malformed line in {}: {}", path.display(), e);
                continue;
            }
        };

        if session_uuid.is_none() {
            session_uuid = raw.session_id.clone();
        }

        // Timestamp (ISO8601) → epoch seconds (for display) + epoch ms (for
        // duration, so sub-second sessions aren't rounded away).
        if let Some(ts_str) = raw.timestamp.as_deref() {
            if let Ok(dt) = DateTime::parse_from_rfc3339(ts_str) {
                let utc = dt.with_timezone(&Utc);
                let secs = utc.timestamp();
                let ms = utc.timestamp_millis();
                first_ts = Some(first_ts.map_or(secs, |cur| cur.min(secs)));
                last_ts = Some(last_ts.map_or(secs, |cur| cur.max(secs)));
                first_ts_ms = Some(first_ts_ms.map_or(ms, |cur| cur.min(ms)));
                last_ts_ms = Some(last_ts_ms.map_or(ms, |cur| cur.max(ms)));
            }
        }

        // `message.usage` on assistant turns carries the token counts. Sum
        // across every turn we see — these are per-turn values, not running
        // totals, so addition is the right aggregation.
        if let Some(usage) = raw.message.as_ref().and_then(|m| m.usage.as_ref()) {
            let pull = |k: &str| usage.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
            input_tokens += pull("input_tokens");
            output_tokens += pull("output_tokens");
            cache_read_tokens += pull("cache_read_input_tokens");
            cache_creation_tokens += pull("cache_creation_input_tokens");
        }

        // cwd is typically on user/assistant message lines.
        if cwd.is_none() {
            if let Some(c) = raw.cwd.as_deref() {
                if !c.is_empty() {
                    cwd = Some(c.to_string());
                }
            }
        }

        let line_type = raw.r#type.as_deref().unwrap_or("");

        // Count user + assistant messages separately (split out from the
        // single `message_count` field).
        match line_type {
            "user" => user_message_count += 1,
            "assistant" => assistant_message_count += 1,
            _ => {}
        }

        // `last-prompt` line type carries the most-recent user prompt verbatim.
        // Preferred over the historical "first user message" summary because it
        // reflects what the session was last working on.
        if line_type == "last-prompt" {
            if let Some(lp) = raw.last_prompt.as_deref() {
                let trimmed = lp.trim();
                if !trimmed.is_empty() {
                    last_prompt = Some(truncate(trimmed, 200));
                }
            }
        }

        // Pointer fields: keep the most recent observed value. `parentUuid` is
        // per-record on user/assistant lines; `requestId` and `message.id`
        // identify the latest API call.
        if let Some(p) = raw.parent_uuid.as_deref() {
            if !p.is_empty() {
                parent_uuid = Some(p.to_string());
            }
        }
        if let Some(r) = raw.request_id.as_deref() {
            if !r.is_empty() {
                request_id = Some(r.to_string());
            }
        }
        if let Some(mid) = raw.message.as_ref().and_then(|m| m.id.as_deref()) {
            if !mid.is_empty() {
                message_id = Some(mid.to_string());
            }
        }

        if raw.is_sidechain.unwrap_or(false) {
            is_sidechain_count += 1;
        }

        if let Some(model) = raw.message.as_ref().and_then(|m| m.model.as_deref()) {
            if !model.is_empty() {
                models_used.insert(model.to_string());
            }
        }
        if let Some(sr) = raw.message.as_ref().and_then(|m| m.stop_reason.as_deref()) {
            if !sr.is_empty() {
                *stop_reason_counts.entry(sr.to_string()).or_insert(0) += 1;
            }
        }

        // Walk message content blocks once to collect tool-use names and
        // tool_result errors. Sessions are short (~hundreds of turns) so this
        // is cheap and lets us avoid threading the state through walk_content.
        if let Some(content) = raw.message.as_ref().and_then(|m| m.content.as_ref()) {
            collect_tool_signals(
                content,
                &mut tools_used,
                &mut tool_call_counts,
                &mut pending_tool_uses,
            );
        }

        // Build the renderable/indexable turn for user/assistant/system
        // lines. Same walk as `parse_transcript`, folded into this pass so
        // a session file is parsed exactly once.
        let turn_role = match line_type {
            "user" => Some(TurnRole::User),
            "assistant" => Some(TurnRole::Assistant),
            "system" => Some(TurnRole::System),
            _ => None,
        };
        if let Some(role) = turn_role {
            let ts = raw
                .timestamp
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc).timestamp());
            let mut turn = Turn {
                role,
                timestamp: ts,
                texts: Vec::new(),
                tool_uses: Vec::new(),
                tool_results: Vec::new(),
                thinking: Vec::new(),
            };
            let content = raw
                .message
                .as_ref()
                .and_then(|m| m.content.as_ref())
                .or(raw.content.as_ref());
            if let Some(v) = content {
                walk_content(v, &mut turn);
            }
            if !turn.texts.is_empty()
                || !turn.tool_uses.is_empty()
                || !turn.tool_results.is_empty()
                || !turn.thinking.is_empty()
            {
                turns.push(turn);
            }
        }
    }

    // Derive a session id if none was found (fall back to the filename stem).
    if session_uuid.is_none() {
        session_uuid = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());
    }

    let Some(uuid) = session_uuid else {
        return Ok(None);
    };

    let duration_ms = match (first_ts_ms, last_ts_ms) {
        (Some(a), Some(b)) if b >= a => Some(b - a),
        _ => None,
    };

    let tool_call_counts_json = serde_json::to_string(
        &tool_call_counts
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    serde_json::json!({"calls": v.calls, "errors": v.errors}),
                )
            })
            .collect::<BTreeMap<_, _>>(),
    )
    .unwrap_or_else(|_| "{}".into());
    let stop_reason_counts_json =
        serde_json::to_string(&stop_reason_counts).unwrap_or_else(|_| "{}".into());

    Ok(Some((
        SessionRecord {
            session_uuid: uuid,
            cwd,
            started_at: first_ts,
            ended_at: last_ts,
            message_count: user_message_count + assistant_message_count,
            user_message_count,
            assistant_message_count,
            last_prompt,
            source_file: path.display().to_string(),
            duration_ms,
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
            parent_uuid,
            request_id,
            message_id,
            is_sidechain_count,
            models_used: models_used.into_iter().collect(),
            tools_used: tools_used.into_iter().collect(),
            tool_call_counts_json,
            stop_reason_counts_json,
        },
        turns,
    )))
}

/// Walk one assistant/user `message.content` and collect tool-use names plus
/// per-tool call + error counts. Tool results are matched back to their
/// tool_use by `tool_use_id` so an `is_error: true` result increments the
/// originating tool's error count, not a generic "errors" bucket.
fn collect_tool_signals(
    v: &serde_json::Value,
    tools_used: &mut std::collections::BTreeSet<String>,
    counts: &mut BTreeMap<String, ToolStat>,
    pending: &mut BTreeMap<String, String>,
) {
    let serde_json::Value::Array(arr) = v else {
        return;
    };
    for block in arr {
        let Some(obj) = block.as_object() else {
            continue;
        };
        match obj.get("type").and_then(|x| x.as_str()).unwrap_or("") {
            "tool_use" => {
                let name = obj
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("(tool)")
                    .to_string();
                tools_used.insert(name.clone());
                counts.entry(name.clone()).or_default().calls += 1;
                if let Some(id) = obj.get("id").and_then(|x| x.as_str()) {
                    pending.insert(id.to_string(), name);
                }
            }
            "tool_result" => {
                let is_err = obj
                    .get("is_error")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                if !is_err {
                    continue;
                }
                if let Some(id) = obj.get("tool_use_id").and_then(|x| x.as_str()) {
                    if let Some(name) = pending.remove(id) {
                        counts.entry(name).or_default().errors += 1;
                    }
                }
            }
            _ => {}
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Renderable turn for the session-detail transcript.
#[derive(Debug, Clone)]
pub struct Turn {
    pub role: TurnRole,
    pub timestamp: Option<i64>,
    /// Plain text blocks (user content, assistant prose).
    pub texts: Vec<String>,
    /// Tool uses on this turn: (tool name, serialized args JSON).
    pub tool_uses: Vec<(String, String)>,
    /// Tool results attached to this turn: truncated previews.
    pub tool_results: Vec<String>,
    /// Chain-of-thought blocks (collapsed by default on the page).
    pub thinking: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnRole {
    User,
    Assistant,
    System,
}

/// Parse every user/assistant line of a JSONL and return a flat list of
/// turns for rendering. Silent on malformed lines (same tolerance as the
/// metadata pass — one bad line doesn't kill the whole view).
pub fn parse_transcript(path: &Path) -> Result<Vec<Turn>> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut out = Vec::new();
    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(raw): std::result::Result<RawLine, _> = serde_json::from_str(&line) else {
            continue;
        };
        let role = match raw.r#type.as_deref().unwrap_or("") {
            "user" => TurnRole::User,
            "assistant" => TurnRole::Assistant,
            "system" => TurnRole::System,
            _ => continue,
        };
        let ts = raw
            .timestamp
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc).timestamp());

        let mut turn = Turn {
            role,
            timestamp: ts,
            texts: Vec::new(),
            tool_uses: Vec::new(),
            tool_results: Vec::new(),
            thinking: Vec::new(),
        };

        // Walk whichever content shape we have. A `user` turn often uses
        // top-level `content` (string); assistant turns nest under
        // `message.content` as an array of typed blocks.
        let content = raw
            .message
            .as_ref()
            .and_then(|m| m.content.as_ref())
            .or(raw.content.as_ref());
        if let Some(v) = content {
            walk_content(v, &mut turn);
        }

        // Skip turns that produced nothing visible (usually tool-result
        // only bookkeeping turns from the user role).
        if !turn.texts.is_empty()
            || !turn.tool_uses.is_empty()
            || !turn.tool_results.is_empty()
            || !turn.thinking.is_empty()
        {
            out.push(turn);
        }
    }
    Ok(out)
}

/// Scan a session file for `<owner>/<repo>#<n>` and PR / issue URL
/// references. Returns the deduped set of `(owner, repo)` pairs found in
/// any text or tool field of the JSONL — we use the whole-file string
/// since the references are stable shape regardless of which Claude Code
/// record type they land in.
///
/// Lower-cases names so the caller can match against repo remotes
/// case-insensitively.
pub fn gh_refs_in_file(path: &Path) -> Vec<(String, String)> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut hits = crate::process::join::gh_refs_in_text(&content);
    hits.sort();
    hits.dedup();
    hits
}

/// Same as `gh_refs_in_file` but also keeps the issue/PR number. Used
/// by the issue-ref ingest pass (#92, #111) so the recall-dispatch
/// planner can ask "which sessions touched issue N in this repo."
/// Returns deduped `GhRef`s.
pub fn issue_refs_in_file(path: &Path) -> Vec<crate::process::join::GhRef> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut hits = crate::process::join::gh_refs_with_issue_in_text(&content);
    hits.sort_by(|a, b| {
        (a.owner.as_str(), a.repo.as_str(), a.issue).cmp(&(
            b.owner.as_str(),
            b.repo.as_str(),
            b.issue,
        ))
    });
    hits.dedup();
    hits
}

/// Scan a single JSONL file for bare-word mentions of each repo name.
/// `needles` is a list of `(repo_id, name)` pairs; the name is case-folded
/// and matched with word boundaries (ASCII only — names with weird chars
/// will still match, just less cleanly).
///
/// Returns the set of matching `repo_id`s. Purely best-effort: common
/// English-word names ("backend", "website") will over-match, which is why
/// the UI labels these as fuzzy content-mentions separate from cwd matches.
pub fn mentions_in_file(path: &Path, needles: &[(i64, String)]) -> Vec<i64> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let haystack = content.to_ascii_lowercase();

    // Build the Aho-Corasick automaton once and stream the haystack through
    // it in a single pass — multi-pattern, ASCII-case-insensitive (we
    // pre-lowered the haystack, so plain matching suffices). Names shorter
    // than 3 chars are dropped: "io", "ai", etc. would match constantly.
    let mut pat_owners: Vec<i64> = Vec::with_capacity(needles.len());
    let mut patterns: Vec<String> = Vec::with_capacity(needles.len());
    for (id, name) in needles {
        if name.len() < 3 {
            continue;
        }
        patterns.push(name.to_ascii_lowercase());
        pat_owners.push(*id);
    }
    if patterns.is_empty() {
        return Vec::new();
    }
    let Ok(ac) = aho_corasick::AhoCorasick::new(&patterns) else {
        return Vec::new();
    };

    let bytes = haystack.as_bytes();
    let mut hit_set: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for m in ac.find_iter(&haystack) {
        let start = m.start();
        let end = m.end();
        // Word-boundary check: bordering byte (or start/end) must not be
        // alphanumeric, so "backend" doesn't match "backends" or "bugbackend".
        let left_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let right_ok = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if left_ok && right_ok {
            hit_set.insert(pat_owners[m.pattern().as_usize()]);
        }
    }
    hit_set.into_iter().collect()
}

/// Like `mentions_in_file`, but also returns a short text snippet around the
/// first valid match per repo. Used by the session detail view to show *why*
/// a fuzzy content-mention fired — the bare-word context, not just the repo
/// name. Returns `repo_id -> (matched_text, snippet_with_match_marker)`. The
/// matched text is sliced from the original (case-preserving) content; the
/// snippet replaces the match span with `\u{1}MATCH\u{1}` sentinels so the
/// renderer can highlight without re-searching.
pub fn mention_snippets_in_file(
    path: &Path,
    needles: &[(i64, String)],
) -> std::collections::HashMap<i64, MentionSnippet> {
    let mut out: std::collections::HashMap<i64, MentionSnippet> = std::collections::HashMap::new();
    let Ok(content) = std::fs::read_to_string(path) else {
        return out;
    };
    let haystack = content.to_ascii_lowercase();
    debug_assert_eq!(haystack.len(), content.len());

    let mut pat_owners: Vec<i64> = Vec::with_capacity(needles.len());
    let mut patterns: Vec<String> = Vec::with_capacity(needles.len());
    for (id, name) in needles {
        if name.len() < 3 {
            continue;
        }
        patterns.push(name.to_ascii_lowercase());
        pat_owners.push(*id);
    }
    if patterns.is_empty() {
        return out;
    }
    let Ok(ac) = aho_corasick::AhoCorasick::new(&patterns) else {
        return out;
    };
    let bytes = haystack.as_bytes();
    for m in ac.find_iter(&haystack) {
        let start = m.start();
        let end = m.end();
        let left_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let right_ok = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if !(left_ok && right_ok) {
            continue;
        }
        let id = pat_owners[m.pattern().as_usize()];
        if out.contains_key(&id) {
            continue;
        }
        out.insert(id, build_snippet(&content, start, end, 60));
    }
    out
}

#[derive(Debug, Clone)]
pub struct MentionSnippet {
    /// The matched text exactly as it appeared in the source (case preserved).
    pub matched: String,
    /// Whitespace-collapsed context surrounding the match. The match itself is
    /// at byte offsets `[match_start, match_end)` within this string.
    pub context: String,
    pub match_start: usize,
    pub match_end: usize,
}

fn build_snippet(s: &str, start: usize, end: usize, ctx: usize) -> MentionSnippet {
    let lo = floor_char_boundary(s, start.saturating_sub(ctx));
    let hi = ceil_char_boundary(s, end.saturating_add(ctx).min(s.len()));
    let prefix_raw = &s[lo..start];
    let match_raw = &s[start..end];
    let suffix_raw = &s[end..hi];

    let mut context = String::new();
    if lo > 0 {
        context.push('…');
    }
    let prefix = collapse_ws(prefix_raw);
    context.push_str(&prefix);
    let match_start = context.len();
    context.push_str(match_raw);
    let match_end = context.len();
    context.push_str(&collapse_ws(suffix_raw));
    if hi < s.len() {
        context.push('…');
    }
    MentionSnippet {
        matched: match_raw.to_string(),
        context,
        match_start,
        match_end,
    }
}

fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    out
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    let n = s.len();
    while i < n && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

fn walk_content(v: &serde_json::Value, turn: &mut Turn) {
    match v {
        serde_json::Value::String(s) if !s.trim().is_empty() => {
            turn.texts.push(s.clone());
        }
        serde_json::Value::Array(arr) => {
            for block in arr {
                let Some(obj) = block.as_object() else {
                    continue;
                };
                let ty = obj.get("type").and_then(|x| x.as_str()).unwrap_or("");
                match ty {
                    "text" => {
                        if let Some(t) = obj.get("text").and_then(|x| x.as_str()) {
                            turn.texts.push(t.to_string());
                        }
                    }
                    "thinking" => {
                        if let Some(t) = obj.get("thinking").and_then(|x| x.as_str()) {
                            turn.thinking.push(t.to_string());
                        }
                    }
                    "tool_use" => {
                        let name = obj
                            .get("name")
                            .and_then(|x| x.as_str())
                            .unwrap_or("(tool)")
                            .to_string();
                        let args = obj
                            .get("input")
                            .map(|i| serde_json::to_string(i).unwrap_or_default())
                            .unwrap_or_default();
                        turn.tool_uses.push((name, args));
                    }
                    "tool_result" => {
                        // `content` here is either a string or an array of
                        // text blocks. Truncate to a preview — the raw file
                        // is there for deep inspection.
                        let text = obj
                            .get("content")
                            .map(|c| match c {
                                serde_json::Value::String(s) => s.clone(),
                                serde_json::Value::Array(arr) => arr
                                    .iter()
                                    .filter_map(|b| {
                                        b.get("text").and_then(|x| x.as_str()).map(String::from)
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                                _ => String::new(),
                            })
                            .unwrap_or_default();
                        if !text.trim().is_empty() {
                            turn.tool_results.push(truncate(&text, 2_000));
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}
