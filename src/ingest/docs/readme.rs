//! `README.md` ingest source.

use std::path::Path;

use crate::ingest::health::{IngestSource, Report};

pub struct ReadmeSource;

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
        ))
    }
}
