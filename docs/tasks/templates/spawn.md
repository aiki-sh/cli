# Template Syntax: `spawns` and Template Composition

Templates are markdown files with YAML frontmatter that define task workflows. The `spawns` frontmatter field controls what happens when a task closes: it can conditionally create new tasks, enabling iterative workflows and chained pipelines. Templates also support composition via `{% subtask %}` directives and dynamic subtask generation via the `subtasks` data source field.

## `spawns`

The `spawns` field is a list of conditional spawn entries. When a task created from this template closes, each entry's `when` condition is evaluated. If true, a new task is created from the specified template.

```yaml
---
spawns:
  - when: <rhai-expression>
    task:                    # or `subtask:`
      template: <template-name>
      priority: <p0|p1|p2|p3>   # optional
      assignee: <agent-name>     # optional
      data:                      # optional
        key: <value-or-expression>
---
```

### Entry fields

| Field | Required | Description |
|-------|----------|-------------|
| `when` | yes | Rhai expression evaluated against task state at close time. Spawn triggers if truthy. |
| `task` | one of | Creates a **standalone task** (no parent relationship to the spawner). |
| `subtask` | one of | Creates a **subtask** (the spawner becomes the parent). |

Each entry must have exactly one of `task` or `subtask` — not both, not neither.

### Task/subtask config fields

