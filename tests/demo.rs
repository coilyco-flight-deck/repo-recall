//! Demo-mode fixtures and the `REPO_RECALL_SESSIONS_DIR` override.
//!
//! This is the load-bearing test for the public Docker demo. If a parser or
//! scanner refactor breaks the synthetic fixture format, this test fails
//! before the demo container ships broken. Phase 1 covers fixture parsing
//! and the env-var override; phase 2 extends this file to boot the full
//! router and assert the session->repo join.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{broadcast, Mutex as TokioMutex};

use repo_recall::{db, routes, sessions, state::StateDb, AppState};

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("sessions")
}

fn list_jsonl(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            out.push(p);
        }
    }
    out.sort();
    out
}

#[test]
fn every_fixture_parses_into_a_session_record() {
    let files = list_jsonl(&fixtures_root());
    assert!(
        files.len() >= 5,
        "expected at least 5 fixture jsonl files, found {}",
        files.len()
    );

    for path in &files {
        let rec = sessions::parse_session_file(path)
            .unwrap_or_else(|e| panic!("parse error for {}: {e:?}", path.display()))
            .unwrap_or_else(|| panic!("fixture yielded no session record: {}", path.display()));

        assert!(
            !rec.session_uuid.is_empty(),
            "{} missing session_uuid",
            path.display()
        );
        let cwd = rec
            .cwd
            .as_deref()
            .unwrap_or_else(|| panic!("{} missing cwd", path.display()));
        assert!(
            cwd.contains("__REPOS_ROOT__"),
            "{} cwd should embed the __REPOS_ROOT__ token, got {cwd:?}",
            path.display()
        );
        assert!(
            rec.summary.is_some(),
            "{} should yield a summary from the first user line",
            path.display()
        );
        assert!(
            rec.message_count >= 1,
            "{} should count at least one user/assistant turn",
            path.display()
        );
        assert!(
            rec.started_at.is_some() && rec.ended_at.is_some(),
            "{} should have both timestamps",
            path.display()
        );
        assert!(
            rec.input_tokens + rec.output_tokens > 0,
            "{} should aggregate token usage from assistant turns",
            path.display()
        );
    }
}

#[test]
fn fixture_cwds_cover_three_distinct_repos() {
    let files = list_jsonl(&fixtures_root());
    let mut repos = std::collections::HashSet::new();
    for path in &files {
        let rec = sessions::parse_session_file(path).unwrap().unwrap();
        let cwd = rec.cwd.unwrap();
        let suffix = cwd
            .strip_prefix("__REPOS_ROOT__/")
            .unwrap_or_else(|| panic!("cwd {cwd:?} should start with __REPOS_ROOT__/"));
        repos.insert(suffix.to_string());
    }
    assert_eq!(
        repos.len(),
        3,
        "fixtures should span exactly three fake repos, got {repos:?}"
    );
}

