# Build

`aiki build` takes a plan file, decomposes it into an epic with implementation subtasks, and executes them until the plan is fully implemented.

## Usage

```bash
# Build from a plan file
aiki build ops/now/user-auth.md

# Build from an existing epic
aiki build <epic-id>

# Build in the background
aiki build ops/now/user-auth.md --async

# Build then review
aiki build ops/now/user-auth.md -r

# Build, review, and auto-fix
aiki build ops/now/user-auth.md -f
```

## How It Works

Build is a pipeline of three stages: **epic** → **decompose** → **loop**.

### Stage 1: Epic

Finds or creates an epic task for the plan file. If an epic already exists (from a previous build), it's reused. The epic is the parent task under which all implementation subtasks live.

Draft plans (with `draft: true` in frontmatter) are rejected — finalize the plan first.

### Stage 2: Decompose

An agent reads the plan file and creates subtasks under the epic. For each step identified in the plan, the agent creates a subtask with detailed instructions (enough context for an executing agent to complete it without re-reading the plan).

The agent sets up dependencies between subtasks using links:

| Link type | When to use | Effect |
|-----------|-------------|--------|
| `--depends-on` | Task B needs A's output but not its session context | B waits for A, then runs in a fresh session |
| `--needs-context` | Task B must share A's in-memory understanding | B runs in the same agent session as A |
| *(no link)* | Tasks are independent | Tasks run as parallel lanes |

See [Decompose](decompose.md) for full details.

### Stage 3: Loop

The loop orchestrator derives parallel lanes from the dependency graph and executes them concurrently. Independent lanes run in parallel; dependent lanes wait for predecessors.

```
         ┌─ Frontend ─┐
Plan ──▶ │             ├──▶ Tests
         └─ Backend  ──┘
```

In this example, Frontend and Backend fan out from Plan (both `--depends-on` Plan) and run in parallel. Tests fans in (depends on both) and waits for both to finish.

See [Loop](loop.md) for full details on lane derivation and execution.

## Options

| Flag | Effect |
|------|--------|
| `--async` | Run in the background, return immediately |
| `--restart` | Ignore existing epic, create a new one |
| `-r`, `--review` | Run a review after all subtasks complete |
| `-f`, `--fix` | Run review + fix loop after build (implies `--review`) |
| `--decompose-template <name>` | Custom decompose template (default: `decompose`) |
| `--loop-template <name>` | Custom loop template (default: `loop`) |
| `--agent <type>` | Choose orchestrator agent (default: `claude-code`) |
| `-o id` | Output bare task ID to stdout |

## Subcommands

```bash
# Show build status for a plan
aiki build show ops/now/user-auth.md
```

## Idempotency

Running `aiki build` on the same plan file is idempotent. If an epic already exists for that plan, Aiki reuses it rather than creating a new one. Use `--restart` to force a fresh decomposition.
