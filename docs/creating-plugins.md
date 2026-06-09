# Creating Plugins

Plugins are how you package and share reusable Aiki behaviors — custom hooks, templates, or both. A plugin is a GitHub repo with a specific directory structure.

## What Is a Plugin?

A plugin is a GitHub repository containing:

- `plugin.yaml` — plugin metadata (name, description) — required
- `hooks.yaml` — flow definitions (event handlers)
- `tasks/` — markdown templates for tasks (plans, reviews, etc.)

A `plugin.yaml` manifest is required. At least one of `hooks.yaml` or `tasks/` must also exist.

## Plugin Structure

Minimal hooks-only plugin:

```
my-plugin/
├── plugin.yaml
└── hooks.yaml
```

Minimal tasks-only plugin:

```
my-plugin/
├── plugin.yaml
└── tasks/
    └── review.md
```

Full plugin:

```
my-plugin/
├── plugin.yaml
├── hooks.yaml
└── tasks/
    ├── plan.md
    └── review.md
```

## Naming and References

Plugins use a `namespace/name` format:

- The **namespace** is your GitHub username or organization
- The **name** is the repository name

Examples: `somecorp/security`, `myuser/style-checks`

When referencing plugin templates, use three-part paths: `namespace/name/template`. For example, `somecorp/security/review` refers to the `tasks/review.md` file in the `somecorp/security` plugin.

## The `aiki` Namespace

The `aiki` namespace is reserved and maps to the `glasner` GitHub organization. Don't use it for your own plugins. Built-in flows like `aiki/core` and `aiki/default` use this namespace.

## Creating a Hooks Plugin

Here's a plugin that blocks dangerous shell commands:

```yaml
# hooks.yaml
name: "Safety Guard"
description: "Block dangerous shell commands during AI sessions"
version: "1"

shell.permission_asked:
  - if: event.command starts_with "rm -rf /"
    then:
      - block: "Blocked dangerous command: {{event.command}}"

  - if: event.command starts_with "git push --force"
    then:
      - block: "Force-push blocked. Use regular git push."
```

Users include it in their project:

```yaml
# .aiki/hooks.yml
include:
  - aiki/default
  - myuser/safety-guard
```

## The plugin.yaml Manifest

Every plugin requires a `plugin.yaml` file at the root. It contains metadata used for discovery and display:

```yaml
# plugin.yaml
name: My Plugin
description: One-line summary of what this plugin does
```

| Field | Required | Purpose |
|-------|----------|---------|
| `name` | Recommended | Human-readable display name. Falls back to `{namespace}/{plugin}` path if omitted. |
| `description` | Recommended | One-line summary of what the plugin does |

### Display Name Resolution

The `name` field is a **display name**, not an identifier. The machine identifier is always the `{namespace}/{plugin}` path (e.g., `eslint/standard`).

Fallback chain:
1. `plugin.yaml` `name:` field
2. `hooks.yaml` `name:` field (standalone hook files with no `plugin.yaml`)
3. `{namespace}/{plugin}` path as display name

For plugins with both files, `plugin.yaml` takes precedence.

## Creating a Templates Plugin

Templates are markdown files that define task structure. They can use `{{variable}}` interpolation and `{{> partial}}` includes.

Example: a custom review template:

```markdown
---
assignee: claude-code
---

# Code Review

Review the changes for:
- Security vulnerabilities
- Performance issues
- Test coverage gaps

{{> review/criteria/code}}

Report findings as task comments.
```

The `{{> review/criteria/code}}` syntax includes a partial from another plugin's templates.

## Dependencies

Plugin dependencies are **auto-derived** from content.

Aiki scans your `hooks.yaml` and `tasks/` for references to other plugins:

- `include:` directives in hooks.yaml
- `hook:` actions referencing other plugins
- `{{> namespace/plugin/template}}` partials in task templates
- Template references in spawn configs

References inside fenced code blocks (`` ``` ``) are excluded from dependency scanning.

## Testing Locally

Before publishing, test your plugin by symlinking it into the plugins directory:

```bash
# Create the namespace directory
mkdir -p ~/.aiki/plugins/myuser

# Symlink your plugin
ln -s /path/to/my-plugin ~/.aiki/plugins/myuser/my-plugin
```

Then include it in a test project's `.aiki/hooks.yml` and verify the behavior.

Alternatively, use project-level overrides — place files directly in `.aiki/tasks/myuser/my-plugin/` to test template changes without a symlink.

## Publishing

Push your plugin to GitHub:

```bash
cd my-plugin
git init && git add . && git commit -m "Initial plugin"
gh repo create myuser/my-plugin --public --source=. --push
```

Users can reference it immediately — plugins are **auto-fetched** on first use. No install step required:

```yaml
# .aiki/hooks.yml
include:
  - aiki/default
  - myuser/my-plugin   # auto-fetched from GitHub on first reference
```

To pre-fetch (e.g., for CI or offline use):

```bash
aiki plugin install myuser/my-plugin
```

## Plugin Management

```bash
# Pre-fetch a plugin (optional — plugins auto-fetch on first reference)
aiki plugin install myuser/my-plugin

# Pre-fetch all plugins referenced by the current project
aiki plugin install

# Update a specific plugin
aiki plugin update myuser/my-plugin

# Update all installed plugins
aiki plugin update

# List plugins and their status (includes auto-fetched plugins)
aiki plugin list

# Remove a plugin
aiki plugin remove myuser/my-plugin
```

> **Note:** `aiki plugin install` is a pre-fetch optimization, not a prerequisite. Plugins are automatically cloned from GitHub the first time they're referenced via `include:`, `hook:`, `before:`/`after:`, or `--template`. The first use is slightly slower (one-time clone); subsequent sessions use the cached copy.

## Composition

Plugins can build on each other:

### Include

Pull in another plugin's handlers for all events:

```yaml
# hooks.yaml
include:
  - somecorp/base-hooks
```

### Before/After

Run handlers before or after the main flow:

```yaml
# hooks.yaml
before:
  include:
    - somecorp/pre-checks

after:
  include:
    - somecorp/post-validation
```

Before/after blocks can also define inline event handlers:

```yaml
before:
  session.started:
    - context: "Pre-check: verify environment"
```

### Hook Action

Invoke another plugin's handler for the current event from within a handler:

```yaml
change.completed:
  - hook: "somecorp/formatter"
  - log: "Formatting applied"
```

## Best Practices

- **Keep plugins small and focused** — one plugin per concern (security, formatting, CI integration)
- **Document with comments** — add YAML comments explaining what each handler does
- **Avoid side effects in `permission_asked` handlers** — they gate editor operations; keep them fast and deterministic
- **Use `on_failure: continue`** for non-critical actions — don't block the user's workflow over optional features
- **Test with multiple editors** — behavior may vary across Claude Code, Cursor, and Codex
