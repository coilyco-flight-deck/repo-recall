//! Health-indicator surface shared across every ingest source.
//!
//! The per-source health view designed in #92 iterates every
//! `IngestSource` implementor and renders a Green / Yellow / Red dot
//! plus the reason string. Sources can decline to apply to a given
//! repo (e.g. a docs source for a file that does not exist), in
//! which case the row is omitted.
//!
//! No concrete implementations live here. Each ingest source defines
//! its own type and implements this trait.

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
    /// "CI red since 2026-05-09".
    pub reason: String,
}

/// One ingest source. Each implementation reads one specific piece
/// of substrate (one file, one command, one API call) and reports
/// its health for a given repo independently of every other source.
///
/// The "one source per file" rule from #92 means even closely-related
/// files (README.md vs AGENTS.md vs docs/FEATURES.md) are separate
/// implementations, so a missing one shows up as its own red dot
/// rather than dragging a composite "docs" indicator down.
pub trait IngestSource {
    /// Stable identifier. Convention: dotted, lowercase, namespaced
    /// by data-source family. Examples: `"docs.readme"`,
    /// `"docs.agents_md"`, `"git.log"`, `"github.issues"`.
    fn id(&self) -> &'static str;

    /// Human-friendly column header for the dashboard.
    fn label(&self) -> &'static str;

    /// Compute health for a given repo path. Returns `None` when
    /// this source is not applicable to this repo at all (e.g. a
    /// github source against a repo with no `origin`). Returning
    /// `Some(Red, "missing")` is a different statement: "this source
    /// applies but the data is not there."
    fn report(&self, repo_path: &Path) -> Option<Report>;
}
