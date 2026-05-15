//! `AGENTS.md` ingest source. The canonical agent-facing readme.

use std::path::Path;
use std::time::Duration;

use crate::ingest::health::{IngestSource, Report};

pub struct AgentsMdSource {
    stale_after: Duration,
}

impl AgentsMdSource {
    pub fn new(stale_after: Duration) -> Self {
        Self { stale_after }
    }

    pub fn from_config(cfg: &crate::config::IngestDocs) -> Self {
        Self::new(super::file_health::stale_after_from_days(
            cfg.file_stale_after_days,
        ))
    }
}

impl IngestSource for AgentsMdSource {
    fn id(&self) -> &'static str {
        "docs.agents_md"
    }

    fn label(&self) -> &'static str {
        "AGENTS.md"
    }

    fn report(&self, repo_path: &Path) -> Option<Report> {
        Some(super::file_health::file_report(
            self.id(),
            repo_path,
            "AGENTS.md",
            self.stale_after,
        ))
    }
}
