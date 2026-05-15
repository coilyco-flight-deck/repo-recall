# Autonomous Engineering Handbook

Detailed mechanics for the `kai-autonomous-engineering` skill. SKILL.md is the entrypoint and overview; this file holds the full triage, scoring, and output specs.

## Triage phase

Walk every active ticket. Per ticket, decide:

* **Split** - if the ticket is larger than one tracer-bullet vertical slice, propose a split into independently-grabbable issues. Add priorities to each child. Defer to the `to-issues` skill conventions.
* **Rehome** - if the ticket lives in the wrong repo, propose a move. Source repo, target repo, direction.
  - **Public to public** - safe to move with approval.
  - **Private to public** - run the leak-check pass on the body (and any referenced file paths) before proposing. Surface result alongside the proposal. Default to "do not move" if any signal is ambiguous. Refer to agentic-os-kai's `scripts/leak-check.py` denylist conventions.
  - **Public to private** - safe, do silently in the proposal list (still requires Kai approval before the move).
* **Dedup** - cross-repo title/body similarity. Flag candidates for close-as-dup with a link to the canonical issue. Do not auto-close.
* **Stale-close** - issues older than 90 days, no comments in 60 days, no commits referencing them. Flag, do not close.
* **Acceptance criteria check** - if no clear "done when" is present, flag the ticket for either splitting, hydration, or a one-line clarifying ask to Kai. This is the single biggest predictor of mid-run blocking. Do not score these as high-autonomy until criteria are clear.
* **HITL vs AFK tag** - tag HITL when the ticket requires Kai's eyes (UI verification she has to look at, taste calls, social posts, anything touching coily-gated ops, AWS writes, brew release ride, destructive infra). HITL tickets are excluded from the autonomous dispatch list regardless of score. Surface separately.
* **Blocked-on-external** - waiting on upstream PR, vendor reply, brew pipeline, etc. Surface with the unblock condition named. Excluded from dispatch.
* **Spec-before-code** - if the right next step is a refactor RFC or paragraph spec rather than code, route the ticket to `request-refactor-plan` or kick to Kai. Do not dispatch.
* **Supersession-close** - if a ticket has been made redundant by a skill, repo, or feature that landed after the ticket was filed (e.g. "go look for new skills" tickets after `capability-scout` ships), propose closure with the supersession reason. Each supersession-close set is its own enumerated proposal block in the output, not folded into a prior approval. List explicit issue numbers; do not bulk-close from a vague Kai-direction.
* **Mega-ticket compile** - if scattered context across N issues should consolidate into one home (e.g. a new repo just got created, or a tier menu lives across three issues), propose compiling the relevant slices into one organized parent ticket in the canonical home. Light commentary, literal data.

## Scoring

For every ticket that survives triage (not HITL, not blocked-on-external, not spec-before-code):

```
score =
  (4 - priority)                        # P4 = 0, P0 = 4
  + autonomy_confidence                 # 1 = low, 2 = medium, 3 = high
  + structural_context_bonus            # +2 if a durable answer from Kai would lift autonomy_confidence
  + acceptance_criteria_clarity         # +1 if "done when" is explicit
  + batch_bonus                         # +1 if 2+ sibling tickets in the same repo touch overlapping files
  + recency_bonus                       # +2 if updatedAt within 7 days, +1 within 30 days, else 0
  - privileged_ops_penalty              # -2 if requires coily-gated op, AWS write, social post, brew release wait
  - external_block_penalty              # -2 if waiting on upstream / vendor / person
  - ui_verification_penalty             # -1 if requires Kai to eyeball a UI to confirm
  - speculative_penalty                 # -1 if speculative (see below)
```

`autonomy_confidence` is the load-bearing variable. Be honest. Per ticket, write one sentence: "I could do this autonomously because X" or "I'd block at Y." If you can't write that sentence, score it 1.

