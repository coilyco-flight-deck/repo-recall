//! Config-file loader. See [#145](https://github.com/coilysiren/repo-recall/issues/145).
//!
//! Layered precedence, highest wins:
//!   1. `REPO_RECALL_*` env vars (per host, final override)
//!   2. `./repo-recall.yaml` in the process cwd (per workspace)
//!   3. `$XDG_CONFIG_HOME/repo-recall/config.yaml` (per user)
//!   4. Built-in defaults (this file)
//!
//! The shape mirrors `config.example.yaml` at the repo root. Forward-leaning
//! keys (e.g. `refresh.per_source`, `card.short.rows`) deserialise into the
//! struct but aren't consumed by the runtime yet - they wait for #144 / #146.
//! Until those land, this loader only swaps the env-var orchestration knobs
//! main.rs already reads. Per-module paths (sessions dir, dispatch root, etc.)
//! continue to resolve from their own env-var sites; a follow-up issue
//! migrates them in one sweep.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Top-level config. Every nested struct uses `#[serde(default)]` so a
/// partial YAML file overlays the built-in defaults rather than nulling
/// them out.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub server: Server,
    pub paths: Paths,
    pub discovery: Discovery,
    pub refresh: Refresh,
    pub ingest: Ingest,
    pub signals: Signals,
    pub dashboard: Dashboard,
    pub card: Card,
    pub mcp: Mcp,
    pub privacy: Privacy,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Server {
    pub port: u16,
    pub host: String,
    pub static_dir: Option<PathBuf>,
    pub mcp_origins: Vec<String>,
}

