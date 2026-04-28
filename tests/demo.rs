//! Demo-mode fixtures and the `REPO_RECALL_SESSIONS_DIR` override.
//!
//! This is the load-bearing test for the public Docker demo. If a parser or
//! scanner refactor breaks the synthetic fixture format, this test fails
//! before the demo container ships broken. Phase 1 covers fixture parsing
//! and the env-var override; phase 2 extends this file to boot the full
//! router and assert the session->repo join.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use repo_recall::sessions;

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
