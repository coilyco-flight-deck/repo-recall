//! End-to-end smoke test: boot the real router on a random port, exercise the
//! public endpoints, and assert they return the right status codes and have
//! the expected HTML scaffolding. This is intentionally shallow — it catches
//! "did the router compile and serve HTML" regressions, not fine-grained
//! content bugs.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, Mutex};

use repo_recall::{db::CacheDb, routes, AppState};

async fn boot() -> (String, tokio::task::JoinHandle<()>) {
    boot_with(repo_recall::commits::GhHealth::Ok).await
}

async fn boot_with(gh: repo_recall::commits::GhHealth) -> (String, tokio::task::JoinHandle<()>) {
    // Unique cache dir per test run so parallel `cargo test` invocations
    // don't collide.
    let cache_dir = std::env::temp_dir().join(format!("repo-recall-test-{}", uuid_like()));
    let cache_db = CacheDb::open_in_dir(&cache_dir).expect("cache db");

    let state_dir = std::env::temp_dir().join(format!("repo-recall-state-{}", uuid_like()));
    std::fs::create_dir_all(&state_dir).unwrap();
    let index_dir = state_dir.join("idx");
    let search_index = repo_recall::search::SearchIndex::open_at(&index_dir).expect("search index");

    let (progress_tx, _) = broadcast::channel::<String>(16);
    let state = AppState {
        cache_db,
        cwd: std::env::temp_dir(),
        scan_depth: 0,
        commits_per_repo: 50,
        refresh_interval_secs: 0,
        remote_target_limit: 0,
        progress_tx,
        refresh_lock: Arc::new(Mutex::new(())),
        last_scan: Arc::new(Mutex::new(None)),
        gh_health: Arc::new(Mutex::new(gh)),
        my_gh_login: Arc::new(Mutex::new(None)),
        my_git_email: Arc::new(Mutex::new(None)),
        scan_version: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        search_index,
        demo_mode: false,
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
async fn dashboard_renders() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    let res = client.get(format!("{base}/")).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let body = res.text().await.unwrap();
    assert!(body.contains("<title>repo-recall"), "missing title tag");
    assert!(
        body.contains("id=\"scan-status\""),
        "missing scan-status element"
    );
    assert!(
        body.contains("/livereload"),
        "livereload script not wired in"
    );
}

#[tokio::test]
async fn unknown_path_is_404() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/does-not-exist"))
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
    let body = res.text().await.unwrap();
    assert!(body.contains("404"), "404 page should say 404");
    assert!(body.contains("/does-not-exist"), "404 should echo the path");
}

#[tokio::test]
async fn unknown_repo_and_session_return_404() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{base}/repos/99999"))
            .send()
            .await
            .unwrap()
            .status(),
        404,
    );
    assert_eq!(
        client
            .get(format!("{base}/sessions/99999"))
            .send()
            .await
            .unwrap()
            .status(),
        404,
    );
}

#[tokio::test]
async fn static_assets_are_served() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::new();
    for path in [
        "/static/tailwind.css",
        "/static/livereload.js",
        "/static/icons/icon-192.png",
        "/static/icons/icon-512.png",
        "/static/manifest.webmanifest",
    ] {
        let res = client.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(res.status(), 200, "expected 200 for {path}");
    }
}

#[tokio::test]
async fn dashboard_serves_json_via_accept() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::new();
    let res = client
        .get(format!("{base}/"))
        .header("accept", "application/json")
        .send()
        .await
        .unwrap();
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
}

