//! End-to-end smoke test: boot the real router on a random port, exercise the
//! public JSON endpoints, and assert status + shape.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use repo_recall::{db::CacheDb, display::routes, AppState};

async fn boot() -> (String, tokio::task::JoinHandle<()>) {
    boot_with(repo_recall::ingest::github::RemoteFetchState::Ok(
        repo_recall::ingest::github::AuthedUser {
            login: "test-viewer".into(),
        },
    ))
    .await
}

async fn boot_with(
    viewer: repo_recall::ingest::github::RemoteFetchState<repo_recall::ingest::github::AuthedUser>,
) -> (String, tokio::task::JoinHandle<()>) {
    // Point session ingest at an empty dir, not the operator's real
    // `~/.claude/projects`. These tests exercise the HTTP surface, not
    let sessions_dir = std::env::temp_dir().join(format!("repo-recall-sessions-{}", uuid_like()));
    std::fs::create_dir_all(&sessions_dir).unwrap();
    std::env::set_var("REPO_RECALL_SESSIONS_DIR", &sessions_dir);

    // Unique cache dir per test run so parallel `cargo test` invocations
    // don't collide.
    let cache_dir = std::env::temp_dir().join(format!("repo-recall-test-{}", uuid_like()));
    let cache_db = CacheDb::open_in_dir(&cache_dir).expect("cache db");

    let state_dir = std::env::temp_dir().join(format!("repo-recall-state-{}", uuid_like()));
    std::fs::create_dir_all(&state_dir).unwrap();
    let index_dir = state_dir.join("idx");
    let search_index = repo_recall::search::SearchIndex::open_at(&index_dir).expect("search index");

    let state = AppState {
        cache_db,
        cwd: std::env::temp_dir(),
        scan_depth: 0,
        commits_per_repo: 50,
        refresh_interval_secs: 0,
        remote_target_limit: 0,
        remote_first: false,
        refresh_lock: Arc::new(Mutex::new(())),
        last_scan: Arc::new(Mutex::new(None)),
        viewer: Arc::new(Mutex::new(viewer)),
        my_git_email: Arc::new(Mutex::new(None)),
        scan_version: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        search_index,
        remote_backoff_until: Arc::new(Mutex::new(None)),
        remote_backoff_secs: Arc::new(Mutex::new(0)),
        last_good_remote: Arc::new(Mutex::new(std::collections::HashMap::new())),
        github_client: repo_recall::ingest::github::build_client(),
        forgejo_client: repo_recall::ingest::forgejo::build_client("forgejo.coilysiren.me"),
        remote_kind_cache: repo_recall::ingest::remote_kind::RemoteKindCache::new(),
    };

    let app = routes::router(state);
    let addr: SocketAddr = ([127, 0, 0, 1], 0).into();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{bound}"), handle)
}

fn uuid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    format!("{nanos}-{}-{n}", std::process::id())
}

#[tokio::test]
async fn dashboard_returns_json() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let res = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/json"),
    );
    assert!(res.headers().get("etag").is_some(), "missing ETag");
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.get("repos").is_some());
    assert!(body.get("banner").is_some());
    assert!(body.get("scan_version").is_some());
    assert!(body.get("action_required").is_some());
    assert!(body.get("gh_health").is_some());
}

#[tokio::test]
async fn unknown_path_is_404_json() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/does-not-exist"))
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
    assert_eq!(
        res.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/json"),
    );
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["error"], "not_found");
    assert_eq!(body["path"], "/does-not-exist");
}

#[tokio::test]
async fn removed_html_routes_return_404() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::new();
    for path in ["/repos/99999", "/sessions/99999", "/search", "/refresh"] {
        assert_eq!(
            client
                .get(format!("{base}{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            404,
            "expected 404 for removed route {path}"
        );
    }
}

#[tokio::test]
async fn action_required_endpoint_returns_json() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/api/action-required"))
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert!(res.headers().get("etag").is_some());
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.get("repos").is_some(), "missing repos array");
    assert!(body.get("scan_version").is_some());
    assert!(body.get("generated_at").is_some());
}

#[tokio::test]
async fn ticket_history_endpoint_returns_empty_for_unknown_issue() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/api/repos/1/tickets/42/history"))
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["repo_id"], 1);
    assert_eq!(body["issue_number"], 42);
    assert!(
        body["sessions"].as_array().unwrap().is_empty(),
        "expected empty sessions, got {body}"
    );
    assert!(
        body["commits"].as_array().unwrap().is_empty(),
        "expected empty commits, got {body}"
    );
}

