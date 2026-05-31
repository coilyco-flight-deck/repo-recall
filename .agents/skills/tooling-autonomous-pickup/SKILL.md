---
name: tooling-autonomous-pickup
description: Drain repo-recall's on-disk dispatch queue into Claude Desktop "Start locally" cards. For each unhandled `~/.repo-recall/dispatch/<repo>/*.md`, calls `mcp__scheduled-tasks__create_scheduled_task` with a near-future fireAt, then writes a `.handled` sidecar. Pairs with `recall-dispatch` to close the autonomous-engineering loop. Triggers - autonomous pickup, pickup dispatches, drain dispatch queue, fan out dispatches, run AFK pickup, dispatch queue, lights-out pickup, sweep dispatches.
---

# autonomous-pickup

The pickup side of `recall-dispatch`. The planner writes markdown artifacts to `~/.repo-recall/dispatch/<repo>/<slug>.md`. This skill reads them and creates one cowork-scheduled-task per artifact so Claude Desktop renders a right-side "Start locally" card. Kai clicks each card to spawn a Desktop session seeded with the artifact's prompt.

The skill is the bridge between the on-disk dispatch queue and the Desktop session-manager. Without it, artifacts pile up and a human fans them out by hand. With it, Desktop becomes the queue UI.

## When to run

- After a `recall-dispatch` run completes and Kai wants queued prompts to surface in Desktop.
- On a recurring cadence (every 10-15 minutes via a self-perpetuating scheduled-task) so new artifacts get picked up.
- When Kai dictates any trigger phrasing above.

## How

See [`details.md`](details.md) for the full mechanics, hard rules, output shape, failure modes, and related skills.

Summary of the loop:

1. `script.py scan` → JSON of unhandled artifacts.
2. Per item, call `mcp__scheduled-tasks__create_scheduled_task` with `taskId`, verbatim `prompt`, staggered `fireAt`.
3. On success, `script.py mark-handled <path>` writes the sidecar.
4. Print one-line summary.

## See also

- [coilysiren/repo-recall#125](https://github.com/coilyco-flight-deck/repo-recall/issues/125) - feature design.
- [coilysiren/repo-recall#126](https://github.com/coilyco-flight-deck/repo-recall/issues/126) - long-shot to eliminate the in-Desktop step entirely.
- [coilysiren/agentic-os-kai#383](https://github.com/coilyco-bridge/agentic-os-kai/issues/383) - the prior spike.
