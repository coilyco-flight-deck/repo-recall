//! Per-doc-file ingest sources.
//!
//! The "one file per data source" rule from #92 applies even when the
//! files would naturally cluster. README.md and AGENTS.md ship as
//! independent `IngestSource` implementations so a missing one shows
//! up as its own red dot rather than dragging a composite "docs"
//! indicator down.

pub mod agents_md;
pub mod autonomy_md;
pub mod features_md;
pub mod readme;
pub mod repo_dispatch;

mod file_health;
