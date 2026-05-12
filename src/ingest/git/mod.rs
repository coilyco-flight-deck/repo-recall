//! Git ingest sources. Each command lives in its own file so each one can
//! report its own Green / Yellow / Red health independently. The current
//! `log` file is a transitional landing spot for the pre-refactor
//! `commits.rs` and still contains a few github-CLI helpers that will
//! split out into `crate::ingest::github::*` in a follow-up. See #92.

pub mod log;
