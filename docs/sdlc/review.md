# Review

`aiki review` creates a structured evaluation of code or plans. Issues are tracked as review comments that can be piped into `aiki fix` for automated remediation.

## Usage

```bash
# Review a task's changes
aiki review <task-id>

# Review a plan document
aiki review ops/now/user-auth.md

# Review code described by a plan
aiki review ops/now/user-auth.md --code

# Review current session's changes
aiki review

# Review and auto-fix
aiki review <task-id> -f

# Hand off review to calling agent (don't run autonomously)
aiki review <task-id> --start

# Run review in the background
aiki review <task-id> --async
```

## How It Works

A review runs through three phases as subtasks:

1. **Explore scope** — The agent examines the code or plan being reviewed to build understanding. Uses `aiki explore` to read files, trace code paths, and understand context.

2. **Apply criteria** — The agent evaluates the work against a structured rubric (see [Criteria](#criteria) below).

3. **Record issues** — Each problem found is tracked as a review issue:
   ```bash
   aiki review issue add <review-id> - <<'ISSUE'
   Description of the issue
   ISSUE
   ```
   Issues are distinct from regular comments — only issues trigger followup tasks when piped to `aiki fix`.

## Review Scopes

The target determines what kind of review runs:

| Target | Scope | What gets reviewed |
|--------|-------|--------------------|
| Task ID | Task | The task's code changes (diff) |
| `.md` file | Plan | The plan document's quality |
| `.md` file + `--code` | Code | Implementation against the plan |
| *(nothing)* | Session | All changes in the current session |

## Criteria

Reviews evaluate against different criteria depending on scope.

### Plan Reviews

| Category | What's checked |
|----------|----------------|
| **Completeness** | All sections filled, no TODOs or placeholders |
| **Clarity** | Unambiguous requirements, clear acceptance criteria |
| **Implementability** | Can be decomposed into tasks, sufficient technical detail |
| **UX** | User experience considered, intuitive command syntax, errors defined |

### Code Reviews

| Category | What's checked |
|----------|----------------|
| **Plan coverage** | All requirements from the plan are implemented |
| **Code quality** | Clean, idiomatic, maintainable code |
| **Security** | No vulnerabilities introduced (OWASP top 10, injection, etc.) |
| **Plan alignment** | Implementation matches the plan's intent, not just its letter |

## Agent Assignment

By default, Aiki assigns a different agent type for reviews than the one that wrote the code. This ensures independent evaluation. Use `--agent` to override.

## Options

| Flag | Effect |
|------|--------|
| `-f`, `--fix` | Auto-fix issues after review |
| `--async` | Run in the background |
| `--start` | Assign the review to the calling agent (hand off) |
| `--code` | Review code implementation (only with file targets) |
| `--autorun` | Auto-start review when its target task closes |
| `--template <name>` | Use a custom review template (default: `review`) |
| `--agent <type>` | Override reviewer agent |
| `-o id` | Output bare review task ID to stdout |

## Subcommands

```bash
# List review tasks
aiki review list
aiki review list --all    # include closed reviews

# Show review details
aiki review show <review-id>

# Manage issues on a review
aiki review issue add <review-id> - <<'ISSUE'
Issue description
ISSUE
aiki review issue add <review-id> - --high --file src/auth.rs:42 <<'ISSUE'
Bug in auth
ISSUE
aiki review issue add <review-id> - --low --file src/utils.rs <<'ISSUE'
Style nit
ISSUE
aiki review issue add <review-id> - --file src/a.rs:10 --file src/b.rs:20 <<'ISSUE'
Cross-file issue
ISSUE
aiki review issue list <review-id>
```

### Issue severity

| Flag | Level | Meaning |
|------|-------|---------|
| `--high` | High | Must fix: incorrect behavior, bug, or contract violation |
| *(default)* | Medium | Should fix: suboptimal, missing, or inconsistent |
| `--low` | Low | Could fix: style, naming, cosmetic |

Alternatively, use `--severity high|medium|low`.

### Issue locations

Use `--file` (repeatable) to attach file locations to an issue:

- `--file src/auth.rs` — file only
- `--file src/auth.rs:42` — file and line
- `--file src/auth.rs:42-50` — file and line range
- `--file src/a.rs:10 --file src/b.rs:20` — multiple files
