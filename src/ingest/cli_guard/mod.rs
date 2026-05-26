//! cli-guard audit log ingest. Reads coily's per-scope JSONL audit shards
//! at `~/.coily/audit/*.jsonl` and groups rows by `commit_scope` (the git

pub mod audit_jsonl;