#[tokio::test]
async fn etag_returns_304_when_unchanged() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::new();
    let first = client
        .get(format!("{base}/api/action-required"))
        .send()
        .await
        .unwrap();
    let etag = first.headers().get("etag").unwrap().clone();
    let second = client
        .get(format!("{base}/api/action-required"))
        .header("if-none-match", etag)
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 304);
}

#[tokio::test]
async fn scan_version_endpoint_is_cheap_poll() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/api/scan-version"))
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.get("scan_version").is_some());
}

#[tokio::test]
async fn api_refresh_returns_ok() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{base}/api/refresh"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
}

#[tokio::test]
async fn gh_health_surfaces_in_dashboard_json() {
    let (base, _h) = boot_with(repo_recall::ingest::github::RemoteFetchState::Unconfigured).await;
    let body: serde_json::Value = reqwest::get(format!("{base}/"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["gh_health"], "unconfigured");
}

#[tokio::test]
async fn openapi_doc_is_served() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/openapi.json")).await.unwrap();
    assert_eq!(res.status(), 200);
    let doc: serde_json::Value = res.json().await.unwrap();
    assert_eq!(doc["openapi"], "3.1.0");
    assert!(doc["paths"]["/"].is_object(), "openapi missing root path");
    assert!(
        doc["paths"]["/api/action-required"].is_object(),
        "openapi missing /api/action-required"
    );
}

