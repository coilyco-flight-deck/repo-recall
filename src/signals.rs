//! Shared action-required signal derivation. Both the HTTP `routes::api`
//! surface and the MCP `mcp::tools` surface call into this. Adding a new
//! signal here lights it up on both paths at once.
//!
//! Keep this list in sync with [`crate::activity::is_action_required`] —
//! same triggers, just exploded into per-signal records.

use crate::{activity, db};

pub struct DerivedSignal {
    pub signal: &'static str,
    pub detail: String,
}

/// Map a `Repo` row's individual fields onto the curated set of signals
/// that drive `is_action_required`. One repo can produce multiple items
/// (e.g. failing CI *and* a dirty tree) — the orchestrator can act on
/// each independently.
pub fn derive_action_signals(r: &db::Repo) -> Vec<DerivedSignal> {
    let mut out = Vec::new();
    if activity::is_vendored(r) {
        return out;
    }
    if r.ci_status.as_deref() == Some("failure") {
        out.push(DerivedSignal {
            signal: "ci_failing",
            detail: "default-branch CI failed".into(),
        });
    }
    let dirty = r.untracked_files + r.modified_files;
    if dirty > 0 {
        out.push(DerivedSignal {
            signal: "dirty_tree",
            detail: format!(
                "{dirty} uncommitted file{} ({} modified, {} untracked)",
                if dirty == 1 { "" } else { "s" },
                r.modified_files,
                r.untracked_files,
            ),
        });
    }
    if let Some(op) = r.in_progress_op.as_deref() {
        out.push(DerivedSignal {
            signal: "in_progress_op",
            detail: format!("{op} in progress"),
        });
    }
    if r.head_ref.as_deref() == Some("detached") {
        out.push(DerivedSignal {
            signal: "detached_head",
            detail: "HEAD is detached".into(),
        });
    }
    if r.prs_awaiting_my_review > 0 {
        let n = r.prs_awaiting_my_review;
        out.push(DerivedSignal {
            signal: "review_requested",
            detail: format!(
                "{n} PR{} awaiting your review",
                if n == 1 { "" } else { "s" },
            ),
        });
    }
    if r.my_draft_prs > 0 {
        let n = r.my_draft_prs;
        out.push(DerivedSignal {
            signal: "my_draft_pr",
            detail: format!(
                "{n} draft PR{} of yours - get into a reviewable state",
                if n == 1 { "" } else { "s" },
            ),
        });
    }
    if r.prs_mine_no_reviewer > 0 {
        let n = r.prs_mine_no_reviewer;
        out.push(DerivedSignal {
            signal: "pr_no_reviewer",
            detail: format!(
                "{n} of your open PR{} {} no reviewer - request one or self-merge",
                if n == 1 { "" } else { "s" },
                if n == 1 { "has" } else { "have" },
            ),
        });
    }
    if r.prs_mine_awaiting_review > 0 {
        let n = r.prs_mine_awaiting_review;
        out.push(DerivedSignal {
            signal: "my_open_pr",
            detail: format!(
                "{n} open PR{} of yours - test it before they review",
                if n == 1 { "" } else { "s" },
            ),
        });
    }
    if r.issues_assigned_to_me > 0 {
        let n = r.issues_assigned_to_me;
        out.push(DerivedSignal {
            signal: "issue_assigned",
            detail: format!("{n} issue{} assigned to you", if n == 1 { "" } else { "s" },),
        });
    }
    if activity::is_deploy_failing(r) {
        let wf = r.deploy_workflow.as_deref().unwrap_or("deploy");
        out.push(DerivedSignal {
            signal: "deploy_failing",
            detail: format!("last `{wf}` run on the default branch failed"),
        });
    } else if activity::is_deploy_stale(r) {
        let wf = r.deploy_workflow.as_deref().unwrap_or("deploy");
        let days = r
            .deploy_last_success_ts
            .map(|ts| (chrono::Utc::now().timestamp() - ts) / 86_400)
            .unwrap_or(0);
        out.push(DerivedSignal {
            signal: "deploy_stale",
            detail: format!("`{wf}` last green {days}d ago"),
        });
    }
    let _ = activity::is_action_required;
    out
}