`speculative_penalty` applies to any ticket carrying the `speculative` label, plus tickets framed as "we might want to ...", "explore whether ...", "investigate if ...", or otherwise lacking a concrete user-visible problem or motivating incident. The label is authoritative; the framing heuristics catch unlabeled cases. The work might be valuable but the value isn't proven, so a confirmed-real ticket of equal autonomy should ship first. Does not apply to a speculative ticket that is also the cheapest tracer-bullet on a larger validated effort.

`recency_bonus` keys on `updatedAt` (last activity: last comment, last edit, last cross-reference), not `createdAt`. Rationale: `autonomy_confidence` is downstream of correctly modeling Kai's current mental model, and the freshest proxy for that model is recent ticket activity. A ticket Kai touched this week is more likely to reflect what she actually thinks now than one that has been sitting untouched for a year. An old ticket comes back to full strength as soon as Kai re-engages with it. Note: this can compound with stale-close triage in opposite directions, which is fine. An old, untouched ticket gets flagged for stale-close (0 recency bonus, and a triage flag); an old ticket Kai commented on yesterday is alive (+2 recency, no stale-close flag).

`structural_context_bonus` triggers a corresponding entry in the structural-context asks output. Tag each ask with which tickets it would lift, and by how much.

Threshold for dispatch: **score >= 5**.

## Outputs

Emit these in order. All text, copy-pastable, no escaping required for phone use.

### 1. Rehoming, dedup, and stale-close proposals

Awaits Kai approval before any move or close. One block per category. Format per item:

```
* <issue url> - <action> - <one-line reason>
  leak-check: <pass | flagged: ...>   # only on private-to-public moves
```

### 2. Structural-context asks

The list of durable, cross-cutting things Kai could answer that would lift autonomy_confidence on one or more tickets.

**Each ask must be a literal interrogative sentence**, not a topic label. "Threat model for o2r" is a topic; "Who's trusted: the relay operator, each tenant agent, the OTel backend, or the wire-format consumer? What are you defending against?" is an ask. When useful, attach explicit multiple-choice options (a/b/c/d) so Kai can answer with a letter and a sentence.

Format per ask:

```
* <literal question, with multiple-choice options where applicable>
  lifts: <issue url> (+N), <issue url> (+N), ...
```

Pause here for Kai to answer. After answers land, re-score affected tickets and re-emit downstream sections.

### 3. Blocked-on-Kai list

Tickets that need a Kai decision before any work happens. One-line ask each, with the ticket link.

### 4. Per-repo dispatch prompts

For every repo that has at least one ticket scoring >= 5, emit one prompt block in the shape below. Order tickets within the prompt to maximize completed work without blocking: cheap and independent first to bank wins, group by file overlap so context carries, defer dep bumps and cross-cutting changes to last.

```
coilysiren/<repo-name>

<github description verbatim>

Load <repo>/AGENTS.md before starting. Git workflow: commit to main, push after each commit, every commit closes its issue with `closes #N`. Never use --no-verify.

Work these tickets in this order. The order maximizes your chance of completing work without becoming blocked.

1. <ticket title> - <gh issue url> - <one sentence of useful context not already in the ticket>
2. <ticket title> - <gh issue url> - <one sentence of useful context not already in the ticket>
...

You are operating under the autonomous-engineering skill. Get as much of this list done as you can without my input.

