---
name: kai-autonomous-engineering
description: Long-horizon autonomous engineering planner. Triages the org backlog, splits oversized tickets, proposes rehoming/dedup/stale-close, scores tickets for AFK-ability, surfaces structural-context asks for Kai, emits per-repo copy-pastable prompts ordered to maximize work without intervention. Aliases - autonomous engineering, AFK queue, lights-out queue, autonomous dispatch, AFK dispatch, plan the AFK run, queue autonomous work, run the lights-out factory, what should the bots work on.
---

# Autonomous Engineering

Plan a long autonomous engineering session. Triage the org backlog, score every ticket for AFK-ability, surface what Kai needs to decide before kickoff, then emit one copy-pastable prompt per repo that a fresh Claude session can execute without further input.

The skill assumes Kai will copy each per-repo prompt into a fresh session (phone or desktop). The skill itself does not spawn agents. Output is text Kai dispatches.

The detailed mechanics (triage decisions, scoring math, output blocks, anti-patterns, decision recording, dispatch ordering) live in `references/handbook.md`. Read it before producing output.

## Preflight

1. Confirm `daily-backlog` ran in the last 24h. If not, run it first. The backlog routine is the upstream truth for what's on the board.
2. cd into a real git repo before any `coily ops gh` call. Coily binds every audit row to a commit scope; `agentic-os-kai/` is a safe default cwd. Outside a repo, coily errors with `scope: cwd is not inside a git repo`.
3. Pull priorities from the project board (priorities live as a project field, not as issue labels):
   ```
   coily ops gh project item-list 2 --owner coilysiren --format json --limit 400
   ```
   Each open item has `.priority` (P0..P4), `.status`, `.content.repository`, `.content.number`, `.content.title`. Filter to `select(.status != "Done" and .content.type == "Issue")`.
4. Defer per-repo `git status` checks until prompt-assembly time. Walking 19 repos upfront wastes context for low signal; check a repo only when you're about to emit its dispatch block.
5. **Scale check.** If active items > ~100, do not score every ticket. Propose a scope cut to Kai before triage: full pass on P0-P2, title-scan P3 for AFK candidates, bulk-defer P4. Wait for confirmation.

## Phases

1. **Triage** - per-ticket decisions: split, rehome, dedup, stale-close, acceptance-criteria check, HITL-vs-AFK tag, blocked-on-external, spec-before-code, supersession-close, mega-ticket compile. See handbook.
2. **Score** - dispatch threshold is `score >= 5`. Formula and weights in handbook. `autonomy_confidence` is the load-bearing variable; if you cannot write a one-sentence justification, score it 1.
3. **Emit outputs in order**:
   1. Rehoming, dedup, and stale-close proposals.
   2. Structural-context asks (literal interrogative sentences, not topic labels).
   3. Blocked-on-Kai list.
   4. Per-repo dispatch prompts.
   5. Deferred list.
4. **Pause after structural-context asks** so Kai can answer. Re-score affected tickets before emitting per-repo prompts.

## Hard rules

* **Org-level writes are not agent-authorized.** Repo creation, ruleset edits past the canonical baseline, loosening any coily deny rule, removing audit, adding allow rules, mass-closes outside an explicitly-enumerated approved list. Surface these as Kai action items, do not work around.
* **No auto-moves, no auto-closes.** Rehomings, dedups, and stale-closes are proposals that wait for Kai approval. Private-to-public moves require a leak-check pass before the proposal lands; default to "do not move" if any signal is ambiguous.
* **Privileged-ops tickets are HITL.** AWS writes, social posts, brew-release rides, coily lockdown changes. Excluded from dispatch regardless of score.
* **Decision recording is part of the run.** When Kai answers a structural-context ask, land the answer durably in the same pass (close the originating ticket with the decision, or propose a tracker issue / AGENTS.md edit). The next run must not re-ask.
* **Dispatched prompts inherit the git workflow.** Commit to main, push after each commit, every commit closes its issue with `closes #N`, never `--no-verify`.
* **Blocked recovery is in-band.** Spawned sessions file `autonomous-block: <short description>` issues with the failing reference, failure mechanism, and unblock condition, then move on. Do not stop the run on a single block.

See `references/handbook.md` for the full mechanics.
