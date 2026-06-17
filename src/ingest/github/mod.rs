//! GitHub REST ingest. Two calls per remote-tracked repo:
//!

pub mod client;
pub mod commits;
pub mod fetch_state;
pub mod issues;
pub mod milestones;
pub mod pulls;

pub use client::{build_client, AuthedUser, FixturesClient, GithubClient, OctocrabClient};
pub use commits::parse_commits_json;
pub use fetch_state::{classify_gh_failure, classify_gh_stderr, RemoteFetchState};
pub use issues::{parse_issues_json, IssueRecordInput};
pub use milestones::{parse_milestones_json, MilestoneInput};
pub use pulls::{parse_prs_json, PrRecordInput};
