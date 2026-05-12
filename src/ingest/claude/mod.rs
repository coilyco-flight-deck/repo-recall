//! Claude Code ingest sources.
//!
//! `sessions_jsonl` parses `~/.claude/projects/**/*.jsonl` for session
//! metadata. Future siblings (`tool_calls`, `issue_refs`) will provide
//! derived views over the same JSONL stream. See #92.

pub mod sessions_jsonl;
