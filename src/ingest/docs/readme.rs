//! `README.md` ingest source.

use std::path::Path;
use std::time::Duration;

use crate::ingest::health::{IngestSource, Report};

pub struct ReadmeSource {
    stale_after: Duration,
}

impl ReadmeSource {
    pub fn new(stale_after: Duration) -> Self {
        Self { stale_after }
    }

    pub fn from_config(cfg: &crate::config::IngestDocs) -> Self {
        Self::new(super::file_health::stale_after_from_days(
            cfg.file_stale_after_days,
        ))
    }
}

impl IngestSource for ReadmeSource {
    fn id(&self) -> &'static str {
        "docs.readme"
    }

    fn label(&self) -> &'static str {
        "README"
    }

    fn report(&self, repo_path: &Path) -> Option<Report> {
        Some(super::file_health::file_report(
            self.id(),
            repo_path,
            "README.md",
            self.stale_after,
        ))
    }
}
