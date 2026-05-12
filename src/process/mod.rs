//! Process layer: derive cross-source views from ingest output.
//!
//! Holds the joiner (cwd to repo), activity scoring, readiness scorecards,
//! cross-repo block clustering, dispatch-ledger assembly. No I/O. See
//! issue #92 for the design.

pub mod activity;
pub mod join;
