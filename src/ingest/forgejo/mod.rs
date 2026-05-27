//! Forgejo/Gitea REST ingest. See docs/forgejo-dispatch.md.

pub mod client;

pub use client::{build_client, ReqwestForgejoClient};
