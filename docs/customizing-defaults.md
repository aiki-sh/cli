# Customizing Defaults

Aiki's behavior is driven by **flows** — declarative YAML workflows that react to editor events. The bundled `aiki/core` flow handles provenance tracking automatically. You can extend or override it without writing a full plugin.

## The Hookfile

`.aiki/hooks.yml` is the entry point for customization. It's a flow definition that can include plugins, define event handlers, and compose behaviors.

A minimal hookfile:

```yaml
name: "My Project"

include:
  - aiki/default

session.started:
  - context: "Always run tests before committing"
```

The `include:` directive pulls in other flows. `aiki/default` includes the core provenance flow and default behaviors.

## Events

Events fire at specific points in the AI agent lifecycle. Each event can have handlers that run actions.

### Session Lifecycle

| Event | When it fires |
|-------|--------------|
| `session.started` | New agent session begins |
| `session.resumed` | Continuing a previous session |
| `session.ended` | Agent session terminates |

### Turn Lifecycle

| Event | When it fires |
|-------|--------------|
| `turn.started` | User submits a prompt or autoreply triggers |
| `turn.completed` | Agent finishes processing a turn |

### File Changes

| Event | When it fires |
|-------|--------------|
| `change.permission_asked` | Agent is about to write, delete, or move a file |
| `change.completed` | Agent finished a file mutation |

### Reads

| Event | When it fires |
|-------|--------------|
| `read.permission_asked` | Agent is about to read a file |
| `read.completed` | Agent finished reading a file |

### Shell Commands

| Event | When it fires |
|-------|--------------|
| `shell.permission_asked` | Agent is about to execute a shell command |
| `shell.completed` | Shell command finished |

### Web Access

| Event | When it fires |
|-------|--------------|
| `web.permission_asked` | Agent is about to make a web request |
| `web.completed` | Web request finished |

### MCP Tools

| Event | When it fires |
|-------|--------------|
| `mcp.permission_asked` | Agent is about to call an MCP tool |
| `mcp.completed` | MCP tool call finished |

### Git Integration

| Event | When it fires |
|-------|--------------|
| `commit.message_started` | Git's `prepare-commit-msg` hook fired |

### Repository

| Event | When it fires |
|-------|--------------|
| `repo.changed` | Session moved to a different JJ repo |

### Tasks

| Event | When it fires |
|-------|--------------|
| `task.started` | Task transitioned to in_progress |
| `task.closed` | Task reached closed state |

Events with `permission_asked` suffix are **gateable** — handlers can block the operation.

## Actions

Actions are the building blocks of flow handlers.

| Action | Description | Example |
|--------|-------------|---------|
| `shell` | Run a shell command | `- shell: cargo test` |
| `jj` | Run a Jujutsu command | `- jj: new` |
| `context` | Inject text into agent prompts | `- context: "Run tests first"` |
| `autoreply` | Send a follow-up message to the agent | `- autoreply: "Check test results"` |
| `commit_message` | Modify the Git commit message | `- commit_message: append: "Signed-off-by: Bot"` |
| `log` | Write to the Aiki log | `- log: "Session started"` |
| `let` | Bind a variable | `- let: count = self.task_list_size` |
| `call` | Call a function without storing result | `- call: self.workspace_absorb_all` |
| `hook` | Invoke another plugin's handler | `- hook: "aiki/my-plugin"` |
| `task.run` | Spawn an agent to work on a task | `- task.run: <task-id>` |
| `review` | Create and run a code review | `- review: <task-id>` |
| `stop` | Stop hook execution silently | `- stop: "Reason"` |
| `block` | Block the editor operation (exit code 2) | `- block: "Dangerous command"` |
| `session.end` | End the current session | `- session.end: "Task completed"` |

### Shell and JJ Actions

Both support `alias` (store stdout in a variable), `timeout`, and `on_failure`:

```yaml
- shell: cargo test
  alias: test_result
  timeout: 60s
  on_failure:
    - stop: "Tests failed"
```

JJ actions also support `with_author` to override the change author:

```yaml
- with_author: "Bot <bot@example.com>"
  jj: describe --message "automated change"
```

## Variables

### Event Variables

Access event data with `{{event.*}}`:

```yaml
session.started:
  - log: "Session {{event.session.uuid}} started by {{event.agent}}"
```

### Let Bindings

Store values for later use:

```yaml
- let: file_count = self.task_list_size
- if: file_count
  then:
    - context: "There are {{file_count}} tasks ready"
```

