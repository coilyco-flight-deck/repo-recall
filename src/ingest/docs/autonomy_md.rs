//! `docs/AUTONOMY.md` ingest source. The per-repo agent profile spec'd
//! in #92 - what this repo is good and bad at automating, recent

use std::path::Path;
use std::time::Duration;

use crate::ingest::health::{IngestSource, Report};

pub struct AutonomyMdSource {
    stale_after: Duration,
}

impl AutonomyMdSource {
    pub fn new(stale_after: Duration) -> Self {
        Self { stale_after }
    }

    pub fn from_config(cfg: &crate::config::IngestDocs) -> Self {
        Self::new(super::file_health::stale_after_from_days(
            cfg.file_stale_after_days,
        ))
    }
}

impl IngestSource for AutonomyMdSource {
    fn id(&self) -> &'static str {
        "docs.autonomy_md"
    }

    fn label(&self) -> &'static str {
        "docs/AUTONOMY.md"
    }

    fn report(&self, repo_path: &Path) -> Option<Report> {
        Some(super::file_health::file_report(
            self.id(),
            repo_path,
            "docs/AUTONOMY.md",
            self.stale_after,
        ))
    }
}
