# Task Links

Links are typed, directed edges between tasks (and sometimes external references). They form the task DAG — a graph that drives blocking, ordering, provenance, and automation.

## Concepts

A link has three parts:

| Part | Description |
|------|-------------|
| **from** | The subject task (always a task ID) |
| **to** | The target (task ID or external reference like `file:design.md`) |
| **kind** | The relationship type (e.g., `blocked-by`, `subtask-of`) |

Links are **directed**: `A --blocked-by--> B` means A is blocked by B, not the other way around. The CLI reads naturally: `aiki task link A --blocked-by B`.

## Creating Links

### Inline on `task add` or `task start`

Most link flags are available directly on `add` and `start`:

```bash
# Create a task blocked by another
aiki task add "Deploy to staging" --blocked-by <test-task-id>

# Quick-start with a dependency
aiki task start "Run integration tests" --depends-on <build-task-id>

# Create a subtask
aiki task add "Fix null check" --subtask-of <parent-id>

# Multiple sources
aiki task add "Implement feature" --sourced-from file:design.md --sourced-from task:abc123

# Link to a plan file
aiki task add "Build auth system" --implements ops/now/auth-plan.md
```

### Explicit `link` command

For adding links after task creation:

```bash
aiki task link <from-id> --<kind> <to>
```

Examples:

```bash
aiki task link B --blocked-by A        # B can't start until A closes
aiki task link A --sourced-from file:design.md  # A came from this design doc
aiki task link child --subtask-of parent
aiki task link review-task --validates impl-task
aiki task link fix-task --remediates review-task
```

Only one link kind flag can be specified per `link` command.

### Removing links

```bash
aiki task unlink <from-id> --<kind> <to>
```

```bash
aiki task unlink B --blocked-by A      # Remove the blocking relationship
aiki task unlink child --subtask-of parent  # Detach from parent
```

## Blocking vs Non-blocking

Links are either **blocking** or **non-blocking**:

- **Blocking links** exclude the `from` task from the ready queue until the `to` task closes. The task shows as blocked in `aiki task` output.
- **Non-blocking links** are informational — they track relationships but don't affect scheduling.

Blocking kinds: `blocked-by`, `depends-on`, `validates`, `remediates`, `needs-context`

Non-blocking kinds: `sourced-from`, `subtask-of`, `implements-plan`, `orchestrates`, `decomposes-plan`, `adds-plan`, `fixes`, `supersedes`, `spawned-by`

See [Link Kinds Reference](kinds.md) for the full list with details.

## Autorun

Blocking links support **autorun** — when the blocker closes, the blocked task automatically starts:

```bash
aiki task add "Deploy" --blocked-by <test-id> --autorun
aiki task link B --depends-on A --autorun
```

Autorun requires a blocking link flag. Non-blocking kinds ignore it.

## Target Resolution

Link targets are resolved flexibly:

| Input | Resolution |
|-------|-----------|
| Full 32-char ID | Used directly |
| Prefix (e.g., `xmry`) | Resolved to matching task (error if ambiguous) |
| `task:xmry...` | Prefix stripped, resolved as task ID |
| `file:path` | Stored as external reference |
| Bare path (e.g., `design.md`) | Prefixed with `file:` for flexible kinds |

**Task-only kinds** (like `blocked-by`, `depends-on`) require targets that resolve to task IDs. External references like `file:path` are rejected.

**Flexible kinds** (like `sourced-from`, `fixes`) accept both task IDs and external references.

## Cardinality

Each kind has cardinality constraints:

- **max_forward** — max links of this kind from a single task. `None` = unlimited, `Some(1)` = single-link (auto-replaces on conflict).
- **max_reverse** — max links of this kind to a single target. Same semantics.

When a single-link kind already has a link and you add another, the old link is automatically removed (auto-replace). For `implements-plan` and `orchestrates`, the old target also gets a `supersedes` link for audit trail.

## Cycle Detection

Blocking and hierarchical kinds (`subtask-of`, plus all `blocks_ready` kinds) are checked for cycles before a link is added. If adding a link would create a cycle, the operation fails with an error.

## Viewing Links

`aiki task show <id>` displays a task's links:

```
Blocked by:
- xmry... — Build project (in_progress)

Blocks:
- kqtu... [open] Deploy to staging

Spawned by: pqrs... — Review auth module

Sources:
- file:ops/now/auth-plan.md
```

## Common Patterns

### Sequential pipeline

```bash
aiki task add "Build"
aiki task add "Test" --depends-on <build-id> --autorun
aiki task add "Deploy" --depends-on <test-id> --autorun
aiki task start <build-id>
# Test auto-starts when Build closes, then Deploy auto-starts when Test closes
```

### Review + fix cycle

```bash
# Review validates the implementation
aiki task add "Review auth" --validates <impl-id>

# Fix remediates the review findings
aiki task add "Fix review issues" --remediates <review-id>
```

### Subtask decomposition

```bash
aiki task add "Implement auth"
aiki task add "Add login endpoint" --subtask-of <parent-id>
aiki task add "Add session management" --subtask-of <parent-id>
aiki task add "Write tests" --subtask-of <parent-id>
```

### Provenance tracking

```bash
# Track where a task came from
aiki task add "Implement X" --sourced-from file:ops/now/design.md
aiki task add "Follow-up fix" --sourced-from task:<review-id>
```