If you become critically blocked on a ticket and cannot continue, open a new issue in this repo via `coily gh issue create` titled `autonomous-block: <short description>` with:
- the failing ticket reference
- the failure mechanism (what specifically broke or what decision you needed)
- the unblock condition (what you'd need from Kai to resume)
Then move to the next ticket. Do not stop the run.

Repo no-go zones for this run:
<list any per-repo restrictions, e.g. "no SSM writes without coily wrapper", "no social posts", "no coily lockdown changes". Omit section if none.>
```

### 5. Deferred list

Every ticket scoring < 5, with a one-line reason. Footer of the output. So nothing silently disappears and Kai can override.

## Decision recording

When Kai answers a structural-context ask, the answer must land somewhere durable in the same pass:

- **Originating ticket exists** - close it with the decision in the comment body (and create a follow-up ticket if implementation work remains and the original framing was off-shape).
- **No originating ticket** - propose either a new tracker issue (e.g. "Decision record: <topic>") or an AGENTS.md edit, depending on scope. Cross-cutting policies belong in AGENTS.md; per-repo decisions belong in the repo's tracker.

The point is to make sure the next autonomous-engineering run does not re-ask the same question because the answer evaporated into chat.

## Org-level operations are not agent-authorized

Some unblocking steps require Kai's hand:

- Repo creation under `coilysiren/`.
- Branch protection / ruleset edits past the canonical baseline.
- Loosening any coily deny rule, removing audit, adding allow rules.
- Mass-closing issues outside an explicitly-enumerated, explicitly-approved list.

When the dispatch needs one of these, surface it as a Kai action item in the blocked-on-Kai list. Do not attempt a workaround.

## Dispatch ordering within a repo

The order inside each per-repo prompt matters. Pick the order that maximizes completed-work-before-block:

1. **Cheap, independent, fully-specified first.** Banks wins, warms cache.
2. **Group by file overlap.** Touching the same area twice in a row keeps the mental model warm and avoids re-discovery.
3. **Defer shared-dep bumps and cross-cutting refactors to last.** They invalidate context for siblings.
4. **Defer anything UI-verification-adjacent to last.** If it blocks waiting for Kai, it should not strand other work.

## Per-repo prompt enrichment

Before emitting a per-repo prompt, do enough lookup per ticket to write the "one sentence of useful context not already in the ticket." Useful sources:

- The repo's AGENTS.md for relevant conventions.
- Recent commits referencing nearby files.
- Linked or back-referenced issues.
- Sibling tickets in the same prompt (call out batching opportunities).

The hydration sentence should change the agent's first move. If the only sentence you can write is "this is the ticket body," skip it.

## Blocked recovery

The per-repo prompts already instruct fresh sessions to file an `autonomous-block: ...` issue and continue. After Kai runs the dispatch, the next pass of `daily-backlog` will surface these blocks as fresh issues, which feed back into the next autonomous-engineering run.

## Anti-patterns

* **Do not auto-move tickets.** All rehomings wait for Kai approval, especially private-to-public.
* **Do not auto-close.** Dedup and stale-close are proposals.
* **Do not score privileged-ops tickets high.** A `coily lockdown` change, an AWS write, a social post, or a brew-release ride is HITL even if technically simple. The privileged_ops_penalty exists for a reason.
* **Do not bundle UI-verification tickets at the front of a per-repo prompt.** They will block the run.
* **Do not skip the leak-check on private-to-public moves.** Default to "do not move" when in doubt.
* **Do not invent acceptance criteria.** If a ticket lacks a "done when," surface it as a Kai ask, do not guess and dispatch.
* **Do not write `autonomy_confidence = 3` without the one-sentence justification.** If the sentence does not write itself, the score is 1 or 2.
* **Do not emit per-repo prompts before structural-context asks have been answered.** The whole point of the asks is to lift scores before dispatch.
* **Do not include `--no-verify`, `git reset --hard`, or any destructive op in dispatched prompts.** Same git workflow rules apply to spawned sessions.
* **Do not bundle newly-discovered closures into a prior approval.** If you spot a supersession-close set mid-run, surface it as a fresh enumerated proposal block. The harness will (correctly) deny mass-closes against agent-inferred targets, and that denial means stop, not work around.
* **Do not write structural-context asks as topic labels.** Each must be a literal question. If you cannot phrase it as one, it is not yet specific enough to lift any score.
* **Do not score every ticket on a >100-item backlog without a Kai-confirmed scope cut.** Scoring 322 tickets in one pass blows context before dispatch can even land. Phase by priority.
* **Do not assume priorities live in issue labels.** They live as a project-board field. Use the project item-list query in Preflight.
* **Do not attempt org-level writes** (repo create, ruleset edit, mass-close, deny-rule loosening). Surface as Kai action items.
