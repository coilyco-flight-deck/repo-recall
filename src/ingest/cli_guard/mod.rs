//! cli-guard audit log ingest. Reads ward's per-scope JSONL audit shards
//! at `~/.ward/audit/*.jsonl`, grouping rows by repo_root (legacy commit_scope).

pub mod audit_jsonl;
