//! MCP-protocol integration smoke. Spawns the `repo-recall` binary, talks
//! line-delimited JSON-RPC over stdio, and asserts the surface every host
//! relies on (initialize, tools/list, resources/list, tools/call) is
//! intact end-to-end.
//!
//! Replaces the deleted axum-router `tests/smoke.rs` (the MCP rewrite removed
//! axum's tool-routes anyway) and supersedes `scripts/mcp-smoke.py`. The
//! Python harness was a single-shot manual probe with no per-tool assertions;
//! this suite gives real failure attribution under `cargo test`.
//!
//! Each test gets its own `$TMPDIR` cache + state dir keyed on nanos + PID +
//! atomic counter so parallel `cargo test` invocations don't collide.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

const PROTOCOL_VERSION: &str = "2025-06-18";

/// Per-test isolation token. Same shape the old smoke.rs used.
fn unique_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{nanos}-{}-{n}", std::process::id())
}

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    /// Buffered stdout. Locked across reads because `read_id` may be called
    /// concurrently in future tests; today everything is single-threaded but
    /// the lock costs nothing and keeps the invariant explicit.
    stdout: Mutex<BufReader<ChildStdout>>,
    next_id: AtomicU64,
    _scratch_dir: PathBuf,
}

impl McpClient {
    fn spawn() -> Self {
        let scratch = std::env::temp_dir().join(format!("repo-recall-mcp-test-{}", unique_id()));
        std::fs::create_dir_all(&scratch).expect("scratch dir");
        let cache_dir = scratch.join("cache");
        std::fs::create_dir_all(&cache_dir).expect("cache dir");

        let state_dir = scratch.join("state");
        let index_dir = scratch.join("idx");
        std::fs::create_dir_all(&state_dir).expect("state dir");
        std::fs::create_dir_all(&index_dir).expect("index dir");

        // Point session ingest at an empty dir, not the operator's real
        // `~/.claude/projects`. The smoke tests assert refresh mechanics
        // and payload shape, not session content — and turn-indexing
        // hundreds of MB of real JSONL into tantivy (#229) under a
        // parallel `cargo test` blows the 20s initial-scan deadline.
        let sessions_dir = scratch.join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("sessions dir");

        let bin = env!("CARGO_BIN_EXE_repo-recall");
        let mut child = Command::new(bin)
            // Loopback bind on a port that is almost certainly free. The
            // test does not exercise the axum surface; the bind exists only
            // so the process stays up. Picking an ephemeral port avoids
            // collisions with brew-services or another test instance.
            .env("REPO_RECALL_PORT", "0")
            .env("REPO_RECALL_CWD", &scratch)
            .env("REPO_RECALL_CACHE_DIR", &cache_dir)
            // State + tantivy must be per-test or redb's exclusive file lock
            // collides with a brew-services-managed repo-recall on the same
            // machine. The cache DB is already per-port (set above).
            .env("REPO_RECALL_STATE_DIR", &state_dir)
            .env("REPO_RECALL_INDEX_DIR", &index_dir)
            .env("REPO_RECALL_REFRESH_INTERVAL_SECS", "0")
            .env("REPO_RECALL_DEPTH", "0")
            .env("REPO_RECALL_SESSIONS_DIR", &sessions_dir)
            .env("RUST_LOG", "warn")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn repo-recall");

        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));

        let mut client = McpClient {
            child,
            stdin,
            stdout: Mutex::new(stdout),
            next_id: AtomicU64::new(1),
            _scratch_dir: scratch,
        };
        client.handshake();
        client
    }

    fn handshake(&mut self) {
        let init = self.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "mcp_smoke", "version": "0"},
            }),
        );
        let server_info = init
            .get("serverInfo")
            .unwrap_or_else(|| panic!("initialize missing serverInfo: {init}"));
        assert_eq!(
            server_info.get("name").and_then(|v| v.as_str()),
            Some("repo-recall")
        );
        let version = server_info
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        assert!(!version.is_empty(), "serverInfo.version was empty");

        self.notify("notifications/initialized", json!({}));
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.send(&msg);
        let resp = self.read_id(id);
        if let Some(err) = resp.get("error") {
            panic!("{method} returned JSON-RPC error: {err}");
        }
        resp.get("result")
            .cloned()
            .unwrap_or_else(|| panic!("{method} missing result: {resp}"))
    }

    /// Like `request`, but tolerates a JSON-RPC error response and returns it.
    fn request_allow_error(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.send(&msg);
        self.read_id(id)
    }

    fn notify(&mut self, method: &str, params: Value) {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.send(&msg);
    }

    fn send(&mut self, msg: &Value) {
        let line = serde_json::to_string(msg).expect("serialize");
        self.stdin
            .write_all(line.as_bytes())
            .expect("write request");
        self.stdin.write_all(b"\n").expect("write newline");
        self.stdin.flush().expect("flush stdin");
    }

    /// Read responses until we see one matching `id`, with a wall-clock
    /// budget. Server-side notifications and unrelated responses are skipped.
    fn read_id(&self, id: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut guard = self.stdout.lock().expect("stdout lock");
        loop {
            if Instant::now() >= deadline {
                panic!("timed out waiting for response id={id}");
            }
            let mut line = String::new();
            let n = guard.read_line(&mut line).expect("read stdout");
            if n == 0 {
                panic!("stdout EOF before response id={id}");
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue, // tracing/log noise leaking onto stdout would break framing; tolerate
            };
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                return v;
            }
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn initialize_returns_real_server_info() {
    // Drop is the real assertion: handshake() inside spawn() panics on
    // missing/empty fields.
    let _client = McpClient::spawn();
}

#[test]
fn tools_list_exposes_seven_tools() {
    let mut client = McpClient::spawn();
    let res = client.request("tools/list", json!({}));
    let tools = res
        .get("tools")
        .and_then(|v| v.as_array())
        .expect("tools array");
    assert_eq!(
        tools.len(),
        7,
        "expected 7 tools, got {}: {res}",
        tools.len()
    );

    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|n| n.as_str()))
        .collect();
    for required in [
        "recall_dashboard",
        "recall_repo",
        "recall_session",
        "recall_search",
        "recall_action_required",
        "recall_ticket_history",
        "recall_refresh",
    ] {
        assert!(
            names.contains(&required),
            "missing tool {required}: {names:?}"
        );
    }
}

