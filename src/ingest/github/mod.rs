//! GitHub REST ingest. Three calls per remote-tracked repo:
//!
//! - `/repos/X/pulls?state=open`     -> open PRs (Source 2 of #155)
//! - `/repos/X/issues?state=open`    -> open issues, with PR rows filtered (Source 3)
//! - `gh run list --json …`          -> recent CI runs incl. job names (Source 4)
//!
//! Stays on REST. No GraphQL.

pub mod ci_runs;
pub mod client;
pub mod fetch_state;
pub mod issues;
pub mod pulls;

pub use ci_runs::{parse_job_names_json, parse_runs_json, CiRunRecordInput};
pub use client::{build_client, AuthedUser, FixturesClient, GithubClient, OctocrabClient};
pub use fetch_state::{classify_gh_failure, classify_gh_stderr, RemoteFetchState};
pub use issues::{parse_issues_json, IssueRecordInput};
pub use pulls::{parse_prs_json, PrRecordInput};
