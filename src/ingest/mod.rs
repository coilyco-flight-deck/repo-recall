//! Ingest layer: one file per data source.
//!
//! Every source reports a Green / Yellow / Red health indicator so the
//! dashboard can show, per repo and per source, what data we are able to
//! pull right now. Future submodules: `github`, `claude`, `docs`, `fs`.
//! See issue #92 for the design.

pub mod claude;
pub mod git;
pub mod health;