impl Default for Server {
    fn default() -> Self {
        Self {
            port: 7777,
            host: "127.0.0.1".to_string(),
            static_dir: None,
            mcp_origins: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Paths {
    pub cwd: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
    pub sessions_dir: Option<PathBuf>,
    pub dispatch_root: Option<PathBuf>,
    pub structural_asks_root: Option<PathBuf>,
    pub agents_drift_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Discovery {
    pub scan_depth: usize,
    pub commits_per_repo: usize,
}

impl Default for Discovery {
    fn default() -> Self {
        Self {
            scan_depth: 4,
            commits_per_repo: 500,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Refresh {
    pub interval_secs: u64,
    pub per_source: PerSource,
}

impl Default for Refresh {
    fn default() -> Self {
        Self {
            interval_secs: 150,
            per_source: PerSource::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PerSource {
    pub git_log: Option<u64>,
    pub github_remote: Option<u64>,
    pub sessions: Option<u64>,
    pub docs: Option<u64>,
    pub cli_guard: Option<u64>,
    /// Cadence for the labeled-issue GraphQL ingest (Source 6 of #155).
    /// Default 3600s (hourly): well inside the GraphQL secondary budget
    /// and the dispatch labels are slow-moving. Consumed by the
    /// per-source refresh substrate (#146).
    pub github_remote_labeled: Option<u64>,
}

impl Default for PerSource {
    fn default() -> Self {
        Self {
            git_log: None,
            github_remote: None,
            sessions: None,
            docs: None,
            cli_guard: None,
            github_remote_labeled: Some(3600),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Ingest {
    pub github: IngestGithub,
    pub git: IngestGit,
    pub sessions: IngestSessions,
    pub docs: IngestDocs,
    pub cli_guard: IngestCliGuard,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct IngestGithub {
    pub remote_target_limit: usize,
    pub per_page: u32,
    pub concurrency: usize,
}

impl Default for IngestGithub {
    fn default() -> Self {
        Self {
            remote_target_limit: 25,
            per_page: 100,
            concurrency: 8,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct IngestGit {
    pub worktree_snapshot_cap: usize,
    pub churn_window_days: u32,
}

impl Default for IngestGit {
    fn default() -> Self {
        Self {
            worktree_snapshot_cap: 50,
            churn_window_days: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct IngestSessions {
    pub summary_truncate_chars: usize,
}

impl Default for IngestSessions {
    fn default() -> Self {
        Self {
            summary_truncate_chars: 200,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct IngestDocs {
    pub file_stale_after_days: u32,
}

impl Default for IngestDocs {
    fn default() -> Self {
        Self {
            file_stale_after_days: 180,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct IngestCliGuard {
    pub audit_dir: Option<PathBuf>,
    pub window_days: u32,
}

impl Default for IngestCliGuard {
    fn default() -> Self {
        Self {
            audit_dir: None,
            window_days: 30,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Signals {
    pub stale_ask_days: u32,
    pub blocked_window_days: u32,
}

impl Default for Signals {
    fn default() -> Self {
        Self {
            stale_ask_days: 7,
            blocked_window_days: 7,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Dashboard {
    pub cards_per_row: CardsPerRow,
    pub sort: Sort,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CardsPerRow {
    pub mobile: usize,
    pub desktop: usize,
    pub ultrawide: usize,
}

impl Default for CardsPerRow {
    fn default() -> Self {
        Self {
            mobile: 1,
            desktop: 2,
            ultrawide: 3,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Sort {
    pub primary: String,
    pub tiebreaker: String,
    pub action_required_floats_to_top: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            primary: "most_recent_commit_ts_desc".to_string(),
            tiebreaker: "name_asc".to_string(),
            action_required_floats_to_top: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Card {
    pub short: CardLayer,
    pub verbose: CardVerbose,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CardLayer {
    pub rows: Vec<CardRow>,
}

impl Default for CardLayer {
    fn default() -> Self {
        Self {
            rows: vec![
                CardRow::enabled("heading"),
                CardRow {
                    id: "banner".into(),
                    enabled: true,
                    cap_rows: Some(5),
                    ..Default::default()
                },
                CardRow::enabled("health"),
                CardRow {
                    id: "activity".into(),
                    enabled: true,
                    cap_rows: Some(5),
                    cap_window_days: Some(30),
                    ..Default::default()
                },
                CardRow {
                    id: "work".into(),
                    enabled: true,
                    cap_rows: Some(5),
                    ..Default::default()
                },
                CardRow {
                    id: "asks".into(),
                    enabled: true,
                    cap_rows: Some(3),
                    sort: Some("oldest_first".into()),
                    ..Default::default()
                },
                CardRow {
                    id: "sessions".into(),
                    enabled: true,
                    cap_rows: Some(5),
                    cap_window_days: Some(90),
                    ..Default::default()
                },
                CardRow {
                    id: "dispatch".into(),
                    enabled: true,
                    cap_rows: Some(3),
                    ..Default::default()
                },
                CardRow {
                    id: "churn".into(),
                    enabled: true,
                    cap_rows: Some(5),
                    cap_window_days: Some(30),
                    ..Default::default()
                },
                CardRow {
                    id: "hot_dirs".into(),
                    enabled: true,
                    cap_rows: Some(5),
                    cap_window_days: Some(30),
                    depth: Some(2),
                    ..Default::default()
                },
                CardRow::enabled("overflow_link"),
            ],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CardVerbose {
    pub show_absent_with_reason: bool,
    pub rows: Vec<CardRow>,
}

impl Default for CardVerbose {
    fn default() -> Self {
        Self {
            show_absent_with_reason: true,
            rows: vec![
                CardRow {
                    id: "activity".into(),
                    enabled: true,
                    cap_rows: Some(100),
                    cap_window_days: Some(90),
                    ..Default::default()
                },
                CardRow {
                    id: "work".into(),
                    enabled: true,
                    cap_rows: Some(50),
                    ..Default::default()
                },
                CardRow {
                    id: "asks".into(),
                    enabled: true,
                    cap_rows: Some(50),
                    ..Default::default()
                },
                CardRow {
                    id: "sessions".into(),
                    enabled: true,
                    cap_rows: Some(100),
                    cap_window_days: Some(90),
                    ..Default::default()
                },
                CardRow {
                    id: "dispatch".into(),
                    enabled: true,
                    cap_rows: Some(50),
                    ..Default::default()
                },
                CardRow {
                    id: "churn".into(),
                    enabled: true,
                    cap_rows: Some(50),
                    cap_window_days: Some(90),
                    ..Default::default()
                },
                CardRow {
                    id: "hot_dirs".into(),
                    enabled: true,
                    cap_rows: Some(50),
                    cap_window_days: Some(90),
                    depth: Some(2),
                    ..Default::default()
                },
            ],
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct CardRow {
    pub id: String,
    pub enabled: bool,
    pub cap_rows: Option<usize>,
    pub cap_window_days: Option<u32>,
    pub sort: Option<String>,
    /// Path-component rollup depth for directory-rollup rows (`hot_dirs`).
    /// Ignored by other row ids.
    pub depth: Option<u32>,
}

impl CardRow {
    fn enabled(id: &str) -> Self {
        Self {
            id: id.into(),
            enabled: true,
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Mcp {
    pub default_commit_limit: usize,
    pub default_search_limit: usize,
    pub token_cost: TokenCost,
}

impl Default for Mcp {
    fn default() -> Self {
        Self {
            default_commit_limit: 50,
            default_search_limit: 20,
            token_cost: TokenCost::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TokenCost {
    pub cache_read_per_m: f64,
}

impl Default for TokenCost {
    fn default() -> Self {
        Self {
            cache_read_per_m: 0.30,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Privacy {
    pub author_email: Option<String>,
    pub sanitize_terms_file: Option<PathBuf>,
}

/// Built-in defaults plus the layered file + env overlay. Best-effort:
/// a malformed YAML file emits a `tracing::warn!` and falls back to the
/// next layer rather than crashing the startup path.
pub fn load() -> Config {
    let mut cfg = Config::default();
    if let Some(path) = xdg_config_path() {
        overlay_file(&mut cfg, &path);
    }
    overlay_file(&mut cfg, Path::new("repo-recall.yaml"));
    overlay_env(&mut cfg);
    cfg
}

/// Resolve `~/.config/repo-recall/config.yaml` honoring `XDG_CONFIG_HOME`.
fn xdg_config_path() -> Option<PathBuf> {
    if let Some(over) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(over).join("repo-recall").join("config.yaml"));
    }
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".config")
            .join("repo-recall")
            .join("config.yaml"),
    )
}

fn overlay_file(cfg: &mut Config, path: &Path) {
    if !path.is_file() {
        return;
    }
    match std::fs::read_to_string(path) {
        Ok(s) => match serde_yaml::from_str::<Config>(&s) {
            Ok(parsed) => {
                *cfg = parsed;
                tracing::info!("config: loaded {}", path.display());
            }
            Err(e) => {
                tracing::warn!("config: {} parse error: {e}; ignoring", path.display());
            }
        },
        Err(e) => {
            tracing::warn!("config: {} read error: {e}; ignoring", path.display());
        }
    }
}

/// Apply `REPO_RECALL_*` overrides. Existing knobs in main.rs are the
/// authoritative set; this function mirrors them. Unset env vars leave
/// the file/default value intact. Malformed values emit a warning and
/// fall back to the prior value.
fn overlay_env(cfg: &mut Config) {
    if let Ok(v) = std::env::var("REPO_RECALL_PORT") {
        match v.parse() {
            Ok(p) => cfg.server.port = p,
            Err(e) => tracing::warn!("REPO_RECALL_PORT={v:?} invalid: {e}"),
        }
    }
    if let Ok(v) = std::env::var("REPO_RECALL_HOST") {
        cfg.server.host = v;
    }
    if let Some(v) = std::env::var_os("REPO_RECALL_CWD") {
        cfg.paths.cwd = Some(PathBuf::from(v));
    }
    if let Some(v) = std::env::var_os("REPO_RECALL_CACHE_DIR") {
        cfg.paths.cache_dir = Some(PathBuf::from(v));
    }
    if let Some(v) = std::env::var_os("REPO_RECALL_INDEX_DIR") {
        cfg.paths.index_dir = Some(PathBuf::from(v));
    }
    if let Some(v) = std::env::var_os("REPO_RECALL_SESSIONS_DIR") {
        cfg.paths.sessions_dir = Some(PathBuf::from(v));
    }
    if let Some(v) = std::env::var_os("REPO_RECALL_DISPATCH_ROOT") {
        cfg.paths.dispatch_root = Some(PathBuf::from(v));
    }
    if let Some(v) = std::env::var_os("REPO_RECALL_STRUCTURAL_ASKS_ROOT") {
        cfg.paths.structural_asks_root = Some(PathBuf::from(v));
    }
    if let Some(v) = std::env::var_os("REPO_RECALL_AGENTS_DRIFT_ROOT") {
        cfg.paths.agents_drift_root = Some(PathBuf::from(v));
    }
    if let Some(v) = std::env::var_os("REPO_RECALL_AUDIT_DIR") {
        cfg.ingest.cli_guard.audit_dir = Some(PathBuf::from(v));
    }
    if let Some(v) = std::env::var_os("REPO_RECALL_STATIC") {
        cfg.server.static_dir = Some(PathBuf::from(v));
    }
    if let Ok(v) = std::env::var("REPO_RECALL_DEPTH") {
        match v.parse() {
            Ok(d) => cfg.discovery.scan_depth = d,
            Err(e) => tracing::warn!("REPO_RECALL_DEPTH={v:?} invalid: {e}"),
        }
    }
    if let Ok(v) = std::env::var("REPO_RECALL_COMMITS_PER_REPO") {
        match v.parse() {
            Ok(n) => cfg.discovery.commits_per_repo = n,
            Err(e) => tracing::warn!("REPO_RECALL_COMMITS_PER_REPO={v:?} invalid: {e}"),
        }
    }
    if let Ok(v) = std::env::var("REPO_RECALL_REFRESH_INTERVAL_SECS") {
        match v.parse() {
            Ok(s) => cfg.refresh.interval_secs = s,
            Err(e) => tracing::warn!("REPO_RECALL_REFRESH_INTERVAL_SECS={v:?} invalid: {e}"),
        }
    }
    if let Ok(v) = std::env::var("REPO_RECALL_REMOTE_TARGET_LIMIT") {
        match v.parse() {
            Ok(n) => cfg.ingest.github.remote_target_limit = n,
            Err(e) => tracing::warn!("REPO_RECALL_REMOTE_TARGET_LIMIT={v:?} invalid: {e}"),
        }
    }
    if let Ok(v) = std::env::var("REPO_RECALL_STALE_ASK_DAYS") {
        match v.parse() {
            Ok(n) => cfg.signals.stale_ask_days = n,
            Err(e) => tracing::warn!("REPO_RECALL_STALE_ASK_DAYS={v:?} invalid: {e}"),
        }
    }
    if let Ok(v) = std::env::var("REPO_RECALL_AUTHOR") {
        cfg.privacy.author_email = Some(v);
    }
    if let Ok(v) = std::env::var("REPO_RECALL_SANITIZE_TERMS") {
        cfg.privacy.sanitize_terms_file = Some(PathBuf::from(v));
    }
}

/// Sanity-check the loaded config. Warnings only - never refuse to boot
/// because of config; surface the problem and let the runtime carry on
/// with whatever the loader produced.
pub fn validate(cfg: &Config) {
    let churn_window = cfg.ingest.git.churn_window_days;
    for row in cfg
        .card
        .short
        .rows
        .iter()
        .chain(cfg.card.verbose.rows.iter())
    {
        if let Some(w) = row.cap_window_days {
            if w > churn_window {
                tracing::warn!(
                    "config: card row {:?} cap_window_days={w} exceeds ingest.git.churn_window_days={churn_window} - the UI may request data the DB doesn't retain",
                    row.id
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_codified_constants() {
        let c = Config::default();
        assert_eq!(c.server.port, 7777);
        assert_eq!(c.server.host, "127.0.0.1");
        assert_eq!(c.discovery.scan_depth, 4);
        assert_eq!(c.discovery.commits_per_repo, 500);
        assert_eq!(c.refresh.interval_secs, 150);
        assert_eq!(c.ingest.github.remote_target_limit, 25);
        assert_eq!(c.ingest.git.churn_window_days, 30);
        assert_eq!(c.signals.stale_ask_days, 7);
        assert_eq!(c.dashboard.cards_per_row.desktop, 2);
        assert_eq!(c.card.short.rows.len(), 11);
        assert_eq!(c.card.verbose.rows.len(), 7);
        assert!(c.dashboard.sort.action_required_floats_to_top);
    }

    #[test]
    fn partial_yaml_overlays_defaults() {
        let yaml = r#"
server:
  port: 9090
discovery:
  scan_depth: 7
"#;
        let parsed: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(parsed.server.port, 9090);
        assert_eq!(parsed.server.host, "127.0.0.1"); // default preserved
        assert_eq!(parsed.discovery.scan_depth, 7);
        assert_eq!(parsed.discovery.commits_per_repo, 500); // default preserved
        assert_eq!(parsed.signals.stale_ask_days, 7); // default preserved
    }

    #[test]
    fn full_yaml_round_trips() {
        // Smoke-check that the shipped example file deserialises cleanly.
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config.example.yaml");
        let content = std::fs::read_to_string(&path).expect("config.example.yaml exists");
        let parsed: Config = serde_yaml::from_str(&content).expect("example file parses");
        // Sanity-check a few fields land where the YAML claims they should.
        assert_eq!(parsed.server.port, 7777);
        assert_eq!(parsed.card.short.rows.len(), 11);
        assert_eq!(parsed.card.verbose.rows.len(), 7);
    }

    #[test]
    fn env_overlay_overrides_defaults() {
        // Hand-build a minimal Config and apply a controlled env set.
        // Avoid writing globals that other tests read; this test owns
        // a narrow blast radius by only checking the keys it set.
        let key = "REPO_RECALL_PORT";
        let prev = std::env::var(key).ok();
        // SAFETY: tests run in parallel; setting a global env var here can
        // race other tests that read REPO_RECALL_PORT. None currently do.
        unsafe { std::env::set_var(key, "8888") };
        let mut c = Config::default();
        overlay_env(&mut c);
        assert_eq!(c.server.port, 8888);
        match prev {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    #[test]
    fn validate_warns_on_card_window_exceeding_ingest_window() {
        let mut c = Config::default();
        c.ingest.git.churn_window_days = 10;
        // Default short rows include activity with cap_window_days=30, so
        // this should now exceed the ingest window. We can't easily
        // capture tracing output here, but `validate` must not panic and
        // must return normally.
        validate(&c);
    }
}
