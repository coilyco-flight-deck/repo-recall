//! `docs/AUTONOMY.md` ingest source. The per-repo agent profile spec'd
//! in #92 - what this repo is good and bad at automating, recent
//! dispatch patterns, open structural-context asks. Most repos will
//! be Red on this source until they adopt the pattern.

use std::path::Path;

use crate::ingest::health::{IngestSource, Report};

pub struct AutonomyMdSource;

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
        ))
    }
}
