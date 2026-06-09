# Link Kinds Reference

Every link in the task graph has a **kind** — a string that defines the relationship's semantics, cardinality rules, and blocking behavior. This page documents all registered kinds.

For an overview of how links work, see [Task Links](links.md).

## Quick Reference

| Kind | Blocks? | Task-only? | Max forward | Max reverse | CLI flag |
|------|---------|------------|-------------|-------------|----------|
| `blocked-by` | yes | yes | unlimited | unlimited | `--blocked-by` |
| `depends-on` | yes | yes | unlimited | unlimited | `--depends-on` |
| `validates` | yes | yes | unlimited | unlimited | `--validates` |
| `remediates` | yes | yes | unlimited | unlimited | `--remediates` |
| `needs-context` | yes | yes | 1 | 1 | `--needs-context` |
| `sourced-from` | no | no | unlimited | unlimited | `--sourced-from` |
| `subtask-of` | no | yes | 1 | unlimited | `--subtask-of` |
| `implements-plan` | no | no | 1 | 1 | `--implements` |
| `orchestrates` | no | yes | 1 | 1 | `--orchestrates` |
| `decomposes-plan` | no | no | unlimited | unlimited | `--decomposes-plan` |
| `adds-plan` | no | no | unlimited | unlimited | `--adds-plan` |
| `fixes` | no | no | unlimited | unlimited | `--fixes` |
| `supersedes` | no | yes | 1 | unlimited | `--supersedes` |
| `spawned-by` | no | yes | 1 | unlimited | (automatic) |

**Blocks?** — Does an open link of this kind prevent the `from` task from appearing in the ready queue?

**Task-only?** — Must the target resolve to a task ID? If no, external references like `file:path` are allowed.

**Max forward/reverse** — Cardinality limits. "1" means single-link (auto-replaces on conflict). "unlimited" means no limit.

---

## Blocking Kinds

These kinds hold the `from` task out of the ready queue until the `to` task closes.

### `blocked-by`

Generic blocking relationship. Task A can't start until task B closes.

```bash
aiki task link A --blocked-by B
aiki task add "Deploy" --blocked-by <test-id>
```

- **Cardinality:** unlimited in both directions
- **Autorun:** supported — use `--autorun` to auto-start when blocker closes
- **Legacy note:** This was the original blocking kind. For new links, prefer the semantic kinds below when they fit.

### `depends-on`

Semantic dependency. Similar to `blocked-by` but conveys that the `from` task depends on work done by the `to` task.

```bash
aiki task link B --depends-on A
aiki task add "Integration tests" --depends-on <build-id> --autorun
```

- **Cardinality:** unlimited in both directions
- **Autorun:** supported

### `validates`

Review relationship. The `from` task reviews/validates the `to` task's work. Used by the `aiki review` pipeline.

```bash
aiki task link review --validates impl-task
```

- **Cardinality:** unlimited in both directions
- **Autorun:** supported
- **Typical usage:** Created automatically by `aiki review <task-id>`

### `remediates`

Fix relationship. The `from` task fixes issues found by the `to` task (typically a review). Used by the `aiki fix` pipeline.

```bash
aiki task link fix --remediates review-task
```

- **Cardinality:** unlimited in both directions
- **Autorun:** supported
- **Typical usage:** Created automatically by `aiki fix <review-id>`

### `needs-context`

Thread-context link. The `from` task must run in the same thread as the `to` task, and the `to` task must complete first. Forms linear chains only.

```bash
aiki task add "Apply fix" --needs-context <investigation-id>
```

- **Cardinality:** max 1 forward, max 1 reverse (linear chains)
- **Autorun:** supported
- **Use case:** When task B needs the in-memory context from task A (e.g., an investigation followed by a fix) — they run in the same thread

---

## Non-blocking Kinds

These kinds track relationships without affecting task scheduling.

### `sourced-from`

Provenance link. Tracks where a task originated — another task, a design doc, a user prompt, etc.

