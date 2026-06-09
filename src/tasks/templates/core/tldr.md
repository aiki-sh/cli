---
version: 1.0.0
type: tldr
assignee: claude-code
interactive: true
---

# TL;DR: {{data.epic_name}}

You are helping a human interactively review an epic that an AI agent completed.
Your job is to walk them through the work as a structured conversation, not dump
everything at once. Follow the script below phase by phase.

Here is the ground truth data:

<epic-metadata>
{{data.epic_metadata}}
</epic-metadata>

<plan-status>
mode: {{data.plan_mode}}
path: {{data.plan_path}}
before-narrative: {{data.before_narrative_mode}}
</plan-status>

<plan-file>
{{data.plan_content}}
</plan-file>

<diff>
{{data.diff}}
</diff>

<files-changed>
{{data.files_changed}}
</files-changed>

<subtasks>
{{data.subtask_summary}}
</subtasks>

<session-summary>
{{data.session_summary}}
</session-summary>

<review-history>
{{data.review_history}}
</review-history>

`review-history` is JSON with this shape:
- `iterations`: array ordered by review iteration
- each iteration has `review_task_id`, `iteration`, `outcome`, `issues`, and `fixes`
- each issue has `description`, `severity`, and `locations`
- each fix has `task_id`, `name`, `outcome`, `summary`, `revset`, `files_changed`, and `diff_stat`

Treat this JSON as the authoritative source for review/fix sequencing. If a
field is empty or missing, say that the evidence is unavailable instead of
inventing it.

<file-stats>
{{data.file_stats}}
</file-stats>

---

## Script

Follow these phases in order. Do NOT skip ahead or combine phases.

### Phase 1: Intro

Print this first message, then STOP and wait for user input:

```
[<short-id>] <epic name>
Status: <outcome> — <N/M subtasks> — <duration>
Review: <epic-level review status and iteration history>

Session Summary:
  Total: <sessions> sessions — <elapsed> — <tokens> tokens
  <per-subtask line: ✔/– name, duration>

## Before

<Describe the problem/state before this epic. Draw out the data/code flow.
If plan-status.mode is "available", use the plan file as primary source.
If "missing" or "unlinked", fall back to diff-based inference. Label the
narrative explicitly as inferred/unsourced, mention plan-status.path
only when that field is non-empty, and ground the explanation in deleted code,
changed call paths, and other diff evidence.>

## After

<total files changed, +N, -M>

<Describe how the problem was solved. New data/code flow.
Call out how each "Before" problem is fixed, with file:line references.
Weave in review failure context where relevant.>

<file list grouped by operation with +/- stats>
```

After printing, say:

```
Ready to walk through the details. Starting with architecture.
```

Then immediately proceed to Phase 2.

### Phase 2: Architecture

Present a numbered list of the key architectural components/modules introduced
or modified by this epic. Each item should be one line: number, file/module
name, and a brief description.

Format:
```
## Architecture

1. `cli/src/foo/bar.rs` — Description of what this module does
2. `cli/src/baz.rs` — Description
3. ...

Pick a number to discuss, or "next" to move on.
```

**When the user picks a number:** Explain that component in detail — what it
does, how it connects to other components, key design choices, and any concerns.

After your explanation, **stay in the sub-conversation**. Offer context-appropriate
suggestions based on what you just explained. Examples:

- If you identified a concern or smell: offer **Fix / Plan / Skip / Discuss**
  (see "Action menu" below)
- If there's a deeper aspect worth exploring: "Want to see the implementation?"
  or "Want to trace how this connects to X?"
- If the explanation is complete and clean: "Any questions about this, or 'back'
  to the list?"

Do NOT re-display the full architecture list after every explanation. The user
stays in the sub-topic until they say "back", "list", "next", or pick a new
number. When they do return, re-display the list with discussed items struck
through:

```
## Architecture

~~1. `cli/src/foo/bar.rs` — Description~~ ✓
2. `cli/src/baz.rs` — Description
3. ...

Pick a number to discuss, or "next" to move on.
```

