---
name: tooling-autonomous-pickup
description: Drain repo-recall's on-disk dispatch queue into Claude Desktop "Start locally" cards. For each unhandled `~/.repo-recall/dispatch/<repo>/*.md`, calls `mcp__scheduled-tasks__create_scheduled_task` with a near-future fireAt, then writes a `.handled` sidecar. Pairs with `recall-dispatch` to close the autonomous-engineering loop. Triggers - autonomous pickup, pickup dispatches, drain dispatch queue, fan out dispatches, run AFK pickup, dispatch queue, lights-out pickup, sweep dispatches.
---

# autonomous-pickup

The pickup side of `recall-dispatch`. The planner writes markdown artifacts to `~/.repo-recall/dispatch/<repo>/<slug>.md`. This skill reads them and creates a cowork-scheduled-task per artifact so Claude Desktop renders a right-side "Start locally" card for each one. Kai clicks each card to spawn a Desktop-registered session seeded with the artifact's prompt.

The skill is the bridge between the on-disk dispatch queue and the Desktop session-manager. Without it, the artifacts pile up and a human has to fan them out by hand. With it, Desktop becomes the queue UI.

## When to run

* After a `recall-dispatch` run completes and Kai wants the queued prompts to surface in Desktop.
* On a recurring cadence (e.g. every 10-15 minutes via a self-perpetuating scheduled-task) so newly-emitted artifacts get picked up without manual triggering.
* When Kai dictates any of the trigger phrasings above.

## Mechanics

1. Run the helper to enumerate unhandled artifacts (those without a `.handled` sidecar):

   ```sh
   python3 /Users/kai/projects/coilysiren/agentic-os-kai/.claude/skills/tooling-autonomous-pickup/script.py scan
   ```

   Output is JSON of shape `{root, count, items: [{path, repo_slug, slug, task_id, description, issue_refs, tracking_issue, prompt}, ...]}`. If `count` is zero, report that and stop.

2. For each item, call `mcp__scheduled-tasks__create_scheduled_task` with:

   * `taskId`: the item's `task_id` (already kebab-sanitized and prefixed with `pickup-`).
   * `prompt`: the item's `prompt` verbatim. The artifact already carries the standard workflow footer (commit to main / closes #N / push / never --no-verify) because `recall-dispatch` emits it. Do not add a second footer; do not edit the prompt.
   * `description`: the item's `description` (already truncated to 120 chars).
   * `fireAt`: an ISO 8601 timestamp ~60 seconds in the future, in local time (e.g. `2026-05-13T10:21:30-07:00`). One-shot. Do not pass `cronExpression`. Compute fresh per call so the Start-locally cards stagger naturally rather than all firing at the same instant.
   * `notifyOnCompletion`: omit (default true is correct - Kai wants the completion ping).

   If the MCP call returns an error (duplicate taskId, schema reject, transport error), surface it and skip step 3 for that item. Do NOT mark the artifact handled - a future run should retry.

3. After the MCP call succeeds, mark the artifact handled so future scans skip it:

   ```sh
   python3 /Users/kai/projects/coilysiren/agentic-os-kai/.claude/skills/tooling-autonomous-pickup/script.py mark-handled <path> --note "<taskId> at <ISO timestamp>"
   ```

   The sidecar is `<path>.handled` and is intentionally outside `.gitignore` consideration because the artifact's repo-local mirror at `<repo>/docs/repo-dispatch/<slug>.md` is the substrate-of-record. The pollable mirror is a working surface.

4. Report a one-line summary: count picked up, count skipped, count errored.

## Output shape (chat)

A short readout after the run:

```
Picked up 3 dispatches from ~/.repo-recall/dispatch/:
  - pickup-repo-recall-2026-05-13-92-abc1234 (coilysiren/repo-recall#92)
  - pickup-agentic-os-kai-2026-05-13-313-def5678 (coilysiren/agentic-os-kai#313)
  - pickup-eco-mods-2026-05-13-44-aaaa999 (coilysiren/eco-mods#44)
Start-locally cards should appear in Desktop within ~60s.
```

Errors get their own line per item with the MCP error message verbatim.

## Hard rules

* **Do not edit prompts.** The artifact body is what `recall-dispatch` agreed to send. Editing here drifts the substrate of record. The only thing this skill adds is the scheduling envelope.
* **Mark-handled is success-gated.** If the MCP create fails, the `.handled` sidecar must not be written. The next sweep retries.
* **No re-dispatch.** A `.handled` sidecar is a permanent commitment for that slug. If Kai wants to re-run an artifact, she deletes the sidecar by hand. Don't expose an unmark verb in this skill.
* **Stagger fireAt.** Each task gets its own near-future timestamp (computed at call time, ~60s out). Setting them all to the same instant produces a stampede of "Start locally" cards.
* **Local time, with offset.** `fireAt` MUST be ISO 8601 with explicit timezone offset (`-07:00` Pacific in winter, `-08:00` in summer). UTC strings work too, but local-with-offset survives DST switches without surprise.
* **Desktop must be open or due-to-open soon.** Scheduled tasks fire when Desktop is open or on next launch. If Desktop has been closed for a while, expect the cards to land in a burst on next launch. That's working as intended.

## Failure modes

* **MCP not available.** If `mcp__scheduled-tasks__create_scheduled_task` is not loaded, surface the error and tell Kai to load it via `ToolSearch select:mcp__scheduled-tasks__create_scheduled_task,mcp__scheduled-tasks__list_scheduled_tasks` and re-run.
* **Empty queue.** If `count == 0`, report that and stop. Do not call the MCP at all.
* **Duplicate taskId.** `create_scheduled_task` rejects duplicate IDs. If the slug has been picked up before and the sidecar got deleted, the retry will collide. Surface the error verbatim and leave the artifact un-handled so Kai sees the duplicate signal.

## Related skills

* [`recall-dispatch`](../../../../repo-recall/.claude/skills/recall-dispatch/SKILL.md) - the producer side, lives in `repo-recall` because that's where the substrate it queries lives.
* [`kai-autonomous-engineering`](../kai-autonomous-engineering/SKILL.md) - the older long-horizon planner. `recall-dispatch` is its substrate-aware successor.
* [`kai-coily-dispatch-shorthand`](../kai-coily-dispatch-shorthand/SKILL.md) - hands-on `coily dispatch` path for one-off dispatches. This skill is the AFK path; coily dispatch is the synchronous path.

## See also

* [coilysiren/repo-recall#125](https://github.com/coilysiren/repo-recall/issues/125) - feature design and acceptance criteria.
* [coilysiren/repo-recall#126](https://github.com/coilysiren/repo-recall/issues/126) - the long-shot research spike to eventually eliminate the in-Desktop step entirely.
* [coilysiren/agentic-os-kai#383](https://github.com/coilysiren/agentic-os-kai/issues/383) - the prior spike where the cowork-scheduled-task path was identified as the right surface.