| Field | Required | Default | Description |
|-------|----------|---------|-------------|
| `template` | yes | — | Template to instantiate (e.g., `fix`). Use `self` to re-instantiate the current template. |
| `priority` | no | inherited | Priority override. Inherits from spawner if not set. |
| `assignee` | no | template default | Assignee override. |
| `data` | no | `{}` | Data fields passed to the spawned task. Values are evaluated (see [Data values](#data-values)). |

### `when` condition scope

The `when` expression has access to these variables:

| Variable | Type | Description |
|----------|------|-------------|
| `status` | string | Always `"closed"` (spawns run post-close) |
| `outcome` | string | `"done"` or `"wont_do"` |
| `priority` | string | Task priority (e.g., `"p2"`) |
| `data.*` | map | The task's data fields as a nested map |
| `comments` | array | Array of comment text strings |
| `subtasks.<slug>.*` | map | Subtask state for subtasks that have slugs |

Subtask maps contain: `status`, `outcome`, `data`, `priority`.

### Examples

**Spawn a fix task when review fails:**

```yaml
spawns:
  - when: "not data.approved"
    task:
      template: fix
      data:
        review_task: "{{this.id}}"
```

**Spawn a subtask based on data:**

```yaml
spawns:
  - when: "data.needs_analysis"
    subtask:
      template: aiki/analysis
      assignee: claude-code
```

**Multiple spawn entries (all evaluated independently):**

```yaml
spawns:
  - when: "not data.approved"
    task:
      template: fix
  - when: "data.issue_count > 3"
    task:
      template: aiki/follow-up
      data:
        issue_count: data.issue_count
```

**Self-spawning loop pattern:**

```yaml
spawns:
  - when: "not subtasks.review.data.approved"
    task:
      template: self
      data:
        loop_index: "data.loop_index + 1"
```

### Data values

Data values in spawn configs follow these rules:

- **YAML strings** are evaluated as **Rhai expressions** against the spawner's post-close scope. If evaluation fails (undefined variable, type error), the entire spawn entry is skipped.
- **YAML booleans and numbers** are passed through as literals (`true` -> `"true"`, `3` -> `"3"`).
- **YAML arrays and maps** are converted directly to JSON without Rhai evaluation.

To pass a **string literal** (not a Rhai expression), use Rhai string syntax with nested quotes:

```yaml
data:
  # Rhai expression -- evaluates data.issue_count from the spawner
  issue_count: data.issue_count

  # Rhai expression -- arithmetic
  next_index: "data.loop_index + 1"

  # Literal number -- passed as "3"
  max_retries: 3

  # Literal boolean -- passed as "true"
  is_critical: true

  # String literal -- the inner quotes make it a Rhai string
  label: '"urgent"'

  # YAML array -- converted to JSON directly
  tags:
    - review
    - urgent
```

### Subtask precedence rule

If both `task` and `subtask` spawn entries trigger in the same close event, **only the subtask spawns are created** — all standalone task spawns are discarded. This ensures that when a task needs to add subtasks to itself, it doesn't also create unrelated standalone tasks.

---

## Template Composition with `{% subtask %}`

Templates can include other templates as subtasks using the `{% subtask %}` directive. This enables composable workflows where small, focused templates are assembled into larger ones.

### Syntax

```markdown
# Subtasks

{% subtask <template-name> %}
{% subtask <template-name> if <condition> %}
```

### How it works

- `{% subtask fix %}` — Always includes the `fix` template as a subtask
- `{% subtask plan if not data.plan %}` — Conditionally includes `plan` only when `data.plan` is falsy
- `{% subtask review/criteria/plan if data.scope.kind == "plan" %}` — Conditional on data values

The included template's `# Task Name` heading becomes a subtask heading (`## Subtask Name`), and its instructions become the subtask body. If the included template has its own frontmatter with `slug`, `priority`, `assignee`, etc., those are preserved.

### Example: Build template composing Plan

```markdown
---
version: 2.0.0
type: orchestrator
---

# Build: {{data.plan}}

**Overall Goal**: Execute the plan.

# Subtasks

{% subtask plan if not data.plan %}

## Execute Subtasks
---
slug: execute
---

Execute each subtask of the plan task sequentially.
```

### Inline subtasks

Subtasks can also be defined inline in the same file:

```markdown
# Subtasks

## First Subtask
---
slug: first
priority: p1
assignee: claude-code
sources:
  - "task:{{source.id}}"
---

Instructions for the first subtask.

## Second Subtask
---
slug: second
---

Instructions for the second subtask.
```

Inline subtask frontmatter supports: `slug`, `priority`, `assignee`, `sources`, `data`.

---

## Dynamic Subtasks with `subtasks` Data Source

The `subtasks` frontmatter field enables declarative subtask generation from a data source. Instead of defining static subtasks, the template provides a subtask template that is instantiated once per item in the data source.

### Syntax

```yaml
---
subtasks: <data-source-path>
---
```

Where `<data-source-path>` is a dotted path like `source.comments` that resolves to an iterable collection at task creation time.

### Example

```yaml
---
version: 1.0.0
subtasks: source.comments
---

# Fix Issues from Review

Address all issues found during review.

# Subtasks

## Fix: {{item.text}}

Fix the issue described above.
```

When the `subtasks` field is present, the `# Subtasks` section is treated as a **template** rather than a static definition. The heading and body are instantiated for each item in the data source.

---

## Template Frontmatter Reference

Full list of frontmatter fields:

```yaml
---
# Identity
slug: review              # Stable slug for automation references
version: "2.0.0"          # Semantic version
description: "..."        # Human-readable description

# Task defaults
type: review              # Task type (enables sugar triggers)
assignee: claude-code     # Default assignee
priority: p1              # Default priority

# Data
data:                     # Default data values
  scope: "@"
  max_retries: 3

# Dynamic subtasks
subtasks: source.comments # Data source path for subtask iteration

# Spawn rules
spawns:                   # Conditional task creation on close
  - when: "not data.approved"
    task:
      template: fix
---
```

---

## Rhai Expression Reference

Both `when` conditions and string data values are evaluated as [Rhai](https://rhai.rs/) expressions. Some common patterns:

```ruby
# Boolean logic
not data.approved
data.approved and outcome == "done"
not subtasks.review.data.approved

# Comparisons
data.issue_count > 3
data.loop_index >= 10

# String comparison
outcome == "done"
outcome == "wont_do"

# Nested data access
data.loop_index + 1
subtasks.review.data.score
```

Note: `!` is rewritten to `not` during preprocessing for readability, so `!data.approved` and `not data.approved` are equivalent.

---

## Template Variables

Templates support Mustache-style double-brace variable interpolation:

| Variable | Description |
|----------|-------------|
| `{{id}}` | The task's own ID |
| `{{parent.id}}` | Parent task's ID |
| `{{parent.subtasks.<slug>}}` | Reference to a sibling subtask by slug |
| `{{source.id}}` | Source task's ID (from `--source task:...`) |
| `{{data.<key>}}` | Task data field |
| `{{data.scope.name}}` | Nested data access |
| `{{this.id}}` | Alias for current task ID (useful in spawn data) |
