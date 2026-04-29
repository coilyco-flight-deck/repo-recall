//! MCP App server. Tools expose repo-recall's data layer to MCP hosts;
//! widgets render the dashboard inside the host's iframe.
//!
//! Six tools, one widget (so far):
//!
//! - `recall_dashboard` (with widget) — repo list + action-required + counts.
//! - `recall_repo` — single repo detail.
//! - `recall_session` — single session detail.
//! - `recall_search` — unified search.
//! - `recall_action_required` — thin orchestrator slice.
//! - `recall_refresh` — trigger a rescan.

use std::sync::atomic::Ordering;

use pmcp::{
    ResourceCollection, Server, ServerCapabilities, TypedTool, UIResourceBuilder,
};

use crate::AppState;

mod tools;

const DASHBOARD_HTML: &str = include_str!("../widgets/dashboard.html");
const DASHBOARD_URI: &str = "ui://repo-recall/dashboard.html";

pub async fn run_stdio(state: AppState) -> anyhow::Result<()> {
    // Widget resources. Dashboard is the only one with a widget so far;
    // every other tool returns text content the host displays directly.
    let (dashboard_resource, dashboard_contents) =
        UIResourceBuilder::new(DASHBOARD_URI, "repo-recall dashboard")
            .description("Ranked repo list with action-required signals and session counts.")
            .html_template(DASHBOARD_HTML)
            .build_with_contents()
            .map_err(|e| anyhow::anyhow!("UIResourceBuilder failed: {e:?}"))?;

    let resources = ResourceCollection::new()
        .add_ui_resource(dashboard_resource, dashboard_contents);

    // Tools.
    let dashboard = {
        let state = state.clone();
        TypedTool::new("recall_dashboard", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::dashboard(s, args, extra))
        })
        .with_description(
            "Ranked list of repos discovered on disk with their action-required signals, \
             session counts, and 30-day activity. Returns structured data for the dashboard widget.",
        )
        .with_ui(DASHBOARD_URI)
    };

    let repo = {
        let state = state.clone();
        TypedTool::new("recall_repo", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::repo(s, args, extra))
        })
        .with_description(
            "Detail view for a single repo: hottest files by churn, sessions that touched it, \
             recent commits, remote state.",
        )
    };

    let session = {
        let state = state.clone();
        TypedTool::new("recall_session", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::session(s, args, extra))
        })
        .with_description(
            "Detail view for a single Claude Code session: metadata, repos it touched, summary.",
        )
    };

    let search = {
        let state = state.clone();
        TypedTool::new("recall_search", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::search(s, args, extra))
        })
        .with_description(
            "Unified search across repos, sessions, and commits. Returns partitioned hits.",
        )
    };

    let action_required = {
        let state = state.clone();
        TypedTool::new("recall_action_required", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::action_required(s, args, extra))
        })
        .with_description(
            "Thin orchestrator slice: only the repos with at least one action-required signal \
             (failing CI, dirty tree, in-progress git op, detached HEAD, awaiting review, \
             assigned issue, deploy failing/stale).",
        )
    };

    let refresh_tool = {
        let state = state.clone();
        TypedTool::new("recall_refresh", move |args, extra| {
            let s = state.clone();
            Box::pin(tools::refresh(s, args, extra))
        })
        .with_description(
            "Trigger a fresh scan of repos, sessions, commits, and remote state. Awaits \
             completion. Coalesces with any in-flight scan.",
        )
    };

    let server = Server::builder()
        .name("repo-recall")
        .version(env!("REPO_RECALL_VERSION"))
        .capabilities(ServerCapabilities::default())
        .resources(resources)
        .tool("recall_dashboard", dashboard)
        .tool("recall_repo", repo)
        .tool("recall_session", session)
        .tool("recall_search", search)
        .tool("recall_action_required", action_required)
        .tool("recall_refresh", refresh_tool)
        .build()
        .map_err(|e| anyhow::anyhow!("Server::build failed: {e:?}"))?;

    tracing::info!(
        "MCP server ready: scan_version={}",
        state.scan_version.load(Ordering::Acquire)
    );
    server
        .run_stdio()
        .await
        .map_err(|e| anyhow::anyhow!("server stdio loop failed: {e:?}"))?;
    Ok(())
}
