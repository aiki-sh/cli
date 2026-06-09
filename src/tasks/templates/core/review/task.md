---
version: 3.0.0
type: review
---

# Review: {{data.scope.name}}

**Your role is REVIEWER.** Evaluate and provide feedback on the implementation. Do NOT fix or make changes to any code being reviewed. Your output is review issues, not code.

When all subtasks are closed, close this task with a summary:

```bash
aiki task close {{id}} --confidence <1-4> --summary "Review complete (N issues: X high, Y medium, Z low)"
```

# Subtasks

## Explore Scope
---
slug: explore
---

Run the following command to explore the task's changes. The `--start` flag assigns the explore task to you so the context is available for the review phase.

**Important: You are exploring to understand the changes, not to modify them.**

```bash
aiki explore {{data.scope.id}} --start
```

Then check for prior exclusions (issues closed as won't-do in earlier rounds):

```bash
aiki task list --wont-do --descendant-of {{data.scope.id}}
```

If any won't-do tasks are listed, note them — these are explicitly out of scope.

When the explore is complete, close this subtask with a summary of what you found and your confidence in your understanding of the task.

## Review & Record Issues
---
slug: criteria
needs-context: subtasks.explore
---

**Before recording an issue**, check the won't-do list from the explore phase.
Do NOT re-raise issues that were previously closed as won't-do unless you have
new evidence that the original dismissal was wrong.

Evaluate the implementation against the criteria below. **As you find each issue, record it immediately** before moving to the next criterion.

**Plan Coverage**
- All requirements from the plan exist in the codebase
- No missing features or unimplemented sections
- No scope creep beyond what the plan describes

**Code Quality**
- Logic errors, incorrect assumptions, edge cases
- Error handling and resource management
- Code clarity and maintainability

**Security**
- Injection vulnerabilities (command, SQL, XSS)
- Authentication and authorization issues
- Data exposure or crypto misuse

**Plan Alignment**
- UX matches plan design (commands, flags, output format)
- Architecture follows plan's prescribed approach
- Acceptance criteria from plan are met

### How to record

For **each** issue, run:

```bash
aiki review issue add {{parent.id}} "Description" --file path/to/file.rs:42
```

- `--high` — Must fix: incorrect behavior, bug, or contract violation
- (default) — Should fix: suboptimal, missing, or inconsistent (no flag needed)
- `--low` — Could fix: style, naming, cosmetic

**Location** (`--file`, repeatable):
- `--file src/auth.rs` — file only
- `--file src/auth.rs:42` — file and line
- `--file src/auth.rs:42-50` — file and line range
- `--file src/a.rs:10 --file src/b.rs:20` — multiple files

Each recorded issue becomes a trackable fix item. Regular comments (`aiki task comment`) are for progress notes and won't trigger fixes.

When done, close this subtask — **do not close the parent directly**.