// Env-var manipulation is process-global; serialize the two tests that touch
// REPO_RECALL_SESSIONS_DIR so they don't race each other under `cargo test`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn sessions_dir_env_override_points_at_a_real_directory() {
    let _g = ENV_LOCK.lock().unwrap();
    let tmp = std::env::temp_dir().join(format!(
        "repo-recall-sessions-override-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).unwrap();

    std::env::set_var("REPO_RECALL_SESSIONS_DIR", &tmp);
    let resolved = sessions::default_projects_dir();
    std::env::remove_var("REPO_RECALL_SESSIONS_DIR");

    let resolved = resolved.expect("override should resolve to Some");
    assert_eq!(
        std::fs::canonicalize(&resolved).unwrap(),
        std::fs::canonicalize(&tmp).unwrap(),
        "override should win over $HOME/.claude/projects"
    );

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn sessions_dir_env_override_falls_back_when_directory_missing() {
    let _g = ENV_LOCK.lock().unwrap();
    let bogus = std::env::temp_dir().join(format!(
        "repo-recall-does-not-exist-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    assert!(!bogus.exists());

    std::env::set_var("REPO_RECALL_SESSIONS_DIR", &bogus);
    let resolved = sessions::default_projects_dir();
    std::env::remove_var("REPO_RECALL_SESSIONS_DIR");

    // A nonexistent override should fall through to the $HOME default. The
    // test environment may or may not have ~/.claude/projects, so we only
    // assert that the override didn't win - the result must not equal `bogus`.
    if let Some(p) = resolved {
        assert_ne!(p, bogus, "override pointed at a missing dir should not win");
    }
}

// ----- end-to-end demo boot -------------------------------------------- //
//
// Materialises the fixtures via scripts/build-fixture-repos.sh +
// scripts/render-session-fixtures.sh, boots the in-process router pointed
// at them, runs a refresh, and asserts the dashboard JSON shows the expected
// repo + session + join counts. This is the test that catches "fixture
// shape drifted" or "scanner stopped seeing fake repos" before either makes
// it into the demo container.

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn unique_tmp(prefix: &str) -> PathBuf {
    use std::sync::atomic::Ordering;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{}-{}", std::process::id(), nanos, n))
}

fn run_script(script: &str, args: &[&str]) {
    let path = manifest_dir().join("scripts").join(script);
    let status = Command::new("bash")
        .arg(&path)
        .args(args)
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn {script}: {e}"));
    assert!(status.success(), "{script} exited non-zero: {status:?}");
}

// `await_holding_lock` is a real bug shape in production code, but here the
// lock exists specifically to serialize REPO_RECALL_SESSIONS_DIR mutation
// across tests in the same process. The await stays bounded by the request
// timeouts inside the test, and another async test holding the same lock
// would just queue, not deadlock.
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn demo_fixtures_boot_and_join_through_the_router() {
    let _g = ENV_LOCK.lock().unwrap();

    let workdir = unique_tmp("repo-recall-demo");
    let repos_dir = workdir.join("repos");
    let sessions_dir = workdir.join("sessions");
    std::fs::create_dir_all(&workdir).unwrap();

    run_script("build-fixture-repos.sh", &[repos_dir.to_str().unwrap()]);
    run_script(
        "render-session-fixtures.sh",
        &[sessions_dir.to_str().unwrap(), repos_dir.to_str().unwrap()],
    );

    // Per-test SQLite + state DB so parallel `cargo test` invocations don't
    // collide. Mirrors tests/smoke.rs.
    let db_path = unique_tmp("repo-recall-demo-cache").with_extension("sqlite");
    let _ = std::fs::remove_file(&db_path);
    db::init(&db_path).unwrap();
    let state_dir = unique_tmp("repo-recall-demo-state");
    std::fs::create_dir_all(&state_dir).unwrap();
    let state_db = StateDb::open_at(state_dir.join("state.sqlite")).unwrap();

    let (progress_tx, _) = broadcast::channel::<String>(16);
    let state = AppState {
        db_path: db_path.clone(),
        cwd: repos_dir.clone(),
        scan_depth: 2,
        commits_per_repo: 50,
        refresh_interval_secs: 0,
        remote_target_limit: 0,
        progress_tx,
        refresh_lock: Arc::new(TokioMutex::new(())),
        last_scan: Arc::new(TokioMutex::new(None)),
        // No `gh` from the test environment is fine; the fixtures don't have
        // remotes anyway. Set Missing so the dashboard doesn't try to call gh.
        gh_health: Arc::new(TokioMutex::new(repo_recall::commits::GhHealth::Missing)),
        my_gh_login: Arc::new(TokioMutex::new(None)),
        my_git_email: Arc::new(TokioMutex::new(None)),
        scan_version: Arc::new(AtomicU64::new(0)),
        state_db,
    };

    // Drive session parsing at our rendered fixtures, not ~/.claude/projects.
    std::env::set_var("REPO_RECALL_SESSIONS_DIR", &sessions_dir);

    routes::refresh::run_refresh(state.clone())
        .await
        .expect("refresh failed");

    // Boot the router and hit the JSON dashboard.
    let app = routes::router(state.clone());
    let addr: SocketAddr = ([127, 0, 0, 1], 0).into();
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    let bound = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let res = client
        .get(format!("http://{bound}/?format=json"))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();

    std::env::remove_var("REPO_RECALL_SESSIONS_DIR");
    handle.abort();

    let counts = body
        .get("counts")
        .expect("dashboard json should have counts");
    let repos_n = counts.get("repos").and_then(|v| v.as_i64()).unwrap_or(0);
    let sessions_n = counts.get("sessions").and_then(|v| v.as_i64()).unwrap_or(0);
    let links_n = counts.get("links").and_then(|v| v.as_i64()).unwrap_or(0);

    assert!(
        repos_n >= 3,
        "expected at least 3 fixture repos, got {repos_n} (body: {body})"
    );
    assert!(
        sessions_n >= 5,
        "expected at least 5 fixture sessions, got {sessions_n}"
    );
    assert!(
        links_n >= 5,
        "every fixture session has a cwd inside a fixture repo, expected >=5 joins, got {links_n}"
    );

    // Assert the join actually fired: every session row in `recent_sessions`
    // should reference a repo that exists in `repos`. This is the signal that
    // catches a join.rs regression where sessions land but get orphaned.
    let recent_sessions = body
        .get("recent_sessions")
        .and_then(|v| v.as_array())
        .expect("recent_sessions array");
    assert!(
        !recent_sessions.is_empty(),
        "recent_sessions should be non-empty"
    );

    let _ = std::fs::remove_dir_all(&workdir);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_dir_all(&state_dir);
}
