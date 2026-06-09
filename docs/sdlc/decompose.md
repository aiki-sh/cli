# Decompose

`aiki decompose` reads a plan file and creates implementation subtasks under a target task. It's the bridge between planning and execution — turning a high-level plan into actionable work items that agents can complete independently.

## Usage

```bash
# Decompose a plan into subtasks under a target task
aiki decompose ops/now/user-auth.md --target <task-id>

# With a custom template
aiki decompose ops/now/user-auth.md --target <task-id> --template my/decompose

# With a specific agent
aiki decompose ops/now/user-auth.md --target <task-id> --agent codex

# Output just the task ID (for piping)
aiki decompose ops/now/user-auth.md --target <task-id> -o id
```

## How It Works

1. **Validate** — Confirms the target task exists.

2. **Link plan** — Writes an `implements-plan` link from the target task to the plan file (`file:<path>`).

3. **Create decompose task** — Creates a task from the `decompose` template with `data.target` (the parent task ID) and `data.plan` (the plan file path).

4. **Wire provenance** — Writes `decomposes-plan` (decompose task → plan file) and `populated-by` (target → decompose task) links for traceability.

5. **Run the agent** — Executes the decompose task. The agent reads the plan, creates subtasks under the target, and sets up dependencies between them.

## Subtask Dependencies

The decompose agent sets up two types of links between subtasks, which control how `aiki loop` schedules them into parallel lanes:

| Link type | When to use | Effect on execution |
|-----------|-------------|---------------------|
| `--depends-on` | Task B needs A's output (files on disk) but not its session context | B waits for A to finish, then runs in a fresh agent session |
| `--needs-context` | Task B must share A's in-memory understanding | B runs in the same agent session as A (linear chain, max 1 each direction) |
| *(no link)* | Tasks are independent | Tasks run as separate parallel lanes |

### Example dependency graph

```
         ┌─ Frontend ─┐
Plan ──▶ │             ├──▶ Tests
         └─ Backend  ──┘
```

- Frontend and Backend both `--depends-on` Plan → they fan out into parallel lanes
- Tests `--depends-on` both Frontend and Backend → it fans in and waits for both

### `needs-context` chains

Tasks linked with `--needs-context` form linear chains that execute in a single agent session. This is useful when the second task needs the first task's in-memory codebase understanding (e.g., explore → implement).

```bash
EXPLORE=$(aiki task add "Explore auth module" --subtask-of $EPIC --output id)
IMPL=$(aiki task add "Implement auth changes" --subtask-of $EPIC --needs-context $EXPLORE --output id)
```

The loop orchestrator will run both tasks in one session, preserving context between them.

## Options

| Flag | Effect |
|------|--------|
| `--target <id>` | **(required)** Parent task ID to create subtasks under |
| `--template <name>` | Decompose template (default: `decompose`) |
| `--agent <type>` | Agent for decomposition (default: `claude-code`) |
| `-o id` | Output bare task ID to stdout |

## Internal Use

`decompose` is called internally by both `build` and `fix`:

- **`aiki build`** calls `decompose` after creating an epic from the plan
- **`aiki fix`** calls `decompose` after creating a fix plan from review issues

You can also call it directly when you have an existing parent task and want to populate it with subtasks from a plan file.

## Provenance Links

Decompose creates several links for traceability:

| Link | From | To | Purpose |
|------|------|----|---------|
| `implements-plan` | target task | `file:<plan>` | Tracks which plan a task implements |
| `decomposes-plan` | decompose task | `file:<plan>` | Tracks which plan was decomposed |
| `populated-by` | target task | decompose task | Tracks who created the subtasks |
