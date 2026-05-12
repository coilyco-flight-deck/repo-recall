---
name: recall-dispatch
description: Substrate-querying autonomous-engineering planner. Reads org backlog from GitHub, recent agent activity from repo-recall's session index, prior dispatch outcomes from closed issues plus their commits, and per-repo agent profiles from docs/AUTONOMY.md. Triages tickets, scores them for AFK-ability against substrate-derived priors, surfaces structural-context asks Kai still needs to answer, and emits one copy-pastable dispatch prompt per repo, ordered to maximize work without intervention. Aliases - recall dispatch, recall-dispatch, recall launchpad, autonomous engineering, AFK queue, lights-out queue, autonomous dispatch, AFK dispatch, plan the AFK run, queue autonomous work, run the lights-out factory, what should the bots work on.
---

# Recall Dispatch

Plan a long autonomous-engineering session. The planner is a query-and-emit loop over repo-recall's substrate. The substrate is git, github, and Claude Code sessions. The planner reads, decides, emits dispatch prompts. It does not spawn agents. Kai (or external automation) copies each per-repo prompt into a fresh Claude session.

The detailed mechanics (triage decisions, scoring formula, output block shapes, anti-patterns, decision recording, dispatch ordering) live in `references/handbook.md`. Read it before producing output.

## Three sources, no local database

Everything the planner reads comes from one of three places:
- git (`git log`, working tree, branches, remotes).
- github (issues, PRs, comments, labels, project board, runs, codeowners).
- Claude Code sessions (`~/.claude/projects/**/*.jsonl`).

repo-recall is the view layer. Its cache is throwaway. Anything the planner wants to remember lives back in git or github. Closing comments. Issue labels (`autonomy:*`, `dispatched:*`, `structural-ask`, `autonomous-block`, `hitl`). Per-repo `docs/AUTONOMY.md`. AGENTS.md edits. No local-DB state, ever. Anything readable only via session id is private signal, not durable evidence.

## Preflight

1. cd into a real git repo before any `coily ops gh` call. Coily binds every audit row to a commit scope. Outside a repo, coily errors with `scope: cwd is not inside a git repo`. repo-recall itself is the natural cwd for a recall-dispatch run.
2. Confirm the repo-recall scan is fresh. Boot the dashboard or hit `GET /api/scan-version` to confirm a recent scan. If stale, `POST /api/refresh` and wait for the version to bump.
3. Pull priorities from the project board (priorities live as a project field, not as labels):
   ```
   coily ops gh project item-list 2 --owner coilysiren --format json --limit 400
   ```
   Filter to `select(.status != "Done" and .content.type == "Issue")`.
4. For each candidate ticket, query repo-recall for evidence before scoring:
   - `recall_repo(id)` for the repo's session and commit history.
   - `recall_search(q)` for past sessions or commits that touched the ticket's issue number, file paths, or title keywords.
   - `recall_action_required` for live block signals on the repo.
   - Read the repo's `docs/AUTONOMY.md` if present for the repo's self-described AFK strengths and weaknesses.
5. Defer per-repo `git status` checks until prompt-assembly time. Walking 19 repos upfront wastes context for low signal.
6. **Scale check.** If active items > ~100, do not score every ticket. Propose a scope cut to Kai before triage. Full pass on P0-P2, title-scan P3 for AFK candidates, bulk-defer P4. Wait for confirmation.

## Phases

1. **Triage.** Per-ticket: split, rehome, dedup, stale-close, acceptance-criteria check, HITL-vs-AFK tag, blocked-on-external, spec-before-code, supersession-close, mega-ticket compile. See handbook.
2. **Score.** Dispatch threshold is `score >= 5`. Formula and weights in handbook. `autonomy_confidence` is the central variable. It must be anchored to substrate evidence (closed similar tickets, prior autonomous-blocks, this repo's AUTONOMY.md). If you cannot write a one-sentence justification citing one of those, score it 1.
3. **Emit outputs in order:**
   1. Rehoming, dedup, and stale-close proposals.
   2. Structural-context asks (literal interrogative sentences). Each ask carries the `structural-ask` label as a draft issue or as a comment on the originating ticket.
   3. Blocked-on-Kai list.
   4. Per-repo dispatch prompts. Cite substrate evidence in each prompt's per-ticket context line ("Based on #87 closed by similar dispatch on 2026-04-22, this should be AFK-friendly").
   5. Deferred list.
4. **Pause after structural-context asks** so Kai can answer. Re-score affected tickets before emitting per-repo prompts.

## Hard rules

* **Persistence in substrate only.** No new state in repo-recall's local DB. Dispatch outcomes get derived from closed issues plus their commits. Structural-context asks land as github issues with the `structural-ask` label, closed with the answer. AFK priors get rolled up on demand from those closed issues.
* **Org-level writes are not agent-authorized.** Repo creation, ruleset edits past the canonical baseline, loosening any coily deny rule, removing audit, adding allow rules, mass-closes outside an explicitly-enumerated approved list. Surface these as Kai action items, do not work around.
* **No auto-moves, no auto-closes.** Rehomings, dedups, and stale-closes are proposals that wait for Kai approval. Private-to-public moves require a leak-check pass before the proposal lands. Default to "do not move" if any signal is ambiguous.
* **Privileged-ops tickets are HITL.** AWS writes, social posts, brew-release rides, coily lockdown changes. Excluded from dispatch regardless of score.
* **Decision recording is part of the run.** When Kai answers a structural-context ask, close the originating ticket with the decision (or propose a tracker issue / AGENTS.md edit). The next run must not re-ask.
* **Dispatched prompts inherit the git workflow.** Commit to main, push after each commit, every commit closes its issue with `closes #N`, never `--no-verify`.
* **Blocked recovery is in-band.** Spawned sessions file `autonomous-block: <short description>` issues with the failing reference, failure mechanism, and unblock condition, then move on. Do not stop the run on a single block. These issues then re-enter the substrate via `recall_action_required` for the next run to address.

See `references/handbook.md` for the full mechanics.
