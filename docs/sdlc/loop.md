# Loop

`aiki loop` orchestrates a parent task's subtasks via parallel lanes. It derives an execution graph from subtask dependencies, schedules independent work concurrently, and runs threads until all lanes complete.

## Usage

```bash
# Orchestrate subtasks of a parent task
aiki loop <parent-task-id>

# Run in the background
aiki loop <parent-task-id> --async

# With a specific agent
aiki loop <parent-task-id> --agent codex

# With a custom template
aiki loop <parent-task-id> --template my/loop

# Output just the loop task ID
aiki loop <parent-task-id> -o id
```

## How It Works

1. **Validate** — Confirms the parent task exists and has subtasks.

2. **Create loop task** — Creates a task from the `loop` template with `data.target` set to the parent task ID.

3. **Wire orchestration link** — Writes an `orchestrates` link from the loop task to the parent (1:1 — each parent has at most one orchestrator).

4. **Derive lanes** — Analyzes the subtask dependency graph to derive parallel lanes (see [Lanes](#lanes) below).

5. **Execute** — The loop agent iterates:
   - Get ready lanes via `aiki task lane <parent-id>`
   - Start each ready lane with `aiki run <parent-id> --next-thread --lane <lane-id> --async`
   - Wait for any thread to finish with `aiki task wait <ids> --any`
   - Loop back — finished threads may unblock new lanes
   - Exit when no ready lanes remain

## Lanes

Lanes are the execution units derived from the subtask dependency graph. They are computed at query time from `depends-on` and `needs-context` links — nothing is persisted.

### How lanes are derived

1. **`needs-context` chains** form threads — tasks linked by `needs-context` run in a single thread (same process, shared memory).

2. **`depends-on` edges** between threads form the lane DAG:
   - **Independent roots** (no dependencies) become separate parallel lanes
   - **Linear `depends-on` paths** stay in one lane as sequential threads
   - **Fan-out** creates separate parallel lanes
   - **Fan-in** creates a lane that waits on predecessor lanes

### Example

Given this subtask graph:

```
A (explore)
B (frontend) --depends-on A
C (backend)  --depends-on A
D (tests)    --depends-on B, C
```

The lane decomposition is:

```
Lane 1: A → B     (A runs first, then B)
Lane 2: C          (runs in parallel with Lane 1, after A)
Lane 3: D          (waits for Lanes 1 and 2)
```

### Lane statuses

| Status | Meaning |
|--------|---------|
| `● ready` | Prerequisites met, next thread can start |
| `▶ in-progress` | A thread in this lane is currently running |
| `◌ blocked` | Waiting on predecessor lanes |
| `✓ complete` | All tasks in the lane are done |
| `✗ failed` | At least one task stopped or closed as won't-do |

### Viewing lanes

```bash
# Show ready lanes (what can run now)
aiki task lane <parent-id>

# Show all lanes with status
aiki task lane <parent-id> --all
```

## Threads

Within a lane, work is divided into **threads**. Each thread is one agent invocation.

- A single-task thread runs one subtask in a fresh agent
- A `needs-context` chain runs multiple tasks in the same thread, preserving in-memory context between them

The loop orchestrator starts threads via `aiki run <parent-id> --next-thread --lane <lane-id>`, which automatically picks the next ready task (or chain) in the lane.

## Failure Handling

- If a thread fails, its lane cannot proceed
- Dependent lanes (via `depends-on`) are also blocked
- Independent lanes continue running
- Use `aiki task lane <parent-id> --all` to see which lanes are blocked

## Options

| Flag | Effect |
|------|--------|
| `--async` | Run in the background, return immediately |
| `--template <name>` | Loop template (default: `loop`) |
| `--agent <type>` | Agent for loop orchestration (default: `claude-code`) |
| `-o id` | Output bare loop task ID to stdout |

## Internal Use

`loop` is called internally by both `build` and `fix` after decomposition:

- **`aiki build`**: `plan` → `decompose` → **`loop`**
- **`aiki fix`**: `fix` → `decompose` → **`loop`** → review → *(repeat)*

It can also be invoked standalone when you've manually created subtasks under a parent and want to orchestrate their execution.
