//! `AGENTS.md` ingest source. The canonical agent-facing readme.

use std::path::Path;

use crate::ingest::health::{IngestSource, Report};

pub struct AgentsMdSource;

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
        ))
    }
}