#[tokio::test]
async fn git_log_parses_commits() {
    // Build a throwaway repo in a tempdir, drop two commits, and assert
    // `commits::scan` pulls them back with correct SHAs + subjects. Catches
    use std::process::Command;

    let dir = std::env::temp_dir().join(format!("repo-recall-gittest-{}", uuid_like()));
    std::fs::create_dir_all(&dir).unwrap();
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed: {:?}", out);
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test User"]);
    run(&["commit", "--allow-empty", "-q", "-m", "first: add nothing"]);
    run(&[
        "commit",
        "--allow-empty",
        "-q",
        "-m",
        "second: still nothing, with a \t tab",
    ]);

    let records = repo_recall::ingest::git::log::scan(&dir, 10).unwrap();
    assert_eq!(
        records.len(),
        2,
        "expected 2 commits, got {}",
        records.len()
    );
    // git log returns newest-first.
    assert!(records[0].subject.starts_with("second:"));
    assert!(records[1].subject.starts_with("first:"));
    assert_eq!(records[0].author_name, "Test User");
    assert_eq!(records[0].sha.len(), 40);

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn worktree_snapshot_drops_stat_stale_modifications() {
    // Set up a real repo, commit a file, then mutate the file's mtime
    // without changing its content. `git status` will report it as
    use std::process::Command;

    let dir = std::env::temp_dir().join(format!("repo-recall-statestale-{}", uuid_like()));
    std::fs::create_dir_all(&dir).unwrap();
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed: {:?}", out);
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test User"]);
    let tracked = dir.join("tracked.txt");
    std::fs::write(&tracked, "hello\n").unwrap();
    run(&["add", "tracked.txt"]);
    run(&["commit", "-q", "-m", "add tracked.txt"]);

    let touch = Command::new("touch")
        .args(["-t", "203012311200", tracked.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(touch.success(), "touch failed");

    std::fs::write(dir.join("new.txt"), "fresh\n").unwrap();

    let snap = repo_recall::ingest::git::log::worktree_snapshot(&dir, 16);
    assert_eq!(
        snap.total_modified, 0,
        "stat-stale modification should be dropped, got {} modified",
        snap.total_modified
    );
    assert_eq!(
        snap.total_untracked, 1,
        "untracked count should be unaffected by stat-stale logic"
    );
    assert!(
        snap.files.iter().all(|f| f.path != "tracked.txt"),
        "stat-stale path should not appear in the file sample"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn cli_guard_audit_ingest_round_trips_through_cache() {
    // End-to-end exercise of the cli-guard audit JSONL ingest path (#148).
    use repo_recall::ingest::cli_guard::audit_jsonl as audit;

    let cache_dir = std::env::temp_dir().join(format!("repo-recall-audit-{}", uuid_like()));
    let cache_db = CacheDb::open_in_dir(&cache_dir).expect("cache db");

    let audit_dir = std::env::temp_dir().join(format!("repo-recall-audit-shard-{}", uuid_like()));
    std::fs::create_dir_all(&audit_dir).unwrap();
    std::fs::write(
        audit_dir.join("coilysiren-repo-recall.jsonl"),
        concat!(
            r#"{"id":"019e288b-280c-7fde-85fd-f323b3086b13","ts":1778796668,"decision":"accept","verb":"ops.gh","argv":["ward-kdl","ops","gh","whoami"],"exit_code":0,"duration_ms":1000,"commit_scope":"/repo/r1"}"#,
            "\n",
            r#"{"id":"019e2890-aaaa-bbbb-cccc-dddddddddddd","ts":1778796700,"decision":"deny","verb":"pkg.cargo","argv":["ward","pkg","cargo","build"],"exit_code":1,"commit_scope":"/repo/r2","audit_override":true}"#,
            "\n",
            r#"{"id":"019e2891-eeee-ffff-aaaa-bbbbbbbbbbbb","ts":1778796720,"verb":"whoami","argv":["ward","whoami"]}"#,
            "\n",
            // Current ward stamps the git toplevel as `repo_root` and omits
            // `commit_scope`; this row must still route to r1 via the fallback.
            r#"{"id":"019e2892-1111-2222-3333-444444444444","ts":1778796740,"decision":"accept","verb":"ops.aws","argv":["ward-kdl","ops","aws","whoami"],"repo_root":"/repo/r1"}"#,
            "\n",
            "\nnot json\n",
        ),
    )
    .unwrap();

    let repos = cache_db
        .write_batch(|w| {
            let id1 = w.upsert_repo("/repo/r1", "r1", 0, None, None)?;
            let id2 = w.upsert_repo("/repo/r2", "r2", 0, None, None)?;
            Ok(vec![
                (id1, std::path::PathBuf::from("/repo/r1")),
                (id2, std::path::PathBuf::from("/repo/r2")),
            ])
        })
        .expect("seed repos");

    let files = audit::list_audit_files(&audit_dir).expect("list");
    assert_eq!(files.len(), 1);

    let by_path: std::collections::HashMap<String, i64> = repos
        .iter()
        .map(|(id, p)| (p.to_string_lossy().into_owned(), *id))
        .collect();

    cache_db
        .write_batch(|w| {
            for path in &files {
                for rec in audit::parse_audit_file(path).expect("parse") {
                    let repo_id = rec
                        .commit_scope
                        .as_deref()
                        .or(rec.repo_root.as_deref())
                        .and_then(|s| by_path.get(s).copied())
                        .unwrap_or(0);
                    w.upsert_audit_event(repo_id, &rec)?;
                }
            }
            Ok(())
        })
        .expect("write");

    let all = cache_db.list_all_audit_events().expect("read all");
    assert_eq!(
        all.len(),
        4,
        "three routed + one unrouted, malformed dropped"
    );

    let r1 = cache_db
        .audit_events_for_repo(repos[0].0, None, 50)
        .expect("r1");
    // commit_scope row + repo_root-only row both route to r1; newest first.
    assert_eq!(r1.len(), 2);
    assert_eq!(r1[0].verb, "ops.aws");
    assert_eq!(r1[1].verb, "ops.gh");

    let r2 = cache_db
        .audit_events_for_repo(repos[1].0, None, 50)
        .expect("r2");
    assert_eq!(r2.len(), 1);
    assert_eq!(r2[0].decision, "deny");
    assert!(r2[0].audit_override);

    let unrouted = cache_db
        .audit_events_for_repo(0, None, 50)
        .expect("unrouted");
    assert_eq!(unrouted.len(), 1);
    assert_eq!(unrouted[0].verb, "whoami");

    cache_db
        .write_batch(|w| {
            for path in &files {
                for rec in audit::parse_audit_file(path).expect("parse2") {
                    let repo_id = rec
                        .commit_scope
                        .as_deref()
                        .or(rec.repo_root.as_deref())
                        .and_then(|s| by_path.get(s).copied())
                        .unwrap_or(0);
                    let (_id, was_new) = w.upsert_audit_event(repo_id, &rec)?;
                    assert!(!was_new, "second upsert of same event_id should not insert");
                }
            }
            Ok(())
        })
        .expect("write 2");
    assert_eq!(cache_db.list_all_audit_events().expect("count").len(), 4);

    std::fs::remove_dir_all(&audit_dir).ok();
    std::fs::remove_dir_all(&cache_dir).ok();
}
