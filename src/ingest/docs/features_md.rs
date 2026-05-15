//! `docs/FEATURES.md` ingest source. The "what ships today" inventory.

use std::path::Path;
use std::time::Duration;

use crate::ingest::health::{IngestSource, Report};

pub struct FeaturesMdSource {
    stale_after: Duration,
}

impl FeaturesMdSource {
    pub fn new(stale_after: Duration) -> Self {
        Self { stale_after }
    }

    pub fn from_config(cfg: &crate::config::IngestDocs) -> Self {
        Self::new(super::file_health::stale_after_from_days(
            cfg.file_stale_after_days,
        ))
    }
}

impl IngestSource for FeaturesMdSource {
    fn id(&self) -> &'static str {
        "docs.features_md"
    }

    fn label(&self) -> &'static str {
        "docs/FEATURES.md"
    }

    fn report(&self, repo_path: &Path) -> Option<Report> {
        Some(super::file_health::file_report(
            self.id(),
            repo_path,
            "docs/FEATURES.md",
            self.stale_after,
        ))
    }
}
