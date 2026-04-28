//! End-to-end smoke test: boot the real router on a random port, exercise the
//! public endpoints, and assert they return the right status codes and have
//! the expected HTML scaffolding. This is intentionally shallow — it catches
//! "did the router compile and serve HTML" regressions, not fine-grained
//! content bugs.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, Mutex};

use repo_recall::{db, routes, state::StateDb, AppState};

async fn boot() -> (String, tokio::task::JoinHandle<()>) {
    boot_with(repo_recall::commits::GhHealth::Ok).await
}

async fn boot_with(gh: repo_recall::commits::GhHealth) -> (String, tokio::task::JoinHandle<()>) {
    // Unique DB per test run so parallel `cargo test` invocations don't collide.
    let db_path: PathBuf =
        std::env::temp_dir().join(format!("repo-recall-test-{}.sqlite", uuid_like()));
    let _ = std::fs::remove_file(&db_path);
    db::init(&db_path).expect("db init");

    // Each test gets its own state DB too, so VAPID + subscriptions
    // do not bleed across parallel runs.
    let state_dir = std::env::temp_dir().join(format!("repo-recall-state-{}", uuid_like()));
    std::fs::create_dir_all(&state_dir).unwrap();
    let state_db = StateDb::open_at(state_dir.join("state.sqlite")).expect("state db");

    let (progress_tx, _) = broadcast::channel::<String>(16);
    let state = AppState {
        db_path,
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
        state_db,
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
        "/sw.js",
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
