//! cli-guard audit log ingest. Reads coily's per-scope JSONL audit shards
//! at `~/.coily/audit/*.jsonl` and groups rows by `commit_scope` (the git
//! toplevel), which is the same join key the rest of repo-recall uses for
//! per-repo data.
//!
//! See [issue #148](https://github.com/coilysiren/repo-recall/issues/148).

pub mod audit_jsonl;