```bash
aiki task add "Implement X" --sourced-from file:ops/now/design.md
aiki task add "Follow-up" --sourced-from task:<review-id>
```

- **Cardinality:** unlimited in both directions
- **Task-only:** no — accepts external references (`file:`, `task:`, `prompt:`, `issue:`, `comment:`)
- **Typical usage:** Set via `--source` / `--sourced-from` on `task add` and `task start`
- **Alias:** `--source` is a hidden alias for `--sourced-from`

### `subtask-of`

Parent-child hierarchy. The `from` task is a child of the `to` task.

```bash
aiki task add "Fix null check" --subtask-of <parent-id>
aiki task link child --subtask-of parent
```

- **Cardinality:** max 1 forward (a task has one parent), unlimited reverse (a parent can have many children)
- **Task-only:** yes
- **Cycle detection:** yes — prevents circular parent chains
- **Auto-replace:** If a task already has a parent and you add a new `subtask-of` link, the old parent link is removed (re-parenting)
- **Alias:** `--parent` is a hidden alias for `--subtask-of`

### `implements-plan`

Links a task to the plan file it implements. Single-link in both directions — one task implements one plan, one plan is implemented by one task.

```bash
aiki task add "Build auth" --implements ops/now/auth-plan.md
```

- **Cardinality:** max 1 forward, max 1 reverse
- **Task-only:** no — typically targets a file path
- **Auto-replace:** Adding a new `implements-plan` link replaces the old one and emits a `supersedes` link to the old target (if it was a task)

### `orchestrates`

Links an orchestrator task to the epic it coordinates. The orchestrator drives subtask execution for the epic.

```bash
aiki task link orchestrator --orchestrates epic
```

- **Cardinality:** max 1 forward, max 1 reverse (one orchestrator per epic)
- **Task-only:** yes
- **Auto-replace:** Same as `implements-plan` — replaces old link and emits `supersedes`

### `decomposes-plan`

Tracks that a task breaks down (decomposes) a plan file into subtasks.

```bash
aiki task link decomposer --decomposes-plan file:ops/now/big-feature.md
```

- **Cardinality:** unlimited in both directions
- **Task-only:** no

### `adds-plan`

Tracks that a task adds or creates a plan file.

```bash
aiki task link planner --adds-plan file:ops/now/new-plan.md
```

- **Cardinality:** unlimited in both directions
- **Task-only:** no

### `fixes`

Tracks that a task fixes something — a file, another task, or an external issue.

```bash
aiki task add "Fix login bug" --fixes file:src/auth.rs
aiki task add "Hotfix" --fixes task:<broken-id>
```

- **Cardinality:** unlimited in both directions
- **Task-only:** no

### `supersedes`

Tracks that a task replaces a predecessor. Often emitted automatically when `implements-plan` or `orchestrates` links are replaced.

```bash
aiki task link new-impl --supersedes old-impl
```

- **Cardinality:** max 1 forward (a task supersedes one predecessor), unlimited reverse (a task can be superseded by many)
- **Task-only:** yes
- **Typical usage:** Emitted automatically during auto-replace of `implements-plan` and `orchestrates` links. Rarely used manually.

### `spawned-by`

Provenance link from a spawned task to the task that spawned it. Set automatically when `aiki run` creates a child agent task.

- **Cardinality:** max 1 forward (a task is spawned by one parent), unlimited reverse
- **Task-only:** yes
- **Typical usage:** Automatic — not typically set manually

---

## Adding New Kinds

Link kinds are defined in `cli/src/tasks/graph.rs` in the `LINK_KINDS` constant. Adding a new kind requires only a new entry there — zero changes to the edge store, graph materialization, or storage layer.

```rust
LinkKind {
    name: "my-new-kind",
    max_forward: None,      // unlimited
    max_reverse: None,      // unlimited
    blocks_ready: false,    // non-blocking
    task_only: true,        // targets must be task IDs
},
```

After adding the entry, add a corresponding CLI flag to the `Link` and `Unlink` commands in `cli/src/commands/task.rs`, and update the `extract_link_flag` function.
