//! cli-guard audit log ingest. Reads coily's per-scope JSONL audit shards
//! at `~/.coily/audit/*.jsonl`, grouping rows by repo_root (legacy commit_scope).

pub mod audit_jsonl;
