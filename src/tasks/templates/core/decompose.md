---
version: 1.0.0
type: decompose
slug: decompose
---

# Decompose: {{data.plan}}

**Goal**: Read the plan file and create a parent task with implementation subtasks.

## Read and Extract from the Plan

Read the plan file at `{{data.plan}}` and build a mental inventory of:
- **Goal**: What is being built or changed?
- **File inventory**: Which files are created, modified, or deleted? What changes in each?
- **Data structures**: New types, enums, fields, or schemas introduced
- **Dependencies between steps**: Which changes must land before others?
- **Tests**: What test cases does the plan specify?

You will use this inventory to write subtask instructions. Every detail that an implementing agent needs must be extracted from the plan and placed into subtask instructions — the agents cannot read the plan themselves.

## Parent Task

The parent task has already been created: `{{data.target}}`

Set instructions on the parent task that summarize the plan's goal, scope, and key decisions. Do NOT just reference the plan file — the parent instructions are read by reviewers and orchestrators who need context without opening the plan.

```bash
aiki task set {{data.target}} -i <<'MD'
<plan title>
See plan: {{data.plan}}

## Goal
<1-2 sentence summary of what this epic delivers>

## Scope
<key files/modules affected, what changes and what doesn't>

## Key decisions
<important design choices from the plan that constrain implementation>
MD
```

## Add Subtasks

**Every subtask MUST have self-contained instructions via `-i`.** The executing agent runs in a fresh session with no access to this conversation or the plan file. If a subtask has no instructions, the agent has nothing to work with and the task will fail.

For each implementation step, create a subtask with instructions that include everything the agent needs:

```bash
TASK_ID=$(aiki task add "<step description>" --subtask-of {{data.target}} --output id -i <<'MD'
## Goal
<what this step accomplishes>

## Context
<relevant background from the plan — copy the specifics, don't just say "see plan">

## Files to change
<list specific files and what to change in each>

## Acceptance criteria
<how the agent knows it's done — e.g., tests pass, function exists, output matches>
MD
)
```

**What good instructions look like:**
- Copy relevant sections from the plan into the instructions (file paths, function signatures, data structures, test cases)
- Specify which files to create or modify and what changes to make
- Include acceptance criteria (tests to run, behavior to verify)
- Reference specific line numbers or function names when the plan provides them

**What bad instructions look like:**
- "See plan at ops/now/foo.md" (agent can't access your context)
- "Implement the feature described above" (there is no "above")
- A task name with no `-i` flag at all (empty instructions = guaranteed failure)

### Linking subtasks

Links between subtasks control how the orchestrator schedules work into parallel **lanes**. Use the right link type:

**`--depends-on`** — The later task needs the earlier task's *output* but runs in a fresh thread. Creates lane boundaries: fan-out points split into parallel lanes, fan-in points create lanes that wait on predecessors.

```bash
# implement-frontend can't start until plan is done, but doesn't need plan's thread context
aiki task link $FRONTEND_ID --depends-on $PLAN_ID
aiki task link $BACKEND_ID --depends-on $PLAN_ID    # fan-out: frontend + backend run in parallel
aiki task link $TESTS_ID --depends-on $FRONTEND_ID   # fan-in: tests waits on both
aiki task link $TESTS_ID --depends-on $BACKEND_ID
```

**`--needs-context`** — The later task must run in the *same thread* as the earlier task (shared in-memory context). Forms linear chains within a lane. Max one forward, one reverse.

```bash
# explore then plan in one thread — plan needs explore's codebase understanding
EXPLORE_ID=$(aiki task add "Explore codebase" --subtask-of {{data.target}} --output id)
PLAN_ID=$(aiki task add "Create implementation plan" --subtask-of {{data.target}} --needs-context $EXPLORE_ID --output id)
```

**When to use which:**

| Situation | Link type | Why |
|-----------|-----------|-----|
| Task B needs A's code changes on disk | `--depends-on` | Fresh thread is fine, just needs the committed output |
| Task B needs A's in-memory understanding | `--needs-context` | Same thread preserves context |
| Tasks are independent (no shared state) | No link | Each becomes its own parallel lane |
| Task B reviews/validates A's work | `--depends-on` | Review runs independently after implementation |

**Guidelines for subtasks:**
- **Instructions are mandatory** — never create a subtask without `-i`
- Instructions must be self-contained: include file paths, function names, data structures, and test cases from the plan
- Each subtask should be discrete and actionable
- Order subtasks logically (dependencies first)
- Use `--depends-on` for tasks that need another's output but not its thread context
- Use `--needs-context` for tasks that must share a thread (e.g., explore → plan)
- Leave tasks unlinked when they are truly independent — they'll run as parallel lanes
- Keep subtask names concise but descriptive
- Each subtask should have verifiable acceptance criteria


## Close Decompose Task

When all subtasks are added, close this decompose task and report the parent task:

```bash
aiki task close {{id}} --confidence <1-4> --summary "Created N subtasks. Parent task ID: {{data.target}}"
```

Output the parent task ID and subtask summary for the user.
