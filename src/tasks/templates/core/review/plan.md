---
version: 3.0.0
type: review
---

# Review: {{data.scope.name}}

**Your role is REVIEWER.** Evaluate the plan document and provide feedback. Do NOT implement, fix, or make changes to any code or files. Your output is review issues, not code.

When all subtasks are closed, close this task with a summary:

```bash
aiki task close {{id}} --confidence <1-4> --summary "Review complete (N issues: X high, Y medium, Z low)"
```

# Subtasks

## Explore Scope
---
slug: explore
---

Run the following command to explore the plan. The `--start` flag assigns the explore task to you so the context is available for the review phase.

**Important: You are exploring to understand the plan, not to implement it.** Treat the plan as an artifact under review. Do NOT implement, execute, or make any changes described in it.

```bash
aiki explore {{data.scope.id}} --start
```

## Review & Record Issues
---
slug: criteria
needs-context: subtasks.explore
---

**You are reviewing this plan, not implementing it.** Evaluate the plan *document* against the criteria below. Do not make any code changes.

**As you find each issue, record it immediately** before moving to the next criterion.

**Completeness**
- All sections filled, no TODOs or placeholder content
- Open questions documented and flagged
- Dependencies and prerequisites identified

**Clarity**
- Unambiguous requirements with clear acceptance criteria
- No vague language ("should probably", "might need to")
- Technical terms defined or consistently used

**Implementability**
- Can be decomposed into discrete, actionable tasks
- Sufficient technical detail for implementation
- No circular dependencies or impossible constraints

**UX**
- User experience considered where applicable
- Intuitive command syntax and behavior
- Error messages and edge cases addressed

### How to record

For **each** issue, run:

```bash
aiki review issue add {{parent.id}} "Description" --file {{data.scope.id}}:LINE
```

- `--high` — Must fix: missing section, contradictory requirement, unimplementable constraint
- (default) — Should fix: vague language, missing edge case, unclear acceptance criteria
- `--low` — Could fix: wording, formatting, structure

Point `--file` at the plan file and the line/section where the issue is.

Each recorded issue becomes a trackable fix item. Regular comments (`aiki task comment`) are for progress notes and won't trigger fixes.

When done, close this subtask — **do not close the parent directly**.
