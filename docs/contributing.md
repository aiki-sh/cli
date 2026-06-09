# Contributing to Aiki

This guide covers everything you need to develop Aiki itself — the CLI, core flows, built-in templates, and plugins.

## Development Setup

### Build from source

```bash
git clone https://github.com/glasner/aiki.git
cd aiki/cli
cargo build
cargo test
```

To install a local binary for development work, run:

```bash
cargo install --path .
```

The project uses standard Rust tooling. No pinned toolchain version — stable Rust should work.
## Project Structure

```
cli/src/
├── main.rs              # CLI parsing (clap) and dispatch to commands/
├── commands/            # One module per CLI subcommand
│   ├── init.rs          # aiki init
│   ├── doctor.rs        # aiki doctor
│   ├── blame.rs         # aiki blame
│   ├── authors.rs       # aiki authors
│   ├── task.rs          # aiki task (add/start/close/run/etc.)
│   ├── review.rs        # aiki review
│   ├── fix.rs           # aiki fix
│   ├── build.rs         # aiki build
│   ├── plugin.rs        # aiki plugin (install/update/list/remove)
│   ├── session.rs       # aiki session
│   └── ...
├── editors/             # Editor integrations
│   ├── claude_code/     # Claude Code hooks → unified events
│   ├── cursor/          # Cursor hooks → unified events
│   ├── codex/           # Codex OTel traces → unified events
│   └── acp/             # ACP bidirectional proxy (Zed, Neovim)
├── events/              # Unified event system
│   ├── mod.rs           # AikiEvent enum (21 variants)
│   ├── session_started.rs
│   ├── change_completed.rs
│   └── ...
├── flows/               # Flow engine
│   ├── core/            # Bundled aiki/core flow + native Rust functions
│   ├── engine.rs        # Flow execution
│   ├── types.rs         # Hook, Action, HookStatement types
│   ├── composer.rs      # Flow composition (include, before/after)
│   └── variables.rs     # Variable resolution ({{event.*}}, let bindings)
├── tasks/               # Task system
│   ├── manager.rs       # TaskManager (event-sourced state)
│   ├── graph.rs         # Task DAG with typed links
│   ├── runner.rs        # Agent task execution
│   ├── spawner.rs       # Conditional task spawning (spawns: config)
│   └── templates/       # Template parsing and resolution
├── plugins/             # Plugin system (install, deps, scanning)
├── session/             # Session lifecycle (isolation, workspaces)
├── jj/                  # Jujutsu integration (jj-lib bindings)
├── provenance.rs        # [aiki] metadata parsing
├── blame.rs             # Line-level attribution logic
├── event_bus.rs         # Event routing and dispatch
├── error.rs             # AikiError enum (structured errors)
└── config.rs            # Configuration management
```

See the README's [Project Structure](../README.md#project-structure) for the full tree.

## Architecture Overview

The core mental model: **events → flow engine → actions**.

```
┌──────────┐     ┌──────────────┐     ┌─────────────┐     ┌─────────┐
│  Editor   │────▶│  Editor Hook  │────▶│  Unified     │────▶│  Flow   │
│ (Claude,  │     │  (translates  │     │  AikiEvent   │     │  Engine │
│  Cursor,  │     │   to unified  │     │              │     │         │
│  Codex)   │     │   events)     │     │              │     │         │
└──────────┘     └──────────────┘     └──────────────┘     └─────────┘
                                                                │
                                                                ▼
                                                          ┌─────────┐
                                                          │ Actions │
                                                          │ (shell, │
                                                          │  jj,    │
                                                          │  context│
                                                          │  etc.)  │
                                                          └─────────┘
```

