//! `docs/FEATURES.md` ingest source. The "what ships today" inventory.

use std::path::Path;

use crate::ingest::health::{IngestSource, Report};

pub struct FeaturesMdSource;

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
        ))
    }
}
