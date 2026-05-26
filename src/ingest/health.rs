//! Health-indicator surface shared across every ingest source.
//!

use std::path::Path;

/// One source's data-quality state for one repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    /// Source is present, fresh, and parseable.
    Green,
    /// Source is present but stale, partial, or has caveats.
    Yellow,
    /// Source is missing, broken, or unparseable.
    Red,
}

impl Health {
    /// Emoji glyph for compact rendering ("🟢" / "🟡" / "🔴").
    pub fn dot(self) -> &'static str {
        match self {
            Health::Green => "🟢",
            Health::Yellow => "🟡",
            Health::Red => "🔴",
        }
    }

    /// Lowercase string label for JSON / data attributes.
    pub fn label(self) -> &'static str {
        match self {
            Health::Green => "green",
            Health::Yellow => "yellow",
            Health::Red => "red",
        }
    }
}

/// One per-(source, repo) health report. The dashboard renders one
/// of these per visible cell.
#[derive(Debug, Clone)]
pub struct Report {
    /// Stable id of the source that produced this report. Matches
    /// `IngestSource::id()`.
    pub source_id: &'static str,
    pub health: Health,
    /// One-line, human-readable explanation. Examples: "AGENTS.md
    /// 28 KB, modified 3 days ago", "no docs/AUTONOMY.md",
    pub reason: String,
}

/// One ingest source. Each implementation reads one specific piece
/// of substrate (one file, one command, one API call) and reports
pub trait IngestSource {
    /// Stable identifier. Convention: dotted, lowercase, namespaced
    /// by data-source family. Examples: `"docs.readme"`,
    fn id(&self) -> &'static str;

    /// Human-friendly column header for the dashboard.
    fn label(&self) -> &'static str;

    /// Compute health for a given repo path. Returns `None` when
    /// this source is not applicable to this repo at all (e.g. a
    fn report(&self, repo_path: &Path) -> Option<Report>;
}
