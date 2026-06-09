# Claude AI Guidelines for Aiki Development

This document provides important context and guidelines for AI assistants (especially Claude) working on the Aiki codebase.

---

## Table of Contents
1. [JJ vs Git Terminology](#critical-jj-vs-git-terminology)
2. [Error Handling with Structured Types](#error-handling-with-structured-types)
3. [Rust Idioms and Best Practices](#rust-idioms-and-best-practices)
4. [Module Organization](#module-organization)
5. [Architecture](#architecture-change-centric-not-commit-centric)
6. [Metadata Storage](#metadata-storage-pattern)
7. [Task Link Naming](#task-link-naming)
8. [Testing](#testing)

---

## Error Handling with Structured Types

### Core Principle

Aiki uses **structured error types** via `thiserror`, not generic string-based errors.

**DO:**
```rust
use crate::error::{AikiError, Result};

fn parse_agent_type(agent: &str) -> Result<AgentType> {
    match agent {
        "claude-code" => Ok(AgentType::ClaudeCode),
        "cursor" => Ok(AgentType::Cursor),
        _ => Err(AikiError::UnknownAgentType(agent.to_string())),
    }
}
```

**DON'T:**
```rust
// ❌ WRONG: String-based errors
anyhow::bail!("Unknown agent type: '{}'. Supported values: ...", agent);
```

### When to Use Which Error Type

| Situation | Use | Example |
|-----------|-----|---------|
| **New Aiki-specific error** | `AikiError` variant | Repository not found, invalid agent type |
| **JJ-lib interop** | `anyhow::Result` | Working with jj-lib APIs that return `BackendError` |
| **Generic I/O error** | Let `?` convert | File I/O via `#[from] std::io::Error` |
| **Third-party library error** | `AikiError::Other` | Wrap with `.into()` or use `?` |

### Adding New Error Types

**Step 1:** Define the error variant in `cli/src/error.rs`:

```rust
#[derive(Error, Debug)]
pub enum AikiError {
    // ... existing variants ...
    
    #[error("Your descriptive error message here: {0}")]
    YourNewError(String),
    
    #[error("Error with multiple fields: {field1}, {field2}")]
    ComplexError {
        field1: String,
        field2: PathBuf,
    },
}
```

**Step 2:** Use it in your code:

```rust
// Simple variant
return Err(AikiError::YourNewError(value.to_string()));

// Complex variant
return Err(AikiError::ComplexError {
    field1: name.to_string(),
    field2: path.clone(),
});
```

### Error Message Guidelines

1. **Be specific and actionable**
   ```rust
   ✅ #[error("Not in a JJ repository. Run 'jj init' or 'aiki init' first")]
   ❌ #[error("Repository error")]
   ```

2. **Include context in the variant**
   ```rust
   ✅ #[error("File not found: {0}")]
      FileNotFound(PathBuf),
   ❌ #[error("File not found")]
      FileNotFound,
   ```

3. **Suggest solutions when possible**
   ```rust
   ✅ #[error("Invalid timeout format: {0}. Use 's', 'm', or 'h' suffix")]
   ❌ #[error("Invalid timeout: {0}")]
   ```

4. **List valid options for enums**
   ```rust
   ✅ #[error("Unknown agent type: '{0}'. Supported values: 'claude-code', 'cursor'")]
   ❌ #[error("Unknown agent type: '{0}'")]
   ```

### Error Conversion Patterns

#### Pattern 1: Return AikiError directly

```rust
use crate::error::{AikiError, Result};

fn validate_input(input: &str) -> Result<()> {
    if input.is_empty() {
        return Err(AikiError::InvalidInput("Input cannot be empty".to_string()));
    }
    Ok(())
}
```

#### Pattern 2: Convert to anyhow for jj-lib interop

```rust
// In modules that heavily use jj-lib
type Result<T> = anyhow::Result<T>;

fn work_with_jj(repo: &Repo) -> Result<()> {
    let commit = repo.store().get_commit(&commit_id)?;  // jj-lib error
    
    if some_condition {
        return Err(AikiError::YourError("details".to_string()).into());
    }
    
    Ok(())
}
```

#### Pattern 3: Propagate errors across boundaries

```rust
use crate::error::Result;

fn outer() -> Result<()> {
    // vendor functions return anyhow::Result
    Ok(vendor::some_function()?)  // Converts via AikiError::Other
}
```

### Testing Error Types

```rust
#[test]
fn test_error_message() {
    let err = AikiError::UnknownAgentType("vscode".to_string());
    assert_eq!(
        err.to_string(),
        "Unknown agent type: 'vscode'. Supported values: 'claude-code', 'cursor'"
    );
}

#[test]
fn test_error_type() {
    let result = parse_agent_type("invalid");
    assert!(matches!(result, Err(AikiError::UnknownAgentType(_))));
}
```

### Common Error Variants

See `cli/src/error.rs` for the full list. Common ones:

- **Repository**: `NotInJjRepo`, `JjInitFailed`
- **Files**: `FileNotFound(PathBuf)`, `FileNotFoundNoParents`
- **Agents**: `UnknownAgentType(String)`, `UnsupportedAgentType(String)`
- **Flows**: `InvalidLetSyntax(String)`, `InvalidVariableName(String)`, `ActionFailed`
- **Commands**: `JjCommandFailed(String)`, `GitDiffFailed(String)`

### Main Function Error Handling

Always use this pattern for `main()`:

```rust
fn main() {
    if let Err(err) = run() {
        eprintln!("Error: {}", err);  // Uses Display trait
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    // Actual application logic
    Ok(())
}
```

**Why**: Rust's default `main() -> Result<()>` uses Debug formatting, which prints `Error: NotInJjRepo` instead of the user-friendly message. The wrapper ensures Display is used.

---

## Rust Idioms and Best Practices

### Core Principle

Aiki follows idiomatic Rust patterns for better API ergonomics, type safety, and performance.

### Using `#[must_use]` on Constructors and Builder Methods

**DO:** Add `#[must_use]` to constructors and methods that create new values
```rust
impl ActionResult {
    #[must_use]
    pub fn success() -> Self {
        Self {
            success: true,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }
    }
    
    #[must_use]
    pub fn failure(exit_code: i32, stderr: String) -> Self {
        Self {
            success: false,
            exit_code: Some(exit_code),
            stdout: String::new(),
            stderr,
        }
    }
}

impl AikiEvent {
    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}
```

**Why**: The `#[must_use]` attribute generates compiler warnings when the return value is ignored, preventing bugs like:
```rust
// Without #[must_use], this silently does nothing
ActionResult::success();  // Oops, forgot to assign!

// With #[must_use], the compiler warns:
// warning: unused `ActionResult` that must be used
```

**When to use `#[must_use]`:**
- Constructor functions (`new()`, `default()`)
- Result-returning functions (`success()`, `failure()`)
- Builder pattern methods (`with_session_id()`, `with_metadata()`)
- Pure functions that don't modify state

### Using `impl AsRef<Path>` for Path Parameters

**DO:** Use `impl AsRef<Path>` for functions that need to store or use paths
```rust
impl BlameCommand {
    #[must_use]
    pub fn new(repo_path: impl AsRef<Path>) -> Self {
        Self {
            repo_path: repo_path.as_ref().to_path_buf(),
        }
    }
}

impl ExecutionContext {
    #[must_use]
    pub fn new(cwd: impl AsRef<Path>) -> Self {
        Self {
            cwd: cwd.as_ref().to_path_buf(),
            // ...
        }
    }
}
```

**Why**: This allows callers to pass many different types without conversion:
```rust
// All of these work:
let ctx1 = ExecutionContext::new("/tmp");              // &str
let ctx2 = ExecutionContext::new(String::from("/tmp")); // String
let ctx3 = ExecutionContext::new(&s);                  // &String
let ctx4 = ExecutionContext::new(PathBuf::from("/tmp")); // PathBuf
let ctx5 = ExecutionContext::new(&pb);                 // &PathBuf
let ctx6 = ExecutionContext::new(pb.as_path());        // &Path
```

**When to use `impl AsRef<Path>`:**
- Constructor functions that take paths
- Functions that need to convert the path to `PathBuf` for storage
- Functions that only need to read the path

**DON'T use it:**
- For functions that take `&Path` (already optimal for borrowed paths)
- For functions that need to take ownership of a specific `PathBuf`

### `.to_string()` Usage Patterns

**When `.to_string()` is necessary:**
```rust
// ✅ GOOD: Creating owned String for HashMap keys
let mut map = HashMap::new();
map.insert(key.to_string(), value);

// ✅ GOOD: Converting from &str to String when ownership is needed
pub struct Config {
    name: String,  // Owned data
}

impl Config {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),  // Necessary conversion
        }
    }
}

// ✅ GOOD: String formatting requires owned String
format!("Error: {}", err.to_string())
```

**When to avoid `.to_string()`:**
```rust
// ❌ BAD: Unnecessary when impl Into<String> works
pub fn set_name(name: String) { ... }
// call site: set_name("test".to_string());  // Wasteful

// ✅ GOOD: Use Into
pub fn set_name(name: impl Into<String>) { ... }
// call site: set_name("test");  // No conversion needed
```

**Summary:** Most `.to_string()` calls in the codebase are necessary for HashMap keys, owned String fields, or format! macros. Avoid only when a more generic trait (`Into<String>`, `AsRef<str>`) would work.

### Pattern Summary

| Pattern | When to Use | Example |
|---------|-------------|---------|
| `#[must_use]` | Constructors, builders, pure functions | `ActionResult::success()` |
| `impl AsRef<Path>` | Path parameters that need flexibility | `ExecutionContext::new(cwd: impl AsRef<Path>)` |
| `impl Into<String>` | String parameters that accept &str or String | `fn set_name(name: impl Into<String>)` |
| `.to_string()` | Creating owned Strings for storage | `map.insert(key.to_string(), value)` |

---

## Module Organization

### Core Principle

Aiki uses a **command-based module structure** where each CLI command has its own module in `cli/src/commands/`.

### Structure

```
cli/src/
├── main.rs (138 lines) - CLI parsing and dispatch only
└── commands/
    ├── mod.rs - Module exports
    ├── init.rs - Repository initialization
    ├── doctor.rs - Health checks and diagnostics
    ├── hooks.rs - Hook installation and management
    ├── blame.rs - File attribution
    ├── authors.rs - Author extraction
    └── record_change.rs - Legacy change recording
```

### Adding a New Command

**DO:** Create a new module in `commands/`
```rust
// cli/src/commands/my_command.rs
use crate::error::Result;

pub fn run(arg1: String, arg2: bool) -> Result<()> {
    // Command implementation
    Ok(())
}

// Helper functions specific to this command
fn helper_function() -> Result<()> {
    // ...
}
```

**Then add to commands/mod.rs:**
```rust
pub mod my_command;
```

**Then dispatch in main.rs:**
```rust
Commands::MyCommand { arg1, arg2 } => commands::my_command::run(arg1, arg2),
```

### Module Responsibilities

Each command module should:
1. **Export a `run()` function** - Single entry point
2. **Contain command-specific logic** - Don't spread across files
3. **Include helper functions** - Keep them private to the module
4. **Import what it needs** - Be self-contained
5. **Return `Result<()>`** - Use structured errors

### Example: Simple Command

```rust
// cli/src/commands/hello.rs
use crate::error::Result;

pub fn run(name: String) -> Result<()> {
    println!("Hello, {}!", name);
    Ok(())
}
```

### Example: Complex Command with Helpers

```rust
// cli/src/commands/init.rs
use crate::config;
use crate::error::Result;
use crate::jj;
use std::io::{self, Write};

pub fn run(quiet: bool) -> Result<()> {
    if !quiet {
        println!("Initializing...");
    }
    
    let choice = prompt_choice("Select option", 1, 3)?;
    handle_choice(choice)?;
    
    Ok(())
}

// Helper functions - private to this module
fn prompt_choice(prompt: &str, min: usize, max: usize) -> Result<usize> {
    loop {
        print!("{} [{}]: ", prompt, min);
        io::stdout().flush()?;
        
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        
        match input.trim().parse::<usize>() {
            Ok(n) if n >= min && n <= max => return Ok(n),
            _ => println!("Please enter a number between {} and {}", min, max),
        }
    }
}

fn handle_choice(choice: usize) -> Result<()> {
    match choice {
        1 => { /* ... */ }
        2 => { /* ... */ }
        3 => { /* ... */ }
        _ => unreachable!(),
    }
    Ok(())
}
```

### Benefits of This Structure

1. **Clear ownership** - Each command owns its code
2. **Easy navigation** - Find command by module name
3. **Scalable** - New commands don't bloat main.rs
4. **Testable** - Each module can be unit tested
5. **Maintainable** - Changes are localized

### When to Extract a Helper Module

If multiple commands need the same helper function, consider creating a shared module:

```
cli/src/
├── commands/
│   ├── blame.rs
│   └── authors.rs
└── utils/
    └── jj_workspace.rs  # Shared JJ workspace utilities
```

But only do this when:
- ✅ The function is used by 3+ commands
- ✅ The logic is truly generic
- ✅ It has a clear, single responsibility

**Don't prematurely extract** - Keep it in the command module until you have multiple users.

### Anti-Patterns to Avoid

❌ **DON'T put command logic in main.rs**
```rust
// BAD - logic in main.rs
fn run() -> Result<()> {
    match cli.command {
        Commands::Init { quiet } => {
            let current_dir = env::current_dir()?;
            // 50 lines of init logic here...
        }
    }
}
```

✅ **DO dispatch to command modules**
```rust
// GOOD - dispatch only
fn run() -> Result<()> {
    match cli.command {
        Commands::Init { quiet } => commands::init::run(quiet),
    }
}
```

❌ **DON'T create generic "utils" modules prematurely**
```rust
// BAD - unclear responsibility
cli/src/utils.rs  // What goes here? Everything?
```

✅ **DO keep helpers with their commands until needed elsewhere**
```rust
// GOOD - clear ownership
cli/src/commands/init.rs  // Contains prompt_choice()
cli/src/commands/doctor.rs  // Contains prompt_yes_no()
```

### Summary

| Aspect | Guideline |
|--------|-----------|
| **File location** | `cli/src/commands/{command}.rs` |
| **Entry point** | `pub fn run(...) -> Result<()>` |
| **Helper functions** | Keep private in command module |
| **Module exports** | Add to `commands/mod.rs` |
| **Dispatch** | Simple match in `main.rs` |
| **Size limit** | ~300 lines (split if larger) |

---

## Critical: JJ vs Git Terminology

### The Fundamental Distinction

Aiki uses **both Jujutsu (jj) and Git** as independent systems. JJ tracks AI changes with a hidden internal Git backend, while users maintain their own separate Git repository for version control. Their data models are fundamentally different:

| Concept | Git | Jujutsu (jj) |
|---------|-----|--------------|
| **Atomic unit** | Commit (immutable) | **Change** (mutable) |
| **Primary identifier** | Commit SHA (changes on rewrite) | **Change ID** (stable across rewrites) |
| **Metadata location** | Commit message | **Change description** |
| **Content hash** | Commit SHA | Commit ID (transient) |

### Key Terminology Rules

**USE "CHANGE" when referring to:**
- ✅ JJ's atomic unit of version control (working copy state)
- ✅ User-facing error messages about JJ state
- ✅ Documentation describing JJ operations
- ✅ API documentation for provenance tracking
- ✅ Comments explaining JJ behavior

**USE "COMMIT" only when referring to:**
- ✅ Git commits (when interfacing with Git)
- ✅ JJ operation log commits (`tx.commit()` calls)
- ✅ The jj-lib API (which uses `get_commit()` method names)
- ✅ Git import operations

### Examples

**CORRECT:**
```rust
// Get the working copy change ID (stable identifier)
let change_id = commit.change_id();  // API method name is unavoidable

// Error: File not found in any head changes
anyhow::bail!("File not found in any head changes");

/// Parse provenance metadata from change description
pub fn from_description(description: &str) -> Result<Option<Self>>

/// The change_id is a stable identifier that persists across rewrites,
/// unlike commit_id which changes every time the change is rewritten.
```

**INCORRECT:**
```rust
// ❌ WRONG: File not found in any head commits
anyhow::bail!("File not found in any head commits");

// ❌ WRONG: Parse provenance from commit description
pub fn from_description(description: &str) -> Result<Option<Self>>

// ❌ WRONG: commit_id changes when commit content changes
/// unlike commit_id which changes every time the commit content changes.
```

---

## Architecture: Change-Centric, Not Commit-Centric

### Core Principle

Aiki tracks **JJ changes** with stable **change IDs**, not Git commits or JJ commit IDs.

```
┌─────────────────────────────────────────────────┐
│  Working Copy = A "Change" in JJ               │
│  • Has stable change_id (persists on rewrite)  │
│  • Has transient commit_id (changes on rewrite)│
│  • Has mutable description (stores [aiki] data)│
└─────────────────────────────────────────────────┘
```

### Why This Matters

1. **Change IDs are stable** - They don't change when you amend, rebase, or rewrite
2. **Commit IDs are transient** - They change on every rewrite operation
3. **Provenance uses change IDs** - All our metadata references stable change_ids
4. **Git commits are different** - When we export to Git, that's a separate concern

### Implementation Pattern

```rust
// CORRECT PATTERN: Always use change_id as the primary identifier
let change_id = get_working_copy_change_id(&repo_path)?;
set_change_description(&repo_path, &change_id, &provenance)?;

// The jj-lib API uses "commit" in method names, but we're working with changes:
let commit = repo.store().get_commit(&commit_id)?;  // API method name
let change_id = commit.change_id();  // Extract stable identifier
let description = commit.description();  // Get change description

// Document this clearly in comments:
// Load the commit object (represents a change in jj) to get its change_id
```

### JJ and Git Separation

Aiki uses JJ and Git as separate, independent systems:

- **JJ (.jj/)** - Tracks AI changes with [aiki] metadata blocks
  - Uses internal Git backend at `.jj/repo/store/git` (non-colocated mode)
  - Every AI edit creates a JJ change
  - Change descriptions store provenance data
  - Completely independent from user's `.git` directory

- **Git (.git/)** - User's version control system
  - Git commits created only when user runs `git commit`
  - `prepare-commit-msg` hook extracts co-authors from JJ
  - No automatic synchronization with JJ
  - Users control when Git commits are created

This separation ensures:
1. Users control when Git commits are created
2. AI changes are tracked independently in JJ
3. Git history stays clean and user-managed
4. No automatic Git commits on `jj describe` or `jj new`

---

## JJ --help

These are the built in commands for jj. We should also try to take advantage of native functionality before building our own.

Commands:
  abandon           Abandon a revision
  absorb            Move changes from a revision into the stack of mutable revisions
  bisect            Find a bad revision by bisection
  bookmark          Manage bookmarks [default alias: b]
  commit            Update the description and create a new change on top [default alias: ci]
  config            Manage config options
  describe          Update the change description or other metadata [default alias: desc]
  diff              Compare file contents between two revisions
  diffedit          Touch up the content changes in a revision with a diff editor
  duplicate         Create new changes with the same content as existing ones
  edit              Sets the specified revision as the working-copy revision
  evolog            Show how a change has evolved over time [aliases: evolution-log]
  file              File operations
  fix               Update files with formatting fixes or other changes
  gerrit            Interact with Gerrit Code Review
  git               Commands for working with Git remotes and the underlying Git repo
  help              Print this message or the help of the given subcommand(s)
  interdiff         Compare the changes of two commits
  log               Show revision history
  metaedit          Modify the metadata of a revision without changing its content
  new               Create a new, empty change and (by default) edit it in the working copy
  next              Move the working-copy commit to the child revision
  operation         Commands for working with the operation log [aliases: op]
  parallelize       Parallelize revisions by making them siblings
  prev              Change the working copy revision relative to the parent revision
  rebase            Move revisions to different parent(s)
  redo              Redo the most recently undone operation
  resolve           Resolve conflicted files with an external merge tool
  restore           Restore paths from another revision
  revert            Apply the reverse of the given revision(s)
  root              Show the current workspace root directory (shortcut for `jj workspace root`)
  show              Show commit description and changes in a revision
  sign              Cryptographically sign a revision
  simplify-parents  Simplify parent edges for the specified revision(s)
  sparse            Manage which paths from the working-copy commit are present in the working copy
  split             Split a revision in two
  squash            Move changes from a revision into another revision
  status            Show high-level repo status [default alias: st]
  tag               Manage tags
  undo              Undo the last operation
  unsign            Drop a cryptographic signature
  util              Infrequently used commands such as for generating shell completions
  version           Display version information
  workspace         Commands for working with workspaces
  
---


## Code Review Checklist

When reviewing or writing code, check for these common mistakes:

### ❌ Common Mistakes

1. **Using "commit" when referring to JJ changes**
   - Error messages mentioning "commits" instead of "changes"
   - Comments saying "commit description" instead of "change description"
   - Documentation referring to "commit IDs" instead of "change IDs"

2. **Confusing commit_id with change_id**
   - Using commit_id as a stable identifier (it's not!)
   - Storing commit_id in metadata instead of change_id
   - Comparing commit_ids across rewrites (they'll differ)

3. **Mixing Git and JJ concepts**
   - Calling JJ operations "git commits"
   - Using Git terminology for JJ-specific features

### ✅ Best Practices

1. **Be explicit about what you're referring to**
   ```rust
   // GOOD: Clear that we're working with JJ changes
   /// Get the stable change_id from the working copy
   fn get_working_copy_change_id(repo_path: &str) -> Result<String>
   
   // BAD: Ambiguous
   /// Get the working copy ID
   fn get_working_copy_id(repo_path: &str) -> Result<String>
   ```

2. **Add clarifying comments when using jj-lib API**
   ```rust
   // GOOD: Explains the API's terminology
   // Load the commit object (represents a change in jj)
   let commit = repo.store().get_commit(&commit_id)?;
   let change_id = commit.change_id();  // The stable change identifier
   
   // BAD: No context
   let commit = repo.store().get_commit(&commit_id)?;
   let change_id = commit.change_id();
   ```

3. **Use consistent terminology in user-facing output**
   ```rust
   // GOOD: Uses "change" terminology
   eprintln!("  Change ID: {}", change_id);
   
   // BAD: Confusing mix
   eprintln!("  Commit: {}", change_id);
   ```

---

## Metadata Storage Pattern

### The `[aiki]...[/aiki]` Block

All provenance metadata is stored in JJ **change descriptions** using this format:

```
[aiki]
agent=claude-code
session=claude-session-abc123
tool=Edit
confidence=High
method=Hook
[/aiki]
```

**Key points:**
- Stored in the **change description** (not commit message)
- Change description is mutable (can be updated via `jj describe`)
- Associated with a **change_id** (stable) not commit_id (transient)
- Retrieved from JJ when needed (not stored separately)

### Why No Separate Database?

Originally, Aiki considered using SQLite to store provenance. We pivoted to storing everything in JJ change descriptions because:

1. **Single source of truth** - JJ already has all the data (diffs, timestamps, files)
2. **No sync issues** - Metadata can't drift from the actual changes
3. **Simpler architecture** - No database to manage or migrate
4. **JJ-native** - Uses JJ's own data model correctly
5. **Queryable** - Can use JJ revsets: `jj log -r 'description("agent=claude-code")'`

---

## Task Link Naming

When creating new link types for the task DAG, follow these rules so that the CLI reads naturally and the direction is unambiguous.

### The Sentence Rule

`aiki task link A --predicate B` must read as a sentence. Passive links use **"A is [predicate] B"**; active links drop the "is": **"A [predicate] B"**.

| Link | Reads as |
|------|----------|
| `link epic --implements plan.md` | "epic implements-plan plan.md" |
| `link build --orchestrates epic` | "build orchestrates epic" |
| `link task --blocked-by other` | "task is blocked-by other" |
| `link subtask --subtask-of parent` | "subtask is subtask-of parent" |
| `link task --sourced-from file` | "task is sourced-from file" |
| `link fix --fixes task:target` | "fix fixes task:target" |
| `link new-plan --supersedes old-plan` | "new-plan supersedes old-plan" |

### Argument Roles

- **A** (first positional arg) is always the entity that was created later, or exists in service of B.
- **B** (flag value) is always the **anchor** — the thing A depends on, derives from, or serves.

### Allowed Grammar

Link predicates use one of these patterns:

| Pattern | Examples |
|---------|----------|
| `[active-verb]` | `orchestrates`, `supersedes`, `fixes` |
| `[active-verb]-[noun]` | `implements-plan`, `decomposes-plan`, `adds-plan` |
| `[noun]-of` | `subtask-of` |
| `[past-participle]-by` | `blocked-by`, `spawned-by` |
| `[past-participle]-from` | `sourced-from` |
| `[active-verb]-on` | `depends-on` |
| `[active-verb]` | `validates`, `remediates` |

### Adding a New Link Type

1. Write the sentence: "A _____ B". If it doesn't read naturally, pick a different predicate.
2. Verify A is the dependent/later/service entity and B is the anchor.
3. Choose a grammar pattern from the table above.
4. Add forward and reverse indexes to `TaskGraph`.

---

## Common Scenarios

### Scenario 1: Adding New Fields to Provenance

**DO:**
```rust
// Add to the [aiki] block format
format!(
    "[aiki]\nagent={}\nsession={}\ntool={}\nnew_field={}\n[/aiki]",
    agent_type, session_id, tool_name, new_value
)
```

**DON'T:**
```rust
// Don't create a separate storage mechanism
// Don't use commit_id as identifier
// Don't store redundant data that JJ already has
```

### Scenario 2: Querying Provenance

**DO:**
```rust
// Use change_id to look up changes
let change_id = get_working_copy_change_id(repo)?;
let commit = repo.store().get_commit_by_change_id(&change_id)?;
let description = commit.description();
let provenance = ProvenanceRecord::from_description(description)?;
```

**DON'T:**
```rust
// Don't query by commit_id (it changes on rewrite)
// Don't cache change descriptions separately
// Don't duplicate JJ's native queries
```

### Scenario 3: Error Messages

**DO:**
```rust
anyhow::bail!("File not found in any head changes. Has the file been tracked in jj?");
```

**DON'T:**
```rust
anyhow::bail!("File not found in any head commits. Has it been committed?");
```

---

## When "Commit" is Correct

There are legitimate uses of "commit" in this codebase:

### 1. JJ Operation Log

```rust
// CORRECT: This commits a transaction to the operation log
tx.commit("aiki: embed provenance metadata")?;
```

This is a JJ operation, not a version control commit.

### 2. Git Interop

```rust
// CORRECT: Importing Git commits
pub fn git_import(&self) -> Result<()> {
    git::import_refs(tx.repo_mut(), &git_settings)?;
    // ...
}
```

When actually dealing with Git, "commit" is correct.

### 3. JJ-lib API Methods

```rust
// CORRECT: The API method is named get_commit()
let commit = repo.store().get_commit(&commit_id)?;
```

The jj-lib API uses "commit" in method names. This is unavoidable but should be documented:

```rust
// Load the commit object (represents a change in jj)
let commit = repo.store().get_commit(&commit_id)?;
```

---

## Testing

When writing tests, maintain the change/commit distinction:

```rust
#[test]
fn test_change_description_metadata() {  // ✅ GOOD: Uses "change"
    let change_id = get_working_copy_change_id(temp_dir.path())?;
    set_change_description(&change_id, &provenance)?;
    // ...
}

#[test]
fn test_commit_metadata() {  // ❌ BAD: Ambiguous
    // ...
}
```

---

## Documentation

When documenting Aiki's architecture:

**DO:**
- Explain the change/commit distinction upfront
- Use "change" for JJ's atomic unit
- Use "change ID" for stable identifiers
- Use "change description" for metadata storage
- Add a "JJ Terminology" section to major docs

**DON'T:**
- Assume readers know JJ terminology
- Use "commit" and "change" interchangeably
- Skip explaining the stability of change_id

---

## Summary: Quick Reference

| Term | Meaning | When to Use |
|------|---------|-------------|
| **Change** | JJ's atomic unit (mutable) | Most of the time in Aiki |
| **Change ID** | Stable identifier (hex string) | Primary identifier for provenance |
| **Change description** | Metadata field on a change | Where [aiki] blocks live |
| **Commit** | Git's atomic unit OR jj operation | Git interop, operation log |
| **Commit ID** | Content hash (transient in jj) | Internal jj-lib usage only |

**Default assumption:** When in doubt, you're probably talking about a **change** with a **change ID** in a **change description**.

---

## Questions?

If you're unsure whether to use "commit" or "change":

1. Are you referring to something in JJ's data model? → Use **"change"**
2. Are you calling a jj-lib API method? → Use the API's terminology but add a comment
3. Are you interfacing with Git? → Use **"commit"**
4. Are you committing a JJ transaction? → Use **"commit"** (operation log)

**When in doubt, use "change"** - it's correct 90% of the time in Aiki's codebase.

---

## Related Documentation

**IMPORTANT:** The `ops/` folder is at the **repo root** (`aiki/ops/`), not in `cli/`. Always use:
- `ops/now/` - Current work and active plans
- `ops/done/` - Completed work
- `ops/future/` - Future plans and ideas

Documentation:
- `ops/CHANGE_ID_IMPLEMENTATION.md` - Technical deep-dive on change_id vs commit_id
- `ops/phase-1.md` - Architecture docs (now includes terminology guide)
- `ops/ROADMAP.md` - Strategic vision showing the change-centric approach
- **ACP Protocol Specification**: https://agentclientprotocol.com/protocol/schema - Official Agent Client Protocol schema for session/update notifications, tool calls, and file tracking
