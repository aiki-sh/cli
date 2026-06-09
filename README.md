45
23
> ⚠️ **EXPERIMENTAL** — This repo is under active development. Reach out to glasner@aiki.sh if you have issues.

# Aiki: Structured AI coding with safe defaults

Aiki gives teams a practical way to let AI edit code without losing control.
It provides a structured workflow for planning, executing, reviewing, and fixing
AI-suggested work, while keeping the whole process visible, attributable, and
safe to adapt.

## What Aiki is

Aiki is a layer on top of your repo and your AI tools that turns AI edits into
trackable work:

- **Opinionated defaults** for task tracking, provenance, and review/fix loops
- **Consistent handoffs** across Claude Code, Codex, and other agents
- **Safe concurrency** via isolated sessions and session-aware workflows

## Why it matters

Most teams start with fast AI code changes and immediately lose one of two things:

1. **Context** — what changed, why, and who changed it
2. **Quality control** — why some changes are reviewed, others are skipped

Aiki addresses both by giving AI work a workflow shape that stays
human-readable: every change is attached to a task, reviewed in a loop, and
recorded in history.

## What changes when you use Aiki

When your team adopts Aiki:

- Every AI task is started, described, and tracked as a task.
- You can inspect work in real time (`aiki task show`), and review exact edits
  (`aiki task diff`).
- Reviews and fixes become part of the same workflow instead of a separate ad hoc step.
- You retain control points (`aiki doctor`, stop conditions, and review gates)
  without removing automation.

### Why JJ fits well

Aiki is built on a change-based workflow (Jujutsu/JJ). That makes AI edits
naturally reviewable and reversible: each task creates a JJ change record with
stable IDs, so provenance, reruns, and follow-up fixes are easier to track.

## Opinionated defaults, composable underneath

Aiki starts with sensible defaults for teams that want guardrails out of the box,
and gives you extension points when you want more control:

- **Customize behavior** via flow hooks in `.aiki/hooks.yml`.
- **Adapt templates** and **extend with plugins** to encode team-specific policies.
- **Build your own agent harness** by composing primitives (task links, hooks,
  templates) instead of rewriting core behavior.

## Run your first workflow (about 2 minutes)

1. Follow **[Getting Started](docs/getting-started.md)** to install and bootstrap.
2. In one repo: `aiki init` and then `aiki doctor`.
3. Run a tiny change in your chat workflow and verify:
   - `aiki task show <task-id>`
   - `aiki task diff <task-id>`

## Two paths, same foundation

### 1) Chat mode (human-in-the-loop)
Use AI interactively in your editor, with task-level traceability and review
readiness built in.

### 2) Headless mode (Plan → Build → Review → Fix)
Use `aiki plan`, `aiki build`, and `aiki review`/`aiki fix` for repeatable
spec-to-implementation runs.

## Next: deeper docs

- **[Getting Started](docs/getting-started.md)** — install and first run
- **[SDLC: Plan, Build, Review, Fix](docs/sdlc.md)** — end-to-end flow model
- **[Customizing Defaults](docs/customizing-defaults.md)** — hook/events and custom flow behavior
- **[Creating Plugins](docs/creating-plugins.md)** — packages for reusable harness logic
- **[Task Types and Links](docs/tasks/kinds.md)** — dependency and review graph semantics
- **[Session Isolation Workflow](docs/session-isolation.md)** — safe multi-agent execution