1. An AI editor fires a hook (e.g., Claude Code's `PostToolUse`)
2. The editor integration module translates it to a unified `AikiEvent` (e.g., `change.completed`)
3. The event bus routes it to the flow engine
4. The flow engine executes matching handlers from the composed hook stack
5. Handlers run actions (shell commands, JJ operations, context injection, etc.)

The bundled `aiki/core` flow (at `cli/src/flows/core/hooks.yaml`) handles all provenance tracking — creating JJ changes, recording metadata, detecting user edits, and managing workspace isolation.

## Key Abstractions

| Type | Location | Purpose |
|------|----------|---------|
| `AikiEvent` | `events/mod.rs` | Unified event enum (21 variants covering all editor interactions) |
| `Hook` / `HookStatement` / `Action` | `flows/types.rs` | Flow definition types (the YAML maps to these) |
| `FlowEngine` | `flows/engine.rs` | Executes hook statements against events |
| `Composer` | `flows/composer.rs` | Resolves `include:`, `before:`, `after:` composition |
| `TaskManager` | `tasks/manager.rs` | Event-sourced task state management |
| `TaskGraph` | `tasks/graph.rs` | DAG of task relationships with typed links |
| `ProvenanceRecord` | `provenance.rs` | Parses `[aiki]...[/aiki]` metadata from change descriptions |
| `AikiError` | `error.rs` | Structured error types via `thiserror` |

## JJ vs Git Terminology

This is critical for Aiki development. Aiki uses Jujutsu (jj) internally, and the terminology matters:

| Use This | Not This | Why |
|----------|----------|-----|
| **Change** | Commit | JJ's atomic unit is a "change" (mutable) |
| **Change ID** | Commit ID | Change IDs are stable across rewrites |
| **Change description** | Commit message | Where `[aiki]` metadata lives |

**Use "commit" only when:**
- Referring to Git commits (Git interop)
- Calling jj-lib API methods (which use `get_commit()` names)
- Committing JJ transactions (`tx.commit()`)

**Default assumption:** when in doubt, you're talking about a **change** with a **change ID**.

See [`cli/src/CLAUDE.md`](../cli/src/CLAUDE.md) for the full terminology guide.

## Error Handling

Use `AikiError` variants via `thiserror`, not `anyhow::bail!`.

### Adding a New Error

1. Define the variant in `cli/src/error.rs`:

```rust
#[error("Your descriptive message: {0}")]
YourNewError(String),
```

2. Use it in your code:

```rust
use crate::error::{AikiError, Result};

fn my_function() -> Result<()> {
    if bad_condition {
        return Err(AikiError::YourNewError("details".to_string()));
    }
    Ok(())
}
```

**Error message guidelines:**
- Be specific and actionable
- Include context in the variant (paths, names, values)
- Suggest solutions when possible
- List valid options for enum-like inputs

**Exception:** use `anyhow::Result` in modules that heavily interop with jj-lib, since jj-lib returns `BackendError`.

## Adding a New CLI Command

1. Create `cli/src/commands/my_command.rs`:

```rust
use crate::error::Result;

pub fn run(arg1: String) -> Result<()> {
    // Implementation
    Ok(())
}
```

2. Add to `cli/src/commands/mod.rs`:

```rust
pub mod my_command;
```

3. Add the variant and dispatch in `cli/src/main.rs`:

```rust
// In the Commands enum:
/// Description of my command
MyCommand {
    /// Argument description
    #[arg(short, long)]
    arg1: String,
},

// In the match:
Commands::MyCommand { arg1 } => commands::my_command::run(arg1),
```

Every command follows the `pub fn run(...) -> Result<()>` pattern.

## Adding a New Event Type

1. Create the payload struct in `cli/src/events/my_event.rs`:

```rust
use serde::{Deserialize, Serialize};
use super::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiMyEventPayload {
    pub cwd: PathBuf,
    pub session: SessionInfo,
    // ... event-specific fields
}
```

2. Add the variant to `AikiEvent` in `events/mod.rs`:

```rust
#[serde(rename = "my.event")]
MyEvent(AikiMyEventPayload),
```

3. Add the event handler field to `EventHandlers` in `flows/types.rs`:

```rust
#[serde(rename = "my.event", default, deserialize_with = "deserialize_null_as_empty_vec")]
pub my_event: Vec<HookStatement>,
```

4. Wire it into the flow engine's event dispatch in `flows/engine.rs`.

5. Emit it from the appropriate editor integration module.

## Adding a New Flow Action

1. Define the action struct in `flows/types.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MyAction {
    pub my_action: String,
    #[serde(default)]
    pub on_failure: OnFailure,
}
```

2. Add it to the `Action` enum in `flows/types.rs`.

3. Implement execution in `flows/engine.rs`.

## Testing

```bash
# Run all tests
cargo test

# Run a specific test
cargo test test_name

# Run tests in a specific module
cargo test tasks::
```

**Conventions:**
- Unit tests live in `#[cfg(test)] mod tests` blocks within each module
- Integration tests live in `cli/tests/`
- Use temp directories for tests that touch the filesystem
- No network dependencies in tests

## The `ops/` Directory

Planning happens in `ops/` at the repo root:

| Directory | Purpose |
|-----------|---------|
| `ops/now/` | Active plans — work currently in progress |
| `ops/next/` | Upcoming plans — planned but not started |
| `ops/research/` | Research notes and explorations |
| `ops/ROADMAP.md` | Long-term vision |

**Workflow:** write a plan in `ops/now/` before implementing. Plans describe the problem, approach, and deliverables. Mark status as "Done" when complete.

## Code Style

Aiki follows idiomatic Rust patterns:

- **`#[must_use]`** on constructors and builder methods
- **`impl AsRef<Path>`** for path parameters
- **`impl Into<String>`** for string parameters that accept `&str` or `String`
- **Structured errors** via `thiserror` (not `anyhow::bail!`)
- **Command modules** — each CLI command in its own file under `commands/`
- **`pub fn run() -> Result<()>`** — standard entry point for commands

See [`cli/src/CLAUDE.md`](../cli/src/CLAUDE.md) for the full coding standards.

## Submitting Changes

1. Fork and create a feature branch
2. Write your changes following the patterns above
3. Run `cargo test` to verify
4. Run `aiki doctor` to validate configuration
5. Submit a pull request with a clear description of what and why