/// Pull the JSON payload out of a `tools/call` result. pmcp returns either
/// `structuredContent` (when the handler's return type generates a schema) or
/// a single text content item whose body is the JSON string. Tolerate both —
/// the contract every host actually consumes is "this is the payload."
fn tool_payload(res: &Value) -> Value {
    if let Some(s) = res.get("structuredContent") {
        return s.clone();
    }
    let text = res
        .pointer("/content/0/text")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            panic!("tool result has neither structuredContent nor content[0].text: {res}")
        });
    serde_json::from_str(text).unwrap_or_else(|e| panic!("content text is not JSON ({e}): {text}"))
}

/// Block until the initial background scan bumps `scan_version` past 0, so a
/// subsequent `recall_refresh` is not coalesced into the still-running
/// initial scan. ~10s for the scan body plus a tantivy commit on a fresh
/// index, which is the bulk of the time on an empty cwd.
fn wait_for_initial_scan(client: &mut McpClient) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let res = client.request(
            "tools/call",
            json!({"name": "recall_dashboard", "arguments": {}}),
        );
        let payload = tool_payload(&res);
        let v = payload
            .get("scan_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if v > 0 {
            return v;
        }
        if Instant::now() >= deadline {
            panic!("initial scan never bumped scan_version: {payload}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn refresh_runs_and_bumps_scan_version() {
    let mut client = McpClient::spawn();
    let initial = wait_for_initial_scan(&mut client);

    let res = client.request(
        "tools/call",
        json!({"name": "recall_refresh", "arguments": {}}),
    );
    let payload = tool_payload(&res);
    assert_eq!(
        payload.get("ran").and_then(|v| v.as_bool()),
        Some(true),
        "ran=true expected: {payload}"
    );
    let after = payload
        .get("scan_version_after")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| panic!("scan_version_after missing: {payload}"));
    let before = payload
        .get("scan_version_before")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| panic!("scan_version_before missing: {payload}"));
    assert!(
        after > before,
        "scan_version did not advance ({before} -> {after})"
    );
    assert!(
        after > initial,
        "scan_version below pre-refresh baseline ({initial} -> {after})"
    );
}

#[test]
fn dashboard_returns_structured_payload() {
    let mut client = McpClient::spawn();
    let _ = wait_for_initial_scan(&mut client);

    let res = client.request(
        "tools/call",
        json!({"name": "recall_dashboard", "arguments": {}}),
    );
    let payload = tool_payload(&res);
    for key in [
        "scan_version",
        "session_count",
        "commits_30d",
        "repos",
        "action_required",
    ] {
        assert!(
            payload.get(key).is_some(),
            "payload missing {key}: {payload}"
        );
    }
    assert!(
        payload.get("repos").and_then(|v| v.as_array()).is_some(),
        "repos should be an array: {payload}"
    );
}

#[test]
fn search_with_empty_query_rejects() {
    let mut client = McpClient::spawn();
    // Empty query is a validation error: the tool exists, the input is bad.
    // pmcp may surface this as a JSON-RPC error or as a tool-result with
    // `isError: true`. Accept either shape; the contract is "this is not a
    // success."
    let resp = client.request_allow_error(
        "tools/call",
        json!({"name": "recall_search", "arguments": {"q": ""}}),
    );
    if let Some(err) = resp.get("error") {
        assert!(!err.is_null());
        return;
    }
    let result = resp
        .get("result")
        .unwrap_or_else(|| panic!("response missing result: {resp}"));
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    assert!(
        is_error,
        "empty-query search should fail (JSON-RPC error or isError=true): {result}"
    );
}
