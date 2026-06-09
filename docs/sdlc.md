# SDLC: Plan, Build, Review, Fix

Aiki provides a structured software development lifecycle for AI-assisted coding. Four commands form a closed feedback loop: you plan what to build, an agent builds it, another agent reviews the output, and issues get fixed automatically.

## The Workflow

```
  plan            build           review            fix
┌────────┐    ┌──────────┐    ┌───────────┐    ┌──────────┐
│ Write a │───▶│ Plan,    │───▶│ Evaluate  │───▶│ Plan,    │
│  plan   │    │decompose,│    │ against   │    │decompose,│──┐
│         │    │& loop    │    │ criteria  │    │& loop    │  │
└────────┘    └──────────┘    └───────────┘    └──────────┘  │
                                    ▲                         │
                                    └─────────────────────────┘
                                       re-review until clean
```

## Phases

### [Plan](sdlc/plan.md)

Interactive session where you and an AI agent collaborate on a specification. The output is a plan file (typically in `ops/now/`) that describes what to build, the requirements, constraints, and implementation approach.

```bash
aiki plan ops/now/user-auth.md
```

### [Build](sdlc/build.md)

Takes a plan file and runs it through the pipeline: `plan` → `decompose` → `loop`. The plan stage creates an epic, decompose breaks it into implementation subtasks, and loop orchestrates their execution via parallel lanes.

```bash
aiki build ops/now/user-auth.md
```

### [Review](sdlc/review.md)

Evaluates code or plans against structured criteria. Reviews track issues as comments that can be piped directly into `fix`. Aiki auto-assigns a different agent than the one that wrote the code.

```bash
aiki review <task-id>
```

### [Fix](sdlc/fix.md)

Reads issues from a review and runs them through a pipeline: `fix` → `decompose` → `loop`. A Rust-driven review loop re-reviews fixes until the code is clean (up to 10 iterations). Use `--once` for a single pass without the review loop.

```bash
aiki fix <review-task-id>
```

### [Decompose](sdlc/decompose.md)

Reads a plan file and creates implementation subtasks under a target task. Each subtask gets detailed instructions and dependency links (`--depends-on`, `--needs-context`) that determine the execution graph. Used internally by `build` and `fix`, but also available standalone.

```bash
aiki decompose ops/now/user-auth.md --target <task-id>
```

### [Loop](sdlc/loop.md)

Orchestrates a parent task's subtasks via parallel lanes. Derives an execution graph from subtask dependencies, runs independent lanes concurrently, and waits for all to complete. Used internally by `build` and `fix`, but also available standalone.

```bash
aiki loop <parent-task-id>
```

### Resolve

`aiki resolve` resolves JJ merge conflicts. It takes a change ID with conflicts, creates a task from the `resolve` template, and runs an agent to resolve the conflicts.

```bash
aiki resolve <change-id>
```

## Composable Stages

Build and fix are pipelines composed from three reusable stages:

### `plan`

Produces a plan file. Two families exist:

- **`plan`** — interactive plan authoring for new features (used by `aiki plan` and `aiki build`)
- **`fix`** — generates a fix plan from review issues (used internally by `aiki fix`)

### [`decompose`](sdlc/decompose.md)

Reads a plan file and creates subtasks under a parent task. Each subtask contains enough context for an agent to complete it independently. Dependencies between subtasks are expressed via links (`--depends-on`, `--needs-context`), which determine the execution graph. See [Decompose](sdlc/decompose.md) for details on dependency types and provenance links.

### [`loop`](sdlc/loop.md)

Orchestrates subtask execution via parallel lanes. Derives lanes from `needs-context` chains and `depends-on` edges, running independent lanes concurrently and respecting ordering constraints. See [Loop](sdlc/loop.md) for details on lane derivation, threads, and failure handling.

**Build pipeline:** `plan` → `decompose` → `loop`
**Fix pipeline:** `fix` → `decompose` → `loop` → review → *(repeat until clean)*

## Pipelines

Commands are composable. Use pipes or flags to chain phases together:

```bash
# Review then fix
aiki review <task-id> | aiki fix

# Build then review
aiki build ops/now/plan.md --review

# Build, review, and auto-fix
aiki build ops/now/plan.md --fix

# Review with auto-fix
aiki review <task-id> --fix
```

## Commands

| Command | Default mode | Flags | Notes |
|---------|-------------|-------|-------|
| `aiki build` | blocking | `--async`, `--review`, `--fix`, `--restart` | Full pipeline: epic → decompose → loop |
| `aiki decompose` | blocking | `--target`, `--template`, `--agent` | Requires `--target` (parent task ID) |
| `aiki loop` | blocking | `--async`, `--loop-template`, `--agent` | Standalone subtask orchestration |
| `aiki review` | blocking | `--async`, `--fix`, `--start` | Auto-assigns a different agent |
| `aiki fix` | blocking (review loop) | `--once`, `--async` | `--once` disables the post-fix review loop |
| `aiki resolve` | blocking | `--async`, `--start` | Merge-conflict resolution |

## Next Steps

- [Plan](sdlc/plan.md) — interactive plan authoring
- [Build](sdlc/build.md) — decomposition and execution
- [Decompose](sdlc/decompose.md) — breaking plans into subtasks
- [Loop](sdlc/loop.md) — parallel lane orchestration
- [Review](sdlc/review.md) — structured evaluation
- [Fix](sdlc/fix.md) — automated remediation
