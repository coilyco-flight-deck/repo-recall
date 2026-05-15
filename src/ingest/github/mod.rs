//! GitHub REST ingest. Three calls per remote-tracked repo:
//!
//! - `/repos/X/pulls?state=open`     -> open PRs (Source 2 of #155)
//! - `/repos/X/issues?state=open`    -> open issues, with PR rows filtered (Source 3)
//! - `gh run list --json …`          -> recent CI runs incl. job names (Source 4)
//!
//! Stays on REST. Per AGENTS.md "No GraphQL" except where #155 Source 6
//! explicitly carves out a sanctioned site (labeled-issue ingest), and
//! that's a separate module.

pub mod ci_runs;
pub mod issues;
pub mod pulls;

pub use ci_runs::{fetch_recent_runs, CiRunRecordInput};
pub use issues::{fetch_open_issues, IssueRecordInput};
pub use pulls::{fetch_open_prs, PrRecordInput};
