# tooling-autonomous-pickup: details

Extended notes for the [`tooling-autonomous-pickup`](SKILL.md) skill.

## Mechanics

1. Run the helper to enumerate unhandled artifacts (those without a `.handled` sidecar):

   ```sh
   python3 /Users/kai/projects/coilysiren/agentic-os-kai/.agents/skills/tooling-autonomous-pickup/script.py scan
   ```

   Output is JSON `{root, count, items: [{path, repo_slug, slug, task_id, description, issue_refs, tracking_issue, prompt}, ...]}`. If `count` is zero, report and stop.

2. For each item, call `mcp__scheduled-tasks__create_scheduled_task` with:

   - `taskId`: item's `task_id` (already kebab-sanitized + `pickup-`-prefixed).
   - `prompt`: item's `prompt` verbatim. The artifact already carries the standard workflow footer. Do not add a second footer; do not edit the prompt.
   - `description`: item's `description` (already 120-char truncated).
   - `fireAt`: ISO 8601 timestamp ~60s in the future, local time with explicit offset. One-shot. No `cronExpression`. Compute fresh per call so cards stagger.
   - `notifyOnCompletion`: omit (default true is correct).

   If the MCP call errors (duplicate taskId, schema reject, transport error), surface it and skip step 3 for that item. Do NOT mark the artifact handled.

3. After the MCP call succeeds, mark handled:

   ```sh
   python3 /Users/kai/projects/coilysiren/agentic-os-kai/.agents/skills/tooling-autonomous-pickup/script.py mark-handled <path> --note "<taskId> at <ISO timestamp>"
   ```

   Sidecar is `<path>.handled`. The repo-local mirror at `<repo>/docs/repo-dispatch/<slug>.md` is substrate-of-record.

4. Report a one-line summary: count picked up, skipped, errored.

## Output shape (chat)

```
Picked up 3 dispatches from ~/.repo-recall/dispatch/:
  - pickup-repo-recall-2026-05-13-92-abc1234 (coilysiren/repo-recall#92)
  - pickup-agentic-os-kai-2026-05-13-313-def5678 (coilysiren/agentic-os-kai#313)
  - pickup-eco-mods-2026-05-13-44-aaaa999 (coilysiren/eco-mods#44)
Start-locally cards should appear in Desktop within ~60s.
```

Errors get their own line per item with the MCP error message verbatim.

## Hard rules

- **Don't edit prompts.** The artifact body is what `recall-dispatch` agreed to send.
- **Mark-handled is success-gated.** If the MCP create fails, no sidecar. The next sweep retries.
- **No re-dispatch.** A `.handled` sidecar is permanent for that slug. To re-run, delete the sidecar by hand.
- **Stagger fireAt.** Each task gets its own near-future timestamp.
- **Local time with offset.** `fireAt` MUST be ISO 8601 with explicit TZ offset.
- **Desktop must be open or due-to-open.** Scheduled tasks fire on next launch if Desktop closed.

## Failure modes

- **MCP not available.** Surface error, tell Kai to load via `ToolSearch select:mcp__scheduled-tasks__create_scheduled_task,mcp__scheduled-tasks__list_scheduled_tasks`.
- **Empty queue.** Report and stop. Don't call MCP.
- **Duplicate taskId.** `create_scheduled_task` rejects. Surface verbatim, leave artifact un-handled.

## Related

- [`recall-dispatch`](../../../../repo-recall/.agents/skills/recall-dispatch/SKILL.md) - producer side.
- [`kai-autonomous-engineering`](../kai-autonomous-engineering/SKILL.md) - older long-horizon planner.
- [`kai-coily-dispatch-shorthand`](../kai-coily-dispatch-shorthand/SKILL.md) - synchronous `coily dispatch`.
