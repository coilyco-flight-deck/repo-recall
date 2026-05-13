//! Autonomous-dispatch interactive flow. The MCP tool surface is shaped so
//! a fresh Claude session can drive Kai through triage → score → emit →
//! spawn with no skill loaded — every tool description explains where it
//! sits in the protocol, and every tool response carries a literal
//! `next_instructions` field in plain English aimed at the model.
//!
//! State is held in-memory on `AppState`. No DB persistence: a server
//! restart drops the in-flight dispatch, matching the "substrate-only"
//! rule from `.claude/skills/recall-dispatch/SKILL.md`. Anything worth
//! keeping after a restart already lives in git, github, or as a written
//! dispatch artifact under `docs/repo-dispatch/`.

use std::collections::HashMap;
use std::sync::Arc;

use pmcp::RequestHandlerExtra;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::AppState;

fn new_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("ds-{nanos:032x}")
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Triaging tickets one at a time.
    Triage,
    /// All triage done; scoring AFK candidates.
    Score,
    /// Scores locked in; rendering per-repo dispatch prompts in memory.
    EmitPlan,
    /// Prompts approved; ready to write the docs/repo-dispatch artifacts.
    EmitCommit,
    /// Artifacts written; ready to spawn agents one by one.
    Spawn,
    /// Run complete.
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TriageChoice {
    /// Keep as-is, dispatch this autonomously (AFK).
    KeepAfk,
    /// Keep as-is, but Kai will handle it herself (HITL).
    KeepHitl,
    /// Too big; needs splitting before dispatch.
    Split,
    /// Belongs in a different repo.
    Rehome,
    /// Duplicate of another open issue.
    Dedup,
    /// Stale; propose closing.
    StaleClose,
    /// Defer for a future run.
    Defer,
}

