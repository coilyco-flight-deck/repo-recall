//! Shared action-required signal derivation. Both the HTTP `routes::api`
//! surface and the MCP `mcp::tools` surface call into this. Adding a new

use crate::db;
use crate::process::activity;

pub struct DerivedSignal {
    pub signal: &'static str,
    /// Chatty, JSON-facing description. Carries counts, op names, branch
    /// text - whatever a machine consumer wants spelled out.
    pub detail: String,
    /// Pre-capped one-liner for the human `watch curl` render. Never wraps,
    /// flat, greppable. Rendered as `<repo>: <terse>` (see #233).
    pub terse: String,
}

/// Map a `Repo` row's individual fields onto the curated set of signals
/// that drive `is_action_required`. One repo can produce multiple items
pub fn derive_action_signals(r: &db::Repo) -> Vec<DerivedSignal> {
    let mut out = Vec::new();
    if activity::is_vendored(r) {
        return out;
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
            terse: format!(
                "{dirty} uncommitted file{}",
                if dirty == 1 { "" } else { "s" }
            ),
        });
    }
    if let Some(op) = r.in_progress_op.as_deref() {
        out.push(DerivedSignal {
            signal: "in_progress_op",
            detail: format!("{op} in progress"),
            terse: format!("{op} in progress"),
        });
    }
    if r.head_ref.as_deref() == Some("detached") {
        out.push(DerivedSignal {
            signal: "detached_head",
            detail: "HEAD is detached".into(),
            terse: "detached HEAD".into(),
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
            terse: format!(
                "{n} PR{} awaiting your review",
                if n == 1 { "" } else { "s" }
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
            terse: format!("{n} draft PR{} to finish", if n == 1 { "" } else { "s" }),
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
            terse: format!("{n} PR{} need a reviewer", if n == 1 { "" } else { "s" }),
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
            terse: format!("{n} open PR{} to test", if n == 1 { "" } else { "s" }),
        });
    }
    if r.issues_assigned_to_me > 0 {
        let n = r.issues_assigned_to_me;
        out.push(DerivedSignal {
            signal: "issue_assigned",
            detail: format!("{n} issue{} assigned to you", if n == 1 { "" } else { "s" },),
            terse: format!("{n} issue{} assigned to you", if n == 1 { "" } else { "s" }),
        });
    }
    if r.commits_ahead > 0 {
        let n = r.commits_ahead;
        out.push(DerivedSignal {
            signal: "push",
            detail: format!(
                "{n} local commit{} not pushed to the upstream branch",
                if n == 1 { "" } else { "s" },
            ),
            terse: format!("push {n} commit{}", if n == 1 { "" } else { "s" }),
        });
    }
    if activity::is_deploy_failing(r) {
        let wf = r.deploy_workflow.as_deref().unwrap_or("deploy");
        out.push(DerivedSignal {
            signal: "deploy_failing",
            detail: format!("last `{wf}` run on the default branch failed"),
            terse: "deploy failing".into(),
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
            terse: format!("deploy stale ({days}d)"),
        });
    }
    if !r.stale_branches.is_empty() {
        let n = r.stale_branches.len();
        let plural = if n == 1 { "" } else { "es" };
        let list = r
            .stale_branches
            .iter()
            .map(|b| format!("{} ({})", b.name, humanize_age(b.tip_age_secs)))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(DerivedSignal {
            signal: "stale_branch",
            detail: format!(
                "{n} stale local branch{plural} - unmerged work with a tip older \
                 than 24h, land it or delete it: {list}"
            ),
            terse: format!("{n} stale branch{plural} to land or delete"),
        });
    }
    let _ = activity::is_action_required;
    out
}

/// Compact age rendering for branch staleness - whole days once past 24h,
/// hours below that. Stale branches are always >24h old by construction, so
fn humanize_age(secs: i64) -> String {
    let days = secs / 86_400;
    if days >= 1 {
        format!("{days}d")
    } else {
        format!("{}h", secs / 3_600)
    }
}