#[tokio::test]
async fn dashboard_serves_json_via_format_param() {
    let (base, _h) = boot().await;
    let res = reqwest::get(format!("{base}/?format=json")).await.unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert!(body.get("action_required").is_some());
    assert!(body.get("gh_health").is_some());
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
async fn dashboard_html_carries_data_attrs_when_repos_present() {
    // No repos in the test boot, so data-repo-id won't appear, but the
    // surrounding markup contract should still be present (no panic, valid
    // HTML). Just verifies the new attribute names don't break rendering.
    let (base, _h) = boot().await;
    let body = reqwest::get(format!("{base}/"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    // The repo-list block is empty in the test environment, but the action
    // banner / counters should still render.
    assert!(body.contains("<title>repo-recall"));
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
async fn refresh_returns_accepted() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::new();
    let res = client.post(format!("{base}/refresh")).send().await.unwrap();
    assert_eq!(res.status(), 202);
}

#[tokio::test]
async fn gh_missing_shows_warning_banner() {
    let (base, _h) = boot_with(repo_recall::commits::GhHealth::Missing).await;
    let body = reqwest::get(format!("{base}/"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("gh CLI not found"), "banner message missing");
    assert!(body.contains("⚠"), "warning emoji missing");
}

#[tokio::test]
async fn gh_ok_hides_warning_banner() {
    let (base, _h) = boot_with(repo_recall::commits::GhHealth::Ok).await;
    let body = reqwest::get(format!("{base}/"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        !body.contains("gh CLI not found"),
        "banner leaked when healthy"
    );
    assert!(!body.contains("gh CLI not authenticated"));
}

#[tokio::test]
async fn api_spans_filters_by_repo_and_since() {
    use repo_recall::db::CacheDb;

    // Boot a fresh router and inject a few spans directly into its cache,
    // then hit /api/spans with various filter combos.
    let cache_dir = std::env::temp_dir().join(format!("repo-recall-spans-api-{}", uuid_like()));
    let cache_db = CacheDb::open_in_dir(&cache_dir).expect("cache db");
    cache_db
        .write_batch(|w| {
            // Older span in repo `luca`.
            w.upsert_span(
                "trace-a",
                "span-old",
                None,
                "agent.run",
                1_000_000_000_000_000_000,
                1_000_000_000_000_000_000,
                Some("attacker"),
                Some("sess-1"),
                Some("luca"),
                "{}",
                "/spans/old.json",
            )?;
            // Newer span in repo `luca`.
            w.upsert_span(
                "trace-a",
                "span-new",
                Some("span-old"),
                "agent.run",
                2_000_000_000_000_000_000,
                2_000_000_000_000_000_000,
                Some("inspector"),
                Some("sess-1"),
                Some("luca"),
                "{}",
                "/spans/new.json",
            )?;
            // Span in a different repo.
            w.upsert_span(
                "trace-b",
                "span-other",
                None,
                "agent.run",
                1_500_000_000_000_000_000,
                1_500_000_000_000_000_000,
                Some("attacker"),
                Some("sess-2"),
                Some("repo-recall"),
                "{}",
                "/spans/other.json",
            )?;
            Ok(())
        })
        .expect("seed");

    // Hand-roll the AppState rather than using boot() so we can pre-seed
    // the cache before the router starts serving.
    let state_dir = std::env::temp_dir().join(format!("repo-recall-state-spans-{}", uuid_like()));
    std::fs::create_dir_all(&state_dir).unwrap();
    let search_index =
        repo_recall::search::SearchIndex::open_at(&state_dir.join("idx")).expect("idx");
    let (progress_tx, _) = tokio::sync::broadcast::channel::<String>(16);
    let state = repo_recall::AppState {
        cache_db,
        cwd: std::env::temp_dir(),
        scan_depth: 0,
        commits_per_repo: 50,
        refresh_interval_secs: 0,
        remote_target_limit: 0,
        progress_tx,
        refresh_lock: std::sync::Arc::new(tokio::sync::Mutex::new(())),
        last_scan: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        gh_health: std::sync::Arc::new(tokio::sync::Mutex::new(repo_recall::commits::GhHealth::Ok)),
        my_gh_login: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        my_git_email: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
        scan_version: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        search_index,
        demo_mode: false,
    };
    let app = repo_recall::routes::router(state);
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], 0).into();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound = listener.local_addr().unwrap();
    let _h = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{bound}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // No filters: all three, newest-first.
    let body: serde_json::Value = client
        .get(format!("{base}/api/spans"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let spans = body["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 3);
    assert_eq!(spans[0]["span_id"], "span-new");

    // Filter by repo.
    let body: serde_json::Value = client
        .get(format!("{base}/api/spans?repo=luca"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let spans = body["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 2);
    assert!(spans.iter().all(|s| s["repo"].as_str() == Some("luca")));

    // Filter by since (in unix-seconds): drop the oldest.
    let body: serde_json::Value = client
        .get(format!("{base}/api/spans?since=1500000000"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let spans = body["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 2);
    assert!(spans
        .iter()
        .all(|s| s["start_time_unix_nano"].as_i64().unwrap() >= 1_500_000_000_000_000_000));

    // Combined filter + limit.
    let body: serde_json::Value = client
        .get(format!("{base}/api/spans?repo=luca&since=0&limit=1"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let spans = body["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0]["span_id"], "span-new");

    std::fs::remove_dir_all(&cache_dir).ok();
    std::fs::remove_dir_all(&state_dir).ok();
}

#[tokio::test]
async fn span_ingest_round_trips_through_cache() {
    // End-to-end exercise of the OTel-spans ingest path (luca#27, repo-recall#63).
    // Drop two span files in a temp dir, run the producer pipeline
    // (list + parse + upsert), then verify list_all_spans returns them
    // both. Skips the env-var-resolution glue (default_spans_dir) on
    // purpose — that's a thin shim and process-global env writes race
    // other parallel tests.
    use repo_recall::spans;

    let cache_dir = std::env::temp_dir().join(format!("repo-recall-spans-{}", uuid_like()));
    let cache_db = repo_recall::db::CacheDb::open_in_dir(&cache_dir).expect("cache db");

    let spans_dir = std::env::temp_dir().join(format!("repo-recall-spansdir-{}", uuid_like()));
    std::fs::create_dir_all(&spans_dir).unwrap();
    std::fs::write(
        spans_dir.join("first.json"),
        r#"{"trace_id":"trace-a","span_id":"span-1","name":"agent.run",
            "attributes":{"agent.role":"attacker","repo":"luca"}}"#,
    )
    .unwrap();
    std::fs::write(
        spans_dir.join("second.json"),
        r#"{"traceId":"trace-a","spanId":"span-2","parentSpanId":"span-1",
            "name":"subagent.invoke","startTimeUnixNano":1700000000000000000,
            "endTimeUnixNano":1700000001000000000,
            "attributes":{"agent.role":"inspector","session.id":"sess-1"}}"#,
    )
    .unwrap();
    std::fs::write(spans_dir.join("ignored.txt"), "not a span").unwrap();

    let files = spans::list_span_files(&spans_dir).expect("list");
    assert_eq!(files.len(), 2, "list_span_files filters non-json");

    cache_db
        .write_batch(|w| {
            for path in &files {
                let rec = spans::parse_span_file(path).unwrap().expect("parsed");
                w.upsert_span(
                    &rec.trace_id,
                    &rec.span_id,
                    rec.parent_span_id.as_deref(),
                    &rec.name,
                    rec.start_time_unix_nano,
                    rec.end_time_unix_nano,
                    rec.agent_role.as_deref(),
                    rec.session_uuid.as_deref(),
                    rec.repo.as_deref(),
                    &rec.attributes_json,
                    &rec.source_file,
                )?;
            }
            Ok(())
        })
        .expect("write");

    let mut got = cache_db.list_all_spans().expect("read");
    got.sort_by(|a, b| a.span_id.cmp(&b.span_id));
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].span_id, "span-1");
    assert_eq!(got[0].agent_role.as_deref(), Some("attacker"));
    assert_eq!(got[0].repo.as_deref(), Some("luca"));
    assert_eq!(got[1].span_id, "span-2");
    assert_eq!(got[1].parent_span_id.as_deref(), Some("span-1"));
    assert_eq!(got[1].start_time_unix_nano, 1700000000000000000);
    assert_eq!(got[1].session_uuid.as_deref(), Some("sess-1"));

    // Idempotent: re-running the upsert produces no duplicates.
    cache_db
        .write_batch(|w| {
            for path in &files {
                let rec = spans::parse_span_file(path).unwrap().expect("parsed");
                let (_id, was_new) = w.upsert_span(
                    &rec.trace_id,
                    &rec.span_id,
                    rec.parent_span_id.as_deref(),
                    &rec.name,
                    rec.start_time_unix_nano,
                    rec.end_time_unix_nano,
                    rec.agent_role.as_deref(),
                    rec.session_uuid.as_deref(),
                    rec.repo.as_deref(),
                    &rec.attributes_json,
                    &rec.source_file,
                )?;
                assert!(
                    !was_new,
                    "second upsert of same (trace,span) should not insert"
                );
            }
            Ok(())
        })
        .expect("write 2");
    assert_eq!(cache_db.list_all_spans().expect("read").len(), 2);

    std::fs::remove_dir_all(&spans_dir).ok();
    std::fs::remove_dir_all(&cache_dir).ok();
}

#[tokio::test]
async fn git_log_parses_commits() {
    // Build a throwaway repo in a tempdir, drop two commits, and assert
    // `commits::scan` pulls them back with correct SHAs + subjects. Catches
    // regressions in the NUL-separated parse path without needing any real
    // git history on the test machine.
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

    let records = repo_recall::commits::scan(&dir, 10).unwrap();
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
    // *without* changing its content. `git status` will report it as
    // modified because the cached stat info no longer matches; `git diff`
    // will be silent. Confirm the snapshot drops the phantom from the
    // count + path sample. Untracked entries should still come through.
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

    // Force the index to think tracked.txt's stat is stale: bump the
    // mtime forward without rewriting bytes. `touch -t` lands a fixed
    // future timestamp on macOS + Linux without dragging in a filetime
    // crate just for one test.
    let touch = Command::new("touch")
        .args(["-t", "203012311200", tracked.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(touch.success(), "touch failed");

    // Drop an untracked file alongside it so we can prove untracked
    // counts still come through unaffected.
    std::fs::write(dir.join("new.txt"), "fresh\n").unwrap();

    let snap = repo_recall::commits::worktree_snapshot(&dir, 16);
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
async fn json_surface_is_discoverable() {
    let (base, _h) = boot().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();

    // Mechanism 1: <link rel="alternate"> in the HTML head.
    let html = client.get(format!("{base}/")).send().await.unwrap();
    let html_headers = html.headers().clone();
    let body = html.text().await.unwrap();
    assert!(
        body.contains("rel=\"alternate\"") && body.contains("application/json"),
        "dashboard <head> missing alternate-json link"
    );

    // Mechanism 2: Vary + Link headers on every response.
    let vary = html_headers
        .get("vary")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(vary.contains("Accept"), "Vary header missing Accept");
    let link = html_headers
        .get("link")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        link.contains("rel=\"alternate\"") && link.contains("application/json"),
        "Link header missing alternate-json target"
    );
    assert!(
        link.contains("rel=\"service-desc\""),
        "Link header missing service-desc pointer at /openapi.json"
    );

    // Mechanism 3: /openapi.json serves a real OpenAPI doc.
    let oa = client
        .get(format!("{base}/openapi.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(oa.status(), 200);
    let oa_doc: serde_json::Value = oa.json().await.unwrap();
    assert_eq!(oa_doc["openapi"], "3.1.0");
    assert!(
        oa_doc["paths"]["/"].is_object(),
        "openapi missing root path"
    );
    assert!(
        oa_doc["paths"]["/api/action-required"].is_object(),
        "openapi missing /api/action-required"
    );
}
