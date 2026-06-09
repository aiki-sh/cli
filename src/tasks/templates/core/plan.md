---
version: 1.0.0
type: plan
assignee: claude-code
interactive: true
---

# Plan: {{data.plan_path}}

Guide the user through creating or refining a plan document.

# Instructions

You are helping the user write a plan at `{{data.plan_path}}`.

**User's guidance:** {{data.initial_idea}}

**For new plans:** Work through the subtasks in order (they are defined at the end of this template).

**For existing plans:**
1. Read and summarize current state
2. Identify which phase the plan is in (incomplete sections? unresolved questions?)
3. Mark already-completed subtasks as `wont_do` (e.g., if plan is already drafted, close "Clarify user intent" and "Draft initial plan" as `--wont-do`)
4. Start the appropriate subtask for where work is needed
5. Work through remaining subtasks as needed

**Throughout:**
- Write to the plan file incrementally (don't wait until the end)
- Keep the conversation focused
- Use subtasks to track progress through the plan workflow
- Track open questions as subtasks under "Resolve open questions", NOT in the plan file's "Open Questions" section

Any questions?

## Plan Structure (suggested)

Use this structure for new plans:

    # <Title>

    **Date**: <today>
    **Status**: Draft
    **Purpose**: <one line>

    **Related Documents**:
    - [Related Doc](path/to/doc.md) - Brief description

    ---

    ## Executive Summary

    <2-3 sentences describing what this is and why>

    ---

    ## User Experience

    <If applicable - command syntax and options>

    ---

    ## How It Works

    <Explanation of the mechanism/flow>

    ---

    ## Use Cases

    <Concrete examples of how this is used>

    ---

    ## Implementation Plan

    <Phases and deliverables>

    ---

    ## Error Handling

    <If applicable - error scenarios and handling>

    ---

    ## Open Questions

    1. ...
    2. ...

    ---


# Subtasks

## Clarify user intent and requirements

1. Review any user-provided context above (topic and guidance text)
2. Ask clarifying questions where you have them:
   - What problem does this solve?
   - Who is the user/audience?
   - What are the key requirements?
   - Any constraints or non-goals?
3. If the user already provided detailed guidance, skip questions that are already answered
4. Close subtask when you have enough clarity to draft the plan

## Draft initial plan structure

1. Create the plan file using the template structure
2. Fill out Summary, Requirements, and Open Questions sections
3. Write to the file incrementally as you work
4. Close subtask when initial draft is complete

## Resolve open questions

1. Work through each question subtask with the user
2. Update plan based on answers
3. Close each question subtask when resolved
4. Add any new questions that emerge as additional subtasks
5. Close this subtask when all question subtasks are resolved

## Validate completeness

Evaluate the plan for quality and readiness. For each issue found, add it as a subtask under this validation task.

**Evaluation criteria:**

| Category | What to Check |
|----------|---------------|
| **Completeness** | All sections filled out, no TODOs or placeholders |
| **Clarity** | Requirements unambiguous, clear acceptance criteria |
| **Implementability** | Can be decomposed into tasks, technical details sufficient |
| **UX** | User experience considered, command syntax intuitive, errors defined |

**Workflow:**

1. Read the complete plan
2. Evaluate against each category
3. For each issue found, add a subtask:
   ```bash
   aiki task add --subtask-of {{parent.id}} "<Category>: <brief description>"
   # If the subtask needs detailed instructions for a subagent:
   # aiki task add --subtask-of {{parent.id}} "<description>" -i "detailed instructions here"
   ```
4. Once all issues are identified, work through each subtask with the user:
   - Discuss the issue
   - Update the plan to address it
   - Close the subtask when resolved
5. When all issue subtasks are closed, close this validation subtask
6. If no issues found, close immediately with "Plan validated - no issues found"

## Confirm completion

Ask the user if the plan is complete and ready for implementation.

1. Ask: "Is this plan complete and ready for implementation?"
2. If the user confirms yes:
   - Remove the `draft` field from the plan file's YAML frontmatter
   - Read the file, find the `---` frontmatter block, remove the `draft: true` line, write back
   - If removing `draft` leaves the frontmatter empty, remove the `---` block entirely
   - Close this subtask with `--confidence <1-4>` and summary "Plan marked as ready"
3. If the user wants to continue editing:
   - Leave `draft: true` in place
   - Close this subtask as wont_do with summary "User wants to continue editing, draft status preserved"