#[derive(Debug, Clone, Serialize)]
pub struct Ticket {
    /// Canonical `owner/repo#N`.
    pub r#ref: String,
    pub title: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TriagedTicket {
    pub r#ref: String,
    pub title: String,
    pub choice: TriageChoice,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScoredTicket {
    pub r#ref: String,
    pub title: String,
    pub score: u32,
    pub autonomy_confidence: u32,
    pub basis: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DispatchPrompt {
    pub slug: String,
    pub repo: String,
    pub issue_refs: Vec<String>,
    pub score: u32,
    pub autonomy_confidence: u32,
    pub basis: String,
    pub prompt_body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DispatchSession {
    pub id: String,
    pub phase: Phase,
    pub pending_triage: Vec<Ticket>,
    pub triaged: Vec<TriagedTicket>,
    pub scored: Vec<ScoredTicket>,
    pub prompts: Vec<DispatchPrompt>,
    pub committed_paths: Vec<String>,
    pub spawned: Vec<String>,
}

/// In-memory session store. Lives on `AppState`.
pub type DispatchSessions = Arc<RwLock<HashMap<String, DispatchSession>>>;

pub fn new_store() -> DispatchSessions {
    Arc::new(RwLock::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// recall_dispatch_begin
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BeginArgs {
    /// Optional candidate ticket list in `owner/repo#N` form. When empty,
    /// the tool falls back to seeding from `recall_action_required`-style
    /// signals (open `autonomous-block` and `structural-ask` issues plus
    /// issues assigned to the viewer). The project-board pull is not yet
    /// wired in this build — pass tickets explicitly if you want a
    /// hand-picked queue.
    #[serde(default)]
    pub tickets: Vec<TicketSeed>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TicketSeed {
    /// `owner/repo#N`.
    pub r#ref: String,
    /// Optional title; will be displayed back to Kai during triage. If
    /// omitted, the ref is shown alone.
    #[serde(default)]
    pub title: Option<String>,
}

pub async fn begin(
    state: AppState,
    args: BeginArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let id = new_session_id();

    let pending_triage: Vec<Ticket> = args
        .tickets
        .into_iter()
        .map(|t| Ticket {
            r#ref: t.r#ref.clone(),
            title: t.title.unwrap_or_else(|| t.r#ref.clone()),
            url: None,
        })
        .collect();

    let sess = DispatchSession {
        id: id.clone(),
        phase: Phase::Triage,
        pending_triage,
        triaged: vec![],
        scored: vec![],
        prompts: vec![],
        committed_paths: vec![],
        spawned: vec![],
    };

    let next_instructions = build_triage_instructions(&sess);
    let snapshot = sess.clone();

    state
        .dispatch_sessions
        .write()
        .await
        .insert(id.clone(), sess);

    Ok(json!({
        "session_id": id,
        "phase": snapshot.phase,
        "pending_triage_count": snapshot.pending_triage.len(),
        "current_ticket": snapshot.pending_triage.first(),
        "next_instructions": next_instructions,
    }))
}

/// Humanized choice menu as a markdown bulleted list. Display-only - the
/// wire enum stays in snake_case.
const TRIAGE_MENU_HUMAN: &str = "\
1. 🤖 **keep AFK** *(dispatch this autonomously - a spawned Claude session works it end-to-end without you)* - `keep_afk`\n\
2. 👤 **keep human** *(the ticket stays open but you'll work it yourself, no agent fires)* - `keep_hitl`\n\
3. ✂️ **split** *(the ticket is too big to dispatch as-is, propose breaking it into smaller pieces first)* - `split`\n\
4. 📦 **rehome** *(move this ticket to a different repo where it actually belongs)* - `rehome`\n\
5. 👯 **dedup** *(this duplicates another open issue, propose closing it as a dup)* - `dedup`\n\
6. 🗑️ **stale close** *(nothing is going to happen here, propose closing it as stale)* - `stale_close`\n\
7. ⏭️ **defer** *(skip it for this run, leave it open for a future dispatch session)* - `defer`\n\
\n\
Kai may answer by ordinal (\"option two\", \"three\") or by emoji - map either to the wire token before calling `recall_dispatch_triage`.";

fn build_triage_instructions(sess: &DispatchSession) -> String {
    if sess.pending_triage.is_empty() {
        return "No tickets in the queue. Ask Kai to dictate tickets she wants to consider \
            (`owner/repo#N` refs, one at a time, or a paragraph she has prepared), then \
            call `recall_dispatch_add_tickets` with the parsed list. If she's done, call \
            `recall_dispatch_finalize_triage` to move to the score phase."
            .to_string();
    }
    let t = &sess.pending_triage[0];
    format!(
        "Triage phase. The next ticket is {} - \"{}\". \
        Read the ref and title aloud to Kai (she is driving by voice). Form a quick \
        read on which option seems to fit best (e.g. straightforward code work with \
        no judgment calls is almost always **keep AFK**; anything privileged or \
        identity-touching is **keep human**; obvious infra-bloat is **stale close**). \
        State your recommendation in one sentence, then list the full ordered menu \
        so Kai can override:\n\n{}\n\nWhen she answers (by name OR ordinal), call \
        `recall_dispatch_triage` with {{ session_id, ref: \"{}\", choice, notes? }} \
        - the `choice` field takes the wire token. The response will hand you the \
        next ticket or signal that triage is done.",
        t.r#ref, t.title, TRIAGE_MENU_HUMAN, t.r#ref,
    )
}

// ---------------------------------------------------------------------------
// MCP tool descriptions (the protocol manual, embedded in the schema)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// recall_dispatch_triage
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TriageArgs {
    pub session_id: String,
    /// `owner/repo#N` of the ticket Kai just answered. Must match the
    /// session's current `pending_triage[0]` — out-of-order calls return
    /// an error so the model can't lose its place.
    pub r#ref: String,
    pub choice: TriageChoice,
    #[serde(default)]
    pub notes: Option<String>,
}

pub async fn triage(
    state: AppState,
    args: TriageArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let mut store = state.dispatch_sessions.write().await;
    let sess = match store.get_mut(&args.session_id) {
        Some(s) => s,
        None => {
            return Ok(json!({
                "error": "unknown session_id",
                "session_id": args.session_id,
                "next_instructions": "The session_id Kai gave you is not in the live store. \
                    The server may have restarted (in-memory only). Call \
                    `recall_dispatch_begin` again and rebuild the queue.",
            }))
        }
    };

    if sess.phase != Phase::Triage {
        return Ok(json!({
            "error": "wrong phase",
            "phase": sess.phase,
            "next_instructions": format!(
                "This session is past triage — current phase is {:?}. Re-read the prior \
                response's next_instructions; that names the right tool to call.",
                sess.phase
            ),
        }));
    }

    let Some(next) = sess.pending_triage.first() else {
        // Empty queue: advance straight to score.
        sess.phase = Phase::Score;
        return Ok(json!({
            "session_id": sess.id,
            "phase": sess.phase,
            "triaged_count": sess.triaged.len(),
            "next_instructions": "Triage queue is empty. Phase advanced to `score`. \
                Call `recall_dispatch_score` to walk Kai through scoring the \
                AFK-tagged tickets.",
        }));
    };

    if next.r#ref != args.r#ref {
        return Ok(json!({
            "error": "ref mismatch",
            "expected_ref": next.r#ref,
            "got_ref": args.r#ref,
            "next_instructions": format!(
                "The next ticket in the queue is `{}`, not `{}`. Read the right ticket \
                to Kai and ask her again. If she insists on the ref she gave, the queue \
                ordering needs to be edited — call `recall_dispatch_begin` again.",
                next.r#ref, args.r#ref
            ),
        }));
    }

    // Pop the head, record the decision.
    let head = sess.pending_triage.remove(0);
    sess.triaged.push(TriagedTicket {
        r#ref: head.r#ref.clone(),
        title: head.title,
        choice: args.choice,
        notes: args.notes.clone(),
    });

    // Decide what's next.
    if sess.pending_triage.is_empty() {
        sess.phase = Phase::Score;
        return Ok(json!({
            "session_id": sess.id,
            "phase": sess.phase,
            "triaged_count": sess.triaged.len(),
            "last_decision": { "ref": head.r#ref, "choice": args.choice, "notes": args.notes },
            "next_instructions": format!(
                "Recorded `{:?}` on {}. That was the last ticket - triage is done. \
                Phase advanced to `score`. Tell Kai: \"Triage complete, {} tickets decided. \
                Ready to score.\" Then call `recall_dispatch_score` with this session_id.",
                args.choice, head.r#ref, sess.triaged.len()
            ),
        }));
    }

    let upcoming = &sess.pending_triage[0];
    Ok(json!({
        "session_id": sess.id,
        "phase": sess.phase,
        "triaged_count": sess.triaged.len(),
        "pending_triage_count": sess.pending_triage.len(),
        "last_decision": { "ref": head.r#ref, "choice": args.choice, "notes": args.notes },
        "current_ticket": upcoming,
        "next_instructions": format!(
            "Recorded `{:?}` on {}. Next ticket is {} - \"{}\". Read the ref and title to \
            Kai, then present the same bulleted menu:\n\n{}\n\nWhen she answers, call \
            `recall_dispatch_triage` again.",
            args.choice, head.r#ref, upcoming.r#ref, upcoming.title, TRIAGE_MENU_HUMAN
        ),
    }))
}

// ---------------------------------------------------------------------------
// recall_dispatch_score_next + _set
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScoreNextArgs {
    pub session_id: String,
}

pub async fn score_next(
    state: AppState,
    args: ScoreNextArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let mut store = state.dispatch_sessions.write().await;
    let Some(sess) = store.get_mut(&args.session_id) else {
        return Ok(unknown_session(&args.session_id));
    };

    if sess.phase != Phase::Score {
        return Ok(wrong_phase(sess.phase, "recall_dispatch_score_next"));
    }

    // Find the next AFK-tagged triaged ticket without a score yet.
    let scored_refs: std::collections::HashSet<&str> =
        sess.scored.iter().map(|s| s.r#ref.as_str()).collect();
    let next = sess
        .triaged
        .iter()
        .find(|t| {
            matches!(t.choice, TriageChoice::KeepAfk) && !scored_refs.contains(t.r#ref.as_str())
        })
        .cloned();

    match next {
        None => {
            sess.phase = Phase::EmitPlan;
            Ok(json!({
                "session_id": sess.id,
                "phase": sess.phase,
                "scored_count": sess.scored.len(),
                "next_instructions": format!(
                    "All AFK-tagged tickets scored ({} of them). Phase advanced to \
                    `emit_plan`. Tell Kai: \"Scoring complete. Ready to render per-repo \
                    prompts.\" Then call `recall_dispatch_emit_plan` with this session_id.",
                    sess.scored.len()
                ),
            }))
        }
        Some(t) => Ok(json!({
            "session_id": sess.id,
            "phase": sess.phase,
            "current_ticket": { "ref": t.r#ref, "title": t.title, "notes": t.notes },
            "scored_count": sess.scored.len(),
            "next_instructions": format!(
                "Score phase. Next ticket: {} - \"{}\". \
                **Do not ask Kai to score.** This step is yours to automate. Pull \
                substrate evidence with `recall_ticket_history` for {} (sessions and \
                commits that touched the issue), and `recall_repo` for context on the \
                target repo's recent activity. Read the repo's `docs/AUTONOMY.md` if \
                present. Then compute: \
                \n\n\
                - **score (1-10)**: dispatch threshold is 5. Weights live in the \
                recall-dispatch handbook; for now, a simple rubric is fine - higher \
                for narrow concrete work with clear acceptance criteria, lower for \
                ambiguous or judgment-heavy work. \
                \n\
                - **autonomy_confidence (1-5)**: how likely a spawned session finishes \
                without escalating. MUST be anchored to a substrate citation. If you \
                can't write a one-sentence citation (closed similar ticket, prior \
                `autonomous-block`, or this repo's `docs/AUTONOMY.md`), the rule from \
                the recall-dispatch SKILL is: confidence is 1. \
                \n\
                - **basis**: that one-sentence citation. \
                \n\n\
                State your computed values to Kai (\"I'm scoring this 7, confidence 4, \
                basis: closed #87 shipped a similar emitter, took one dispatch.\") and \
                ask her to confirm or override. If she overrides, use her numbers. \
                Then call `recall_dispatch_score_set` with the final values. \
                Notes from triage: {}.",
                t.r#ref,
                t.title,
                t.r#ref,
                t.notes.as_deref().unwrap_or("none"),
            ),
        })),
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ScoreSetArgs {
    pub session_id: String,
    pub r#ref: String,
    /// 1-10. Threshold for dispatch is 5.
    pub score: u32,
    /// 1-5. Must be 1 if no substrate citation is given in `basis`.
    pub autonomy_confidence: u32,
    /// One sentence citing the substrate evidence (a closed similar ticket,
    /// a prior autonomous-block, this repo's docs/AUTONOMY.md). Required.
    pub basis: String,
}

pub async fn score_set(
    state: AppState,
    args: ScoreSetArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let mut store = state.dispatch_sessions.write().await;
    let Some(sess) = store.get_mut(&args.session_id) else {
        return Ok(unknown_session(&args.session_id));
    };
    if sess.phase != Phase::Score {
        return Ok(wrong_phase(sess.phase, "recall_dispatch_score_set"));
    }

    // Confirm the ticket is AFK and not already scored.
    let triaged = sess.triaged.iter().find(|t| t.r#ref == args.r#ref).cloned();
    let Some(triaged) = triaged else {
        return Ok(json!({
            "error": "ref not in triaged set",
            "ref": args.r#ref,
            "next_instructions": "That ref isn't in the triaged set. Call \
                `recall_dispatch_score_next` to get the correct ref to score.",
        }));
    };
    if !matches!(triaged.choice, TriageChoice::KeepAfk) {
        return Ok(json!({
            "error": "ticket is not AFK-tagged",
            "ref": args.r#ref,
            "choice": triaged.choice,
            "next_instructions": "Only AFK-tagged tickets are scored. Call \
                `recall_dispatch_score_next` for the right next ticket.",
        }));
    }
    if sess.scored.iter().any(|s| s.r#ref == args.r#ref) {
        return Ok(json!({
            "error": "already scored",
            "ref": args.r#ref,
            "next_instructions": "That ticket already has a score. Call \
                `recall_dispatch_score_next` for the next one.",
        }));
    }

    sess.scored.push(ScoredTicket {
        r#ref: args.r#ref.clone(),
        title: triaged.title.clone(),
        score: args.score,
        autonomy_confidence: args.autonomy_confidence,
        basis: args.basis.clone(),
    });

    Ok(json!({
        "session_id": sess.id,
        "phase": sess.phase,
        "scored_count": sess.scored.len(),
        "last_score": {
            "ref": args.r#ref,
            "score": args.score,
            "autonomy_confidence": args.autonomy_confidence,
            "basis": args.basis,
            "above_threshold": args.score >= 5,
        },
        "next_instructions": format!(
            "Recorded score {} (confidence {}) on {}. {} \
            Call `recall_dispatch_score_next` for the next ticket or the phase advance.",
            args.score,
            args.autonomy_confidence,
            args.r#ref,
            if args.score >= 5 {
                "Above threshold - this will get a dispatch prompt."
            } else {
                "Below threshold (5) - this will be deferred, no prompt rendered."
            },
        ),
    }))
}

pub const SCORE_NEXT_DESCRIPTION: &str = "\
Walk through scoring the AFK-tagged tickets from triage, one at a time. \
No args except session_id. Each call returns the next unscored AFK ticket \
plus instructions to gather score / autonomy_confidence / basis from Kai. \
When every AFK ticket has a score, the session auto-advances to \
`emit_plan` and the response tells you to call `recall_dispatch_emit_plan`. \
Only valid in the `score` phase.
";

pub const SCORE_SET_DESCRIPTION: &str = "\
Submit Kai's score for a single ticket. Requires {{ session_id, ref, score \
(1-10), autonomy_confidence (1-5), basis }}. The basis is a one-sentence \
substrate citation — the rule in the recall-dispatch SKILL is \
\"if you cannot write a one-sentence justification citing one of \
(closed similar ticket, prior autonomous-block, this repo's docs/AUTONOMY.md), \
score it 1\". Refuses to score the same ref twice, or to score a non-AFK \
ticket. After recording, call `recall_dispatch_score_next` for the next \
ticket or phase advance.
";

// ---------------------------------------------------------------------------
// recall_dispatch_emit_plan
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmitPlanArgs {
    pub session_id: String,
}

pub async fn emit_plan(
    state: AppState,
    args: EmitPlanArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let mut store = state.dispatch_sessions.write().await;
    let Some(sess) = store.get_mut(&args.session_id) else {
        return Ok(unknown_session(&args.session_id));
    };
    if sess.phase != Phase::EmitPlan {
        return Ok(wrong_phase(sess.phase, "recall_dispatch_emit_plan"));
    }

    // Render one DispatchPrompt per scored ticket >= threshold. Group by repo
    // so docs/repo-dispatch/<slug>.md ends up under the right repo.
    let mut prompts: Vec<DispatchPrompt> = Vec::new();
    for s in &sess.scored {
        if s.score < 5 {
            continue;
        }
        let (repo, issue_num) = match split_ref(&s.r#ref) {
            Some(parts) => parts,
            None => continue,
        };
        let slug = format!(
            "{}-{}-{}",
            chrono::Utc::now().format("%Y%m%d"),
            issue_num,
            slugify(&s.title),
        );
        let body = render_prompt_body(&s.r#ref, &s.title, &s.basis, s.score, s.autonomy_confidence);
        prompts.push(DispatchPrompt {
            slug,
            repo,
            issue_refs: vec![s.r#ref.clone()],
            score: s.score,
            autonomy_confidence: s.autonomy_confidence,
            basis: s.basis.clone(),
            prompt_body: body,
        });
    }

    sess.prompts = prompts.clone();
    let count = sess.prompts.len();

    Ok(json!({
        "session_id": sess.id,
        "phase": sess.phase,
        "prompts": prompts,
        "next_instructions": format!(
            "Rendered {} dispatch prompts (one per scored ticket >= 5). These are in \
            memory only - no files written yet. Read each prompt's `slug`, `repo`, \
            and the first line of `prompt_body` to Kai. She can: \
            (a) approve as-is - call `recall_dispatch_emit_commit` next, \
            (b) ask for edits - re-do scoring or skip to commit and edit the file after. \
            When approved, call `recall_dispatch_emit_commit`.",
            count
        ),
    }))
}

fn split_ref(r: &str) -> Option<(String, u64)> {
    // "owner/repo#N" → ("owner/repo", N)
    let (left, num) = r.rsplit_once('#')?;
    let n: u64 = num.parse().ok()?;
    Some((left.to_string(), n))
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars().flat_map(|c| c.to_lowercase()) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 40 {
        out.truncate(40);
        while out.ends_with('-') {
            out.pop();
        }
    }
    if out.is_empty() {
        out.push_str("ticket");
    }
    out
}

fn render_prompt_body(r: &str, title: &str, basis: &str, score: u32, conf: u32) -> String {
    format!(
        "# {title}\n\
        \n\
        Resolve {r}.\n\
        \n\
        - score: {score} (threshold 5)\n\
        - autonomy_confidence: {conf}/5\n\
        - basis: {basis}\n\
        \n\
        Git workflow: commit to main, push after each commit, every commit closes its \
        issue with `closes #N`, never `--no-verify`. If blocked, file an \
        `autonomous-block` issue with the failing reference, mechanism, and unblock \
        condition, then move on.\n",
    )
}

// ---------------------------------------------------------------------------
// recall_dispatch_emit_commit
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmitCommitArgs {
    pub session_id: String,
}

pub async fn emit_commit(
    state: AppState,
    args: EmitCommitArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    // Take a snapshot of prompts to write, then mutate state under a short lock.
    let prompts: Vec<DispatchPrompt> = {
        let store = state.dispatch_sessions.read().await;
        let Some(sess) = store.get(&args.session_id) else {
            return Ok(unknown_session(&args.session_id));
        };
        if sess.phase != Phase::EmitPlan {
            return Ok(wrong_phase(sess.phase, "recall_dispatch_emit_commit"));
        }
        sess.prompts.clone()
    };

    // Resolve repos via the cache once.
    let cache = state.cache_db.clone();
    let repos = tokio::task::spawn_blocking(move || cache.list_repos_with_counts())
        .await
        .map_err(|e| pmcp::Error::internal(format!("join error: {e}")))?
        .map_err(|e| pmcp::Error::internal(format!("db error: {e}")))?;

    let mut written: Vec<Value> = Vec::new();
    let mut errors: Vec<Value> = Vec::new();

    for p in &prompts {
        // Repo name segment after "owner/" in "owner/repo".
        let repo_short = p.repo.split('/').next_back().unwrap_or(&p.repo);
        let Some(repo) = repos.iter().find(|r| r.name == repo_short) else {
            errors.push(json!({
                "slug": p.slug,
                "repo": p.repo,
                "error": format!("repo `{}` not found in repo-recall scan; skipping", repo_short),
            }));
            continue;
        };
        let repo_path = std::path::PathBuf::from(&repo.path);
        let repo_name = repo.name.clone();
        let req = crate::display::dispatch_artifacts::EmitDispatchRequest {
            issue_refs: p.issue_refs.clone(),
            score: Some(p.score as i64),
            autonomy_confidence: Some(p.autonomy_confidence as i64),
            autonomy_confidence_basis: Some(p.basis.clone()),
            tracking_issue: None,
            prompt: p.prompt_body.clone(),
            slug: Some(p.slug.clone()),
        };
        let result = tokio::task::spawn_blocking(move || {
            crate::display::dispatch_artifacts::emit_dispatch(&repo_path, &repo_name, &req)
        })
        .await;
        match result {
            Err(e) => errors.push(json!({ "slug": p.slug, "error": format!("join: {e}") })),
            Ok(Err(e)) => errors.push(json!({ "slug": p.slug, "error": format!("{e}") })),
            Ok(Ok(resp)) => written.push(serde_json::to_value(resp).unwrap_or(json!(null))),
        }
    }

    let mut store = state.dispatch_sessions.write().await;
    let Some(sess) = store.get_mut(&args.session_id) else {
        return Ok(unknown_session(&args.session_id));
    };
    sess.phase = Phase::Spawn;
    sess.committed_paths = written
        .iter()
        .filter_map(|v| {
            v.get("in_repo_path")
                .and_then(|p| p.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    Ok(json!({
        "session_id": sess.id,
        "phase": sess.phase,
        "written": written,
        "errors": errors,
        "next_instructions": format!(
            "Wrote {} dispatch artifact(s){}. Phase advanced to `spawn`. Tell Kai the \
            paths so she can git-commit them (the recall-dispatch rule is that the \
            caller commits the in-repo file; repo-recall never touches git on its own). \
            Then walk her through spawning: for each prompt, ask her go/no-go and \
            call `recall_dispatch_spawn` with the issue ref. She wants confirmation \
            BEFORE every spawn - never batch.",
            written.len(),
            if errors.is_empty() { String::new() } else { format!(", {} errors", errors.len()) },
        ),
    }))
}

pub const EMIT_COMMIT_DESCRIPTION: &str = "\
Actually write the dispatch artifacts to disk. Calls the existing \
`recall_record_dispatch` writer per prompt, which produces a write-once \
file at `<repo>/docs/repo-dispatch/<slug>.md` plus the pollable mirror \
under `~/.repo-recall/dispatch/<repo>/<slug>.md`. \
\
repo-recall never touches git - Kai (or the spawned session itself) is \
responsible for committing the in-repo file. The response includes the \
written paths and any per-prompt errors. After this call the session is \
in the `spawn` phase.
";

// ---------------------------------------------------------------------------
// recall_dispatch_spawn
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SpawnArgs {
    pub session_id: String,
    /// `owner/repo#N` of the prompt to spawn. Must be one of the issue_refs
    /// from a prompt rendered in `emit_plan`.
    pub r#ref: String,
    /// If true, runs `coily dispatch --dry-run <ref>` so you can preview the
    /// resolved prompt + repo path without actually exec'ing claude.
    #[serde(default)]
    pub dry_run: bool,
}

pub async fn spawn(
    state: AppState,
    args: SpawnArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let r_ref = args.r#ref.clone();
    {
        let store = state.dispatch_sessions.read().await;
        let Some(sess) = store.get(&args.session_id) else {
            return Ok(unknown_session(&args.session_id));
        };
        if sess.phase != Phase::Spawn {
            return Ok(wrong_phase(sess.phase, "recall_dispatch_spawn"));
        }
        let known = sess
            .prompts
            .iter()
            .any(|p| p.issue_refs.iter().any(|r| r == &r_ref));
        if !known {
            return Ok(json!({
                "error": "ref not in committed prompts",
                "ref": r_ref,
                "next_instructions": "That ref isn't in any rendered prompt for this \
                    session. Re-read the emit_plan response for the valid refs.",
            }));
        }
    }

    let mut cmd = tokio::process::Command::new("coily");
    cmd.arg("dispatch");
    if args.dry_run {
        cmd.arg("--dry-run");
    }
    cmd.arg(&r_ref);
    // coily.dispatch reads the issue from gh and exec's `claude -p`. It is the
    // sanctioned path from inside a coily session to a fresh top-level claude.
    // We run it detached so the new session does not inherit this MCP server's
    // stdin/stdout. For --dry-run we capture output to surface back.

    if args.dry_run {
        let out = cmd
            .output()
            .await
            .map_err(|e| pmcp::Error::internal(format!("spawn coily: {e}")))?;
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        return Ok(json!({
            "session_id": args.session_id,
            "ref": r_ref,
            "dry_run": true,
            "exit_code": out.status.code(),
            "stdout": stdout,
            "stderr": stderr,
            "next_instructions": "Dry run complete. Read the resolved prompt + repo \
                path to Kai. If she approves, call `recall_dispatch_spawn` again \
                without `dry_run` to actually fire. If she wants to skip, just move \
                on to the next ref.",
        }));
    }

    // Real spawn: detach the child. We don't have a great host-side handle to
    // it (coily execs `claude -p` which detaches further), so we just confirm
    // the child started and record the ref as "spawned".
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = cmd
        .spawn()
        .map_err(|e| pmcp::Error::internal(format!("spawn coily: {e}")))?;
    let pid = child.id();

    {
        let mut store = state.dispatch_sessions.write().await;
        if let Some(sess) = store.get_mut(&args.session_id) {
            sess.spawned.push(r_ref.clone());
        }
    }

    Ok(json!({
        "session_id": args.session_id,
        "ref": r_ref,
        "spawned": true,
        "pid": pid,
        "next_instructions": "Spawned. Tell Kai: \"{ref} spawned as pid {pid}.\" Ask \
            her about the next ref or whether she's done. If done, call \
            `recall_dispatch_done` to wrap the session. Otherwise, call \
            `recall_dispatch_spawn` for the next approved ref.",
    }))
}

pub const SPAWN_DESCRIPTION: &str = "\
Fire `coily dispatch <ref>` for one approved prompt. Detaches the child so \
the new Claude session is independent. Only valid in the `spawn` phase. \
Pass `dry_run: true` to preview the resolved prompt and repo path \
without exec'ing claude. \
\
NEVER batch spawns. Confirm with Kai go/no-go BEFORE every single spawn. \
This is the one place the dispatch flow leaves the substrate and starts \
a real autonomous run, so the confirmation step is sacred.
";

// ---------------------------------------------------------------------------
// recall_dispatch_done
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DoneArgs {
    pub session_id: String,
}

pub async fn done(
    state: AppState,
    args: DoneArgs,
    _extra: RequestHandlerExtra,
) -> pmcp::Result<Value> {
    let mut store = state.dispatch_sessions.write().await;
    let Some(sess) = store.get_mut(&args.session_id) else {
        return Ok(unknown_session(&args.session_id));
    };
    sess.phase = Phase::Done;
    let summary = sess.clone();
    Ok(json!({
        "session_id": summary.id,
        "phase": summary.phase,
        "triaged_count": summary.triaged.len(),
        "scored_count": summary.scored.len(),
        "prompts_count": summary.prompts.len(),
        "committed_paths": summary.committed_paths,
        "spawned_refs": summary.spawned,
        "next_instructions": "Session marked done. Summarize for Kai: how many triaged, \
            how many AFK, how many spawned. The session record stays in memory until \
            the server restarts.",
    }))
}

pub const DONE_DESCRIPTION: &str = "\
Mark a dispatch session done. Returns the final tally. Call this when Kai \
says she's done spawning, even if not every approved prompt was spawned.
";

pub const EMIT_PLAN_DESCRIPTION: &str = "\
Render per-repo dispatch prompts in memory from the scored tickets. \
No file write yet. Only valid in the `emit_plan` phase. The response \
includes the rendered prompts so you can read them to Kai for approval. \
After approval, call `recall_dispatch_emit_commit` to actually write the \
artifacts via the existing `recall_record_dispatch` writer.
";

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn unknown_session(id: &str) -> Value {
    json!({
        "error": "unknown session_id",
        "session_id": id,
        "next_instructions": "The session_id is not in the live store. The server may \
            have restarted (in-memory only). Call `recall_dispatch_begin` to start over.",
    })
}

fn wrong_phase(actual: Phase, tool: &str) -> Value {
    json!({
        "error": "wrong phase",
        "phase": actual,
        "next_instructions": format!(
            "`{tool}` is not valid in phase `{actual:?}`. Re-read the prior response's \
            next_instructions; that names the right tool to call."
        ),
    })
}

pub const TRIAGE_DESCRIPTION: &str = "\
Submit Kai's triage decision for the current ticket and advance to the next \
one. Only valid while the session is in the `triage` phase. The ref you pass \
must match the session's pending head — if you call this out of order, you \
get a `ref mismatch` error with the right ref to use, no state changes. \
\
Choices: \
- 🤖 `keep_afk` - dispatch this autonomously \
- 👤 `keep_hitl` - Kai will handle it herself \
- ✂️ `split` - too big, needs splitting before dispatch \
- 📦 `rehome` - belongs in a different repo \
- 👯 `dedup` - duplicate of another open issue \
- 🗑️ `stale_close` - propose closing as stale \
- ⏭️ `defer` - skip for this run \
\
When the queue empties, the session auto-advances to the `score` phase and \
the response tells you to call `recall_dispatch_score`. Until then, the \
response hands you the next ticket plus the prompt to read to Kai.
";

pub const BEGIN_DESCRIPTION: &str = "\
Start an interactive autonomous-dispatch run. This is the single entry point: \
when Kai says 'start autonomous dispatch', 'let's do repo recall autonomous \
engineering dispatch', 'let's do autonomous engineering dispatch', 'start \
AFK dispatch', 'plan the AFK run', 'queue autonomous work', or any close \
variant, call this tool first with the candidate ticket list. Voice \
dictation may garble these phrases (e.g. \"non-autonomous\" for \
\"autonomous\", \"vehicle\" for \"AFK\") - trigger on the intent. Every subsequent tool in the `recall_dispatch_*` \
family advances a session created here.

The flow is interactive end-to-end and Kai is driving by voice. Every tool \
response includes a `next_instructions` field telling you, the model, what to \
say to Kai and which tool to call next. Read those instructions verbatim; do \
not invent a different flow.

Phases: triage → score → emit_plan → emit_commit → spawn → done. You stay in \
triage until every ticket has a choice. Then `recall_dispatch_score` walks \
through scoring with substrate-grounded autonomy_confidence priors. Then \
`recall_dispatch_emit_plan` renders per-repo prompt bodies for Kai to approve. \
Then `recall_dispatch_emit_commit` writes the dispatch artifacts via the \
existing `recall_record_dispatch` writer. Then `recall_dispatch_spawn` fires \
`coily dispatch` for each approved prompt, one at a time, with Kai's go/no-go \
between each one.

Args: pass `tickets` as a list of `{ ref, title? }` where ref is \
`owner/repo#N`. An empty list is allowed; the tool will start an empty \
session and prompt you to gather tickets from Kai before triage proceeds.

State is in-memory only. A repo-recall restart drops in-flight dispatch \
sessions on purpose — anything worth keeping is supposed to live as a \
written dispatch artifact, a closed issue, or a labeled issue, never as \
local-DB state.
";
