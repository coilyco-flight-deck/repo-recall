// Data source #3: OTel spans dropped as JSON files into a watched directory.
//
// File-drop is the tracer-quality ingest path for the LUCA substrate (see
// luca#27). Producers (otel-a2a-relay, plus the Claude Code subagent hook)
// write one JSON file per span. Each refresh re-reads the directory; the
// cache is wipe-on-restart so files on disk are the source of truth.
//
// Span schema is OTLP-flavored but flat (one span per file, not OTLP's
// resourceSpans/scopeSpans nesting). Required: trace_id, span_id. Everything
// else is optional. Attributes is an arbitrary JSON object; we extract
// agent.role, session.uuid, and repo for indexing convenience but keep the
// full attributes blob alongside.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct SpanRecord {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub name: String,
    pub start_time_unix_nano: i64,
    pub end_time_unix_nano: i64,
    pub agent_role: Option<String>,
    pub session_uuid: Option<String>,
    pub repo: Option<String>,
    pub attributes_json: String,
    pub source_file: String,
}

#[derive(Debug, Deserialize)]
struct RawSpan {
    #[serde(default, alias = "traceId")]
    trace_id: Option<String>,
    #[serde(default, alias = "spanId")]
    span_id: Option<String>,
    #[serde(default, alias = "parentSpanId")]
    parent_span_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, alias = "startTimeUnixNano")]
    start_time_unix_nano: Option<i64>,
    #[serde(default, alias = "endTimeUnixNano")]
    end_time_unix_nano: Option<i64>,
    #[serde(default)]
    attributes: serde_json::Value,
}

/// Resolve the spans ingest directory. Honors `REPO_RECALL_SPANS_DIR`;
/// otherwise falls back to `~/.local/share/repo-recall/spans/`. Returns
/// `None` when neither resolves to an existing directory, in which case
/// the refresh skips spans ingest entirely.
pub fn default_spans_dir() -> Option<PathBuf> {
    if let Some(over) = std::env::var_os("REPO_RECALL_SPANS_DIR") {
        let dir = PathBuf::from(over);
        if dir.is_dir() {
            return Some(dir);
        }
        tracing::warn!(
            "REPO_RECALL_SPANS_DIR set to {:?} but is not a directory; skipping spans ingest",
            dir
        );
        return None;
    }
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("repo-recall")
        .join("spans");
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

/// Enumerate every `.json` file under the spans directory.
pub fn list_span_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let p = entry.path();
        if p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("json") {
            out.push(p.to_path_buf());
        }
    }
    Ok(out)
}

/// Parse one span file. Returns `Ok(None)` when the file is malformed or
/// missing required fields. Mirrors `sessions::parse_session_file` semantics:
/// individual bad files do not abort the whole ingest sweep.
pub fn parse_span_file(path: &Path) -> Result<Option<SpanRecord>> {
    let bytes = std::fs::read(path)?;
    let raw: RawSpan = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!("span parse error in {}: {}", path.display(), e);
            return Ok(None);
        }
    };
    let (Some(trace_id), Some(span_id)) = (raw.trace_id, raw.span_id) else {
        tracing::debug!(
            "span file {} missing trace_id or span_id; skipping",
            path.display()
        );
        return Ok(None);
    };
    if trace_id.is_empty() || span_id.is_empty() {
        return Ok(None);
    }
    let agent_role = raw
        .attributes
        .get("agent.role")
        .and_then(|v| v.as_str())
        .map(String::from);
    let session_uuid = raw
        .attributes
        .get("session.uuid")
        .and_then(|v| v.as_str())
        .map(String::from);
    let repo = raw
        .attributes
        .get("repo")
        .and_then(|v| v.as_str())
        .map(String::from);
    let attributes_json = serde_json::to_string(&raw.attributes).unwrap_or_else(|_| "{}".into());
    Ok(Some(SpanRecord {
        trace_id,
        span_id,
        parent_span_id: raw.parent_span_id,
        name: raw.name.unwrap_or_default(),
        start_time_unix_nano: raw.start_time_unix_nano.unwrap_or(0),
        end_time_unix_nano: raw.end_time_unix_nano.unwrap_or(0),
        agent_role,
        session_uuid,
        repo,
        attributes_json,
        source_file: path.to_string_lossy().into_owned(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpdir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "repo-recall-spans-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_span(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn parses_minimal_span() {
        let dir = tmpdir();
        let path = write_span(
            &dir,
            "minimal.json",
            r#"{"trace_id":"t1","span_id":"s1","name":"agent.run"}"#,
        );
        let rec = parse_span_file(&path).unwrap().expect("Some");
        assert_eq!(rec.trace_id, "t1");
        assert_eq!(rec.span_id, "s1");
        assert_eq!(rec.name, "agent.run");
        assert_eq!(rec.parent_span_id, None);
        assert_eq!(rec.agent_role, None);
    }

    #[test]
    fn parses_otel_camelcase_aliases() {
        let dir = tmpdir();
        let path = write_span(
            &dir,
            "otel.json",
            r#"{"traceId":"t2","spanId":"s2","parentSpanId":"s1","name":"subagent",
                "startTimeUnixNano":1700000000000000000,
                "endTimeUnixNano":1700000001000000000,
                "attributes":{"agent.role":"attacker","session.uuid":"u","repo":"luca"}}"#,
        );
        let rec = parse_span_file(&path).unwrap().expect("Some");
        assert_eq!(rec.trace_id, "t2");
        assert_eq!(rec.span_id, "s2");
        assert_eq!(rec.parent_span_id.as_deref(), Some("s1"));
        assert_eq!(rec.start_time_unix_nano, 1700000000000000000);
        assert_eq!(rec.agent_role.as_deref(), Some("attacker"));
        assert_eq!(rec.session_uuid.as_deref(), Some("u"));
        assert_eq!(rec.repo.as_deref(), Some("luca"));
    }

    #[test]
    fn rejects_missing_required_fields() {
        let dir = tmpdir();
        let no_trace = write_span(&dir, "no_trace.json", r#"{"span_id":"s"}"#);
        assert!(parse_span_file(&no_trace).unwrap().is_none());
        let no_span = write_span(&dir, "no_span.json", r#"{"trace_id":"t"}"#);
        assert!(parse_span_file(&no_span).unwrap().is_none());
        let empty = write_span(&dir, "empty.json", r#"{"trace_id":"","span_id":""}"#);
        assert!(parse_span_file(&empty).unwrap().is_none());
    }

    #[test]
    fn malformed_json_returns_none_not_error() {
        let dir = tmpdir();
        let path = write_span(&dir, "bad.json", "{this isn't json");
        assert!(parse_span_file(&path).unwrap().is_none());
    }

    #[test]
    fn list_span_files_only_picks_json() {
        let dir = tmpdir();
        write_span(&dir, "a.json", r#"{"trace_id":"t","span_id":"s"}"#);
        write_span(&dir, "b.json", r#"{"trace_id":"t","span_id":"s2"}"#);
        write_span(&dir, "ignored.txt", "not a span");
        let files = list_span_files(&dir).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files
            .iter()
            .all(|p| p.extension().and_then(|e| e.to_str()) == Some("json")));
    }
}