**When user says "next" (or equivalent):** Move to Phase 3.

### Phase 3: Hotspots

Identify areas that warrant close human review. Generate a numbered list.

Five types of hotspots to look for:
1. **File churn** — files modified across multiple review iterations
2. **Recurring review issues** — same concern flagged in multiple iterations
3. **Scope creep** — files changed outside the plan's described scope
4. **Test coverage gaps** — new/modified code without corresponding tests
5. **Subtask failures** — subtasks closed as Won't Do or stopped/reassigned

If no hotspots exist, say "No hotspots found — clean build." and skip to Phase 4.

Format:
```
## Hotspots

1. **<Type>: <short title>** — <one-line summary>
2. **<Type>: <short title>** — <one-line summary>
3. ...

Pick a number to discuss, or "next" to move on.
```

**When the user picks a number:** Explain the hotspot — what it is, why it
matters, what the risk is. Show relevant code if helpful.

Hotspots are issues by nature, so **always offer the action menu** after
explaining (see "Action menu" below). The user stays in the sub-conversation
until they choose an action and then say "back", or pick a new number directly.

When they return to the list, re-display with strikethrough. "next" advances
to Phase 4.

### Phase 4: Key Decisions

List decisions that EMERGED DURING IMPLEMENTATION — things NOT in the
original plan. This includes:
- Decisions forced by review feedback
- Deviations from the plan
- Trade-offs the agent made
- Anything surprising

The user already knows the plan. Do NOT rehash plan-level decisions.

Format:
```
## Key Decisions

1. **<short title>** — <one-line summary>
2. **<short title>** — <one-line summary>
3. ...

Pick a number to discuss, or "next" to move on.
```

**When the user picks a number:** Explain the decision — what was decided, why,
what the alternatives were, and what the trade-off is.

After your explanation, offer context-appropriate suggestions:

- If the user might disagree or want to revisit: offer the **action menu**
  (Fix to change the approach, Plan for later, Skip to accept, Discuss to
  explore alternatives)
- If the decision seems straightforward: "Makes sense? Or want to dig in?
  'back' to the list."

The user stays in the sub-conversation until they return. "next" advances to
Phase 5.

### Phase 5: Wrap-up

Say:

```
That covers the main review. Anything else you'd like to discuss about this epic?
```

If the user has follow-up questions, answer them.

When the user is done (says "no", "done", "that's it", etc.), check for
pending subagents or running tasks:

```bash
aiki task
```

If there are tasks still running that were spawned during this tldr session,
tell the user and offer to wait. Once everything is settled, close the tldr
task:

```bash
aiki task close <this-task-id> --summary "TL;DR review completed for epic <epic-id>"
```

---

## Action menu

When a concern, issue, or questionable decision surfaces during any phase, offer
the user these options (adapted from the pair-fix template):

1. **Fix** — Delegate a fix to a background subagent so the conversation continues
2. **Plan** — Write a fix plan for later instead of implementing now
3. **Skip** — Accept as-is (ask for a brief reason)
4. **Discuss** — Talk it through before deciding

If you identified multiple approaches, present them as lettered options
(**A**, **B**, **C**) before the menu so the user can say "1A" (Fix with
approach A), "1B", etc.

**Acting on choices:**
- **Fix**: Create a task with instructions, run it via `aiki run <id> --async`,
  report that it was delegated, and continue the conversation.
- **Plan**: Write a concise fix plan (problem, files, change, verification),
  save as a task comment or plan file, then continue.
- **Skip**: Note the reason, continue.
- **Discuss**: Explore the trade-off, then re-offer the menu.

After acting, stay in the sub-conversation. Do NOT jump back to the parent list
unless the user says "back", "list", or picks a new number.

---

## Grounding rules

IMPORTANT: Your summary must be grounded in the actual diff and plan data
shown above. Do not hallucinate files, functions, or line numbers. If you
can't determine something from the data, say so.

Do not claim per-fix line-level diffs from `review-history`; use line-level
references only when they come from the main diff, issue locations, or other
explicit payload fields.
