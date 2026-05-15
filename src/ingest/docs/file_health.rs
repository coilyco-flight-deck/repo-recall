//! Shared file-presence health helper for the per-doc ingest sources.
//!
//! Every per-doc source applies the same Red / Yellow / Green logic
//! to its target file. This helper holds that logic so each source
//! is a thin wrapper that names the file and forwards.

use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::ingest::health::{Health, Report};

/// Files smaller than this are treated as placeholders.
pub const MIN_SUBSTANCE_BYTES: u64 = 200;

/// Build a `Duration` from `ingest.docs.file_stale_after_days`. Single
/// conversion site so callers don't reinvent the days->seconds math.
pub fn stale_after_from_days(days: u32) -> Duration {
    Duration::from_secs(u64::from(days) * 86_400)
}

/// Compute a `Report` for one named file inside a repo.
///
/// * `source_id` and `repo_path` identify what we are inspecting.
/// * `relative_path` is the file's path under the repo root
///   (e.g. `"README.md"`, `"docs/FEATURES.md"`).
/// * `stale_after` is the cutoff past which an untouched file is
///   reported Yellow. Sourced from `ingest.docs.file_stale_after_days`
///   at the source's construction site.
pub fn file_report(
    source_id: &'static str,
    repo_path: &Path,
    relative_path: &str,
    stale_after: Duration,
) -> Report {
    let path = repo_path.join(relative_path);
    let Ok(meta) = std::fs::metadata(&path) else {
        return Report {
            source_id,
            health: Health::Red,
            reason: format!("no {relative_path}"),
        };
    };
    let size = meta.len();
    if size < MIN_SUBSTANCE_BYTES {
        return Report {
            source_id,
            health: Health::Yellow,
            reason: format!("{relative_path} present but only {size} bytes"),
        };
    }
    let stale = meta
        .modified()
        .ok()
        .and_then(|m| SystemTime::now().duration_since(m).ok())
        .map(|age| age > stale_after)
        .unwrap_or(false);
    if stale {
        let days = stale_after.as_secs() / 86_400;
        return Report {
            source_id,
            health: Health::Yellow,
            reason: format!("{relative_path} not touched in >{days} days"),
        };
    }
    Report {
        source_id,
        health: Health::Green,
        reason: format!("{relative_path} {size} bytes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::UNIX_EPOCH;

    /// Roll our own scratch-dir helper to avoid pulling in `tempfile`
    /// as a dev-dep just for these four tests. Matches the pattern
    /// `tests/smoke.rs` already uses.
    fn scratch_dir() -> std::path::PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "repo-recall-file-health-{nanos}-{}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const TEST_STALE: Duration = Duration::from_secs(60 * 60 * 24 * 180);

    #[test]
    fn missing_file_is_red() {
        let dir = scratch_dir();
        let r = file_report("docs.readme", &dir, "README.md", TEST_STALE);
        assert_eq!(r.health, Health::Red);
        assert!(r.reason.contains("no README.md"));
    }

    #[test]
    fn tiny_file_is_yellow() {
        let dir = scratch_dir();
        let mut f = fs::File::create(dir.join("README.md")).unwrap();
        f.write_all(b"# hi").unwrap();
        let r = file_report("docs.readme", &dir, "README.md", TEST_STALE);
        assert_eq!(r.health, Health::Yellow);
        assert!(r.reason.contains("only"));
    }

    #[test]
    fn substantial_recent_file_is_green() {
        let dir = scratch_dir();
        let mut f = fs::File::create(dir.join("README.md")).unwrap();
        f.write_all(&vec![b'x'; (MIN_SUBSTANCE_BYTES + 1) as usize])
            .unwrap();
        let r = file_report("docs.readme", &dir, "README.md", TEST_STALE);
        assert_eq!(r.health, Health::Green);
    }

    #[test]
    fn stale_after_from_days_matches_seconds() {
        assert_eq!(stale_after_from_days(180), TEST_STALE);
        assert_eq!(stale_after_from_days(0), Duration::ZERO);
    }
}
