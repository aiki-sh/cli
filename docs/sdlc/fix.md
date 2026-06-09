# Fix

`aiki fix` reads issues from a review task and creates followup tasks to resolve them. It uses a pipeline architecture (plan → decompose → loop) with an optional Rust-driven quality loop.

## Usage

```bash
# Fix issues from a review
aiki fix <review-task-id>

# Pipeline: review then fix
aiki review <task-id> | aiki fix

# Single-pass fix (no quality loop)
aiki fix <review-task-id> --once

# Run fix in the background
aiki fix <review-task-id> --async
```

## How It Works

1. **Read review issues** — Fix reads the review task's issue comments (added via `aiki review issue add`). Regular comments are ignored.

2. **Plan** — A fix plan is created from the `fix` template, describing what needs to change to address each issue.

3. **Decompose** — The plan is decomposed into subtasks, one per issue, each with enough context for an agent to resolve independently.

4. **Loop** — Subtasks are executed via `loop`, which runs each fix through an agent session.

5. **Link back** — Fix tasks emit `remediates` links back to the review and `fixes` links to the reviewed targets, creating a traceable remediation chain.

If no issues are found, fix outputs "Approved" and exits successfully — the review passed.

## Quality Loop

The Rust-driven quality loop chains review and fix with a two-stage review:

```
Review ──▶ Fix ──▶ Stage 1: Review fix ──▶ Stage 2: Re-review original scope ──▶ Approved
                         │                         │
                         ▼                         ▼
                   (issues?) ──▶ Fix again   (regressions?) ──▶ Fix again
```

Each iteration has two review stages:

1. **Stage 1 — Review the fix.** After subtasks complete, the fix-parent task is reviewed. If the reviewer finds issues with the fix itself (incomplete, incorrect, introduces new bugs), the loop feeds those issues back into another fix cycle.

2. **Stage 2 — Re-review the original scope.** Once the fix-parent review passes, the *original* scope is re-reviewed to catch regressions. If the fixes resolved the original issues but broke something else, the loop feeds the regression issues back into another fix cycle.

Only when both stages pass does fix output "Approved". The loop runs up to 10 iterations before stopping with a warning. Use `--once` to disable the loop entirely and run a single fix pass.

## Fix Targets

Where the fix task gets attached depends on the review scope:

| Review scope | Fix behavior |
|-------------|--------------|
| **Task** | Fix added as subtask of the original task (reopened if closed) |
| **Plan / Code** | Standalone fix task (file-targeted) |
| **Conflict** | Merge-conflict resolution task |

## Options

| Flag | Effect |
|------|--------|
| `--async` | Run in the background |
| `--once` | Single-pass fix (no quality loop) |
| `--autorun` | Auto-start fix when review closes |
| `--template <name>` | Custom plan template (default: `fix`) |
| `--decompose-template <name>` | Custom decompose template (default: `decompose`) |
| `--loop-template <name>` | Custom loop template (default: `loop`) |
| `-r`, `--review` | Enable quality loop review step |
| `--review-template [name]` | Custom review template for quality loop (default: `review`) |
| `--agent <type>` | Override fix agent |
| `-o id` | Output bare task ID to stdout |

## Stdin Piping

`aiki fix` reads a task ID from stdin when no argument is provided, enabling pipeline composition:

```bash
aiki review <task-id> | aiki fix
```

The review command outputs its task ID to stdout when piped, which fix reads as input.