### Environment Variables

Access environment variables with `$ENVVAR` syntax in shell commands.

### Built-in Functions

Functions prefixed with `self.*` are native Rust functions:

```yaml
- let: ws_path = self.workspace_ensure_isolated
- let: classification = self.classify_edits_change
- call: self.workspace_absorb_all
```

## Control Flow

### If/Then/Else

```yaml
- if: event.write
  then:
    - jj: diff -r @ --name-only
      alias: changed_files
    - if: changed_files
      then:
        - jj: new
  else:
    - log: "Not a write operation"
```

The condition is truthy if non-empty and not `"false"` or `"0"`.

### Switch/Case

```yaml
- switch: self.classify_edits_change
  cases:
    ExactMatch:
      - log: "AI edits match exactly"
    AdditiveUserEdits:
      - log: "User added edits on top"
    OverlappingUserEdits:
      - log: "User modified AI edits"
  default:
    - log: "Unknown classification"
```

## Failure Handling

Every action can specify `on_failure` behavior:

```yaml
- shell: cargo test
  on_failure:
    - continue: "Tests failed but continuing"  # Log and continue (default)

- shell: npm audit
  on_failure:
    - stop: "Security audit failed"  # Stop hook execution silently

- shell: dangerous-command
  on_failure:
    - block: "Command blocked"  # Block the editor operation (exit code 2)
```

| Behavior | Effect |
|----------|--------|
| `continue` | Log the failure, keep running subsequent actions |
| `stop` | Stop executing this hook, but don't block the editor |
| `block` | Stop and block the editor operation (only meaningful for `permission_asked` events) |

## Context Injection

Inject instructions into agent prompts. This is how you customize what AI agents know about your project.

### At Session Start

```yaml
session.started:
  - context: "This project uses pytest for testing. Always run tests before committing."
```

### Every Turn

```yaml
turn.started:
  - context:
      append: |
        Remember: this project follows strict type checking.
        Run `mypy .` before considering any task complete.
```

### Prepend vs Append

```yaml
# Prepend (higher priority, shown first)
- context:
    prepend: "CRITICAL: Never modify files in the vendor/ directory"

# Append (shown after other context)
- context:
    append: "Tip: use `make lint` to check code style"

# Simple form (same as append)
- context: "Simple context message"
```

## Autoreplies

Send follow-up messages to agents after they respond. Useful for automated workflows:

```yaml
turn.completed:
  - autoreply: "Please run the test suite and report results"
```

Use sparingly — autoreplies trigger additional agent turns.

## Overriding Templates

Aiki uses markdown templates for tasks (plans, reviews, fixes). Override built-in templates by creating files in your project:

> **Upgrading from pre-1.x?** Rename `.aiki/templates/` to `.aiki/tasks/`.

```
.aiki/tasks/plan.md      # Override the plan template
.aiki/tasks/review.md    # Override the review template
.aiki/tasks/fix.md       # Override the fix template
```

**Resolution order:** project (`.aiki/tasks/`) → user (`~/.aiki/tasks/`) → built-in

## User-Level Customization

Apply customizations across all your projects:

- `~/.aiki/hooks/` — global hook files (loaded via `include:`)
- `~/.aiki/tasks/` — global template overrides

Example: create `~/.aiki/hooks/my-defaults/hooks.yaml` and include it in any project:

```yaml
# .aiki/hooks.yml
include:
  - aiki/default
  - my-defaults
```

## Common Recipes

### Inject Project-Specific Instructions

```yaml
# .aiki/hooks.yml
include:
  - aiki/default

session.started:
  - context: |
      This is a Django project using PostgreSQL.
      - Run `python manage.py test` before committing
      - Follow PEP 8 style guidelines
      - All new endpoints need OpenAPI documentation
```

### Block Pushes Without Tests

```yaml
shell.permission_asked:
  - if: event.command starts_with "git push"
    then:
      - shell: cargo test --quiet
        on_failure:
          - block: "Tests must pass before pushing. Run `cargo test` to see failures."
```

### Add Custom Commit Message Trailers

```yaml
commit.message_started:
  - commit_message:
      append: |
        Reviewed-by: AI Review Bot
        Project: my-project
```

### Run a Linter After Every File Edit

```yaml
change.completed:
  - if: event.write
    then:
      - shell: eslint --fix {{event.file_path}}
        on_failure:
          - continue: "Lint failed for {{event.file_path}}"
```
