//! Task management CLI commands
//!
//! Provides the `aiki task` command with subcommands:
//! - `add` - Create a new task
//! - `list` - Show ready queue (default)
//! - `start` - Start working on task(s)
//! - `stop` - Stop current task
//! - `close` - Close task(s) as done

use clap::Subcommand;
use std::path::Path;

use crate::agents::AgentType;
use crate::error::{AikiError, Result};

/// Name for the synthetic `.0` subtask auto-created when starting a parent task.
const DIGEST_SUBTASK_NAME: &str = "Digest subtasks and start first batch";

/// Placeholder prefix/suffix for parent.subtasks.{slug} deferred resolution.
/// During template processing, {{parent.subtasks.criteria}} becomes
/// __AIKI_SUBTASK_SLUG_criteria__ which is replaced with the actual task ID
/// after all sibling subtask IDs are generated.
const SUBTASK_SLUG_PLACEHOLDER_PREFIX: &str = "__AIKI_SUBTASK_SLUG_";
const SUBTASK_SLUG_PLACEHOLDER_SUFFIX: &str = "__";
use crate::events::{AikiEvent, AikiTaskClosedPayload, AikiTaskStartedPayload, TaskEventPayload};
use std::collections::{HashMap, HashSet};

use crate::tasks::types::FastHashMap;

use crate::tasks::{
    generate_task_id, is_task_id, is_task_id_prefix,
    manager::{
        find_task, find_task_in_graph, get_current_scope_set, get_in_progress,
        get_ready_queue_for_agent_scoped, get_ready_queue_for_scope_set, has_subtasks,
        resolve_task_id_in_graph, ScopeSet,
    },
    materialize_graph, materialize_graph_with_ids,
    md::{
        aiki_print, build_context, build_list_output, build_transition_context,
        format_action_added, format_action_closed, format_action_commented, format_action_started,
        format_action_stopped, format_instructions, format_task_list, short_id,
    },
    reopen_if_closed,
    runner::{run_task_with_output, TaskRunOptions},
    storage::{
        read_events, read_events_with_ids, write_event, write_events_batch, write_link_event,
        write_link_event_with_autorun,
    },
    types::{Task, TaskEvent, TaskOutcome, TaskPriority, TaskStatus},
    MdBuilder, TaskGraph,
};

/// Valid prefixes for task sources
const VALID_SOURCE_PREFIXES: &[&str] = &["file:", "task:", "comment:", "issue:", "prompt:"];

/// Validate that all sources have valid prefixes
///
/// Sources must start with one of: file:, task:, comment:, issue:, prompt:
/// The special source "prompt" (without colon) is also valid and will be resolved
/// to the latest prompt's change_id for the current session.
/// Returns an error with the first invalid source if validation fails.
fn validate_sources(sources: &[String]) -> Result<()> {
    for source in sources {
        // "prompt" without colon is special - will be resolved later
        if source == "prompt" {
            continue;
        }
        let has_valid_prefix = VALID_SOURCE_PREFIXES
            .iter()
            .any(|prefix| source.starts_with(prefix));
        if !has_valid_prefix {
            return Err(AikiError::InvalidTaskSource(source.clone()));
        }
    }
    Ok(())
}

/// Resolve "prompt" source to the actual prompt change_id
///
/// If `--source prompt` is used (without an explicit ID), this function resolves it
/// to `prompt:<change_id>` using the latest prompt from the current session.
///
/// Returns the sources with "prompt" replaced, or an error if resolution fails.
fn resolve_prompt_sources(cwd: &Path, mut sources: Vec<String>) -> Result<Vec<String>> {
    use crate::global;
    use crate::history::get_latest_prompt_change_id;
    use crate::session::find_active_session;

    // Check if any source is the special "prompt" (without ID)
    let has_bare_prompt = sources.iter().any(|s| s == "prompt");
    if !has_bare_prompt {
        return Ok(sources);
    }

    // Find the current session via PID or agent-type detection
    let session = find_active_session(cwd).ok_or(AikiError::NoActiveSessionForPromptSource)?;

    // Get the latest prompt's change_id for this session.
    // Conversation history is stored in the global JJ repo at ~/.aiki/, not the project repo.
    // The prompt event may not be written yet (hook fires concurrently with event recording),
    // so retry a few times with backoff before giving up.
    let global_dir = global::global_aiki_dir();
    let mut prompt_change_id = None;
    for attempt in 0..10 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(50 * attempt));
        }
        if let Some(id) = get_latest_prompt_change_id(&global_dir, &session.session_id)? {
            prompt_change_id = Some(id);
            break;
        }
    }

    let change_id = prompt_change_id.ok_or(AikiError::NoPromptEventsForSession)?;

    // Replace "prompt" with "prompt:<change_id>"
    for source in &mut sources {
        if source == "prompt" {
            *source = format!("prompt:{}", change_id);
        }
    }

    Ok(sources)
}

/// Parse --data key=value arguments into a HashMap
///
/// If `coerce` is true, values are type-coerced (booleans/numbers normalized).
/// If `coerce` is false, values are stored verbatim.
fn parse_data_flags(
    args: &[String],
    coerce: bool,
) -> Result<std::collections::HashMap<String, String>> {
    use crate::tasks::templates::coerce_to_string;

    let mut data = std::collections::HashMap::new();
    for arg in args {
        let (key, value) = arg
            .split_once('=')
            .ok_or_else(|| AikiError::InvalidDataFormat(arg.clone()))?;
        let value = if coerce {
            coerce_to_string(value)
        } else {
            value.to_string()
        };
        data.insert(key.to_string(), value);
    }
    Ok(data)
}

/// Resolve `--subtask-of` / `--parent` alias pair.
///
/// Returns the canonical value or errors if both are provided.
fn resolve_subtask_of_alias(
    subtask_of: Option<String>,
    parent: Option<String>,
) -> Result<Option<String>> {
    match (&subtask_of, &parent) {
        (Some(_), Some(_)) => Err(AikiError::InvalidArgument(
            "Cannot use both --subtask-of and --parent (they are aliases)".to_string(),
        )),
        _ => Ok(subtask_of.or(parent)),
    }
}

/// Resolve `--sourced-from` / `--source` alias pair for Vec fields.
///
/// Returns the merged sources or errors if both are provided.
fn resolve_sourced_from_alias(
    sourced_from: Vec<String>,
    source: Vec<String>,
) -> Result<Vec<String>> {
    if !sourced_from.is_empty() && !source.is_empty() {
        return Err(AikiError::InvalidArgument(
            "Cannot use both --sourced-from and --source (they are aliases)".to_string(),
        ));
    }
    let mut all = sourced_from;
    all.extend(source);
    Ok(all)
}

/// Resolve `--sourced-from` / `--source` alias pair for Option fields (Link/Unlink).
///
/// Returns the canonical value or errors if both are provided.
fn resolve_sourced_from_option_alias(
    sourced_from: Option<String>,
    source: Option<String>,
) -> Result<Option<String>> {
    match (&sourced_from, &source) {
        (Some(_), Some(_)) => Err(AikiError::InvalidArgument(
            "Cannot use both --sourced-from and --source (they are aliases)".to_string(),
        )),
        _ => Ok(sourced_from.or(source)),
    }
}

/// Infer task type from task properties
///
/// Looks at task name and sources to determine type:
/// - "review" if task name contains "review" or has task: source (follow-up)
/// - "bug" if task name contains "fix" or "bug"
/// - "feature" otherwise (default)
fn infer_task_type(task: &Task) -> String {
    let name_lower = task.name.to_lowercase();

    // Check name patterns
    if name_lower.contains("review") {
        return "review".to_string();
    }
    if name_lower.contains("fix") || name_lower.contains("bug") {
        return "bug".to_string();
    }

    // Check sources for task: prefix (indicates follow-up/review)
    if task.sources.iter().any(|s| s.starts_with("task:")) {
        return "review".to_string();
    }

    // Default to feature
    "feature".to_string()
}

/// Template subcommands for `aiki task template`
#[derive(Subcommand)]
pub enum TemplateCommands {
    /// List all available templates
    List,

    /// Show details of a specific template
    Show {
        /// Template name (e.g., "aiki/review")
        name: String,
    },
}

/// Task subcommands
#[derive(Subcommand)]
pub enum TaskCommands {
    /// Show ready queue (default when no subcommand given)
    List {
        /// Show all tasks (not just ready queue)
        #[arg(long)]
        all: bool,

        /// Filter to open tasks only
        #[arg(long)]
        open: bool,

        /// Filter to in-progress tasks only
        #[arg(long)]
        in_progress: bool,

        /// Filter to stopped tasks only
        #[arg(long)]
        stopped: bool,

        /// Filter to closed tasks only
        #[arg(long)]
        closed: bool,

        /// Filter to tasks assigned to specific agent or human
        #[arg(long = "assignee", value_name = "AGENT")]
        assignee: Option<String>,

        /// Filter to unassigned tasks only
        #[arg(long)]
        unassigned: bool,

        /// Filter to tasks from a specific source (supports partial matching)
        #[arg(long)]
        source: Option<String>,

        /// Filter to tasks created from a specific template (e.g., "aiki/review", "myorg/build@1.0")
        #[arg(long)]
        template: Option<String>,
    },

    /// List or show templates
    Template {
        #[command(subcommand)]
        command: TemplateCommands,
    },

    /// Create a new task
    ///
    /// Create a task either by name or from a template.
    ///
    /// Examples:
    ///   aiki task add "Implement user auth"
    ///   aiki task add --template aiki/review --data scope="@"
    ///   aiki task add --template myorg/build --source file:ops/now/feature.md
    Add {
        /// Task name (required unless --template is provided)
        name: Option<String>,

        /// Create from a template (e.g., "aiki/review", "myorg/refactor-cleanup")
        #[arg(long)]
        template: Option<String>,

        /// Set task data (for template-based tasks). Can be specified multiple times.
        #[arg(long, value_name = "KEY=VALUE", action = clap::ArgAction::Append)]
        data: Vec<String>,

        /// Create as child of existing task (hidden alias for --subtask-of)
        #[arg(long, hide = true)]
        parent: Option<String>,

        /// Stable slug for this subtask (e.g., "build", "run-tests")
        #[arg(long)]
        slug: Option<String>,

        /// Assign to specific agent or human (claude-code, codex, cursor, gemini, human)
        #[arg(long = "assignee", value_name = "AGENT")]
        assignee: Option<String>,

        /// Source that spawned this task (hidden alias for --sourced-from)
        #[arg(long, hide = true, action = clap::ArgAction::Append)]
        source: Vec<String>,

        /// Task(s) that block this one
        #[arg(long, action = clap::ArgAction::Append)]
        blocked_by: Vec<String>,

        /// Task this supersedes
        #[arg(long)]
        supersedes: Option<String>,

        /// Sources that spawned this task (e.g., "file:ops/now/design.md", "task:abc123")
        /// Can be specified multiple times
        #[arg(long, action = clap::ArgAction::Append)]
        sourced_from: Vec<String>,

        /// Parent task this is a subtask of
        #[arg(long)]
        subtask_of: Option<String>,

        /// Plan file this task implements (emits implements-plan link)
        #[arg(long)]
        implements: Option<String>,

        /// Epic this orchestrator drives (orchestrator → epic)
        #[arg(long)]
        orchestrates: Option<String>,

        /// Target(s) this task fixes (file or task). Can be specified multiple times.
        #[arg(long, action = clap::ArgAction::Append)]
        fixes: Vec<String>,

        /// Plan file this task decomposes
        #[arg(long)]
        decomposes_plan: Option<String>,

        /// Plan file this task adds
        #[arg(long)]
        adds_plan: Option<String>,

        /// Task(s) this depends on (blocks ready state). Can be specified multiple times.
        #[arg(long, action = clap::ArgAction::Append)]
        depends_on: Vec<String>,

        /// Task(s) this validates (review relationship, blocks ready state). Can be specified multiple times.
        #[arg(long, action = clap::ArgAction::Append)]
        validates: Vec<String>,

        /// Task(s) this remediates (fix relationship, blocks ready state). Can be specified multiple times.
        #[arg(long, action = clap::ArgAction::Append)]
        remediates: Vec<String>,

        /// Auto-start this task when its blocker(s) close
        #[arg(long)]
        autorun: bool,

        /// Skip loop iterations (sets data.options.once = true)
        #[arg(long)]
        once: bool,

        /// Set priority to P0 (critical/urgent)
        #[arg(long, group = "priority")]
        p0: bool,

        /// Set priority to P1 (high)
        #[arg(long, group = "priority")]
        p1: bool,

        /// Set priority to P2 (normal, default)
        #[arg(long, group = "priority")]
        p2: bool,

        /// Set priority to P3 (low)
        #[arg(long, group = "priority")]
        p3: bool,
    },

    /// Start working on task(s)
    ///
    /// Accepts either task ID(s), a description, or --template for template-based tasks.
    ///
    /// Examples:
    ///   aiki task start "Implement user auth"  # Quick-start: create and start
    ///   aiki task start xmryrzwl...           # Start existing task by ID
    ///   aiki task start --template aiki/review --data scope="@"  # Create from template and start
    Start {
        /// Task ID(s) or description to start
        ///
        /// If a description (not a task ID), creates and starts a new task.
        #[arg(value_name = "ID_OR_DESCRIPTION")]
        ids: Vec<String>,

        /// Create from template and start (quick-start pattern for templates)
        #[arg(long)]
        template: Option<String>,

        /// Set task data (for template-based tasks). Can be specified multiple times.
        #[arg(long, value_name = "KEY=VALUE", action = clap::ArgAction::Append)]
        data: Vec<String>,

        /// Reopen a closed task before starting
        #[arg(long)]
        reopen: bool,

        /// Reason for reopening (required with --reopen)
        #[arg(long, requires = "reopen")]
        reason: Option<String>,

        /// Set priority to P0 (critical/urgent) for new task
        #[arg(long, group = "priority")]
        p0: bool,

        /// Set priority to P1 (high) for new task
        #[arg(long, group = "priority")]
        p1: bool,

        /// Set priority to P2 (normal, default) for new task
        #[arg(long, group = "priority")]
        p2: bool,

        /// Set priority to P3 (low) for new task
        #[arg(long, group = "priority")]
        p3: bool,

        /// Source that spawned this task (hidden alias for --sourced-from)
        #[arg(long, hide = true, action = clap::ArgAction::Append)]
        source: Vec<String>,

        /// Task(s) that block this one
        #[arg(long, action = clap::ArgAction::Append)]
        blocked_by: Vec<String>,

        /// Task this supersedes
        #[arg(long)]
        supersedes: Option<String>,

        /// Sources that spawned this task (e.g., "file:ops/now/design.md", "task:abc123")
        /// Can be specified multiple times
        #[arg(long, action = clap::ArgAction::Append)]
        sourced_from: Vec<String>,

        /// Parent task this is a subtask of (for quick-start)
        #[arg(long)]
        subtask_of: Option<String>,

        /// Create as child of existing task (hidden alias for --subtask-of)
        #[arg(long, hide = true)]
        parent: Option<String>,

        /// Override template assignee
        #[arg(long = "assignee", value_name = "AGENT")]
        assignee: Option<String>,

        /// Stable slug for this subtask (for quick-start, e.g., "build", "run-tests")
        #[arg(long)]
        slug: Option<String>,

        /// Plan file this task implements (emits implements-plan link)
        #[arg(long)]
        implements: Option<String>,

        /// Epic this orchestrator drives (orchestrator → epic)
        #[arg(long)]
        orchestrates: Option<String>,

        /// Target(s) this task fixes (file or task). Can be specified multiple times.
        #[arg(long, action = clap::ArgAction::Append)]
        fixes: Vec<String>,

        /// Plan file this task decomposes
        #[arg(long)]
        decomposes_plan: Option<String>,

        /// Plan file this task adds
        #[arg(long)]
        adds_plan: Option<String>,

        /// Task(s) this depends on (blocks ready state). Can be specified multiple times.
        #[arg(long, action = clap::ArgAction::Append)]
        depends_on: Vec<String>,

        /// Task(s) this validates (review relationship, blocks ready state). Can be specified multiple times.
        #[arg(long, action = clap::ArgAction::Append)]
        validates: Vec<String>,

        /// Task(s) this remediates (fix relationship, blocks ready state). Can be specified multiple times.
        #[arg(long, action = clap::ArgAction::Append)]
        remediates: Vec<String>,

        /// Auto-start this task when its blocker(s) close
        #[arg(long)]
        autorun: bool,

        /// Skip loop iterations (sets data.options.once = true)
        #[arg(long)]
        once: bool,
    },

    /// Stop task(s)
    Stop {
        /// Task ID(s) to stop (defaults to current in-progress task)
        #[arg(value_name = "ID")]
        ids: Vec<String>,

        /// Reason for stopping
        #[arg(long)]
        reason: Option<String>,

        /// Create blocker task(s) (assigned to human). Can be specified multiple times.
        #[arg(long, action = clap::ArgAction::Append)]
        blocked: Vec<String>,

        /// Force stop even if task is claimed by another session (for orchestrator cleanup)
        #[arg(long)]
        force: bool,
    },

    /// Close task(s) as done
    Close {
        /// Task ID(s) to close (defaults to current in-progress task)
        #[arg(value_name = "ID")]
        ids: Vec<String>,

        /// Closure outcome: done (default), wont_do
        #[arg(long, default_value = "done")]
        outcome: String,

        /// Shortcut for --outcome wont_do
        #[arg(long)]
        wont_do: bool,

        /// Summary of what was accomplished (use "-" for stdin)
        #[arg(long)]
        summary: Option<String>,
    },

    /// Show task details (including subtasks for parent tasks)
    Show {
        /// Task ID to show (defaults to current in-progress task)
        id: Option<String>,

        /// Show full diffs for all changes made during this task
        #[arg(long)]
        diff: bool,

        /// Expand source references (task: name+instructions, file: content, prompt: text, comment: text+data)
        #[arg(long)]
        with_source: bool,

        /// Include instructions in output (hidden by default)
        #[arg(long)]
        with_instructions: bool,
    },

    /// Set fields on a task
    Set {
        /// Task ID (defaults to current in-progress task)
        id: Option<String>,

        /// Set priority to P0 (critical/urgent)
        #[arg(long, group = "priority")]
        p0: bool,

        /// Set priority to P1 (high)
        #[arg(long, group = "priority")]
        p1: bool,

        /// Set priority to P2 (normal)
        #[arg(long, group = "priority")]
        p2: bool,

        /// Set priority to P3 (low)
        #[arg(long, group = "priority")]
        p3: bool,

        /// Set task name
        #[arg(long)]
        name: Option<String>,

        /// Assign to specific agent or human (claude-code, codex, cursor, gemini, human)
        #[arg(long = "assignee", value_name = "AGENT")]
        assignee: Option<String>,

        /// Set or update a data field (can be specified multiple times)
        #[arg(long, value_name = "KEY=VALUE", action = clap::ArgAction::Append)]
        data: Vec<String>,

        /// Set instructions (reads content from stdin)
        #[arg(long)]
        instructions: bool,
    },

    /// Clear optional fields on a task
    Unset {
        /// Task ID (defaults to current in-progress task)
        id: Option<String>,

        /// Clear assignee field
        #[arg(long)]
        assignee: bool,

        /// Clear instructions field
        #[arg(long)]
        instructions: bool,

        /// Clear data field(s) by key. Can be specified multiple times.
        #[arg(long, value_name = "KEY", action = clap::ArgAction::Append)]
        data: Vec<String>,
    },

    /// Add a comment to a task
    Comment {
        /// Task ID to comment on (defaults to current in-progress task)
        id: Option<String>,

        /// Comment text (required)
        text: String,

        /// Add structured data to the comment. Can be specified multiple times.
        #[arg(long, value_name = "KEY=VALUE", action = clap::ArgAction::Append)]
        data: Vec<String>,
    },

    /// Run a task by spawning an agent session
    ///
    /// Spawns an agent session to work on the specified task. The agent will:
    /// 1. Claim the task via `aiki task start`
    /// 2. Execute the task (following instructions/subtasks)
    /// 3. Close the task when complete
    Run {
        /// Task ID to run
        id: String,

        /// Override assignee agent (claude-code, codex)
        #[arg(long)]
        agent: Option<String>,

        /// Run asynchronously (spawn agent and return immediately)
        #[arg(long = "async")]
        run_async: bool,

        /// Automatically pick and run the next ready subtask of the given parent task
        #[arg(long)]
        next_subtask: bool,
    },

    /// Wait for task(s) to complete
    ///
    /// Polls task status until all specified tasks reach a terminal state
    /// (closed or stopped). Outputs each task's final status and closing comment.
    ///
    /// Examples:
    ///   aiki task wait abc123                         # Wait for one task
    ///   aiki task wait abc123 def456                  # Wait for multiple tasks
    ///   aiki task wait abc123 --timeout 300           # Wait up to 5 minutes
    Wait {
        /// Task IDs to wait for
        #[arg(required = true)]
        ids: Vec<String>,
    },

    /// Undo file changes made by a task
    ///
    /// Reverts file changes made by a task or set of tasks, restoring files
    /// to their state before the task started. Creates a backup bookmark by default.
    ///
    /// Examples:
    ///   aiki task undo abc123...                    # Undo a single task
    ///   aiki task undo abc123 def456                # Undo multiple tasks
    ///   aiki task undo abc123 --completed           # Undo completed subtasks of an epic
    ///   aiki task undo abc123 --dry-run             # Preview what would be undone
    Undo {
        /// Task ID(s) to undo
        #[arg(required = true, value_name = "ID")]
        ids: Vec<String>,

        /// For epic tasks: undo all completed subtasks
        #[arg(long)]
        completed: bool,

        /// Force undo despite conflicts (may lose manual edits)
        #[arg(long)]
        force: bool,

        /// Show what would be undone without making changes
        #[arg(long)]
        dry_run: bool,

        /// Skip backup bookmark creation
        #[arg(long)]
        no_backup: bool,
    },

    /// Add a link between tasks
    ///
    /// Creates a relationship between two tasks. The first argument is the
    /// subject task; the flag names the relationship and takes the target.
    ///
    /// Examples:
    ///   aiki task link B --blocked-by A     # B is blocked by A
    ///   aiki task link A --sourced-from file:design.md
    ///   aiki task link child --subtask-of parent
    Link {
        /// Subject task (the "from" node)
        id: String,

        /// Task that blocks this one (from can't start until target closes)
        #[arg(long)]
        blocked_by: Option<String>,

        /// Task this depends on (blocks ready state)
        #[arg(long)]
        depends_on: Option<String>,

        /// Task this validates (review relationship, blocks ready state)
        #[arg(long)]
        validates: Option<String>,

        /// Task this remediates (fix relationship, blocks ready state)
        #[arg(long)]
        remediates: Option<String>,

        /// Origin this task came from (task ID or external ref)
        #[arg(long)]
        sourced_from: Option<String>,

        /// Origin (hidden alias for --sourced-from)
        #[arg(long, hide = true)]
        source: Option<String>,

        /// Parent task this is a subtask of
        #[arg(long)]
        subtask_of: Option<String>,

        /// Parent task (hidden alias for --subtask-of)
        #[arg(long, hide = true)]
        parent: Option<String>,

        /// Plan file this task implements (emits implements-plan link)
        #[arg(long)]
        implements: Option<String>,

        /// Epic this orchestrator drives
        #[arg(long)]
        orchestrates: Option<String>,

        /// Predecessor this task replaces
        #[arg(long)]
        supersedes: Option<String>,

        /// Target this task fixes (file or task)
        #[arg(long)]
        fixes: Option<String>,

        /// Plan file this task decomposes
        #[arg(long)]
        decomposes_plan: Option<String>,

        /// Plan file this task adds
        #[arg(long)]
        adds_plan: Option<String>,
    },

    /// Remove a link between tasks
    ///
    /// Examples:
    ///   aiki task unlink B --blocked-by A
    Unlink {
        /// Subject task (the "from" node)
        id: String,

        /// Remove blocked-by link to this target
        #[arg(long)]
        blocked_by: Option<String>,

        /// Remove depends-on link to this target
        #[arg(long)]
        depends_on: Option<String>,

        /// Remove validates link to this target
        #[arg(long)]
        validates: Option<String>,

        /// Remove remediates link to this target
        #[arg(long)]
        remediates: Option<String>,

        /// Remove sourced-from link to this target
        #[arg(long)]
        sourced_from: Option<String>,

        /// Remove sourced-from link (hidden alias for --sourced-from)
        #[arg(long, hide = true)]
        source: Option<String>,

        /// Remove subtask-of link to this target
        #[arg(long)]
        subtask_of: Option<String>,

        /// Remove subtask-of link (hidden alias for --subtask-of)
        #[arg(long, hide = true)]
        parent: Option<String>,

        /// Remove implements-plan link to this target
        #[arg(long)]
        implements: Option<String>,

        /// Remove orchestrates link to this target
        #[arg(long)]
        orchestrates: Option<String>,

        /// Remove supersedes link to this target
        #[arg(long)]
        supersedes: Option<String>,

        /// Remove fixes link to this target
        #[arg(long)]
        fixes: Option<String>,

        /// Remove decomposes-plan link to this target
        #[arg(long)]
        decomposes_plan: Option<String>,

        /// Remove adds-plan link to this target
        #[arg(long)]
        adds_plan: Option<String>,
    },

    /// Show diff of changes made while working on a task
    ///
    /// Shows the net result of all changes made during a task (baseline → final).
    /// Uses jj revsets to derive the baseline from provenance metadata.
    ///
    /// Examples:
    ///   aiki task diff abc123...     # Full diff for task
    ///   aiki task diff abc123 -s     # Summary (file paths with +/- counts)
    ///   aiki task diff abc123 --stat # Histogram of changes
    Diff {
        /// Task ID to show diff for (required)
        id: String,

        /// Show summary (file paths with +/- counts)
        #[arg(short = 's', long)]
        summary: bool,

        /// Show histogram of changes
        #[arg(long)]
        stat: bool,

        /// Show only changed file names
        #[arg(long)]
        name_only: bool,
    },

    /// Reset all tasks (close as won't-do)
    ///
    /// Requires `--confirm reset` to proceed. This is a destructive operation
    /// that closes all non-closed tasks.
    ///
    /// Examples:
    ///   aiki task reset --confirm reset
    Reset {
        /// Type "reset" to confirm (required)
        #[arg(long)]
        confirm: Option<String>,
    },
}

/// Main entry point for `aiki task` command
///
/// If no subcommand is provided, defaults to `list`.
pub fn run(command: Option<TaskCommands>) -> Result<()> {
    let cwd = std::env::current_dir()?;

    // Default to list if no subcommand provided
    let cmd = command.unwrap_or(TaskCommands::List {
        all: false,
        open: false,
        in_progress: false,
        stopped: false,
        closed: false,
        assignee: None,
        unassigned: false,
        source: None,
        template: None,
    });

    match cmd {
        TaskCommands::List {
            all,
            open,
            in_progress,
            stopped,
            closed,
            assignee,
            unassigned,
            source,
            template,
        } => run_list(
            &cwd,
            None,
            all,
            open,
            in_progress,
            stopped,
            closed,
            assignee,
            unassigned,
            source,
            template,
        ),
        TaskCommands::Template { command } => run_template(&cwd, command),
        TaskCommands::Add {
            name,
            template,
            data,
            parent,
            slug,
            assignee,
            source,
            blocked_by,
            supersedes,
            sourced_from,
            subtask_of,
            implements,
            orchestrates,
            fixes,
            decomposes_plan,
            adds_plan,
            depends_on,
            validates,
            remediates,
            autorun,
            once,
            p0,
            p1,
            p2,
            p3,
        } => run_add(
            &cwd,
            name,
            template,
            data,
            resolve_subtask_of_alias(subtask_of, parent)?,
            slug,
            assignee,
            resolve_sourced_from_alias(sourced_from, source)?,
            blocked_by,
            supersedes,
            implements,
            orchestrates,
            fixes,
            decomposes_plan,
            adds_plan,
            depends_on,
            validates,
            remediates,
            autorun,
            once,
            p0,
            p1,
            p2,
            p3,
        ),
        TaskCommands::Start {
            ids,
            template,
            data,
            reopen,
            reason,
            p0,
            p1,
            p2,
            p3,
            source,
            blocked_by,
            supersedes,
            sourced_from,
            subtask_of,
            parent,
            assignee,
            slug,
            implements,
            orchestrates,
            fixes,
            decomposes_plan,
            adds_plan,
            depends_on,
            validates,
            remediates,
            autorun,
            once,
        } => run_start(
            &cwd,
            ids,
            template,
            data,
            reopen,
            reason,
            p0,
            p1,
            p2,
            p3,
            resolve_sourced_from_alias(sourced_from, source)?,
            blocked_by,
            supersedes,
            resolve_subtask_of_alias(subtask_of, parent)?,
            assignee,
            slug,
            implements,
            orchestrates,
            fixes,
            decomposes_plan,
            adds_plan,
            depends_on,
            validates,
            remediates,
            autorun,
            once,
        ),
        TaskCommands::Stop {
            ids,
            reason,
            blocked,
            force,
        } => run_stop(&cwd, ids, reason, blocked, force),
        TaskCommands::Close {
            ids,
            outcome,
            wont_do,
            summary,
        } => run_close(&cwd, ids, &outcome, wont_do, summary),
        TaskCommands::Show {
            id,
            diff,
            with_source,
            with_instructions,
        } => run_show(&cwd, id, diff, with_source, with_instructions),
        TaskCommands::Set {
            id,
            p0,
            p1,
            p2,
            p3,
            name,
            assignee,
            data,
            instructions,
        } => run_set(&cwd, id, p0, p1, p2, p3, name, assignee, data, instructions),
        TaskCommands::Unset {
            id,
            assignee,
            instructions,
            data,
        } => {
            // Validate that at least one field is specified
            if !assignee && !instructions && data.is_empty() {
                let xml = MdBuilder::new("unset").error().build_error(
                    "No fields specified. Use --assignee, --instructions, or --data <key>",
                );
                aiki_print(&xml);
                Ok(())
            } else {
                run_unset(&cwd, id, assignee, instructions, data)
            }
        }

        TaskCommands::Comment { id, text, data } => run_comment(&cwd, id, text, data),
        TaskCommands::Run {
            id,
            agent,
            run_async,
            next_subtask,
        } => run_run(&cwd, id, agent, run_async, next_subtask),
        TaskCommands::Wait { ids } => run_wait(&cwd, ids),
        TaskCommands::Undo {
            ids,
            completed,
            force,
            dry_run,
            no_backup,
        } => run_undo(&cwd, ids, completed, force, dry_run, no_backup),
        TaskCommands::Link {
            id,
            blocked_by,
            depends_on,
            validates,
            remediates,
            sourced_from,
            source,
            subtask_of,
            parent,
            implements,
            orchestrates,
            supersedes,
            fixes,
            decomposes_plan,
            adds_plan,
        } => run_link(
            &cwd,
            id,
            blocked_by,
            depends_on,
            validates,
            remediates,
            resolve_sourced_from_option_alias(sourced_from, source)?,
            resolve_subtask_of_alias(subtask_of, parent)?,
            implements,
            orchestrates,
            supersedes,
            fixes,
            decomposes_plan,
            adds_plan,
        ),
        TaskCommands::Unlink {
            id,
            blocked_by,
            depends_on,
            validates,
            remediates,
            sourced_from,
            source,
            subtask_of,
            parent,
            implements,
            orchestrates,
            supersedes,
            fixes,
            decomposes_plan,
            adds_plan,
        } => run_unlink(
            &cwd,
            id,
            blocked_by,
            depends_on,
            validates,
            remediates,
            resolve_sourced_from_option_alias(sourced_from, source)?,
            resolve_subtask_of_alias(subtask_of, parent)?,
            implements,
            orchestrates,
            supersedes,
            fixes,
            decomposes_plan,
            adds_plan,
        ),
        TaskCommands::Diff {
            id,
            summary,
            stat,
            name_only,
        } => run_diff(&cwd, id, summary, stat, name_only),
        TaskCommands::Reset { confirm } => run_reset(&cwd, confirm),
    }
}

/// List tasks in the ready queue
fn run_list(
    cwd: &Path,
    scope_override: Option<&str>,
    all: bool,
    filter_open: bool,
    filter_in_progress: bool,
    filter_stopped: bool,
    filter_closed: bool,
    filter_assignee: Option<String>,
    filter_unassigned: bool,
    filter_source: Option<String>,
    filter_template: Option<String>,
) -> Result<()> {
    use crate::agents::{AgentType, Assignee};
    use crate::session::find_active_session;

    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let tasks = &graph.tasks;

    // Determine scope set from override or current in-progress tasks
    let scope_set = if let Some(s) = scope_override {
        ScopeSet {
            include_root: false,
            scopes: vec![s.to_string()],
        }
    } else {
        get_current_scope_set(&graph)
    };

    // Collect active status filters
    let has_status_filters = filter_open || filter_in_progress || filter_stopped || filter_closed;
    let has_explicit_assignee_filters = filter_assignee.is_some() || filter_unassigned;

    // Validate and normalize assignee filter if provided
    // Converts "claude" → "claude-code", "me" → "human"
    let normalized_filter_assignee = if let Some(ref a) = filter_assignee {
        match Assignee::from_str(a) {
            Some(parsed) => parsed.as_str().map(|s| s.to_string()),
            None => return Err(AikiError::UnknownAssignee(a.clone())),
        }
    } else {
        None
    };

    // Session detection: find session by PID matching or agent-type fallback
    // This automatically finds our session without needing --session flag
    let session_match = find_active_session(cwd);
    let detected_agent: Option<AgentType> = session_match.as_ref().map(|m| m.agent_type);
    let our_session_uuid: Option<String> = session_match.map(|m| m.session_id);

    // Determine automatic assignee filtering based on session context
    // If no explicit filter is set and not --all, apply visibility rules:
    // - Agent detected: show tasks assigned to that agent + unassigned
    // - No agent detected: show tasks assigned to human + unassigned (human terminal mode)
    let auto_agent_filter: Option<AgentType> = if !all && !has_explicit_assignee_filters {
        detected_agent
    } else {
        None
    };

    // Whether we should apply human visibility filtering (no agent detected)
    let apply_human_filter = !all && !has_explicit_assignee_filters && detected_agent.is_none();

    // Helper closure to check if a task is visible based on auto-filtering
    let is_auto_visible = |task: &Task| -> bool {
        if all {
            return true; // --all bypasses all auto-filtering
        }

        let assignee = task
            .assignee
            .as_ref()
            .and_then(|s| Assignee::from_str(s))
            .unwrap_or(Assignee::Unassigned);

        if let Some(ref agent) = auto_agent_filter {
            assignee.is_visible_to(agent)
        } else if apply_human_filter {
            // Human mode: show human-assigned + unassigned
            assignee.is_visible_to_human()
        } else {
            true // No auto-filtering
        }
    };

    // Helper closure to check explicit assignee filter
    let matches_explicit_filter = |task: &Task| -> bool {
        if !has_explicit_assignee_filters {
            return true;
        }
        if filter_unassigned {
            task.assignee.is_none()
        } else if let Some(ref target) = normalized_filter_assignee {
            task.assignee.as_ref().map(|a| a == target).unwrap_or(false)
        } else {
            true
        }
    };

    // Helper closure to check session ownership
    // Task is visible if: unclaimed OR claimed by our session
    let matches_session = |task: &Task| -> bool {
        if all {
            return true; // --all bypasses session filtering
        }
        match (&task.claimed_by_session, &our_session_uuid) {
            (None, _) => true,                              // Unclaimed tasks visible to all
            (Some(claimed), Some(ours)) => claimed == ours, // Claimed tasks visible only to owner
            (Some(_), None) => false, // Claimed tasks not visible without session
        }
    };

    // Helper closure to check source filter
    // Supports partial matching: "ops/now/assign.md" matches "file:ops/now/assign.md"
    // Uses graph's sourced-from edges for source matching (includes both
    // old-style task.sources indexed at materialization and explicit LinkAdded events,
    // with LinkRemoved properly removing edges).
    let matches_source = |task: &Task| -> bool {
        match &filter_source {
            None => true, // No filter applied
            Some(query) => {
                let source_match = |source: &str| -> bool {
                    // Exact match
                    source == query ||
                    // Partial match: query without prefix matches source
                    source.ends_with(query) ||
                    // Partial match: source without prefix matches query
                    source.split(':').nth(1).map_or(false, |suffix| suffix == query)
                };
                // Check graph's sourced-from edges (handles LinkAdded and LinkRemoved correctly)
                graph
                    .edges
                    .targets(&task.id, "sourced-from")
                    .iter()
                    .any(|s| source_match(s))
            }
        }
    };

    // Helper closure to check template filter
    // Supports exact match and version-agnostic matching:
    // - "aiki/review" matches "aiki/review" and "aiki/review@1.0.0"
    // - "aiki/review@1.0.0" only matches "aiki/review@1.0.0"
    let matches_template = |task: &Task| -> bool {
        match (&filter_template, &task.template) {
            (None, _) => true,        // No filter applied
            (Some(_), None) => false, // Filter applied but task has no template
            (Some(query), Some(task_template)) => {
                // Exact match
                task_template == query ||
                // Version-agnostic match: query without version matches task_template with version
                task_template.split('@').next() == Some(query)
            }
        }
    };

    // Always compute the actual ready queue for context (maintains contract)
    // Blocking is filtered internally by ready queue functions, then apply agent/human AND session filtering
    let ready_queue: Vec<&Task> = if let Some(ref agent) = auto_agent_filter {
        get_ready_queue_for_agent_scoped(&graph, &scope_set, agent)
            .into_iter()
            .filter(|t| matches_session(t))
            .collect()
    } else if apply_human_filter {
        // Human mode: filter to human-visible tasks
        get_ready_queue_for_scope_set(&graph, &scope_set)
            .into_iter()
            .filter(|t| is_auto_visible(t) && matches_session(t))
            .collect()
    } else {
        get_ready_queue_for_scope_set(&graph, &scope_set)
            .into_iter()
            .filter(|t| matches_session(t))
            .collect()
    };

    // Get list of tasks based on filters (for display in content)
    let has_active_filters = all
        || has_status_filters
        || has_explicit_assignee_filters
        || filter_source.is_some()
        || filter_template.is_some();

    let list_tasks: Vec<&Task> = if has_active_filters {
        // Show tasks with filters applied
        let mut all_tasks: Vec<_> = tasks.values().collect();
        all_tasks.sort_by(|a, b| a.priority.cmp(&b.priority));

        // Apply status filters if active
        let filtered_by_status: Vec<_> = if has_status_filters {
            all_tasks
                .into_iter()
                .filter(|t| {
                    (filter_open && t.status == TaskStatus::Open)
                        || (filter_in_progress && t.status == TaskStatus::InProgress)
                        || (filter_stopped && t.status == TaskStatus::Stopped)
                        || (filter_closed && t.status == TaskStatus::Closed)
                })
                .collect()
        } else {
            all_tasks
        };

        // Apply explicit assignee filters if active
        let filtered_by_assignee: Vec<_> = if has_explicit_assignee_filters {
            filtered_by_status
                .into_iter()
                .filter(|t| matches_explicit_filter(t))
                .collect()
        } else {
            filtered_by_status
        };

        // Apply source filter if active
        let filtered_by_source: Vec<_> = if filter_source.is_some() {
            filtered_by_assignee
                .into_iter()
                .filter(|t| matches_source(t))
                .collect()
        } else {
            filtered_by_assignee
        };

        // Apply template filter if active
        let filtered_by_template: Vec<_> = if filter_template.is_some() {
            filtered_by_source
                .into_iter()
                .filter(|t| matches_template(t))
                .collect()
        } else {
            filtered_by_source
        };

        // Apply auto visibility filter (unless --all is specified or explicit filter is used)
        // This ensures status filters still respect assignee visibility
        // Also apply session filtering
        let filtered_by_visibility: Vec<_> = if !all && !has_explicit_assignee_filters {
            filtered_by_template
                .into_iter()
                .filter(|t| is_auto_visible(t))
                .collect()
        } else {
            filtered_by_template
        };

        // Apply session filtering
        filtered_by_visibility
            .into_iter()
            .filter(|t| matches_session(t))
            .collect()
    } else {
        // Default: show ready queue (same as context)
        ready_queue.clone()
    };

    // Get in-progress tasks, filtered by:
    // 1. Explicit assignee filter (--assignee/--unassigned) if specified
    // 2. Otherwise, auto visibility filter based on session context
    // 3. Session ownership filter
    let in_progress: Vec<&Task> = get_in_progress(&tasks)
        .into_iter()
        .filter(|t| {
            let assignee_visible = if has_explicit_assignee_filters {
                matches_explicit_filter(t)
            } else {
                is_auto_visible(t)
            };
            assignee_visible && matches_session(t)
        })
        .collect();

    let output = if has_active_filters {
        // Filtered view: show filtered list + context (via MdBuilder)
        let content = format_task_list(&list_tasks);
        let mut builder = MdBuilder::new("list");
        let xml_scopes = scope_set.to_xml_scopes();
        if !xml_scopes.is_empty() {
            builder = builder.with_scopes(&xml_scopes);
        }
        builder.build(&content, &in_progress, &ready_queue)
    } else {
        // Default view: nav hint header + context
        build_list_output(&in_progress, &ready_queue)
    };

    aiki_print(&output);
    Ok(())
}

/// Add a new task
fn run_add(
    cwd: &Path,
    name: Option<String>,
    template_name: Option<String>,
    data_args: Vec<String>,
    parent: Option<String>,
    slug: Option<String>,
    assignee_arg: Option<String>,
    sources: Vec<String>,
    blocked_by: Vec<String>,
    supersedes: Option<String>,
    implements: Option<String>,
    orchestrates: Option<String>,
    fixes: Vec<String>,
    decomposes_plan: Option<String>,
    adds_plan: Option<String>,
    depends_on: Vec<String>,
    validates: Vec<String>,
    remediates: Vec<String>,
    autorun: bool,
    once: bool,
    p0: bool,
    p1: bool,
    _p2: bool,
    p3: bool,
) -> Result<()> {
    use crate::agents::Assignee;

    // Validate --autorun requires at least one blocking link flag
    if autorun
        && blocked_by.is_empty()
        && depends_on.is_empty()
        && validates.is_empty()
        && remediates.is_empty()
    {
        return Err(AikiError::InvalidArgument(
            "--autorun requires a blocking link flag (--blocked-by, --depends-on, --validates, or --remediates)".to_string()
        ));
    }

    // If --template is provided, delegate to template-based creation
    if let Some(ref template) = template_name {
        // Template-based creation doesn't support --parent (templates define their own structure)
        if parent.is_some() {
            return Err(AikiError::InvalidArgument(
                "--parent cannot be used with --template (templates define their own task structure)".to_string()
            ));
        }

        // Validate source prefixes
        validate_sources(&sources)?;

        // Resolve "prompt" source to actual prompt change_id
        let sources = resolve_prompt_sources(cwd, sources)?;

        // Parse data arguments (with type coercion for template variable substitution)
        let data = parse_data_flags(&data_args, true)?;

        // Add options.once if flag is set
        let mut data = data;
        if once {
            data.insert("options.once".to_string(), "true".to_string());
        }
        // Resolve assignee
        let assignee = if let Some(ref a) = assignee_arg {
            match crate::agents::Assignee::from_str(a) {
                Some(parsed) => parsed.as_str().map(|s| s.to_string()),
                None => return Err(AikiError::UnknownAssignee(a.clone())),
            }
        } else {
            None
        };

        // Determine priority from flags
        let priority = if p0 {
            Some(TaskPriority::P0)
        } else if p1 {
            Some(TaskPriority::P1)
        } else if p3 {
            Some(TaskPriority::P3)
        } else {
            None // Let template defaults apply
        };

        let params = TemplateTaskParams {
            template_name: template.clone(),
            data,
            sources,
            assignee,
            priority,
            ..Default::default()
        };
        let task_id = create_from_template(cwd, params)?;

        // Read events to get the task we just created
        let events = read_events(cwd)?;
        let tasks = materialize_graph(&events).tasks;

        let task = tasks
            .get(&task_id)
            .ok_or_else(|| AikiError::TaskNotFound(task_id.clone()))?;

        // Slim output: single line confirmation
        aiki_print(&format_action_added(task));
        return Ok(());
    }

    // Manual task creation requires a name
    let name = name.ok_or_else(|| {
        AikiError::InvalidArgument(
            "Task name required. Either provide a name or use --template".to_string(),
        )
    })?;

    // Validate and normalize assignee if provided
    // This converts aliases like "claude" → "claude-code", "me" → "human"
    let assignee = if let Some(ref a) = assignee_arg {
        match Assignee::from_str(a) {
            Some(parsed) => parsed.as_str().map(|s| s.to_string()),
            None => return Err(AikiError::UnknownAssignee(a.clone())),
        }
    } else {
        None
    };

    // Validate source prefixes
    validate_sources(&sources)?;

    // Resolve "prompt" source to actual prompt change_id
    let sources = resolve_prompt_sources(cwd, sources)?;

    // Determine priority from flags (default P2)
    let priority = if p0 {
        TaskPriority::P0
    } else if p1 {
        TaskPriority::P1
    } else if p3 {
        TaskPriority::P3
    } else {
        TaskPriority::P2 // Default, also covers explicit --p2
    };

    // Read current state first
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let tasks = &graph.tasks;

    // Validate slug format if provided
    if let Some(ref s) = slug {
        if !crate::tasks::is_valid_slug(s) {
            return Err(AikiError::InvalidSlug(s.clone()));
        }
    }

    // Determine task ID and possibly inherit parent's assignee
    let (task_id, effective_assignee) = if let Some(ref parent_id) = parent {
        // Validate parent exists; if closed, implicitly reopen it
        let parent_task = find_task_in_graph(&graph, parent_id)?;
        let parent_id = &parent_task.id; // rebind to canonical ID
        reopen_if_closed(cwd, parent_id, &tasks, "Subtasks added")?;

        // Validate slug uniqueness within parent
        if let Some(ref s) = slug {
            crate::tasks::graph::validate_slug_unique(&graph, parent_id, s)?;
        }

        // Generate subtask ID (full 32-char ID, linked via subtask-of edge)
        let child_id = generate_task_id(&name);

        // Inherit parent's assignee if none specified
        let final_assignee = if assignee.is_some() {
            assignee.clone()
        } else {
            parent_task.assignee.clone()
        };

        (child_id, final_assignee)
    } else {
        // Root-level task with new JJ-style ID
        (generate_task_id(&name), assignee.clone())
    };

    let timestamp = chrono::Utc::now();

    let working_copy = get_working_copy_change_id(cwd);

    let event = TaskEvent::Created {
        task_id: task_id.clone(),
        name: name.clone(),
        slug: slug.clone(),
        task_type: None,
        priority,
        assignee: effective_assignee.clone(),
        sources: sources.clone(),
        template: None,
        working_copy: working_copy.clone(),
        instructions: None,
        data: std::collections::HashMap::new(),
        timestamp,
    };

    write_event(cwd, &event)?;

    // Emit subtask-of link if this is a child task
    if let Some(ref parent_id) = parent {
        let parent_id = &find_task_in_graph(&graph, parent_id)?.id.clone();
        write_link_event(cwd, &graph, "subtask-of", &task_id, parent_id)?;
    }

    // Emit sourced-from links for each source
    for source in &sources {
        write_link_event(cwd, &graph, "sourced-from", &task_id, source)?;
    }

    // Emit additional link flags
    emit_link_flags(
        cwd,
        &graph,
        &task_id,
        &blocked_by,
        &depends_on,
        &validates,
        &remediates,
        &supersedes,
        &implements,
        &orchestrates,
        &fixes,
        &decomposes_plan,
        &adds_plan,
        autorun,
    )?;

    // Build new task from event (avoid re-reading)
    let new_task = Task {
        id: task_id,
        name,
        slug,
        task_type: None,
        priority,
        status: TaskStatus::Open,
        assignee: effective_assignee,
        sources,
        template: None,
        working_copy,
        instructions: None,
        data: std::collections::HashMap::new(),
        created_at: timestamp,
        started_at: None,
        claimed_by_session: None,
        last_session_id: None,
        stopped_reason: None,
        closed_outcome: None,
        summary: None,
        turn_started: None,
        turn_closed: None,
        turn_stopped: None,
        comments: Vec::new(),
    };

    // Slim output: single line confirmation
    aiki_print(&format_action_added(&new_task));
    Ok(())
}

/// Start working on task(s)
fn run_start(
    cwd: &Path,
    ids: Vec<String>,
    template_name: Option<String>,
    data_args: Vec<String>,
    reopen: bool,
    reopen_reason: Option<String>,
    p0: bool,
    p1: bool,
    _p2: bool,
    p3: bool,
    sources: Vec<String>,
    blocked_by: Vec<String>,
    supersedes: Option<String>,
    subtask_of: Option<String>,
    assignee_arg: Option<String>,
    slug: Option<String>,
    implements: Option<String>,
    orchestrates: Option<String>,
    fixes: Vec<String>,
    decomposes_plan: Option<String>,
    adds_plan: Option<String>,
    depends_on: Vec<String>,
    validates: Vec<String>,
    remediates: Vec<String>,
    autorun: bool,
    once: bool,
) -> Result<()> {
    use crate::session::find_active_session;

    // Validate --autorun requires at least one blocking link flag
    if autorun
        && blocked_by.is_empty()
        && depends_on.is_empty()
        && validates.is_empty()
        && remediates.is_empty()
    {
        return Err(AikiError::InvalidArgument(
            "--autorun requires a blocking link flag (--blocked-by, --depends-on, --validates, or --remediates)".to_string()
        ));
    }

    // If --template is provided, create from template and start
    if let Some(ref template) = template_name {
        // Validate source prefixes
        validate_sources(&sources)?;

        // Resolve "prompt" source to actual prompt change_id
        let resolved_sources = resolve_prompt_sources(cwd, sources.clone())?;

        // Parse data arguments
        let data = parse_data_flags(&data_args, true)?;

        // Add options.once if flag is set
        let mut data = data;
        if once {
            data.insert("options.once".to_string(), "true".to_string());
        }
        // Resolve assignee
        let assignee = if let Some(ref a) = assignee_arg {
            match crate::agents::Assignee::from_str(a) {
                Some(parsed) => parsed.as_str().map(|s| s.to_string()),
                None => return Err(AikiError::UnknownAssignee(a.clone())),
            }
        } else {
            None
        };

        // Determine priority from flags
        let priority = if p0 {
            Some(TaskPriority::P0)
        } else if p1 {
            Some(TaskPriority::P1)
        } else if p3 {
            Some(TaskPriority::P3)
        } else {
            None
        };

        // Create task from template first
        let params = TemplateTaskParams {
            template_name: template.clone(),
            data,
            sources: resolved_sources,
            assignee,
            priority,
            ..Default::default()
        };
        let task_id = create_from_template(cwd, params)?;
        // Now start that task - recursive call with just the task ID
        return run_start(
            cwd,
            vec![task_id],
            None,
            Vec::new(),
            false,
            None,
            false,
            false,
            false,
            false,
            Vec::new(),
            Vec::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            Vec::new(),
            None,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            false,
            false, // once
        );
    }

    // Validate source prefixes (if any sources provided for quick-start)
    validate_sources(&sources)?;

    // Resolve "prompt" source to actual prompt change_id
    let sources = resolve_prompt_sources(cwd, sources)?;

    // Determine priority for new task (if quick-start is used)
    let priority = if p0 {
        TaskPriority::P0
    } else if p1 {
        TaskPriority::P1
    } else if p3 {
        TaskPriority::P3
    } else {
        TaskPriority::default() // P2
    };
    let events = read_events(cwd)?;
    let mut graph = materialize_graph(&events);
    let mut tasks = graph.tasks.clone();

    // Detect current session early - needed for start event
    let session_match = find_active_session(cwd);
    let our_session_id = session_match.as_ref().map(|m| m.session_id.clone());

    let current_scope_set = get_current_scope_set(&graph);
    let ready = get_ready_queue_for_scope_set(&graph, &current_scope_set);

    // Track if we created a new task (for output formatting)
    let mut created_new_task: Option<Task> = None;

    // Determine which task(s) to start
    let ids_to_start = if ids.is_empty() {
        // Default: start first from ready queue
        if let Some(first) = ready.first() {
            vec![first.id.clone()]
        } else {
            return Err(AikiError::NoTasksReady);
        }
    } else if ids.len() == 1 && !is_task_id(&ids[0]) {
        // Single arg that's not a full task ID — could be prefix or description
        let mut resolved = None;
        if is_task_id_prefix(&ids[0]) || ids[0].contains(':') {
            match resolve_task_id_in_graph(&graph, &ids[0]) {
                Ok(full_id) => resolved = Some(full_id),
                Err(AikiError::TaskNotFound(_)) => {} // fall through to quick-start
                Err(e) => return Err(e),              // ambiguous → error
            }
        }

        if let Some(full_id) = resolved {
            vec![full_id]
        } else {
            // Quick-start: create a new task from the description
            let description = &ids[0];
            let task_id = generate_task_id(description);
            let timestamp = chrono::Utc::now();
            let working_copy = get_working_copy_change_id(cwd);

            // Validate slug format if provided for quick-start
            if let Some(ref s) = slug {
                if !crate::tasks::is_valid_slug(s) {
                    return Err(AikiError::InvalidSlug(s.clone()));
                }
            }

            let create_event = TaskEvent::Created {
                task_id: task_id.clone(),
                name: description.clone(),
                slug: slug.clone(),
                task_type: None,
                priority,
                assignee: None,
                sources: sources.clone(),
                template: None,
                working_copy: working_copy.clone(),
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp,
            };
            write_event(cwd, &create_event)?;

            // Emit sourced-from links for each source
            for source in &sources {
                write_link_event(cwd, &graph, "sourced-from", &task_id, source)?;
            }

            // Emit subtask-of link if --subtask-of was provided
            if let Some(ref parent_id) = subtask_of {
                let parent_id = &find_task_in_graph(&graph, parent_id)?.id.clone();
                write_link_event(cwd, &graph, "subtask-of", &task_id, parent_id)?;
            }

            // Emit additional link flags
            emit_link_flags(
                cwd,
                &graph,
                &task_id,
                &blocked_by,
                &depends_on,
                &validates,
                &remediates,
                &supersedes,
                &implements,
                &orchestrates,
                &fixes,
                &decomposes_plan,
                &adds_plan,
                autorun,
            )?;

            let new_task = Task {
                id: task_id.clone(),
                name: description.clone(),
                slug: slug.clone(),
                task_type: None,
                status: TaskStatus::Open,
                priority,
                assignee: None,
                sources: sources.clone(),
                template: None,
                working_copy,
                instructions: None,
                data: std::collections::HashMap::new(),
                created_at: timestamp,
                started_at: None,
                claimed_by_session: None,
                last_session_id: None,
                stopped_reason: None,
                closed_outcome: None,
                summary: None,
                turn_started: None,
                turn_closed: None,
                turn_stopped: None,
                comments: Vec::new(),
            };
            tasks.insert(task_id.clone(), new_task.clone());
            created_new_task = Some(new_task);

            vec![task_id]
        }
    } else {
        // Resolve all IDs (prefix → full) and validate
        let mut resolved_ids = Vec::new();
        for id in &ids {
            let full_id = resolve_task_id_in_graph(&graph, id)?;
            let task = graph
                .tasks
                .get(&full_id)
                .ok_or_else(|| AikiError::TaskNotFound(full_id.clone()))?;
            if task.status == TaskStatus::Closed {
                if !reopen {
                    let xml = MdBuilder::new("start").error().build_error(&format!(
                        "Task '{}' is closed. Use --reopen --reason to reopen it.",
                        full_id
                    ));
                    aiki_print(&xml);
                    return Ok(());
                }
                if reopen_reason.is_none() {
                    let xml = MdBuilder::new("start")
                        .error()
                        .build_error("--reopen requires --reason");
                    aiki_print(&xml);
                    return Ok(());
                }
            }
            resolved_ids.push(full_id);
        }
        resolved_ids
    };

    // Reopen closed tasks if --reopen was specified
    if reopen {
        if let Some(reason) = &reopen_reason {
            for id in &ids_to_start {
                if let Ok(task) = find_task(&tasks, id) {
                    if task.status == TaskStatus::Closed {
                        let reopen_event = TaskEvent::Reopened {
                            task_id: id.clone(),
                            reason: reason.clone(),
                            timestamp: chrono::Utc::now(),
                        };
                        write_event(cwd, &reopen_event)?;

                        // Update local task state
                        if let Some(t) = tasks.get_mut(id) {
                            t.status = TaskStatus::Open;
                            t.closed_outcome = None;
                        }
                    }
                }
            }
        }
    }

    // Check if we're starting a parent task with subtasks
    // If so, auto-create a planning task (.0) and start that instead
    let mut actual_ids_to_start = ids_to_start.clone();

    if ids_to_start.len() == 1 {
        let task_id = ids_to_start[0].clone();
        if has_subtasks(&graph, &task_id) {
            // Starting a parent task - find or create a digest task among children
            let existing_digest = graph
                .edges
                .referrers(&task_id, "subtask-of")
                .iter()
                .find(|child_id| {
                    graph.tasks.get(*child_id).is_some_and(|t| {
                        t.name == DIGEST_SUBTASK_NAME && t.status != TaskStatus::Closed
                    })
                })
                .cloned();

            let digest_task_id = if let Some(id) = existing_digest {
                id
            } else {
                // Create the planning task with a full 32-char ID
                let id = generate_task_id(DIGEST_SUBTASK_NAME);
                let timestamp = chrono::Utc::now();
                let working_copy = get_working_copy_change_id(cwd);
                let planning_event = TaskEvent::Created {
                    task_id: id.clone(),
                    name: DIGEST_SUBTASK_NAME.to_string(),
                    slug: None,
                    task_type: None,
                    priority: TaskPriority::default(),
                    assignee: None,
                    sources: Vec::new(),
                    template: None,
                    working_copy: working_copy.clone(),
                    instructions: None,
                    data: std::collections::HashMap::new(),
                    timestamp,
                };
                write_event(cwd, &planning_event)?;
                write_link_event(cwd, &graph, "subtask-of", &id, &task_id)?;

                // Add the subtask-of edge to the in-memory graph so the
                // parent preservation lookup (graph.edges.target) works
                // without needing the new_scope workaround.
                graph.edges.add(&id, &task_id, "subtask-of");

                // Add to local tasks map for output
                let task = Task {
                    id: id.clone(),
                    name: DIGEST_SUBTASK_NAME.to_string(),
                    slug: None,
                    task_type: None,
                    status: TaskStatus::Open,
                    priority: TaskPriority::default(),
                    assignee: None,
                    sources: Vec::new(),
                    template: None,
                    working_copy,
                    instructions: None,
                    data: std::collections::HashMap::new(),
                    created_at: timestamp,
                    started_at: None,
                    claimed_by_session: None,
                    last_session_id: None,
                    stopped_reason: None,
                    closed_outcome: None,
                    summary: None,
                    turn_started: None,
                    turn_closed: None,
                    turn_stopped: None,
                    comments: Vec::new(),
                };
                tasks.insert(id.clone(), task);
                id
            };

            // Start both the parent and its digest task.
            // The parent must transition to InProgress so it doesn't re-appear
            // in the ready queue after the .0 digest is closed (which would cause
            // agents to re-start the parent and create duplicate .0 digests).
            actual_ids_to_start = vec![task_id.clone(), digest_task_id];
        }
    }

    // Get tasks before state changes (for output)
    let mut started_tasks: Vec<Task> = actual_ids_to_start
        .iter()
        .filter_map(|id| tasks.get(id).cloned())
        .collect();

    // Query current turn ID from session
    let turn_id = crate::tasks::current_turn_id(our_session_id.as_deref());

    // Start new tasks (batch operation)
    // Reuse session detected earlier for start event
    let agent_type_str = session_match
        .as_ref()
        .map(|m| m.agent_type.as_str().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let session_id = our_session_id.clone();

    let timestamp = chrono::Utc::now();
    let start_event = TaskEvent::Started {
        task_ids: actual_ids_to_start.clone(),
        agent_type: agent_type_str,
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        timestamp,
    };
    write_event(cwd, &start_event)?;

    // Emit link flags for all started tasks (applies to both quick-start and existing tasks)
    // For quick-start: links were already emitted during creation above
    // For existing tasks: emit links now, after the start event
    if created_new_task.is_none() {
        for task_id in &actual_ids_to_start {
            if let Some(ref parent_id) = subtask_of {
                write_link_event(cwd, &graph, "subtask-of", task_id, parent_id)?;
            }
            for source in &sources {
                write_link_event(cwd, &graph, "sourced-from", task_id, source)?;
            }
            emit_link_flags(
                cwd,
                &graph,
                task_id,
                &blocked_by,
                &depends_on,
                &validates,
                &remediates,
                &supersedes,
                &implements,
                &orchestrates,
                &fixes,
                &decomposes_plan,
                &adds_plan,
                autorun,
            )?;
        }
    }

    // Emit task.started flow events for each started task
    for task_id in &actual_ids_to_start {
        if let Ok(task) = find_task(&tasks, task_id) {
            let task_event = AikiEvent::TaskStarted(AikiTaskStartedPayload {
                task: TaskEventPayload {
                    id: task.id.clone(),
                    name: task.name.clone(),
                    task_type: infer_task_type(&task),
                    status: "in_progress".to_string(),
                    assignee: task.assignee.clone(),
                    outcome: None,
                    source: task.sources.first().cloned(),
                    files: None,
                    changes: None,
                },
                cwd: cwd.to_path_buf(),
                timestamp,
            });

            // Dispatch event (fire-and-forget, don't block on failure)
            let _ = crate::event_bus::dispatch(task_event);
        }
    }

    // Update task statuses
    for task in &mut started_tasks {
        task.status = TaskStatus::InProgress;
        task.stopped_reason = None;
        task.claimed_by_session = session_id.clone();
    }

    // Build slim output: no context footer for start
    let mut output = String::new();

    for task in &started_tasks {
        // Hide name on quick-start (user just typed it), show on start-by-ID
        let show_name = created_new_task
            .as_ref()
            .map_or(true, |ct| ct.id != task.id);
        output.push_str(&format_action_started(task, show_name));
    }

    aiki_print(&output);
    Ok(())
}

/// Cascade-close a set of tasks: write Closed event, dispatch flow events, update in-memory state.
///
/// Used by run_close (existing cascade), run_stop (orchestrator), and task_run (orchestrator).
pub(crate) fn cascade_close_tasks(
    cwd: &Path,
    tasks: &mut FastHashMap<String, Task>,
    task_ids: &[String],
    outcome: TaskOutcome,
    summary: &str,
) -> Result<()> {
    if task_ids.is_empty() {
        return Ok(());
    }

    let close_timestamp = chrono::Utc::now();

    // Query current turn ID from session
    let session_match = crate::session::find_active_session(cwd);
    let turn_id =
        crate::tasks::current_turn_id(session_match.as_ref().map(|m| m.session_id.as_str()));

    // 1. Write the Closed event
    let close_event = TaskEvent::Closed {
        task_ids: task_ids.to_vec(),
        outcome,
        summary: Some(summary.to_string()),
        turn_id,
        timestamp: close_timestamp,
    };
    write_event(cwd, &close_event)?;

    // 2. Set data.issues_found for review tasks (before dispatching close events,
    //    so consumers of task.closed see the correct issue count)
    for id in task_ids {
        if let Some(task) = tasks.get(id) {
            if super::fix::is_review_task(task) {
                let issue_count = super::review::get_issue_comments(task).len();
                let data_event = TaskEvent::Updated {
                    task_id: id.clone(),
                    name: None,
                    priority: None,
                    assignee: None,
                    data: Some({
                        let mut m = HashMap::new();
                        m.insert("issues_found".to_string(), issue_count.to_string());
                        m
                    }),
                    instructions: None,
                    timestamp: chrono::Utc::now(),
                };
                write_event(cwd, &data_event)?;
            }
        }
    }

    // 3. Dispatch task.closed flow events for hook automation
    for id in task_ids {
        if let Some(task) = tasks.get(id) {
            let task_event = AikiEvent::TaskClosed(AikiTaskClosedPayload {
                task: TaskEventPayload {
                    id: task.id.clone(),
                    name: task.name.clone(),
                    task_type: infer_task_type(task),
                    status: "closed".to_string(),
                    assignee: task.assignee.clone(),
                    outcome: Some(outcome.to_string()),
                    source: task.sources.first().cloned(),
                    files: None,
                    changes: None,
                },
                cwd: cwd.to_path_buf(),
                timestamp: close_timestamp,
            });
            let _ = crate::event_bus::dispatch(task_event);
        }
    }

    // 4. Update in-memory state
    for id in task_ids {
        if let Some(task) = tasks.get_mut(id) {
            task.status = TaskStatus::Closed;
            task.closed_outcome = Some(outcome);
        }
    }

    Ok(())
}

/// Stop task(s)
fn run_stop(
    cwd: &Path,
    ids: Vec<String>,
    reason: Option<String>,
    blocked: Vec<String>,
    force: bool,
) -> Result<()> {
    let events = read_events(cwd)?;
    let mut graph = materialize_graph(&events);

    // Get in-progress task IDs first (to avoid borrow conflicts)
    let in_progress_ids: Vec<String> = get_in_progress(&graph.tasks)
        .iter()
        .map(|t| t.id.clone())
        .collect();

    // Determine which task(s) to stop
    let task_ids = if ids.is_empty() {
        // Default to current in-progress task
        if in_progress_ids.is_empty() {
            let xml = MdBuilder::new("stop")
                .error()
                .build_error("No task in progress to stop");
            aiki_print(&xml);
            return Ok(());
        }
        // Stop first in-progress task when no IDs specified
        vec![in_progress_ids.first().unwrap().clone()]
    } else {
        // Resolve all IDs (prefix → full) and validate
        let mut resolved = Vec::new();
        for id in &ids {
            let task = find_task_in_graph(&graph, id)?;
            if task.status != TaskStatus::InProgress && task.status != TaskStatus::Open {
                return Err(AikiError::TaskNotFound(format!(
                    "Task '{}' is not in progress",
                    id
                )));
            }
            resolved.push(task.id.clone());
        }
        resolved
    };

    // Session ownership guard: check all tasks (unless --force)
    if !force {
        use crate::session::find_active_session;
        let session_match = find_active_session(cwd);
        let current_session_id = session_match.as_ref().map(|m| m.session_id.as_str());

        for task_id in &task_ids {
            if let Some(task) = graph.tasks.get(task_id) {
                if let Some(ref claimed_session) = task.claimed_by_session {
                    let is_owner = current_session_id
                        .map(|sid| sid == claimed_session.as_str())
                        .unwrap_or(false);

                    if !is_owner {
                        let xml = MdBuilder::new("stop").error().build_error(&format!(
                            "Task '{}' is claimed by another session. Use --force to override.",
                            short_id(task_id)
                        ));
                        aiki_print(&xml);
                        return Ok(());
                    }
                }
            }
        }
    }

    // Stop all tasks in a single event
    let session_match = crate::session::find_active_session(cwd);
    let turn_id =
        crate::tasks::current_turn_id(session_match.as_ref().map(|m| m.session_id.as_str()));
    let stop_event = TaskEvent::Stopped {
        task_ids: task_ids.clone(),
        reason: reason.clone(),
        turn_id,
        timestamp: chrono::Utc::now(),
    };
    write_event(cwd, &stop_event)?;

    // Create blocker tasks for each --blocked flag and emit links to ALL stopped tasks
    let timestamp = chrono::Utc::now();
    let working_copy = get_working_copy_change_id(cwd);
    for blocked_reason in &blocked {
        let blocker_id = generate_task_id(blocked_reason);
        let blocker_event = TaskEvent::Created {
            task_id: blocker_id.clone(),
            name: blocked_reason.clone(),
            slug: None,
            task_type: None,
            priority: TaskPriority::P0, // Blockers are high priority
            assignee: Some("human".to_string()),
            sources: Vec::new(),
            template: None,
            working_copy: working_copy.clone(),
            instructions: None,
            data: std::collections::HashMap::new(),
            timestamp,
        };
        write_event(cwd, &blocker_event)?;

        // Emit links for each stopped task
        for task_id in &task_ids {
            // Emit blocked-by link: stopped task → blocker
            write_link_event(cwd, &graph, "blocked-by", task_id, &blocker_id)?;

            // Emit sourced-from link: blocker → stopped task
            write_link_event(cwd, &graph, "sourced-from", &blocker_id, task_id)?;
        }

        // Add blocker task to in-memory map so it appears in ready queue
        graph.tasks.insert(
            blocker_id.clone(),
            Task {
                id: blocker_id,
                name: blocked_reason.clone(),
                slug: None,
                task_type: None,
                status: TaskStatus::Open,
                priority: TaskPriority::P0,
                assignee: Some("human".to_string()),
                sources: Vec::new(),
                template: None,
                working_copy: working_copy.clone(),
                instructions: None,
                data: std::collections::HashMap::new(),
                created_at: timestamp,
                started_at: None,
                claimed_by_session: None,
                last_session_id: None,
                stopped_reason: None,
                closed_outcome: None,
                summary: None,
                turn_started: None,
                turn_closed: None,
                turn_stopped: None,
                comments: Vec::new(),
            },
        );
    }

    // Update all stopped tasks' status and handle orchestrator cascades
    let mut stopped_tasks = Vec::new();
    for task_id in &task_ids {
        if let Some(task) = graph.tasks.get_mut(task_id) {
            task.status = TaskStatus::Stopped;
            stopped_tasks.push(task.clone());

            // Cascade-close unclosed descendants if this is an orchestrator task
            if task.is_orchestrator() {
                use crate::tasks::manager::get_all_unclosed_descendants;
                let unclosed = get_all_unclosed_descendants(&graph, task_id);
                if !unclosed.is_empty() {
                    let cascade_ids: Vec<String> = unclosed.iter().map(|t| t.id.clone()).collect();
                    cascade_close_tasks(
                        cwd,
                        &mut graph.tasks,
                        &cascade_ids,
                        TaskOutcome::WontDo,
                        "Parent orchestrator stopped",
                    )?;
                }
            }
        }
    }

    // Update context: get in-progress tasks minus the stopped ones
    let updated_in_progress: Vec<Task> = in_progress_ids
        .iter()
        .filter(|id| !task_ids.contains(id))
        .filter_map(|id| graph.tasks.get(id).cloned())
        .collect();

    // Determine scope set based on remaining in-progress tasks
    let mut include_root = false;
    let mut scopes: Vec<String> = Vec::new();
    for task in &updated_in_progress {
        if let Some(parent_id) = graph.edges.target(&task.id, "subtask-of") {
            scopes.push(parent_id.to_string());
        } else {
            include_root = true;
        }
    }
    scopes.sort();
    scopes.dedup();
    let scope_set = ScopeSet {
        include_root,
        scopes,
    };

    // Get scoped ready queue (blocking is filtered internally)
    let mut ready: Vec<Task> = get_ready_queue_for_scope_set(&graph, &scope_set)
        .into_iter()
        .map(|t| (*t).clone())
        .collect();

    // Add stopped tasks if they're in scope
    for stopped_task in &stopped_tasks {
        let stopped_in_scope = match (
            graph.edges.target(&stopped_task.id, "subtask-of"),
            &scope_set,
        ) {
            // Root task in scope if root included or no scopes
            (None, ss) => ss.include_root || ss.is_empty(),
            // Child task in scope if parent is in scopes
            (Some(parent), ss) => ss.scopes.iter().any(|s| s == parent),
        };
        if stopped_in_scope {
            ready.push(stopped_task.clone());
        }
    }
    ready.sort_by(|a, b| a.priority.cmp(&b.priority));

    // Build output: action line(s), then ---/context
    let in_progress_refs: Vec<_> = updated_in_progress.iter().collect();
    let ready_refs: Vec<_> = ready.iter().collect();

    // Format output for single vs multiple tasks
    let mut output = if stopped_tasks.len() == 1 {
        format_action_stopped(&stopped_tasks[0], reason.as_deref())
    } else {
        let mut lines = Vec::new();
        for task in &stopped_tasks {
            lines.push(format!("Stopped: {} — {}", short_id(&task.id), task.name));
        }
        if let Some(r) = &reason {
            lines.push(format!("Reason: {}", r));
        }
        lines.join("\n") + "\n"
    };

    output.push_str(&build_transition_context(&in_progress_refs, &ready_refs));

    aiki_print(&output);
    Ok(())
}

/// Close task(s) as done
fn run_close(
    cwd: &Path,
    ids: Vec<String>,
    outcome_str: &str,
    wont_do: bool,
    summary: Option<String>,
) -> Result<()> {
    use crate::session::find_active_session;
    use crate::tasks::manager::{
        all_subtasks_closed, get_all_unclosed_descendants, get_scoped_ready_queue,
    };
    use std::io::Read;

    // Validate outcome (unless --wont_do is used, which overrides)
    if !wont_do {
        match outcome_str {
            "done" | "wont_do" => {}
            _ => {
                return Err(AikiError::InvalidOutcome(
                    outcome_str.to_string(),
                    vec!["done".to_string(), "wont_do".to_string()],
                ));
            }
        }
    }

    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let mut tasks = graph.tasks.clone();

    // Get in-progress task IDs first (to avoid borrow issues)
    let in_progress_ids: Vec<String> = get_in_progress(&tasks)
        .iter()
        .map(|t| t.id.clone())
        .collect();

    // Determine which task(s) to close
    let mut ids_to_close = if ids.is_empty() {
        // Default to current in-progress tasks
        if in_progress_ids.is_empty() {
            let xml = MdBuilder::new("close")
                .error()
                .build_error("No task in progress to close");
            aiki_print(&xml);
            return Ok(());
        }
        in_progress_ids.clone()
    } else {
        // Resolve all IDs (prefix → full) and validate
        let mut resolved = Vec::new();
        for id in &ids {
            resolved.push(resolve_task_id_in_graph(&graph, id)?);
        }
        resolved
    };

    // Keep track of explicitly requested tasks vs cascade-closed descendants
    let explicit_ids = ids_to_close.clone();

    // Cascade close: collect all unclosed descendants for any parent tasks being closed
    // This allows closing a parent to automatically close all its subtasks
    let mut descendants_to_close: Vec<String> = Vec::new();
    for id in &ids_to_close {
        if has_subtasks(&graph, id) {
            let unclosed = get_all_unclosed_descendants(&graph, id);
            for task in unclosed {
                if !ids_to_close.contains(&task.id) && !descendants_to_close.contains(&task.id) {
                    descendants_to_close.push(task.id.clone());
                }
            }
        }
    }
    // Prepend descendants (they're in depth-first order, deepest first)
    // so they get closed before their parents
    descendants_to_close.append(&mut ids_to_close);
    ids_to_close = descendants_to_close;

    // Handle stdin for --summary -
    let summary_text = if summary.as_deref() == Some("-") {
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer)?;
        Some(buffer.trim().to_string())
    } else {
        summary
    };

    // Determine if summary is required
    let session_match = find_active_session(cwd);
    let our_session_id = session_match.as_ref().map(|m| m.session_id.clone());

    if summary_text.is_none() {
        // --wont-do always requires a summary (rationale for declining)
        if wont_do || outcome_str == "wont_do" {
            return Err(AikiError::TaskCommentRequired(
                "Summary required when closing as won't-do. Explain why:\n  aiki task close <id> --wont-do --summary \"Already handled by existing code\""
                    .to_string(),
            ));
        }

        // Summary required if current session started ANY of the explicit tasks
        let requires_summary: Vec<String> = explicit_ids
            .iter()
            .filter(|id| {
                if let Some(task) = tasks.get(*id) {
                    // Check if current session started this task
                    match (&task.last_session_id, &our_session_id) {
                        (Some(task_session), Some(our)) => task_session == our,
                        _ => false,
                    }
                } else {
                    false
                }
            })
            .cloned()
            .collect();

        if !requires_summary.is_empty() {
            let short_ids: Vec<String> = requires_summary
                .iter()
                .map(|id| crate::tasks::md::short_id(id).to_string())
                .collect();
            return Err(AikiError::TaskCommentRequired(
                format!(
                    "Summary required when closing an in progress task.\n\nInstead close with a summary of your work:\n  aiki task close {} --summary \"What you accomplished\"",
                    short_ids.join(" ")
                ),
            ));
        }
    }

    // --wont_do flag overrides --outcome for backwards compatibility
    let outcome = if wont_do || outcome_str == "wont_do" {
        TaskOutcome::WontDo
    } else {
        TaskOutcome::Done
    };

    // Query current turn ID from session
    let turn_id = crate::tasks::current_turn_id(our_session_id.as_deref());

    // Get tasks before closing (for output)
    let mut closed_tasks: Vec<_> = ids_to_close
        .iter()
        .filter_map(|id| tasks.get(id).cloned())
        .collect();

    // Cascade-close descendants via shared helper (write event, dispatch flow events, update state)
    let cascade_ids: Vec<String> = ids_to_close
        .iter()
        .filter(|id| !explicit_ids.contains(id))
        .cloned()
        .collect();
    cascade_close_tasks(cwd, &mut tasks, &cascade_ids, outcome, "Closed with parent")?;

    let close_timestamp = chrono::Utc::now();

    // Build close event but DO NOT write yet — we'll batch it with spawn-related
    // events (reopen, _spawns_failed) for atomic close+reopen consistency.
    //
    // Atomicity model:
    //   1. Spawned task creation (create_from_template + write_link_event) writes
    //      individual JJ commits per spawn — these happen BEFORE the batch write.
    //   2. Close + reopen + _spawns_failed are batch-written atomically.
    //
    // If the batch write (step 2) fails after spawns (step 1) succeeded, spawned
    // tasks persist without the close transition. This is safe because:
    //   - spawn_key dedup ensures retry won't create duplicates
    //   - Base child index computation excludes spawner-created children, so
    //     retried subtask spawns get the same IDs
    //   - On retry, the correct final state is reached
    //
    // True single-commit atomicity would require refactoring create_from_template
    // to return events instead of writing them — deferred as the failure window
    // is narrow and recovery is automatic.
    let close_event = TaskEvent::Closed {
        task_ids: explicit_ids.clone(),
        outcome,
        summary: summary_text.clone(),
        turn_id: turn_id.clone(),
        timestamp: close_timestamp,
    };

    // Note: We intentionally do NOT terminate background processes on close.
    // Close is called by the agent when it finishes work gracefully.
    // Use `aiki task stop` to forcibly terminate a running agent.

    // Update closed tasks status in local state for explicit IDs
    // (in-memory only — needed for spawn condition evaluation)
    for task in &mut closed_tasks {
        task.status = TaskStatus::Closed;
        task.closed_outcome = Some(outcome);
    }
    for id in &explicit_ids {
        if let Some(task) = tasks.get_mut(id) {
            task.status = TaskStatus::Closed;
            task.closed_outcome = Some(outcome);
        }
    }

    // Collect all unique parent IDs from closed tasks for auto-start check
    let unique_parent_ids: HashSet<String> = ids_to_close
        .iter()
        .filter_map(|id| graph.edges.target(id, "subtask-of").map(|s| s.to_string()))
        .collect();

    // Move mutated tasks into graph for accurate subtask-closed checks and edge lookups
    let mut graph = graph;
    graph.tasks = tasks;

    // Set data.issues_found for explicitly closed review tasks.
    // This must happen BEFORE spawn evaluation so conditions like
    // `data.issues_found > 0` can be checked, and BEFORE batch_events
    // is built so the Updated event is included in the atomic write.
    let mut issues_found_events: Vec<TaskEvent> = Vec::new();
    for id in &explicit_ids {
        if let Some(task) = graph.tasks.get(id) {
            if super::fix::is_review_task(task) {
                let issue_count = super::review::get_issue_comments(task).len();
                // Update in-memory state for spawn condition evaluation
                if let Some(task_mut) = graph.tasks.get_mut(id) {
                    task_mut
                        .data
                        .insert("issues_found".to_string(), issue_count.to_string());
                }
                issues_found_events.push(TaskEvent::Updated {
                    task_id: id.clone(),
                    name: None,
                    priority: None,
                    assignee: None,
                    data: Some({
                        let mut m = HashMap::new();
                        m.insert("issues_found".to_string(), issue_count.to_string());
                        m
                    }),
                    instructions: None,
                    timestamp: chrono::Utc::now(),
                });
            }
        }
    }

    // === Spawn evaluation: check if any closed tasks have spawn configs ===
    // Spawn conditions are evaluated against the post-transition state (including outcome),
    // so we don't gate on outcome here — let `when` expressions decide.
    let mut spawn_notices: Vec<String> = Vec::new();
    // Collect additional events to batch-write with the close event
    let mut batch_events: Vec<TaskEvent> = vec![close_event];
    batch_events.extend(issues_found_events);
    // Track spawners that need reopening (subtask spawns created successfully)
    let mut spawners_to_reopen: HashSet<String> = HashSet::new();
    // Track tasks auto-started via autorun (both spawn autorun and blocking link autorun)
    let mut autorun_started: Vec<Task> = Vec::new();

    for task_id in &explicit_ids {
        if let Some(task) = graph.tasks.get(task_id) {
            if let Some(spawns_json) = task.data.get("_spawns").cloned() {
                if let Ok(spawns_config) = serde_json::from_str::<
                    Vec<crate::tasks::templates::spawn_config::SpawnEntry>,
                >(&spawns_json)
                {
                    // Spawn depth guard: walk spawned-by chain to check depth
                    let depth = spawn_chain_depth(&graph, task_id);
                    if depth >= 10 {
                        eprintln!(
                            "[aiki] Warning: spawn depth limit reached ({}) for task {} — skipping spawns",
                            depth, task_id
                        );
                        continue;
                    }

                    let actions =
                        crate::tasks::spawner::evaluate_spawns(task, &graph, &spawns_config);

                    // Pre-compute child IDs for ALL subtask spawns before executing any.
                    // This ensures deterministic index allocation: indices are assigned
                    // based on spawn entry order, not execution order. If one spawn fails
                    // but another succeeds, retries produce the same index assignments
                    // (combined with spawn_key dedup for idempotency).
                    //
                    // Pre-generate full 32-char IDs for subtask spawns.
                    // Idempotency (retry safety) is handled by the _spawn_key check in
                    // execute_spawn_action — if a spawn already succeeded, its existing
                    // task is returned and the pre-generated ID is unused.
                    let mut child_id_map: HashMap<usize, String> = HashMap::new();
                    for action in &actions {
                        if let crate::tasks::spawner::SpawnAction::CreateSubtask {
                            spawn_index,
                            template,
                            ..
                        } = action
                        {
                            child_id_map
                                .insert(*spawn_index, crate::tasks::id::generate_task_id(template));
                        }
                    }

                    let mut failed_indices: Vec<usize> = Vec::new();
                    for action in &actions {
                        let spawn_index = match action {
                            crate::tasks::spawner::SpawnAction::CreateTask {
                                spawn_index, ..
                            } => *spawn_index,
                            crate::tasks::spawner::SpawnAction::CreateSubtask {
                                spawn_index,
                                ..
                            } => *spawn_index,
                        };
                        // Look up pre-computed child ID for subtask spawns
                        let child_task_id = child_id_map.get(&spawn_index).cloned();
                        match execute_spawn_action(cwd, &mut graph, task_id, action, child_task_id)
                        {
                            Ok(spawned_id) => {
                                let (template, is_next_subtask, should_autorun) = match action {
                                    crate::tasks::spawner::SpawnAction::CreateTask {
                                        template,
                                        autorun,
                                        ..
                                    } => (template.as_str(), false, *autorun),
                                    crate::tasks::spawner::SpawnAction::CreateSubtask {
                                        template,
                                        autorun,
                                        ..
                                    } => (template.as_str(), true, *autorun),
                                };
                                if is_next_subtask {
                                    spawners_to_reopen.insert(task_id.clone());
                                }
                                let kind = if is_next_subtask { "subtask" } else { "task" };
                                spawn_notices.push(format!(
                                    "Spawned {} from template {} (id: {})",
                                    kind,
                                    template,
                                    crate::tasks::md::short_id(&spawned_id),
                                ));

                                // Auto-start spawned task if autorun: true
                                if should_autorun {
                                    if let Some(spawned_task) = graph.tasks.get(&spawned_id) {
                                        if spawned_task.status == TaskStatus::Open || spawned_task.status == TaskStatus::Stopped {
                                            let auto_start_ts = chrono::Utc::now();
                                            let agent_type_str = session_match
                                                .as_ref()
                                                .map(|m| m.agent_type.as_str().to_string())
                                                .unwrap_or_else(|| "unknown".to_string());
                                            let start_event = TaskEvent::Started {
                                                task_ids: vec![spawned_id.clone()],
                                                agent_type: agent_type_str,
                                                session_id: our_session_id.clone(),
                                                turn_id: turn_id.clone(),
                                                timestamp: auto_start_ts,
                                            };
                                            if let Err(e) = write_event(cwd, &start_event) {
                                                eprintln!(
                                                    "[aiki] Warning: failed to auto-start spawned task {}: {}",
                                                    crate::tasks::md::short_id(&spawned_id), e
                                                );
                                            } else {
                                                // Update local state
                                                if let Some(task) = graph.tasks.get_mut(&spawned_id) {
                                                    task.status = TaskStatus::InProgress;
                                                    task.claimed_by_session = our_session_id.clone();
                                                    autorun_started.push(task.clone());
                                                }
                                                spawn_notices.push(format!(
                                                    "Auto-started (autorun): {} (id: {})",
                                                    graph.tasks.get(&spawned_id).map(|t| t.name.as_str()).unwrap_or("?"),
                                                    crate::tasks::md::short_id(&spawned_id),
                                                ));
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                failed_indices.push(spawn_index);
                                eprintln!(
                                    "[aiki] Warning: spawn execution failed for task {}, index {}: {}",
                                    task_id, spawn_index, e
                                );
                            }
                        }
                    }

                    // Record failed spawn indices in the batch so they can be retried
                    if !failed_indices.is_empty() {
                        let failed_str = failed_indices
                            .iter()
                            .map(|i| i.to_string())
                            .collect::<Vec<_>>()
                            .join(",");
                        batch_events.push(TaskEvent::Updated {
                            task_id: task_id.clone(),
                            name: None,
                            priority: None,
                            assignee: None,
                            data: Some({
                                let mut m = std::collections::HashMap::new();
                                m.insert("_spawns_failed".to_string(), failed_str.clone());
                                m
                            }),
                            instructions: None,
                            timestamp: chrono::Utc::now(),
                        });
                        eprintln!(
                            "[aiki] Warning: {} spawn(s) failed for task {} (indices: {}). Spawns are idempotent — re-closing will retry.",
                            failed_indices.len(), crate::tasks::md::short_id(task_id), failed_str
                        );
                    }
                }
            }
        }
    }

    // For subtask spawns that succeeded, add reopen events to the batch.
    // The reopen happens AFTER confirming child creation succeeded — never
    // before, which avoids incorrectly reopening the spawner on template failure.
    for spawner_id in &spawners_to_reopen {
        batch_events.push(TaskEvent::Reopened {
            task_id: spawner_id.clone(),
            reason: "Spawning subtask".to_string(),
            timestamp: chrono::Utc::now(),
        });
        // Update local graph state
        if let Some(task) = graph.tasks.get_mut(spawner_id) {
            task.status = TaskStatus::Open;
            task.closed_outcome = None;
        }
    }

    // Atomic batch write: close + reopen + _spawns_failed in a single JJ commit.
    // Note: spawned task creation (create_from_template) is NOT in this batch —
    // see atomicity model comment above for the rationale and safety guarantees.
    write_events_batch(cwd, &batch_events)?;

    // Emit task.closed flow events AFTER the batch write succeeds
    for task_id in &explicit_ids {
        if let Some(task) = graph.tasks.get(task_id) {
            // For reopened spawners, emit with their current (reopened) status
            let status_str = if spawners_to_reopen.contains(task_id) {
                "open"
            } else {
                "closed"
            };
            let task_event = AikiEvent::TaskClosed(AikiTaskClosedPayload {
                task: TaskEventPayload {
                    id: task.id.clone(),
                    name: task.name.clone(),
                    task_type: infer_task_type(task),
                    status: status_str.to_string(),
                    assignee: task.assignee.clone(),
                    outcome: Some(outcome.to_string()),
                    source: task.sources.first().cloned(),
                    files: None,
                    changes: None,
                },
                cwd: cwd.to_path_buf(),
                timestamp: close_timestamp,
            });
            let _ = crate::event_bus::dispatch(task_event);
        }
    }

    // === Blocking link autorun: auto-start tasks that were blocked by the closed tasks ===
    for task_id in &explicit_ids {
        let autorun_candidates = graph.find_autorun_candidates(task_id);
        for candidate_id in &autorun_candidates {
            if let Some(task) = graph.tasks.get(candidate_id) {
                // Idempotent: only start if Open or Stopped
                if task.status != TaskStatus::Open && task.status != TaskStatus::Stopped {
                    continue;
                }
            }

            let auto_start_timestamp = chrono::Utc::now();
            let agent_type_str = session_match
                .as_ref()
                .map(|m| m.agent_type.as_str().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            let start_event = TaskEvent::Started {
                task_ids: vec![candidate_id.clone()],
                agent_type: agent_type_str,
                session_id: our_session_id.clone(),
                turn_id: turn_id.clone(),
                timestamp: auto_start_timestamp,
            };
            write_event(cwd, &start_event)?;

            // Emit flow event
            if let Some(task) = graph.tasks.get(candidate_id) {
                let task_event = AikiEvent::TaskStarted(AikiTaskStartedPayload {
                    task: TaskEventPayload {
                        id: task.id.clone(),
                        name: task.name.clone(),
                        task_type: infer_task_type(task),
                        status: "in_progress".to_string(),
                        assignee: task.assignee.clone(),
                        outcome: None,
                        source: task.sources.first().cloned(),
                        files: None,
                        changes: None,
                    },
                    cwd: cwd.to_path_buf(),
                    timestamp: auto_start_timestamp,
                });
                let _ = crate::event_bus::dispatch(task_event);
            }

            // Update local state
            if let Some(task) = graph.tasks.get_mut(candidate_id) {
                task.status = TaskStatus::InProgress;
                task.claimed_by_session = our_session_id.clone();
                autorun_started.push(task.clone());
            }
        }
    }

    // Check each parent for auto-start eligibility
    let mut auto_started_parents: Vec<Task> = Vec::new();
    let mut notices: Vec<String> = Vec::new();

    // Add autorun notices
    for task in &autorun_started {
        notices.push(format!(
            "Auto-started (autorun): {} (id: {})",
            task.name, crate::tasks::md::short_id(&task.id)
        ));
    }

    for parent_id in &unique_parent_ids {
        // Check if all subtasks are now closed
        if all_subtasks_closed(&graph, parent_id) {
            if let Some(parent) = graph.tasks.get_mut(parent_id) {
                // Guard: skip if already closed or in-progress
                if parent.status == TaskStatus::Closed {
                    continue;
                }
                if parent.status == TaskStatus::InProgress {
                    continue;
                }

                // Auto-start the parent for review/finalization
                let auto_start_timestamp = chrono::Utc::now();
                let agent_type_str = session_match
                    .as_ref()
                    .map(|m| m.agent_type.as_str().to_string())
                    .unwrap_or_else(|| "claude-code".to_string());
                let start_event = TaskEvent::Started {
                    task_ids: vec![parent_id.clone()],
                    agent_type: agent_type_str,
                    session_id: our_session_id.clone(),
                    turn_id: turn_id.clone(),
                    timestamp: auto_start_timestamp,

                };
                write_event(cwd, &start_event)?;

                // Emit task.started flow event for auto-started parent
                let task_event = AikiEvent::TaskStarted(AikiTaskStartedPayload {
                    task: TaskEventPayload {
                        id: parent.id.clone(),
                        name: parent.name.clone(),
                        task_type: infer_task_type(parent),
                        status: "in_progress".to_string(),
                        assignee: parent.assignee.clone(),
                        outcome: None,
                        source: parent.sources.first().cloned(),
                        files: None,
                        changes: None,
                    },
                    cwd: cwd.to_path_buf(),
                    timestamp: auto_start_timestamp,
                });
                let _ = crate::event_bus::dispatch(task_event);

                parent.status = TaskStatus::InProgress;
                parent.claimed_by_session = None;
                auto_started_parents.push(parent.clone());
                notices.push(format!(
                    "All subtasks complete. Parent task (id: {}) auto-started for review/finalization.",
                    parent_id
                ));
            }
        }
    }

    // Auto-start next subtask: if a subtask was closed and the parent is claimed
    // by the current session, auto-start the next pending subtask in that parent.
    // This only triggers when the session owns the parent (i.e., is working through
    // the batch), NOT when an agent only claimed a single subtask.
    let mut auto_started_subtasks: Vec<Task> = Vec::new();

    for parent_id in &unique_parent_ids {
        // Skip if all subtasks just closed (parent auto-start already handled above)
        if all_subtasks_closed(&graph, parent_id) {
            continue;
        }

        // Check if the parent is claimed by the current session
        let parent_claimed_by_us = if let Some(parent) = graph.tasks.get(parent_id) {
            match (&parent.claimed_by_session, &our_session_id) {
                (Some(claimed), Some(ours)) => claimed == ours,
                _ => false,
            }
        } else {
            false
        };

        if !parent_claimed_by_us {
            continue;
        }

        // Pick the next pending subtask from the scoped ready queue
        let next_subtasks = get_scoped_ready_queue(&graph, Some(parent_id));
        if let Some(next) = next_subtasks.first() {
            let next_id = next.id.clone();
            let next_name = next.name.clone();
            let next_task_type = infer_task_type(next);
            let next_assignee = next.assignee.clone();
            let next_source = next.sources.first().cloned();

            let auto_start_timestamp = chrono::Utc::now();
            let agent_type_str = session_match
                .as_ref()
                .map(|m| m.agent_type.as_str().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            let start_event = TaskEvent::Started {
                task_ids: vec![next_id.clone()],
                agent_type: agent_type_str,
                session_id: our_session_id.clone(),
                turn_id: turn_id.clone(),
                timestamp: auto_start_timestamp,

            };
            write_event(cwd, &start_event)?;

            // Emit flow event
            let task_event = AikiEvent::TaskStarted(AikiTaskStartedPayload {
                task: TaskEventPayload {
                    id: next_id.clone(),
                    name: next_name.clone(),
                    task_type: next_task_type,
                    status: "in_progress".to_string(),
                    assignee: next_assignee,
                    outcome: None,
                    source: next_source,
                    files: None,
                    changes: None,
                },
                cwd: cwd.to_path_buf(),
                timestamp: auto_start_timestamp,
            });
            let _ = crate::event_bus::dispatch(task_event);

            // Update local state
            if let Some(task) = graph.tasks.get_mut(&next_id) {
                task.status = TaskStatus::InProgress;
                task.claimed_by_session = our_session_id.clone();
                auto_started_subtasks.push(task.clone());
            }

            notices.push(format!(
                "Auto-started next subtask: {} (id: {})",
                next_name, next_id
            ));
        }
    }

    // Update context: remove closed tasks from in_progress
    let mut updated_in_progress: Vec<Task> = in_progress_ids
        .iter()
        .filter(|id| !ids_to_close.contains(id))
        .filter_map(|id| graph.tasks.get(id).cloned())
        .collect();

    // Add auto-started parents to in_progress
    for parent in &auto_started_parents {
        updated_in_progress.push(parent.clone());
    }

    // Add auto-started subtasks to in_progress
    for subtask in &auto_started_subtasks {
        updated_in_progress.push(subtask.clone());
    }

    // Add autorun-started tasks to in_progress
    for task in &autorun_started {
        updated_in_progress.push(task.clone());
    }

    // Determine output scope set based on updated in-progress tasks
    let mut include_root = false;
    let mut output_scopes: Vec<String> = Vec::new();
    for task in &updated_in_progress {
        if let Some(parent_id) = graph.edges.target(&task.id, "subtask-of") {
            output_scopes.push(parent_id.to_string());
        } else {
            include_root = true;
        }
    }
    output_scopes.sort();
    output_scopes.dedup();
    let scope_set = ScopeSet {
        include_root,
        scopes: output_scopes,
    };

    // Get scoped ready queue (with blocked-by filtering)
    let ready: Vec<Task> = get_ready_queue_for_scope_set(&graph, &scope_set)
        .into_iter()
        .filter(|t| !ids_to_close.contains(&t.id))
        .map(|t| (*t).clone())
        .collect();

    // Build output: action+hint line, then ---/context
    let mut output = String::new();
    let in_progress_refs: Vec<_> = updated_in_progress.iter().collect();
    let ready_refs: Vec<_> = ready.iter().collect();

    // Closed confirmation with hint
    if closed_tasks.len() == 1 {
        output.push_str(&format_action_closed(&closed_tasks[0]));
    } else {
        output.push_str(&format!("Closed {} tasks\n", closed_tasks.len()));
    }

    // Notices and auto-starts
    let has_intermediates = !notices.is_empty()
        || !spawn_notices.is_empty()
        || !auto_started_parents.is_empty()
        || !auto_started_subtasks.is_empty();
    if has_intermediates {
        for notice in &notices {
            output.push_str(&format!("> {}\n", notice));
        }
        for notice in &spawn_notices {
            output.push_str(&format!("> {}\n", notice));
        }
        for parent in &auto_started_parents {
            output.push_str(&format_action_started(parent, true));
        }
        for subtask in &auto_started_subtasks {
            output.push_str(&format_action_started(subtask, true));
        }
    }

    // Full context after ---
    output.push_str(&build_transition_context(&in_progress_refs, &ready_refs));

    aiki_print(&output);
    Ok(())
}

/// Walk the spawned-by chain to determine spawn depth.
///
/// Returns the number of spawned-by hops from this task to the root.
/// Used to enforce the max spawn depth guard (10 levels).
fn spawn_chain_depth(graph: &TaskGraph, task_id: &str) -> usize {
    let mut depth = 0;
    let mut current = task_id.to_string();
    loop {
        match graph.edges.target(&current, "spawned-by") {
            Some(parent) => {
                depth += 1;
                current = parent.to_string();
                if depth > 20 {
                    break; // Safety: prevent infinite loop from corrupted data
                }
            }
            None => break,
        }
    }
    depth
}

/// Execute a single spawn action: create a task from template with appropriate links.
///
/// The caller is responsible for:
/// - Pre-computing `child_task_id` for subtask spawns (deterministic index allocation)
/// - Writing the close event and reopen event (for atomicity with spawn creation)
///
/// Returns the ID of the spawned task, or an error if creation fails.
fn execute_spawn_action(
    cwd: &Path,
    graph: &mut TaskGraph,
    spawner_id: &str,
    action: &crate::tasks::spawner::SpawnAction,
    child_task_id: Option<String>,
) -> Result<String> {
    use crate::tasks::spawner::SpawnAction;

    let (template, priority, assignee, data, spawn_index, is_next_subtask) = match action {
        SpawnAction::CreateTask {
            template,
            priority,
            assignee,
            data,
            spawn_index,
            ..
        } => (template, priority, assignee, data, spawn_index, false),
        SpawnAction::CreateSubtask {
            template,
            priority,
            assignee,
            data,
            spawn_index,
            ..
        } => (template, priority, assignee, data, spawn_index, true),
    };

    // Idempotency: check if a task with this spawn_key already exists
    let spawn_key = format!("{}:{}", spawner_id, spawn_index);
    for task in graph.tasks.values() {
        if task.data.get("_spawn_key").map(|v| v.as_str()) == Some(&spawn_key) {
            return Ok(task.id.clone()); // Already spawned
        }
    }

    // Get spawner task for context
    let spawner = graph
        .tasks
        .get(spawner_id)
        .ok_or_else(|| AikiError::TaskNotFound(spawner_id.to_string()))?;

    // Determine priority: spawn config > spawner priority > default
    let task_priority = if let Some(p) = priority {
        TaskPriority::from_str(p).unwrap_or(spawner.priority)
    } else {
        spawner.priority
    };

    // Build data map with spawn metadata
    let mut spawn_data: HashMap<String, String> = data.clone();
    spawn_data.insert("_spawn_key".to_string(), spawn_key);

    // Add spawner context as data for template variable substitution
    spawn_data.insert("spawner.id".to_string(), spawner_id.to_string());
    spawn_data.insert("spawner.name".to_string(), spawner.name.clone());
    spawn_data.insert("spawner.status".to_string(), spawner.status.to_string());
    spawn_data.insert("spawner.priority".to_string(), spawner.priority.to_string());
    let approved = spawner
        .data
        .get("approved")
        .map(|v| v.as_str())
        .unwrap_or("false");
    spawn_data.insert("spawner.approved".to_string(), approved.to_string());
    if let Some(ref outcome) = spawner.closed_outcome {
        spawn_data.insert("spawner.outcome".to_string(), outcome.to_string());
    }
    if let Some(ref assignee) = spawner.assignee {
        spawn_data.insert("spawner.assignee".to_string(), assignee.clone());
    }
    if let Some(ref summary) = spawner.summary {
        spawn_data.insert("spawner.summary".to_string(), summary.clone());
    }

    // Add spawner.data.* fields so spawned templates can access spawner's data
    for (key, value) in &spawner.data {
        if !key.starts_with('_') {
            spawn_data.insert(format!("spawner.data.{}", key), value.clone());
        }
    }

    // Add spawner.links.{kind}.task_id for each link kind with targets
    for link_kind in crate::tasks::graph::LINK_KINDS {
        let targets = graph.edges.targets(spawner_id, link_kind.name);
        if let Some(first_target) = targets.first() {
            spawn_data.insert(
                format!("spawner.links.{}.task_id", link_kind.name),
                first_target.clone(),
            );
        }
    }

    // Resolve "self" template to the spawner's own template
    let resolved_template = if template == "self" {
        spawner
            .template
            .as_ref()
            .map(|t| {
                // Strip version suffix (e.g., "aiki/review@1.0.0" -> "aiki/review")
                t.split('@').next().unwrap_or(t).to_string()
            })
            .ok_or_else(|| AikiError::TemplateProcessingFailed {
                details: format!(
                    "spawn config uses template: \"self\" but spawner task {} has no template",
                    spawner_id
                ),
            })?
    } else {
        template.clone()
    };

    // Build template params
    let params = TemplateTaskParams {
        template_name: resolved_template,
        data: spawn_data,
        sources: vec![format!("task:{}", spawner_id)],
        assignee: assignee.clone(),
        priority: Some(task_priority),
        parent_id: if is_next_subtask {
            Some(spawner_id.to_string())
        } else {
            None
        },
        parent_name: if is_next_subtask {
            Some(spawner.name.clone())
        } else {
            None
        },
        source_data: HashMap::new(),
        builtins: HashMap::new(),
        task_id: child_task_id,
    };

    // Create the task from template FIRST — if this fails, no state is changed
    let spawned_id = create_from_template(cwd, params)?;

    // Add spawned-by link from spawned task to spawner
    // Re-read events to get updated graph with the new task
    let fresh_events = read_events(cwd)?;
    let fresh_graph = materialize_graph(&fresh_events);
    write_link_event(cwd, &fresh_graph, "spawned-by", &spawned_id, spawner_id)?;

    // Update the in-memory graph with the spawned task
    if let Some(spawned_task) = fresh_graph.tasks.get(&spawned_id) {
        graph.tasks.insert(spawned_id.clone(), spawned_task.clone());
    }

    Ok(spawned_id)
}

/// Query changes that have a task ID in their provenance
fn query_changes_for_task(cwd: &Path, task_id: &str) -> Result<Vec<ChangeInfo>> {
    use crate::jj::jj_cmd;

    // Use the same revset pattern as task diff to ensure consistency
    // This filters out task lifecycle events on aiki/tasks branch
    let pattern = build_task_revset_pattern(task_id);

    // Query JJ for changes with this task ID in their description
    // Format: change_id timestamp (first line only)
    let output = jj_cmd()
        .current_dir(cwd)
        .args([
            "log",
            "-r",
            &pattern,
            "--no-graph",
            "-T",
            r#"change_id ++ " " ++ author.timestamp().format("%Y-%m-%dT%H:%M:%S") ++ "\n""#,
            "--ignore-working-copy",
        ])
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to query changes: {}", e)))?;

    if !output.status.success() {
        // Empty result is not an error
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut changes = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.splitn(2, ' ').collect();
        if parts.len() >= 1 {
            let change_id = parts[0].to_string();
            let timestamp = parts.get(1).map(|s| s.to_string());
            changes.push(ChangeInfo {
                change_id,
                timestamp,
            });
        }
    }

    Ok(changes)
}

/// Get the diff for a specific change
///
/// Uses git format with 5 lines of context for better agent comprehension.
fn get_change_diff(cwd: &Path, change_id: &str) -> Result<String> {
    use crate::jj::jj_cmd;

    let output = jj_cmd()
        .current_dir(cwd)
        .args([
            "diff",
            "-r",
            change_id,
            "--color=never",
            "--ignore-working-copy",
            "--git",
            "--context",
            "5",
        ])
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to get diff: {}", e)))?;

    if !output.status.success() {
        return Ok(String::new());
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Information about a change linked to a task
struct ChangeInfo {
    change_id: String,
    timestamp: Option<String>,
}

/// Parsed source reference types
enum SourceRef {
    Task { id: String },
    Prompt { id: String },
    File { path: String },
    Comment { id: String },
    Unknown { raw: String },
}

/// Parse a source string into a typed reference
fn parse_source(source: &str) -> SourceRef {
    if let Some((source_type, id)) = source.split_once(':') {
        match source_type {
            "task" => SourceRef::Task { id: id.to_string() },
            "prompt" => SourceRef::Prompt { id: id.to_string() },
            "file" => SourceRef::File {
                path: id.to_string(),
            },
            "comment" => SourceRef::Comment { id: id.to_string() },
            _ => SourceRef::Unknown {
                raw: source.to_string(),
            },
        }
    } else {
        SourceRef::Unknown {
            raw: source.to_string(),
        }
    }
}

/// Format a source reference as markdown
///
/// When `expand` is false, returns a brief source line.
/// When `expand` is true, includes full content from the source.
fn format_source(
    cwd: &Path,
    source: &str,
    tasks: &FastHashMap<String, Task>,
    expand: bool,
) -> String {
    let parsed = parse_source(source);

    match parsed {
        SourceRef::Task { id } => {
            if expand {
                if let Some(task) = tasks.get(&id) {
                    let mut md = format!("- Source: task:{} ({})\n", &id, &task.name);
                    if let Some(ref instructions) = task.instructions {
                        md.push_str(&format!("  Instructions: {}\n", instructions));
                    }
                    for nested_source in &task.sources {
                        let nested_parsed = parse_source(nested_source);
                        md.push_str(&format_source_minimal(&nested_parsed));
                    }
                    md
                } else {
                    format!("- Source: task:{} (not found)\n", &id)
                }
            } else {
                format!("- Source: task:{}\n", &id)
            }
        }
        SourceRef::Prompt { id } => {
            if expand {
                use crate::global::global_aiki_dir;
                use crate::history::get_prompt_by_change_id;

                let global_repo = global_aiki_dir();
                match get_prompt_by_change_id(&global_repo, &id) {
                    Ok(Some(content)) => {
                        format!("- Source: prompt:{}\n  > {}\n", &id, content)
                    }
                    _ => {
                        format!("- Source: prompt:{} (not found)\n", &id)
                    }
                }
            } else {
                format!("- Source: prompt:{}\n", &id)
            }
        }
        SourceRef::File { path } => {
            if expand {
                let full_path = cwd.join(&path);
                match std::fs::read_to_string(&full_path) {
                    Ok(content) => {
                        format!("- Source: file:{}\n```\n{}\n```\n", &path, content)
                    }
                    Err(_) => {
                        format!("- Source: file:{} (not found)\n", &path)
                    }
                }
            } else {
                format!("- Source: file:{}\n", &path)
            }
        }
        SourceRef::Comment { id } => {
            if expand {
                if let Some((task_id, index_str)) = id.split_once(':') {
                    if let Ok(index) = index_str.parse::<usize>() {
                        if let Some(task) = tasks.get(task_id) {
                            if let Some(comment) = task.comments.get(index) {
                                return format!(
                                    "- Source: comment:{} (task:{})\n  > {}\n",
                                    &id, task_id, &comment.text
                                );
                            }
                        }
                    }
                }
                format!("- Source: comment:{} (not found)\n", &id)
            } else {
                format!("- Source: comment:{}\n", &id)
            }
        }
        SourceRef::Unknown { raw } => {
            format!("- Source: {}\n", &raw)
        }
    }
}

/// Format a source reference as minimal markdown (for nested sources)
fn format_source_minimal(source: &SourceRef) -> String {
    match source {
        SourceRef::Task { id } => format!("  - source: task:{}\n", id),
        SourceRef::Prompt { id } => format!("  - source: prompt:{}\n", id),
        SourceRef::File { path } => format!("  - source: file:{}\n", path),
        SourceRef::Comment { id } => format!("  - source: comment:{}\n", id),
        SourceRef::Unknown { raw } => format!("  - source: {}\n", raw),
    }
}

/// Show task details (including subtasks for parent tasks)
fn run_show(
    cwd: &Path,
    id: Option<String>,
    show_diff: bool,
    with_source: bool,
    with_instructions: bool,
) -> Result<()> {
    use crate::tasks::manager::get_subtasks;

    let events = read_events_with_ids(cwd)?;
    let graph = materialize_graph_with_ids(&events);
    let tasks = &graph.tasks;
    let in_progress = get_in_progress(&tasks);

    // Determine which task to show
    let task_id = if let Some(id) = id {
        let task = find_task_in_graph(&graph, &id)?;
        task.id.clone()
    } else {
        // Default to first in-progress task
        if let Some(task) = in_progress.first() {
            task.id.clone()
        } else {
            let xml = MdBuilder::new("show")
                .error()
                .build_error("No task in progress to show");
            aiki_print(&xml);
            return Ok(());
        }
    };

    let task = tasks.get(&task_id).expect("Task should exist");

    // Get subtasks if this is a parent task
    let subtasks = get_subtasks(&graph, &task_id);
    let has_subtasks = !subtasks.is_empty();

    // Calculate progress if has subtasks
    let (completed, total) = if has_subtasks {
        let total = subtasks.len();
        let completed = subtasks
            .iter()
            .filter(|t| t.status == TaskStatus::Closed)
            .count();
        (completed, total)
    } else {
        (0, 0)
    };

    // Build compressed task details (no bold markers, no timestamps/IDs on comments)
    let status_display = if task.status == TaskStatus::Closed {
        format!(
            "{} ({})",
            task.status,
            task.closed_outcome
                .as_ref()
                .map(|o| o.to_string())
                .unwrap_or_else(|| "done".to_string())
        )
    } else {
        task.status.to_string()
    };
    let mut content = format!("Task: {}\nID: {}\n", task.name, task.id,);
    if let Some(ref slug) = task.slug {
        content.push_str(&format!("Slug: {}\n", slug));
    }
    content.push_str(&format!(
        "Status: {}\nPriority: {}\n",
        status_display, task.priority
    ));

    // Add summary for closed tasks
    if task.status == TaskStatus::Closed {
        if let Some(summary) = task.effective_summary() {
            content.push_str(&format!("Summary: {}\n", summary));
        }
    }

    // Add sources: use graph's sourced-from edges (superset of old-style task.sources)
    let source_targets = graph.edges.targets(&task_id, "sourced-from");
    if !source_targets.is_empty() {
        for source in source_targets {
            content.push_str(&format_source(cwd, source, &tasks, with_source));
        }
    }

    // Add blocked-by links
    let blockers = graph.edges.targets(&task_id, "blocked-by");
    if !blockers.is_empty() {
        content.push_str("\nBlocked by:\n");
        for blocker_id in blockers {
            let status = graph
                .tasks
                .get(blocker_id)
                .map(|t| format!("{} — {} ({})", short_id(blocker_id), t.name, t.status))
                .unwrap_or_else(|| short_id(blocker_id).to_string());
            content.push_str(&format!("- {}\n", status));
        }
    }

    // Add blocks links (reverse lookup — what does this task block?)
    let blocks = graph.edges.referrers(&task_id, "blocked-by");
    if !blocks.is_empty() {
        content.push_str("\nBlocks:\n");
        for blocked_id in blocks {
            let status = graph
                .tasks
                .get(blocked_id)
                .map(|t| format!("{} [{}] {}", short_id(blocked_id), t.status, t.name))
                .unwrap_or_else(|| short_id(blocked_id).to_string());
            content.push_str(&format!("- {}\n", status));
        }
    }

    // Add spawned-by link (this task was spawned by another)
    if let Some(spawner_id) = graph.edges.target(&task_id, "spawned-by") {
        let spawner_display = graph
            .tasks
            .get(spawner_id)
            .map(|t| format!("{} — {}", short_id(spawner_id), t.name))
            .unwrap_or_else(|| short_id(spawner_id).to_string());
        content.push_str(&format!("Spawned by: {}\n", spawner_display));
    }

    // Add spawned tasks (this task spawned others)
    let spawned = graph.edges.referrers(&task_id, "spawned-by");
    if !spawned.is_empty() {
        content.push_str("\nSpawned:\n");
        for spawned_id in spawned {
            let display = graph
                .tasks
                .get(spawned_id)
                .map(|t| format!("{} — {} ({})", short_id(spawned_id), t.name, t.status))
                .unwrap_or_else(|| short_id(spawned_id).to_string());
            content.push_str(&format!("- {}\n", display));
        }
    }

    // Add instructions if present and requested
    if with_instructions {
        if let Some(ref instructions) = task.instructions {
            content.push('\n');
            content.push_str(&format_instructions(instructions));
        }
    }

    // Add subtasks section with checklist format and relative IDs
    if has_subtasks {
        let percentage = if total > 0 {
            (completed * 100) / total
        } else {
            0
        };
        content.push_str(&format!(
            "\nSubtasks ({}/{} — {}%):\n",
            completed, total, percentage
        ));
        for subtask in &subtasks {
            let check = match subtask.status {
                TaskStatus::Closed => "[x]",
                TaskStatus::InProgress => "[>]",
                _ => "[ ]",
            };
            // Show slug if present, otherwise use relative ID (.N)
            let label = if let Some(ref slug) = subtask.slug {
                slug.clone()
            } else if let Some(dot_pos) = subtask.id.rfind('.') {
                subtask.id[dot_pos..].to_string()
            } else {
                subtask.id.clone()
            };
            content.push_str(&format!("{} {} {}\n", check, label, subtask.name));
        }
    }

    // Add comments (no timestamps, no IDs - just the text)
    if !task.comments.is_empty() {
        content.push_str("\nComments:\n");
        for comment in &task.comments {
            content.push_str(&format!("- {}\n", &comment.text));
        }
    }

    // Skip Files Changed and Changes sections (use `task diff` for those)
    // Only show diff inline when --diff flag is explicitly requested
    if show_diff {
        let changes = query_changes_for_task(cwd, &task_id)?;
        if !changes.is_empty() {
            content.push_str(&format!("\nChanges ({}):\n", changes.len()));
            for change in &changes {
                let diff = get_change_diff(cwd, &change.change_id)?;
                content.push_str(&format!("- {}\n```\n{}\n```\n", change.change_id, diff));
            }
        }
    }

    // Context footer (read command - keep it)
    let scope_set = get_current_scope_set(&graph);
    let ready = get_ready_queue_for_scope_set(&graph, &scope_set);
    let in_progress_refs: Vec<_> = in_progress.iter().map(|t| *t).collect();
    content.push_str(&build_context(&in_progress_refs, &ready));

    aiki_print(&content);
    Ok(())
}

/// Show diff of changes made while working on a task
///
/// Shows the net result (baseline → final) of all task work.
/// Uses jj revsets to derive baseline from provenance metadata.
/// Undo file changes made by a task or set of tasks
fn run_undo(
    cwd: &Path,
    ids: Vec<String>,
    completed: bool,
    force: bool,
    dry_run: bool,
    no_backup: bool,
) -> Result<()> {
    use crate::jj::jj_cmd;

    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let tasks = &graph.tasks;

    // Resolve task IDs: if --completed, expand to completed subtasks
    let task_ids = if completed {
        if ids.len() != 1 {
            return Err(AikiError::InvalidArgument(
                "--completed requires exactly one epic task ID".to_string(),
            ));
        }
        let epic_task = find_task_in_graph(&graph, &ids[0])?;
        let epic_id = &epic_task.id;

        // Find completed subtasks (direct children of the epic)
        let completed_subtasks: Vec<String> = graph
            .edges
            .referrers(epic_id, "subtask-of")
            .iter()
            .filter_map(|id| graph.tasks.get(id))
            .filter(|t| {
                t.status == TaskStatus::Closed && t.closed_outcome == Some(TaskOutcome::Done)
            })
            .map(|t| t.id.clone())
            .collect();

        if completed_subtasks.is_empty() {
            return Err(AikiError::NoCompletedSubtasks);
        }
        completed_subtasks
    } else {
        // Resolve all IDs (prefix → full) and validate
        let mut resolved = Vec::new();
        for id in &ids {
            resolved.push(resolve_task_id_in_graph(&graph, id)?);
        }
        resolved
    };

    // Build union revset pattern for all tasks being undone
    let patterns: Vec<String> = task_ids
        .iter()
        .map(|id| build_task_revset_pattern(id))
        .collect();
    let union_pattern = patterns.join(" | ");

    // Check if any changes exist for these tasks
    let check_output = jj_cmd()
        .current_dir(cwd)
        .args([
            "log",
            "-r",
            &union_pattern,
            "--no-graph",
            "-T",
            "change_id",
            "--ignore-working-copy",
        ])
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to query changes: {}", e)))?;

    if !check_output.status.success() {
        let stderr = String::from_utf8_lossy(&check_output.stderr);
        if !stderr.contains("no revisions") {
            return Err(AikiError::JjCommandFailed(format!(
                "jj log failed: {}",
                stderr.trim()
            )));
        }
    }

    if String::from_utf8_lossy(&check_output.stdout)
        .trim()
        .is_empty()
    {
        return Err(AikiError::TaskNoChanges);
    }

    // Compute baseline and final revsets
    let from_revset = format!("parents(roots({}))", union_pattern);
    let to_revset = format!("heads({})", union_pattern);

    // Get list of changed files with their change types
    let summary_output = jj_cmd()
        .current_dir(cwd)
        .args([
            "diff",
            "--from",
            &from_revset,
            "--to",
            &to_revset,
            "--summary",
            "--ignore-working-copy",
        ])
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to get diff summary: {}", e)))?;

    if !summary_output.status.success() {
        let stderr = String::from_utf8_lossy(&summary_output.stderr);
        return Err(AikiError::JjCommandFailed(format!(
            "jj diff --summary failed: {}",
            stderr.trim()
        )));
    }

    let summary_text = String::from_utf8_lossy(&summary_output.stdout);
    let file_changes = parse_diff_summary_with_types(&summary_text);

    if file_changes.is_empty() {
        return Err(AikiError::TaskNoChanges);
    }

    // Conflict detection (unless --force)
    if !force {
        // Trigger an explicit working copy snapshot upfront so all subsequent
        // jj calls (both the in-progress check and file_content_differs) see a
        // single consistent snapshot. Without this, the first jj call that omits
        // --ignore-working-copy would trigger an implicit snapshot mid-flow.
        let _ = jj_cmd().current_dir(cwd).args(["status"]).output();

        // Check for in-progress task conflicts
        let in_progress_tasks = get_in_progress(&tasks);
        let undo_set: HashSet<&str> = task_ids.iter().map(|s| s.as_str()).collect();

        let mut ip_conflicts: Vec<String> = Vec::new();
        for ip_task in &in_progress_tasks {
            if undo_set.contains(ip_task.id.as_str()) {
                continue;
            }
            let ip_pattern = build_task_revset_pattern(&ip_task.id);
            // Scope to current workspace: intersect with ::@
            let scoped_pattern = format!("({}) & ::@", ip_pattern);

            let scope_check = jj_cmd()
                .current_dir(cwd)
                .args([
                    "log",
                    "-r",
                    &scoped_pattern,
                    "--no-graph",
                    "-T",
                    "change_id",
                    "--ignore-working-copy",
                ])
                .output();

            if let Ok(output) = scope_check {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if stdout.trim().is_empty() || !output.status.success() {
                    continue; // Task has no commits in current workspace
                }
            } else {
                continue;
            }

            // Get files modified by this in-progress task
            if let Ok(Some(ip_files)) = get_task_changed_files(cwd, &ip_task.id, false) {
                let undo_files: HashSet<&str> =
                    file_changes.iter().map(|(_, f)| f.as_str()).collect();
                for f in &ip_files {
                    if undo_files.contains(f.as_str()) {
                        ip_conflicts.push(format!(
                            "  - Task {}: \"{}\" modified {}",
                            &ip_task.id[..8],
                            ip_task.name,
                            f
                        ));
                    }
                }
            }
        }

        if !ip_conflicts.is_empty() {
            let msg = format!(
                "In-progress tasks affecting these files:\n{}\n\n\
                 Options:\n\
                 1. Complete or stop those tasks first\n\
                 2. Use --force to undo anyway (may cause issues for in-progress work)",
                ip_conflicts.join("\n")
            );
            return Err(AikiError::UndoInProgressConflict(msg));
        }

        // Check for post-task modifications (compare working copy to task's final state)
        let mut conflicts: Vec<String> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();

        for (change_type, file_path) in &file_changes {
            let wc_exists = cwd.join(file_path).exists();

            match change_type.as_str() {
                "A" => {
                    // Task added this file
                    if !wc_exists {
                        skipped.push(file_path.clone());
                        continue; // Already gone, skip
                    }
                    // Compare working copy to task's final state.
                    // Omits --ignore-working-copy intentionally — relies on
                    // the upfront jj status snapshot above.
                    if file_content_differs(cwd, file_path, &to_revset)? {
                        conflicts.push(format!(
                            "  - {} (task created, then manually edited)",
                            file_path
                        ));
                    }
                }
                "D" => {
                    // Task deleted this file
                    if wc_exists {
                        conflicts
                            .push(format!("  - {} (task deleted, then re-created)", file_path));
                    }
                    // If still deleted, nothing to do for conflict check
                }
                _ => {
                    // Modified
                    if !wc_exists {
                        // File was modified by task but deleted afterward
                        conflicts.push(format!("  - {} (task modified, then deleted)", file_path));
                    } else if file_content_differs(cwd, file_path, &to_revset)? {
                        // Omits --ignore-working-copy intentionally — relies on
                        // the upfront jj status snapshot above.
                        conflicts.push(format!(
                            "  - {} (task modified, then manually edited)",
                            file_path
                        ));
                    }
                }
            }
        }

        if !conflicts.is_empty() {
            let msg = format!(
                "Files modified after task completed:\n{}\n\n\
                 Suggestions:\n\
                 1. Review changes manually\n\
                 2. Use --force to undo anyway (WARNING: loses manual edits)\n\
                 3. Use --dry-run to preview what would be undone",
                conflicts.join("\n")
            );
            return Err(AikiError::UndoConflict(msg));
        }
    }

    // Filter out files that should be skipped (added but already deleted)
    let active_changes: Vec<(String, String)> = file_changes
        .iter()
        .filter(|(change_type, file_path)| !(change_type == "A" && !cwd.join(file_path).exists()))
        .cloned()
        .collect();

    if active_changes.is_empty() {
        eprintln!("All task changes have already been reverted (no files to undo).");
        return Ok(());
    }

    // Dry run: just print what would be done
    if dry_run {
        eprintln!("[DRY RUN] Would undo {} task(s)", task_ids.len());
        for id in &task_ids {
            if let Ok(task) = find_task_in_graph(&graph, id) {
                eprintln!("  \"{}\"", task.name);
            }
        }
        eprintln!();
        eprintln!("Files that would be reverted ({}):", active_changes.len());
        for (change_type, file_path) in &active_changes {
            let action = match change_type.as_str() {
                "A" => "remove file",
                "D" => "restore file",
                _ => "restore to previous state",
            };
            eprintln!("  {} {} → {}", change_type, file_path, action);
        }
        eprintln!();
        if force {
            eprintln!("(Conflict checks were not performed due to --force)");
        } else {
            eprintln!("No conflicts detected.");
        }
        return Ok(());
    }

    // Create backup bookmark (unless --no-backup)
    let backup_name = if !no_backup {
        let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let suffix = if task_ids.len() == 1 {
            task_ids[0][..8].to_string()
        } else {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            for id in &task_ids {
                hasher.update(id.as_bytes());
            }
            let hash = hasher.finalize();
            hex::encode(&hash[..4])
        };
        let name = format!("aiki/undo-backup-{}-{}", timestamp, suffix);

        let bookmark_result = jj_cmd()
            .current_dir(cwd)
            .args([
                "bookmark",
                "create",
                &name,
                "-r",
                "@",
                "--ignore-working-copy",
            ])
            .output()
            .map_err(|e| {
                AikiError::JjCommandFailed(format!("Failed to create backup bookmark: {}", e))
            })?;

        if !bookmark_result.status.success() {
            let stderr = String::from_utf8_lossy(&bookmark_result.stderr);
            return Err(AikiError::JjCommandFailed(format!(
                "Failed to create backup bookmark: {}",
                stderr.trim()
            )));
        }
        eprintln!("Creating backup: {}", name);
        Some(name)
    } else {
        None
    };

    // Restore files to baseline state using jj restore
    // For multi-task undo, compute per-file baselines to avoid reverting
    // changes made between tasks by non-undo changes
    //
    // file_to_task_ids is populated in the multi-task branch and reused later
    // for per-task file counts in XML output (avoids redundant jj diff calls).
    let mut file_to_task_ids: HashMap<String, Vec<String>> = HashMap::new();
    let baseline_groups: Vec<(String, Vec<String>)> = if task_ids.len() > 1 {
        // Compute per-task file lists to determine which tasks touched which files
        for id in &task_ids {
            let pattern = build_task_revset_pattern(id);
            let task_from = format!("parents(roots({}))", pattern);
            let task_to = format!("heads({})", pattern);
            let output = jj_cmd()
                .current_dir(cwd)
                .args([
                    "diff",
                    "--from",
                    &task_from,
                    "--to",
                    &task_to,
                    "--summary",
                    "--ignore-working-copy",
                ])
                .output()
                .map_err(|e| {
                    AikiError::JjCommandFailed(format!("Failed to get per-task diff: {}", e))
                })?;
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for f in parse_diff_summary_files(&stdout) {
                    file_to_task_ids.entry(f).or_default().push(id.clone());
                }
            }
        }

        // Group active files by baseline (determined by which tasks touched them)
        let mut groups: HashMap<String, Vec<String>> = HashMap::new();
        for (_, file_path) in &active_changes {
            let baseline = match file_to_task_ids.get(file_path) {
                Some(touching_tasks) if touching_tasks.len() < task_ids.len() => {
                    // Only a subset of tasks touched this file - use specific baseline
                    let sub_patterns: Vec<String> = touching_tasks
                        .iter()
                        .map(|id| build_task_revset_pattern(id))
                        .collect();
                    format!("parents(roots({}))", sub_patterns.join(" | "))
                }
                _ => from_revset.clone(), // All tasks (or unknown) - use global baseline
            };
            groups.entry(baseline).or_default().push(file_path.clone());
        }
        groups.into_iter().collect()
    } else {
        // Single task: one group with global baseline (correct and efficient)
        let files = active_changes.iter().map(|(_, f)| f.clone()).collect();
        vec![(from_revset.clone(), files)]
    };

    for (baseline, files) in &baseline_groups {
        let mut restore_cmd = jj_cmd();
        restore_cmd
            .current_dir(cwd)
            .arg("restore")
            .arg("--from")
            .arg(baseline);
        for path in files {
            restore_cmd.arg(path.as_str());
        }
        let restore_output = restore_cmd
            .output()
            .map_err(|e| AikiError::JjCommandFailed(format!("Failed to restore files: {}", e)))?;
        if !restore_output.status.success() {
            let stderr = String::from_utf8_lossy(&restore_output.stderr);
            return Err(AikiError::JjCommandFailed(format!(
                "jj restore failed: {}",
                stderr.trim()
            )));
        }
    }

    // Human-readable output to stderr
    eprintln!();
    if task_ids.len() == 1 {
        if let Ok(task) = find_task_in_graph(&graph, &task_ids[0]) {
            eprintln!("Undoing task {}", &task_ids[0][..8]);
            eprintln!("  \"{}\"", task.name);
        }
    } else if completed {
        eprintln!("Undoing {} completed subtasks", task_ids.len());
        for id in &task_ids {
            if let Ok(task) = find_task_in_graph(&graph, id) {
                eprintln!("  - {}: {}", &id[..8], task.name);
            }
        }
    } else {
        eprintln!("Undoing {} tasks", task_ids.len());
        for id in &task_ids {
            if let Ok(task) = find_task_in_graph(&graph, id) {
                eprintln!("  - {}: {}", &id[..8], task.name);
            }
        }
    }

    eprintln!();
    eprintln!("Files reverted ({}):", active_changes.len());
    for (change_type, file_path) in &active_changes {
        let desc = match change_type.as_str() {
            "A" => "(file removed)",
            "D" => "(file restored)",
            _ => "(restored to previous state)",
        };
        eprintln!("  {} {} {}", change_type, file_path, desc);
    }
    eprintln!();
    eprintln!("Task changes undone successfully.");

    // Machine-readable XML output to stdout
    // For multi-task undo, derive per-task file counts from file_to_task_ids
    // (already computed during baseline grouping — no extra jj calls needed).
    let mut md_content = String::from("## Undone\n");
    if task_ids.len() > 1 {
        let active_set: HashSet<&str> = active_changes.iter().map(|(_, f)| f.as_str()).collect();
        for id in &task_ids {
            let count = file_to_task_ids
                .iter()
                .filter(|(file, ids)| active_set.contains(file.as_str()) && ids.contains(id))
                .count();
            md_content.push_str(&format!("- **{}** — {} files reverted\n", id, count));
        }
    } else {
        for id in &task_ids {
            md_content.push_str(&format!(
                "- **{}** — {} files reverted\n",
                id,
                active_changes.len()
            ));
        }
    }
    if let Some(ref name) = backup_name {
        md_content.push_str(&format!("- **Backup:** {}\n", name));
    }

    let in_progress = get_in_progress(&tasks);
    let graph = materialize_graph(&events);
    let scope_set = get_current_scope_set(&graph);
    let ready_queue = get_ready_queue_for_scope_set(&graph, &scope_set);
    let xml_scopes = scope_set.to_xml_scopes();
    let mut builder = MdBuilder::new("undo");
    if !xml_scopes.is_empty() {
        builder = builder.with_scopes(&xml_scopes);
    }
    let md = builder.build(&md_content, &in_progress, &ready_queue);
    aiki_print(&md);

    Ok(())
}

/// Parse diff summary output preserving change types (A/M/D/R)
///
/// Handles JJ rename lines (`R old_path => new_path`) by splitting them
/// into a delete of the old path and an add of the new path.
fn parse_diff_summary_with_types(output: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.len() > 2 && line.chars().nth(1) == Some(' ') {
            let change_type = &line[..1];
            let path_part = &line[2..];
            if change_type == "R" {
                // Rename: "R old_path => new_path"
                // Split into delete of old + add of new for undo purposes
                if let Some((old_path, new_path)) = path_part.split_once(" => ") {
                    result.push(("D".to_string(), old_path.to_string()));
                    result.push(("A".to_string(), new_path.to_string()));
                } else {
                    // Fallback: treat as modification if separator not found
                    result.push(("M".to_string(), path_part.to_string()));
                }
            } else {
                result.push((change_type.to_string(), path_part.to_string()));
            }
        }
    }
    result
}

/// Check if a file's working copy content differs from its state in a given revision.
///
/// NOTE: Deliberately omits --ignore-working-copy. We need jj to auto-snapshot
/// the working copy so we compare against the true on-disk state, not a stale snapshot.
/// Callers should trigger an explicit snapshot before calling this in a loop to avoid
/// a mid-loop snapshot that could race with concurrent edits.
fn file_content_differs(cwd: &Path, file_path: &str, revset: &str) -> Result<bool> {
    use crate::jj::jj_cmd;

    // Use jj diff to compare working copy (@) to the given revset for this specific file
    // No --ignore-working-copy: we need the real working copy state (see doc comment)
    let output = jj_cmd()
        .current_dir(cwd)
        .args([
            "diff",
            "--from",
            revset,
            "--to",
            "@",
            "--summary",
            file_path,
        ])
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to compare file: {}", e)))?;

    if !output.status.success() {
        // If the command fails, assume there's a difference (safer)
        return Ok(true);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // If there's any output, the file differs
    Ok(!stdout.trim().is_empty())
}

fn run_diff(cwd: &Path, id: String, summary: bool, stat: bool, name_only: bool) -> Result<()> {
    use crate::jj::jj_cmd;

    // Verify task exists
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let task = find_task_in_graph(&graph, &id)?;
    let id = task.id.clone(); // use canonical ID

    // Build revset pattern for task
    // For parent tasks with subtasks, match task=<id> AND task=<id>.* (subtasks)
    let pattern = build_task_revset_pattern(&id);

    // Check if any changes exist for this task
    let check_output = jj_cmd()
        .current_dir(cwd)
        .args([
            "log",
            "-r",
            &pattern,
            "--no-graph",
            "-T",
            "change_id",
            "--ignore-working-copy",
        ])
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to query changes: {}", e)))?;

    // Distinguish between jj failure and empty results
    if !check_output.status.success() {
        let stderr = String::from_utf8_lossy(&check_output.stderr);
        // "Revset resolved to no revisions" is not an error - just means no matches
        if !stderr.contains("no revisions") {
            return Err(AikiError::JjCommandFailed(format!(
                "jj log failed: {}",
                stderr.trim()
            )));
        }
    }

    if String::from_utf8_lossy(&check_output.stdout)
        .trim()
        .is_empty()
    {
        println!(
            "No changes found for task {}.\n\n\
             The task exists but has no associated code changes in jj history.\n\
             This may happen if:\n\
             - Task has no code changes yet\n\
             - Changes were made without aiki provenance tracking",
            id
        );
        return Ok(());
    }

    // Build revset expressions for baseline and final
    // - roots(pattern) = earliest changes for task
    // - parents(roots(...)) = state before task started (baseline)
    // - heads(pattern) = latest changes for task (final)
    let from_revset = format!("parents(roots({}))", pattern);
    let to_revset = format!("heads({})", pattern);

    // Build jj diff command
    let mut cmd = jj_cmd();
    cmd.current_dir(cwd)
        .arg("diff")
        .arg("--from")
        .arg(&from_revset)
        .arg("--to")
        .arg(&to_revset)
        .arg("--ignore-working-copy");

    // Add format options
    if summary {
        cmd.arg("--summary");
    } else if stat {
        cmd.arg("--stat");
    } else if name_only {
        // jj doesn't have --name-only but we can use -T to just print names
        // Actually, use --summary and filter to just paths
        // For now, use --summary which gives similar output
        cmd.arg("--summary");
    } else {
        // Default: Use git format with 5 lines of context for better agent comprehension.
        // Git diff format is more recognizable to AI agents trained on GitHub/GitLab diffs,
        // and 5 lines of context helps understand surrounding code structure.
        cmd.arg("--git").arg("--context").arg("5");
    }

    let output = cmd
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to get diff: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Handle "no common ancestor" case
        if stderr.contains("no common ancestor") || stderr.contains("empty") {
            println!("Warning: Task changes have no common baseline - showing full content");
        } else {
            return Err(AikiError::JjCommandFailed(format!(
                "jj diff failed: {}",
                stderr.trim()
            )));
        }
    }

    // Output jj's native format directly (no XML wrapper)
    let stdout = String::from_utf8_lossy(&output.stdout);

    if name_only && !summary && !stat {
        // Parse --summary output to extract just file names
        for line in stdout.lines() {
            // Summary format: "M path/to/file" or "A path/to/file"
            let line = line.trim();
            if line.len() > 2 {
                // Skip status char and space
                let parts: Vec<&str> = line.splitn(2, ' ').collect();
                if parts.len() == 2 {
                    println!("{}", parts[1]);
                }
            }
        }
    } else {
        print!("{}", stdout);
    }

    Ok(())
}

/// Build revset pattern for a task, including subtasks
///
/// For task ID "abc123", this matches:
/// - description(substring:"task=abc123") - the task itself (provenance metadata)
/// - description(substring:"task=abc123.") - any subtasks (abc123.1, abc123.2, etc.)
///
/// Excludes `::aiki/tasks` to filter out task lifecycle events (which contain
/// `stopped_task=<id>`, `task_id=<id>`, etc.) that live on a separate branch.
fn build_task_revset_pattern(task_id: &str) -> String {
    format!(
        "(description(substring:\"task={}\") | description(substring:\"task={}.\")) ~ ::aiki/tasks",
        task_id, task_id
    )
}

/// Get list of files changed during a task
///
/// Uses jj diff --summary with revset-based baseline/final approach.
/// Returns None if no changes found, otherwise returns list of file paths.
fn get_task_changed_files(
    cwd: &Path,
    task_id: &str,
    ignore_working_copy: bool,
) -> Result<Option<Vec<String>>> {
    use crate::jj::jj_cmd;

    let pattern = build_task_revset_pattern(task_id);

    // Check if any changes exist for this task
    let check_output = jj_cmd()
        .current_dir(cwd)
        .args([
            "log",
            "-r",
            &pattern,
            "--no-graph",
            "-T",
            "change_id",
            "--ignore-working-copy",
        ])
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to query changes: {}", e)))?;

    // Distinguish between jj failure and empty results
    if !check_output.status.success() {
        let stderr = String::from_utf8_lossy(&check_output.stderr);
        if !stderr.contains("no revisions") {
            return Err(AikiError::JjCommandFailed(format!(
                "jj log failed: {}",
                stderr.trim()
            )));
        }
        return Ok(None);
    }

    if String::from_utf8_lossy(&check_output.stdout)
        .trim()
        .is_empty()
    {
        return Ok(None);
    }

    // Build revset expressions for baseline and final
    let from_revset = format!("parents(roots({}))", pattern);
    let to_revset = format!("heads({})", pattern);

    // Run jj diff --summary to get file paths
    // When checking in-progress task conflicts, skip --ignore-working-copy
    // to capture the latest working copy state (auto-snapshot)
    let mut diff_cmd = jj_cmd();
    diff_cmd.current_dir(cwd).args([
        "diff",
        "--from",
        &from_revset,
        "--to",
        &to_revset,
        "--summary",
    ]);
    if ignore_working_copy {
        diff_cmd.arg("--ignore-working-copy");
    }
    let output = diff_cmd
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to get diff: {}", e)))?;

    if !output.status.success() {
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let files = parse_diff_summary_files(&stdout);

    if files.is_empty() {
        Ok(None)
    } else {
        Ok(Some(files))
    }
}

/// Parse jj diff --summary output to extract file paths
///
/// Example output:
/// ```
/// M src/auth.ts
/// A src/new_file.ts
/// D src/old_file.ts
/// R src/old_name.ts => src/new_name.ts
/// ```
///
/// Rename lines produce both the old and new path.
fn parse_diff_summary_files(output: &str) -> Vec<String> {
    let mut result = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if line.len() > 2 && line.chars().nth(1) == Some(' ') {
            let change_type = &line[..1];
            let path_part = &line[2..];
            if change_type == "R" {
                // Rename: "R old_path => new_path" - include both paths
                if let Some((old_path, new_path)) = path_part.split_once(" => ") {
                    result.push(old_path.to_string());
                    result.push(new_path.to_string());
                } else {
                    result.push(path_part.to_string());
                }
            } else {
                result.push(path_part.to_string());
            }
        }
    }
    result
}

/// Update task details
fn run_set(
    cwd: &Path,
    id: Option<String>,
    p0: bool,
    p1: bool,
    p2: bool,
    p3: bool,
    name: Option<String>,
    assignee_arg: Option<String>,
    data_args: Vec<String>,
    instructions_flag: bool,
) -> Result<()> {
    use crate::agents::Assignee;
    use crate::validation::is_valid_template_identifier;

    // Parse data arguments (verbatim, no coercion)
    let data_updates = parse_data_flags(&data_args, false)?;

    // Validate data keys
    for key in data_updates.keys() {
        if !is_valid_template_identifier(key) {
            return Err(AikiError::InvalidDataKey(key.clone()));
        }
    }

    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let mut tasks = graph.tasks.clone();
    let in_progress = get_in_progress(&tasks);

    // Determine which task to update
    let task_id = if let Some(id) = id {
        let task = find_task_in_graph(&graph, &id)?;
        task.id.clone()
    } else {
        // Default to first in-progress task
        if let Some(task) = in_progress.first() {
            task.id.clone()
        } else {
            let xml = MdBuilder::new("set")
                .error()
                .build_error("No task in progress to update");
            aiki_print(&xml);
            return Ok(());
        }
    };

    // Reject blank name
    if let Some(ref n) = name {
        if n.trim().is_empty() {
            let xml = MdBuilder::new("set")
                .error()
                .build_error("Name cannot be empty");
            aiki_print(&xml);
            return Ok(());
        }
    }

    // Reject blank assignee
    if let Some(ref a) = assignee_arg {
        if a.trim().is_empty() {
            let xml = MdBuilder::new("set").error().build_error(&format!(
                "Use `aiki task unset {} assignee` to clear the assignee",
                task_id
            ));
            aiki_print(&xml);
            return Ok(());
        }
    }

    // Reject empty data values
    for (key, value) in &data_updates {
        if value.is_empty() {
            let xml = MdBuilder::new("set").error().build_error(&format!(
                "Use `aiki task unset {} data.{}` to remove a data key",
                task_id, key
            ));
            aiki_print(&xml);
            return Ok(());
        }
    }

    // Determine new priority if any flag is set
    let new_priority = if p0 {
        Some(TaskPriority::P0)
    } else if p1 {
        Some(TaskPriority::P1)
    } else if p2 {
        Some(TaskPriority::P2)
    } else if p3 {
        Some(TaskPriority::P3)
    } else {
        None
    };

    // Determine new assignee: Some(a) = assign to a, None = no change
    let new_assignee: Option<String> = if let Some(ref a) = assignee_arg {
        // Validate and normalize the assignee
        match Assignee::from_str(a) {
            Some(parsed) => match parsed.as_str() {
                Some(s) => Some(s.to_string()),
                None => None, // "none" assignee means no change
            },
            None => return Err(AikiError::UnknownAssignee(a.clone())),
        }
    } else {
        None // No change
    };

    // Wrap data_updates: Some if non-empty, None if no data flags provided
    let new_data = if data_updates.is_empty() {
        None
    } else {
        Some(data_updates)
    };

    // Read instructions from stdin if --instructions flag is set
    let new_instructions = if instructions_flag {
        let content = std::io::read_to_string(std::io::stdin())
            .map_err(|e| AikiError::JjCommandFailed(format!("Failed to read stdin: {}", e)))?;
        Some(content.trim_end().to_string())
    } else {
        None
    };

    // Check if there's anything to update
    if new_priority.is_none()
        && name.is_none()
        && new_assignee.is_none()
        && new_data.is_none()
        && new_instructions.is_none()
    {
        let xml = MdBuilder::new("set").error().build_error(
            "No updates specified. Use --name, --data, --instructions, --assignee, or --p0/--p1/--p2/--p3",
        );
        aiki_print(&xml);
        return Ok(());
    }

    // Write the update event
    let event = TaskEvent::Updated {
        task_id: task_id.clone(),
        name: name.clone(),
        priority: new_priority,
        assignee: new_assignee.clone(),
        data: new_data.clone(),
        instructions: new_instructions.clone(),
        timestamp: chrono::Utc::now(),
    };
    write_event(cwd, &event)?;

    // Update the in-memory task and insert back into map
    {
        let task = tasks.get_mut(&task_id).expect("Task should exist");
        if let Some(ref new_name) = name {
            task.name = new_name.clone();
        }
        if let Some(new_p) = new_priority {
            task.priority = new_p;
        }
        if let Some(ref new_a) = new_assignee {
            task.assignee = Some(new_a.clone());
        }
        if let Some(ref data) = new_data {
            for (key, value) in data {
                task.data.insert(key.clone(), value.clone());
            }
        }
        if let Some(ref instr) = new_instructions {
            task.instructions = Some(instr.clone());
        }
    }

    // Get updated task for output
    let updated_task = tasks.get(&task_id).expect("Task should exist");

    // Build output - include data if present
    let data_md = if updated_task.data.is_empty() {
        String::new()
    } else {
        let mut fields: Vec<String> = updated_task
            .data
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();
        fields.sort(); // Deterministic output
        format!("- **Data:** {}\n", fields.join(", "))
    };

    let content = format!(
        "## Updated\n- **{}** — {} ({})\n{}",
        updated_task.id, updated_task.name, updated_task.priority, data_md
    );

    // Get scope set and ready queue for context (now uses updated tasks map, with blocked-by filtering)
    let graph = materialize_graph(&events);
    let scope_set = get_current_scope_set(&graph);
    let ready = get_ready_queue_for_scope_set(&graph, &scope_set);
    // Re-calculate in_progress since it may have changed
    let updated_in_progress = get_in_progress(&tasks);
    let in_progress_refs: Vec<_> = updated_in_progress.iter().map(|t| *t).collect();

    let mut builder = MdBuilder::new("set");
    let xml_scopes = scope_set.to_xml_scopes();
    if !xml_scopes.is_empty() {
        builder = builder.with_scopes(&xml_scopes);
    }
    let xml = builder.build(&content, &in_progress_refs, &ready);

    aiki_print(&xml);
    Ok(())
}

/// Clear optional fields on a task
fn run_unset(
    cwd: &Path,
    id: Option<String>,
    clear_assignee: bool,
    clear_instructions: bool,
    data_keys: Vec<String>,
) -> Result<()> {
    // Build field_names list from flags
    let mut field_names = Vec::new();

    if clear_assignee {
        field_names.push("assignee".to_string());
    }
    if clear_instructions {
        field_names.push("instructions".to_string());
    }
    for key in &data_keys {
        if key.is_empty() {
            let xml = MdBuilder::new("unset")
                .error()
                .build_error("Data key cannot be empty");
            aiki_print(&xml);
            return Ok(());
        }
        field_names.push(format!("data.{}", key));
    }

    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let mut tasks = graph.tasks.clone();
    let in_progress = get_in_progress(&tasks);

    // Determine which task to update
    let task_id = if let Some(id) = id {
        let task = find_task_in_graph(&graph, &id)?;
        task.id.clone()
    } else {
        // Default to first in-progress task
        if let Some(task) = in_progress.first() {
            task.id.clone()
        } else {
            let xml = MdBuilder::new("unset")
                .error()
                .build_error("No task in progress to update");
            aiki_print(&xml);
            return Ok(());
        }
    };

    // Write the FieldsCleared event
    let event = TaskEvent::FieldsCleared {
        task_id: task_id.clone(),
        fields: field_names.clone(),
        timestamp: chrono::Utc::now(),
    };
    write_event(cwd, &event)?;

    // Update the in-memory task
    {
        let task = tasks.get_mut(&task_id).expect("Task should exist");
        for field in &field_names {
            if field == "assignee" {
                task.assignee = None;
            } else if field == "instructions" {
                task.instructions = None;
            } else if let Some(key) = field.strip_prefix("data.") {
                task.data.remove(key);
            }
        }
    }

    // Get updated task for output
    let updated_task = tasks.get(&task_id).expect("Task should exist");

    let content = format!(
        "## Cleared\n- **{}** — {} ({})\n- **Fields:** {}\n",
        updated_task.id,
        updated_task.name,
        updated_task.priority,
        field_names.join(", ")
    );

    // Get scope set and ready queue for context
    let graph = materialize_graph(&events);
    let scope_set = get_current_scope_set(&graph);
    let ready = get_ready_queue_for_scope_set(&graph, &scope_set);
    let updated_in_progress = get_in_progress(&tasks);
    let in_progress_refs: Vec<_> = updated_in_progress.iter().map(|t| *t).collect();

    let mut builder = MdBuilder::new("unset");
    let xml_scopes = scope_set.to_xml_scopes();
    if !xml_scopes.is_empty() {
        builder = builder.with_scopes(&xml_scopes);
    }
    let xml = builder.build(&content, &in_progress_refs, &ready);

    aiki_print(&xml);
    Ok(())
}

/// Add a comment to a task
fn run_comment(cwd: &Path, id: Option<String>, text: String, data_args: Vec<String>) -> Result<()> {
    // Parse data arguments (verbatim, no coercion for comment metadata)
    let data = parse_data_flags(&data_args, false)?;

    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let in_progress = get_in_progress(&graph.tasks);

    // Determine which task to comment on
    let task_id = if let Some(id) = id {
        let task = find_task_in_graph(&graph, &id)?;
        task.id.clone()
    } else {
        // Default to first in-progress task
        if let Some(task) = in_progress.first() {
            task.id.clone()
        } else {
            let xml = MdBuilder::new("comment")
                .error()
                .build_error("No task in progress to comment on");
            aiki_print(&xml);
            return Ok(());
        }
    };

    comment_on_task(cwd, &task_id, &text, data)?;

    // Slim output: single line, no context footer
    aiki_print(&format_action_commented());
    Ok(())
}

/// Shared implementation for writing a comment event on a task.
/// Used by both `aiki task comment` and `aiki review issue add` to ensure
/// behavioral consistency (validation, event format, persistence).
pub(crate) fn comment_on_task(
    cwd: &Path,
    task_id: &str,
    text: &str,
    data: HashMap<String, String>,
) -> Result<()> {
    let event = TaskEvent::CommentAdded {
        task_ids: vec![task_id.to_string()],
        text: text.to_string(),
        data,
        timestamp: chrono::Utc::now(),
    };
    write_event(cwd, &event)?;
    Ok(())
}

/// Run a task by spawning an agent session
fn run_run(
    cwd: &Path,
    id: String,
    agent: Option<String>,
    run_async: bool,
    next_subtask: bool,
) -> Result<()> {
    use crate::tasks::runner::{
        resolve_next_subtask, run_task_async_with_output, SubtaskResolution,
    };

    // Parse and validate agent override early, before claiming any subtask.
    // This prevents stranding a subtask InProgress when the agent string is invalid.
    let agent_override = if let Some(ref agent_str) = agent {
        match AgentType::from_str(agent_str) {
            Some(agent_type) => Some(agent_type),
            None => return Err(AikiError::UnknownAgentType(agent_str.clone())),
        }
    } else {
        None
    };

    let actual_id = if next_subtask {
        // Resolve the next ready subtask of the parent
        let events = read_events(cwd)?;
        let graph = materialize_graph(&events);

        // Resolve parent ID (supports prefix matching)
        let parent_id = resolve_task_id_in_graph(&graph, &id)?;

        // Validate parent exists and is not closed
        let parent = find_task(&graph.tasks, &parent_id)?;
        if parent.status == TaskStatus::Closed {
            return Err(AikiError::TaskAlreadyClosed(parent_id));
        }

        match resolve_next_subtask(&graph, &parent_id) {
            SubtaskResolution::Ready(task) => {
                eprintln!("Running subtask {} ({})...", short_id(&task.id), task.name);

                // Claim the subtask: emit Started to prevent double-pick
                let started_event = TaskEvent::Started {
                    task_ids: vec![task.id.clone()],
                    agent_type: agent.clone().unwrap_or_default(),
                    session_id: None,
                    turn_id: None,
                    timestamp: chrono::Utc::now(),

                };
                write_event(cwd, &started_event)?;

                task.id.clone()
            }
            SubtaskResolution::AllComplete => {
                let md = MdBuilder::new("run").build(
                    &format!("All subtasks complete for {}\n", short_id(&parent_id)),
                    &[],
                    &[],
                );
                println!("{}", md);
                return Ok(());
            }
            SubtaskResolution::Blocked(unclosed) => {
                let mut msg = format!(
                    "No ready subtasks for {} ({} subtasks blocked)\n",
                    short_id(&parent_id),
                    unclosed.len()
                );
                for t in &unclosed {
                    // Gather blocker info
                    let blocker_ids = get_blocker_short_ids(&graph, &t.id);
                    let status_str = match t.status {
                        TaskStatus::InProgress => "in progress".to_string(),
                        _ if !blocker_ids.is_empty() => {
                            format!("blocked by: {}", blocker_ids.join(", "))
                        }
                        _ => format!("{}", t.status),
                    };
                    msg.push_str(&format!(
                        "  {} ({}) — {}\n",
                        short_id(&t.id),
                        t.name,
                        status_str,
                    ));
                }
                let md = MdBuilder::new("run").error().build_error(&msg);
                println!("{}", md);
                return Err(AikiError::InvalidArgument(format!(
                    "No ready subtasks for {}",
                    short_id(&parent_id),
                )));
            }
            SubtaskResolution::NoSubtasks => {
                let msg = format!("Task {} has no subtasks", short_id(&parent_id));
                let md = MdBuilder::new("run").error().build_error(&msg);
                println!("{}", md);
                return Err(AikiError::InvalidArgument(msg));
            }
        }
    } else {
        id
    };

    // Build options
    let mut options = TaskRunOptions::new();
    if let Some(agent_type) = agent_override {
        options = options.with_agent(agent_type);
    }

    // Run the task with output - async or blocking
    let result = if run_async {
        run_task_async_with_output(cwd, &actual_id, options)
    } else {
        run_task_with_output(cwd, &actual_id, options)
    };

    // Rollback claim on spawn failure so the subtask isn't stuck InProgress
    if result.is_err() && next_subtask {
        let rollback_event = TaskEvent::Stopped {
            task_ids: vec![actual_id.clone()],
            reason: Some("Spawn failed, rolling back claim".to_string()),
            turn_id: None,
            timestamp: chrono::Utc::now(),
        };
        let _ = write_event(cwd, &rollback_event);
    }

    result
}

/// Get short IDs of all open blockers for a task (for diagnostics)
fn get_blocker_short_ids(graph: &TaskGraph, task_id: &str) -> Vec<String> {
    const BLOCKING_LINK_TYPES: &[&str] = &["blocked-by", "validates", "remediates", "follows-up", "depends-on"];
    let mut blockers = Vec::new();
    for link_type in BLOCKING_LINK_TYPES {
        for blocker_id in graph.edges.targets(task_id, link_type) {
            if let Some(blocker) = graph.tasks.get(blocker_id) {
                if blocker.status != TaskStatus::Closed {
                    blockers.push(short_id(blocker_id).to_string());
                }
            }
        }
    }
    blockers
}

/// Wait for task(s) to reach a terminal state (closed or stopped)
fn run_wait(cwd: &Path, ids: Vec<String>) -> Result<()> {
    use std::time::Duration;

    let poll_interval = Duration::from_millis(500);

    // Resolve all task IDs up front (prefix → full)
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let mut resolved_ids = Vec::new();
    for id in &ids {
        resolved_ids.push(resolve_task_id_in_graph(&graph, id)?);
    }
    let ids = resolved_ids;

    // Poll until all tasks are in terminal state
    loop {
        let events = read_events(cwd)?;
        let tasks = materialize_graph(&events).tasks;

        let all_done = ids.iter().all(|id| {
            find_task(&tasks, id)
                .map(|t| matches!(t.status, TaskStatus::Closed | TaskStatus::Stopped))
                .unwrap_or(false)
        });

        if all_done {
            // Build markdown output with results for each task
            let mut content = String::from("## Wait Complete\n| ID | Name | Status | Outcome | Summary |\n|----|------|--------|---------|--------|\n");
            for id in &ids {
                if let Ok(task) = find_task(&tasks, id) {
                    let status = task.status.to_string();
                    let outcome = task
                        .closed_outcome
                        .as_ref()
                        .map(|o| o.to_string())
                        .unwrap_or_default();
                    let summary = task.effective_summary().unwrap_or_default().to_string();
                    content.push_str(&format!(
                        "| {} | {} | {} | {} | {} |\n",
                        id, task.name, status, outcome, summary,
                    ));
                }
            }
            let in_progress = get_in_progress(&tasks);
            let graph = materialize_graph(&events);
            let scope_set = get_current_scope_set(&graph);
            let ready_queue = get_ready_queue_for_scope_set(&graph, &scope_set);
            let xml_scopes = scope_set.to_xml_scopes();
            let mut builder = MdBuilder::new("wait");
            if !xml_scopes.is_empty() {
                builder = builder.with_scopes(&xml_scopes);
            }
            let xml = builder.build(&content, &in_progress, &ready_queue);
            aiki_print(&xml);
            return Ok(());
        }

        std::thread::sleep(poll_interval);
    }
}

/// Handle template subcommands (list, show)
fn run_template(cwd: &Path, command: TemplateCommands) -> Result<()> {
    use crate::tasks::templates::{find_templates_dir, list_templates, load_template};

    // Find templates directory
    let templates_dir = match find_templates_dir(cwd) {
        Ok(dir) => dir,
        Err(_) => {
            // No templates directory found - show helpful message
            let xml = MdBuilder::new("template").build_error(
                "No templates directory found. Create .aiki/templates/ to add templates.",
            );
            aiki_print(&xml);
            return Ok(());
        }
    };

    match command {
        TemplateCommands::List => {
            let templates = list_templates(&templates_dir)?;

            if templates.is_empty() {
                let md = MdBuilder::new("template")
                    .build_error("No templates found. Create template files in .aiki/templates/");
                aiki_print(&md);
                return Ok(());
            }

            // Build markdown output
            let mut content = String::from("## Templates\n");
            for template in &templates {
                let desc = template.description.as_deref().unwrap_or("");
                if desc.is_empty() {
                    content.push_str(&format!("- **{}**\n", &template.name));
                } else {
                    content.push_str(&format!("- **{}** — {}\n", &template.name, desc));
                }
            }

            let empty: Vec<&Task> = vec![];
            let md = MdBuilder::new("template").build(&content, &empty, &empty);
            aiki_print(&md);
        }
        TemplateCommands::Show { name } => {
            let template = load_template(&name, &templates_dir)?;

            // Build markdown output showing template details
            let mut content = format!("## Template: {}\n", &template.name);

            // Show source location
            if let Some(ref path) = template.source_path {
                content.push_str(&format!("- Source: {}\n", path));
            }

            if let Some(ref v) = template.version {
                content.push_str(&format!("- **Version:** {}\n", v));
            }
            if let Some(ref desc) = template.description {
                content.push_str(&format!("- **Description:** {}\n", desc));
            }
            if let Some(ref t) = template.defaults.task_type {
                content.push_str(&format!("- **Type:** {}\n", t));
            }
            if let Some(ref a) = template.defaults.assignee {
                content.push_str(&format!("- **Assignee:** {}\n", a));
            }
            if let Some(ref p) = template.defaults.priority {
                content.push_str(&format!("- **Priority:** {}\n", p));
            }

            // Show parent task name
            content.push_str(&format!("- **Parent:** {}\n", &template.parent.name));

            // Show subtasks
            if !template.subtasks.is_empty() {
                content.push_str("\n### Subtasks\n");
                for subtask in &template.subtasks {
                    content.push_str(&format!("- {}\n", &subtask.name));
                }
            }

            // Show full template content
            if let Some(ref raw) = template.raw_content {
                content.push_str("\n### Content\n```\n");
                content.push_str(raw);
                content.push_str("\n```\n");
            }

            let empty: Vec<&Task> = vec![];
            let md = MdBuilder::new("template").build(&content, &empty, &empty);
            aiki_print(&md);
        }
    }

    Ok(())
}

/// Parameters for creating a task from a template.
///
/// This struct provides a unified interface for all template-based task creation:
/// `task add --template`, `build`, `fix`, and `review`. Each caller sets only the
/// fields it needs; the rest use defaults.
#[derive(Default)]
pub struct TemplateTaskParams {
    /// Template name (e.g., "aiki/review", "aiki/build")
    pub template_name: String,
    /// Data variables for template substitution (key=value pairs)
    pub data: HashMap<String, String>,
    /// Source references (e.g., "file:path", "task:id")
    pub sources: Vec<String>,
    /// Assignee for the task
    pub assignee: Option<String>,
    /// Priority override (if None, resolved from template defaults)
    pub priority: Option<TaskPriority>,
    /// When set, create as a child task (generates child ID instead of standalone)
    pub parent_id: Option<String>,
    /// Parent name for {{parent.name}} variable (parent.id comes from parent_id)
    pub parent_name: Option<String>,
    /// Source metadata for {{source.*}} variables (e.g., source.name, source.id)
    pub source_data: HashMap<String, String>,
    /// Additional builtins for {{key}} variables
    pub builtins: HashMap<String, String>,
    /// Pre-generated task ID (used by spawn system for deterministic subtask IDs).
    /// If None, a random ID is generated.
    pub task_id: Option<String>,
}

/// Create task(s) from a template (shared logic for all template-based task creation)
///
/// This is the single code path for creating tasks from templates. Used by:
/// - `task add --template` (CLI)
/// - `build` command
/// - `fix` command
/// - `review` command
pub fn create_from_template(cwd: &Path, params: TemplateTaskParams) -> Result<String> {
    use crate::tasks::templates::{
        find_templates_dir, load_template, substitute_with_template_name, VariableContext,
    };

    let template_name = &params.template_name;

    // Find and load template
    let templates_dir = find_templates_dir(cwd)?;
    let template = load_template(template_name, &templates_dir)?;

    // Determine priority: explicit param > template default > P2
    let priority = if let Some(p) = params.priority {
        p
    } else if let Some(ref p) = template.defaults.priority {
        TaskPriority::from_str(p).unwrap_or_default()
    } else {
        TaskPriority::default()
    };

    // Determine assignee: explicit param > template default > None
    let assignee = if params.assignee.is_some() {
        params.assignee.clone()
    } else if let Some(ref a) = template.defaults.assignee {
        Some(a.clone())
    } else {
        None
    };

    // Merge data: template defaults first, then params.data overrides
    let mut data = HashMap::new();
    for (key, value) in &template.defaults.data {
        let value_str = match value {
            serde_json::Value::String(s) => s.clone(),
            _ => value.to_string(),
        };
        data.insert(key.clone(), value_str);
    }
    // Params data overrides template defaults
    for (key, value) in &params.data {
        data.insert(key.clone(), value.clone());
    }

    // Build variable context
    let mut ctx = VariableContext::new();
    for (key, value) in &data {
        ctx.set_data(key, value);
    }
    if let Some(ref a) = assignee {
        ctx.set_builtin("assignee", a);
    }
    ctx.set_builtin("priority", priority.to_string());
    if let Some(ref t) = template.defaults.task_type {
        ctx.set_builtin("type", t);
    }
    if let Some(source) = params.sources.first() {
        ctx.set_source(source);
    }

    // Apply additional source data (overrides auto-parsed source data)
    for (key, value) in &params.source_data {
        ctx.set_source_data(key, value);
    }

    // Apply parent variables
    if let Some(ref parent_id) = params.parent_id {
        ctx.set_parent("id", parent_id);
    }
    if let Some(ref parent_name) = params.parent_name {
        ctx.set_parent("name", parent_name);
    }

    // Apply additional builtins
    for (key, value) in &params.builtins {
        ctx.set_builtin(key, value);
    }

    // Substitute variables in parent task name
    let parent_name =
        substitute_with_template_name(&template.parent.name, &ctx, Some(template_name))?;

    // Use pre-generated task ID if provided, otherwise generate a new one
    let events = read_events(cwd)?;
    let mut graph = materialize_graph(&events);
    let task_id = params
        .task_id
        .unwrap_or_else(|| generate_task_id(&parent_name));

    // Set id in context for substitution
    ctx.set_builtin("id", &task_id);

    let timestamp = chrono::Utc::now();
    ctx.set_builtin("created", timestamp.to_rfc3339());

    // Substitute variables in parent instructions
    let parent_instructions = if !template.parent.instructions.is_empty() {
        Some(substitute_with_template_name(
            &template.parent.instructions,
            &ctx,
            Some(template_name),
        )?)
    } else {
        None
    };

    // Store spawns config on task data so it's available at close time
    if !template.spawns.is_empty() {
        if let Ok(spawns_json) = serde_json::to_string(&template.spawns) {
            data.insert("_spawns".to_string(), spawns_json);
        }
    }

    // Create parent task event
    let working_copy = get_working_copy_change_id(cwd);
    let create_event = TaskEvent::Created {
        task_id: task_id.clone(),
        name: parent_name.clone(),
        slug: None,
        task_type: template.parent.task_type.clone(),
        priority,
        assignee: assignee.clone(),
        sources: params.sources.clone(),
        template: Some(template.template_id()),
        working_copy: working_copy.clone(),
        instructions: parent_instructions.clone(),
        data: data.clone(),
        timestamp,
    };
    write_event(cwd, &create_event)?;

    // Insert into in-memory graph so subtask write_link_event validation passes
    graph.tasks.insert(
        task_id.clone(),
        Task {
            id: task_id.clone(),
            name: parent_name.clone(),
            slug: None,
            task_type: template.parent.task_type.clone(),
            status: TaskStatus::Open,
            priority,
            assignee: assignee.clone(),
            sources: params.sources.clone(),
            template: Some(template.template_id()),
            working_copy,
            instructions: parent_instructions,
            data: data.clone(),
            created_at: timestamp,
            started_at: None,
            claimed_by_session: None,
            last_session_id: None,
            stopped_reason: None,
            closed_outcome: None,
            summary: None,
            turn_started: None,
            turn_closed: None,
            turn_stopped: None,
            comments: vec![],
        },
    );

    // Emit subtask-of link if this is a child task
    if let Some(ref parent_id) = params.parent_id {
        write_link_event(cwd, &graph, "subtask-of", &task_id, parent_id)?;
    }

    // Emit sourced-from links for each source
    for source in &params.sources {
        write_link_event(cwd, &graph, "sourced-from", &task_id, source)?;
    }

    // Set parent.* to current task for subtask variable substitution.
    // Static subtasks within this template use {{parent.id}} to reference their parent.
    ctx.set_parent("id", &task_id);
    ctx.set_parent("name", &parent_name);
    if let Some(ref a) = assignee {
        ctx.set_parent("assignee", a);
    }
    ctx.set_parent("priority", priority.to_string());
    for (key, value) in &data {
        ctx.set_parent(&format!("data.{}", key), value);
    }

    // Pre-scan for parent.subtasks.* references and populate with placeholders.
    // These get replaced with actual task IDs in the two-phase creation below.
    if let Some(ref raw_content) = template.raw_content {
        for var_name in crate::tasks::templates::find_variables(raw_content) {
            if let Some(slug) = var_name.strip_prefix("parent.subtasks.") {
                ctx.set_parent(
                    &format!("subtasks.{}", slug),
                    &format!(
                        "{}{}{}",
                        SUBTASK_SLUG_PLACEHOLDER_PREFIX, slug, SUBTASK_SLUG_PLACEHOLDER_SUFFIX
                    ),
                );
            }
        }
    }

    // Create subtasks - route based on template type
    if template.raw_content.as_ref().is_some_and(|c| {
        crate::tasks::templates::has_subtask_refs(c) || crate::tasks::templates::has_inline_loops(c)
    }) {
        // Composable templates: use entry-based flow for {% subtask %} refs or {% for %} loops
        let (_, entries) =
            crate::tasks::templates::create_subtask_entries_from_template(&template, &ctx, None)?;
        let composition_stack = vec![template_name.to_string()];
        create_subtasks_from_entries(
            cwd,
            &entries,
            template_name,
            &template.template_id(),
            template.defaults.task_type.as_deref(),
            &task_id,
            &parent_name,
            &params.sources,
            priority,
            &assignee,
            &data,
            &ctx,
            timestamp,
            &params.builtins,
            &composition_stack,
            1, // depth starts at 1 (parent template is depth 0)
            &mut graph,
        )?;
    } else {
        // H2 body subtasks: parsed at template level, create entries via the entry-based flow
        let entries: Vec<crate::tasks::templates::SubtaskEntry> = template
            .subtasks
            .iter()
            .map(|def| crate::tasks::templates::SubtaskEntry::Static(def.clone()))
            .collect();
        if !entries.is_empty() {
            let composition_stack = vec![template_name.to_string()];
            create_subtasks_from_entries(
                cwd,
                &entries,
                template_name,
                &template.template_id(),
                template.defaults.task_type.as_deref(),
                &task_id,
                &parent_name,
                &params.sources,
                priority,
                &assignee,
                &data,
                &ctx,
                timestamp,
                &params.builtins,
                &composition_stack,
                1,
                &mut graph,
            )?;
        }
    }

    Ok(task_id)
}

/// Maximum depth for recursive template composition
const MAX_COMPOSITION_DEPTH: usize = 4;

/// Replace subtask slug placeholders with actual task IDs
fn replace_slug_placeholders(text: &str, slug_map: &HashMap<String, String>) -> String {
    let mut result = text.to_string();
    for (slug, task_id) in slug_map {
        let placeholder = format!(
            "{}{}{}",
            SUBTASK_SLUG_PLACEHOLDER_PREFIX, slug, SUBTASK_SLUG_PLACEHOLDER_SUFFIX
        );
        result = result.replace(&placeholder, task_id);
    }
    result
}

/// Check if text contains any unresolved subtask slug placeholders
fn check_unresolved_slug_placeholders(text: &str) -> Result<()> {
    if let Some(start) = text.find(SUBTASK_SLUG_PLACEHOLDER_PREFIX) {
        let after = &text[start + SUBTASK_SLUG_PLACEHOLDER_PREFIX.len()..];
        if let Some(end) = after.find(SUBTASK_SLUG_PLACEHOLDER_SUFFIX) {
            let slug = &after[..end];
            return Err(AikiError::TemplateProcessingFailed {
                details: format!(
                    "Subtask slug '{}' referenced via {{{{parent.subtasks.{}}}}} but no sibling has that slug",
                    slug, slug
                ),
            });
        }
    }
    Ok(())
}

/// Create subtasks from a list of SubtaskEntry items (handles both static and composed)
///
/// Uses a two-phase approach:
/// - **Phase A (Plan)**: Generate all task IDs and collect slug→taskID map
/// - **Phase B (Execute)**: Create events, replacing slug placeholders with actual task IDs
///
/// This enables `{{parent.subtasks.{slug}}}` to resolve to sibling task IDs.
///
/// # Arguments
/// * `cwd` - Working directory
/// * `entries` - List of subtask entries to create
/// * `template_name` - Parent template name (for variable substitution context)
/// * `parent_id` - ID of the parent task
/// * `parent_name` - Resolved name of the parent task (for parent.name in child contexts)
/// * `sources` - Source references for the parent task
/// * `parent_priority` - Priority inherited from parent
/// * `parent_assignee` - Assignee inherited from parent
/// * `parent_data` - Data inherited from parent
/// * `parent_ctx` - Variable context from the parent
/// * `timestamp` - Timestamp for event creation
/// * `extra_builtins` - Additional builtin variables
/// * `composition_stack` - Stack of template names for cycle detection
/// * `depth` - Current composition depth
fn create_subtasks_from_entries(
    cwd: &Path,
    entries: &[crate::tasks::templates::SubtaskEntry],
    template_name: &str,
    template_id: &str,
    parent_task_type: Option<&str>,
    parent_id: &str,
    parent_name: &str,
    sources: &[String],
    parent_priority: TaskPriority,
    parent_assignee: &Option<String>,
    parent_data: &std::collections::HashMap<String, String>,
    parent_ctx: &crate::tasks::templates::VariableContext,
    timestamp: chrono::DateTime<chrono::Utc>,
    extra_builtins: &HashMap<String, String>,
    composition_stack: &[String],
    depth: usize,
    graph: &mut TaskGraph,
) -> Result<()> {
    use crate::tasks::templates::{
        find_templates_dir, load_template, substitute_with_template_name, SubtaskEntry,
        TaskTemplate, VariableContext,
    };

    // ── Phase A: Plan ──
    // Generate all task IDs upfront and collect slug→taskID map.
    // For Composed entries, load the child template to extract its slug.
    struct PlannedSubtask {
        task_id: String,
        slug: Option<String>,
        child_template: Option<TaskTemplate>,
    }

    let mut planned: Vec<PlannedSubtask> = Vec::new();
    let mut slug_map: HashMap<String, String> = HashMap::new();

    for (i, entry) in entries.iter().enumerate() {
        let subtask_id = generate_task_id(&format!("subtask-{}", i + 1));

        let (slug, child_template) = match entry {
            SubtaskEntry::Static(def) => (def.slug.clone(), None),
            SubtaskEntry::Composed {
                template_name: child_template_name,
                line,
            } => {
                // Validate depth and cycles early
                if depth > MAX_COMPOSITION_DEPTH {
                    return Err(AikiError::TemplateProcessingFailed {
                        details: format!(
                            "Template composition depth limit ({}) exceeded at '{}'",
                            MAX_COMPOSITION_DEPTH, child_template_name
                        ),
                    });
                }
                if composition_stack.contains(child_template_name) {
                    let cycle_path = composition_stack.join(" → ");
                    return Err(AikiError::TemplateProcessingFailed {
                        details: format!(
                            "Template cycle detected: {} → {}",
                            cycle_path, child_template_name
                        ),
                    });
                }

                let templates_dir = find_templates_dir(cwd)?;
                let child = load_template(child_template_name, &templates_dir).map_err(|e| {
                    AikiError::TemplateProcessingFailed {
                        details: format!(
                            "Template '{}' not found in {{% subtask %}} at line {}: {}",
                            child_template_name, line, e
                        ),
                    }
                })?;
                let slug = child.parent.slug.clone();
                (slug, Some(child))
            }
        };

        if let Some(ref s) = slug {
            slug_map.insert(s.clone(), subtask_id.clone());
        }
        planned.push(PlannedSubtask {
            task_id: subtask_id,
            slug,
            child_template,
        });
    }

    // ── Phase B: Execute ──
    // Create events for each entry, replacing slug placeholders with actual task IDs.
    for (i, entry) in entries.iter().enumerate() {
        let subtask_id = &planned[i].task_id;
        let entry_slug = &planned[i].slug;

        match entry {
            SubtaskEntry::Static(subtask_def) => {
                let subtask_priority = if let Some(ref p) = subtask_def.priority {
                    TaskPriority::from_str(p).unwrap_or(parent_priority)
                } else {
                    parent_priority
                };

                let subtask_assignee = if let Some(ref a) = subtask_def.assignee {
                    Some(a.clone())
                } else {
                    parent_assignee.clone()
                };

                let mut subtask_data = parent_data.clone();
                for (key, value) in &subtask_def.data {
                    let value_str = match value {
                        serde_json::Value::String(s) => s.clone(),
                        _ => value.to_string(),
                    };
                    subtask_data.insert(key.clone(), value_str);
                }

                let mut subtask_ctx = VariableContext::new();
                for (key, value) in &subtask_data {
                    subtask_ctx.set_data(key, value);
                }
                subtask_ctx.set_builtin("id", subtask_id);
                if let Some(ref a) = subtask_assignee {
                    subtask_ctx.set_builtin("assignee", a);
                }
                subtask_ctx.set_builtin("priority", subtask_priority.to_string());
                subtask_ctx.set_builtin("created", timestamp.to_rfc3339());
                if let Some(t) = parent_task_type {
                    subtask_ctx.set_builtin("type", t);
                }
                subtask_ctx.set_parent("id", parent_id);
                if let Some(ref a) = parent_assignee {
                    subtask_ctx.set_parent("assignee", a);
                }
                subtask_ctx.set_parent("priority", parent_priority.to_string());
                for (key, value) in parent_data {
                    subtask_ctx.set_parent(&format!("data.{}", key), value);
                }
                if let Some(source) = sources.first() {
                    subtask_ctx.set_source(source);
                    subtask_ctx.set_parent("source", source);
                }
                for (key, value) in extra_builtins {
                    subtask_ctx.set_builtin(key, value);
                }

                let subtask_name = substitute_with_template_name(
                    &subtask_def.name,
                    &subtask_ctx,
                    Some(template_name),
                )?;
                let subtask_instructions = if !subtask_def.instructions.is_empty() {
                    let resolved = substitute_with_template_name(
                        &subtask_def.instructions,
                        &subtask_ctx,
                        Some(template_name),
                    )?;
                    // Replace slug placeholders with actual sibling task IDs
                    let replaced = replace_slug_placeholders(&resolved, &slug_map);
                    check_unresolved_slug_placeholders(&replaced)?;
                    Some(replaced)
                } else {
                    None
                };

                let mut subtask_sources: Vec<String> = subtask_def
                    .sources
                    .iter()
                    .map(|s| substitute_with_template_name(s, &subtask_ctx, Some(template_name)))
                    .collect::<Result<Vec<_>>>()?;
                subtask_sources.push(format!("task:{}", parent_id));

                // Validate slug if present
                if let Some(ref s) = subtask_def.slug {
                    if !crate::tasks::is_valid_slug(s) {
                        return Err(AikiError::InvalidSlug(s.clone()));
                    }
                    crate::tasks::graph::validate_slug_unique(graph, parent_id, s)?;
                }

                let subtask_event = TaskEvent::Created {
                    task_id: subtask_id.clone(),
                    name: subtask_name,
                    slug: subtask_def.slug.clone(),
                    task_type: None,
                    priority: subtask_priority,
                    assignee: subtask_assignee,
                    sources: subtask_sources,
                    template: Some(template_id.to_string()),
                    working_copy: None,
                    instructions: subtask_instructions,
                    data: subtask_data,
                    timestamp,
                };
                write_event(cwd, &subtask_event)?;
                write_link_event(cwd, graph, "subtask-of", subtask_id, parent_id)?;

                // Update in-memory slug index
                if let Some(ref s) = subtask_def.slug {
                    graph
                        .slug_index
                        .insert((parent_id.to_string(), s.clone()), subtask_id.clone());
                }
            }

            SubtaskEntry::Composed {
                template_name: child_template_name,
                ..
            } => {
                // Child template was already loaded and validated in Phase A
                let child_template = planned[i]
                    .child_template
                    .as_ref()
                    .expect("child_template should be populated in plan phase");

                let child_priority = if let Some(ref p) = child_template.defaults.priority {
                    TaskPriority::from_str(p).unwrap_or(parent_priority)
                } else {
                    parent_priority
                };
                let child_assignee = if let Some(ref a) = child_template.defaults.assignee {
                    Some(a.clone())
                } else {
                    parent_assignee.clone()
                };

                let mut child_data = parent_data.clone();
                for (key, value) in &child_template.defaults.data {
                    let value_str = match value {
                        serde_json::Value::String(s) => s.clone(),
                        _ => value.to_string(),
                    };
                    child_data.insert(key.clone(), value_str);
                }

                // Build child variable context (inherits parent's full context)
                let mut child_ctx = parent_ctx.clone();
                for (key, value) in &child_data {
                    child_ctx.set_data(key, value);
                }
                child_ctx.set_builtin("id", subtask_id);
                if let Some(ref a) = child_assignee {
                    child_ctx.set_builtin("assignee", a);
                }
                child_ctx.set_builtin("priority", child_priority.to_string());
                child_ctx.set_builtin("created", timestamp.to_rfc3339());
                if let Some(ref t) = child_template.defaults.task_type {
                    child_ctx.set_builtin("type", t);
                }

                // Set parent.* to point to the outer parent
                child_ctx.set_parent("id", parent_id);
                child_ctx.set_parent("name", parent_name);
                if let Some(ref a) = parent_assignee {
                    child_ctx.set_parent("assignee", a);
                }
                child_ctx.set_parent("priority", parent_priority.to_string());
                for (key, value) in parent_data {
                    child_ctx.set_parent(&format!("data.{}", key), value);
                }
                if let Some(source) = sources.first() {
                    child_ctx.set_parent("source", source);
                }

                let child_name = substitute_with_template_name(
                    &child_template.parent.name,
                    &child_ctx,
                    Some(child_template_name),
                )?;

                let child_instructions = if !child_template.parent.instructions.is_empty() {
                    Some(substitute_with_template_name(
                        &child_template.parent.instructions,
                        &child_ctx,
                        Some(child_template_name),
                    )?)
                } else {
                    None
                };

                // Validate and register composed slug
                if let Some(ref s) = entry_slug {
                    if !crate::tasks::is_valid_slug(s) {
                        return Err(AikiError::InvalidSlug(s.clone()));
                    }
                    crate::tasks::graph::validate_slug_unique(graph, parent_id, s)?;
                }

                // Rebind parent.* for sub-subtasks
                child_ctx.set_parent("id", subtask_id);
                child_ctx.set_parent("name", &child_name);
                if let Some(ref a) = child_assignee {
                    child_ctx.set_parent("assignee", a);
                }
                child_ctx.set_parent("priority", child_priority.to_string());
                for (key, value) in &child_data {
                    child_ctx.set_parent(&format!("data.{}", key), value);
                }

                // Create the composed subtask event with slug from child template frontmatter
                let composed_sources = vec![format!("task:{}", parent_id)];
                let composed_event = TaskEvent::Created {
                    task_id: subtask_id.clone(),
                    name: child_name.clone(),
                    slug: entry_slug.clone(),
                    task_type: child_template.defaults.task_type.clone(),
                    priority: child_priority,
                    assignee: child_assignee.clone(),
                    sources: composed_sources.clone(),
                    template: Some(child_template.template_id()),
                    working_copy: None,
                    instructions: child_instructions.clone(),
                    data: child_data.clone(),
                    timestamp,
                };
                write_event(cwd, &composed_event)?;
                write_link_event(cwd, graph, "subtask-of", subtask_id, parent_id)?;

                // Insert into in-memory graph with slug
                graph.tasks.insert(
                    subtask_id.clone(),
                    Task {
                        id: subtask_id.clone(),
                        name: child_name.clone(),
                        slug: entry_slug.clone(),
                        task_type: child_template.defaults.task_type.clone(),
                        status: TaskStatus::Open,
                        priority: child_priority,
                        assignee: child_assignee.clone(),
                        sources: composed_sources.clone(),
                        template: Some(child_template.template_id()),
                        working_copy: None,
                        instructions: child_instructions,
                        data: child_data.clone(),
                        created_at: timestamp,
                        started_at: None,
                        claimed_by_session: None,
                        last_session_id: None,
                        stopped_reason: None,
                        closed_outcome: None,
                        summary: None,
                        turn_started: None,
                        turn_closed: None,
                        turn_stopped: None,
                        comments: vec![],
                    },
                );

                // Update slug index for the composed subtask
                if let Some(ref s) = entry_slug {
                    graph
                        .slug_index
                        .insert((parent_id.to_string(), s.clone()), subtask_id.clone());
                }

                // Recursively create the child template's subtasks
                let mut child_stack = composition_stack.to_vec();
                child_stack.push(child_template_name.clone());

                let child_entries = crate::tasks::templates::create_subtask_entries_from_template(
                    child_template,
                    &child_ctx,
                    None,
                )?;

                if !child_entries.1.is_empty() {
                    create_subtasks_from_entries(
                        cwd,
                        &child_entries.1,
                        child_template_name,
                        &child_template.template_id(),
                        child_template.defaults.task_type.as_deref(),
                        subtask_id,
                        &child_name,
                        &composed_sources,
                        child_priority,
                        &child_assignee,
                        &child_data,
                        &child_ctx,
                        timestamp,
                        extra_builtins,
                        &child_stack,
                        depth + 1,
                        graph,
                    )?;
                }
            }
        }
    }

    Ok(())
}

/// Check if a string has an external reference prefix
fn has_external_ref_prefix(s: &str) -> bool {
    s.starts_with("file:")
        || s.starts_with("prompt:")
        || s.starts_with("comment:")
        || s.starts_with("issue:")
}

/// Normalize a link target to its canonical storage form.
/// Called at write time — the event always stores canonical IDs.
/// Kind-aware: task-only kinds reject non-task targets instead of
/// silently coercing them to file: paths.
fn normalize_link_target(input: &str, kind: &str, graph: &TaskGraph) -> Result<String> {
    use crate::tasks::graph::is_task_only_kind;

    // 1. Strip task: prefix if present
    let stripped = input.strip_prefix("task:").unwrap_or(input);

    // 2. If it's already a full 32-char task ID, use it directly
    if is_task_id(stripped) {
        if graph.tasks.contains_key(stripped) {
            return Ok(stripped.to_string());
        }
        // Full-length ID but not found
        if is_task_only_kind(kind) {
            return Err(AikiError::InvalidLinkTarget {
                kind: kind.to_string(),
                target: stripped.to_string(),
            });
        }
        return Ok(stripped.to_string());
    }

    // 3. If it has an external reference prefix
    if has_external_ref_prefix(stripped) {
        if is_task_only_kind(kind) {
            return Err(AikiError::InvalidLinkTarget {
                kind: kind.to_string(),
                target: stripped.to_string(),
            });
        }
        return Ok(stripped.to_string());
    }

    // 4. Try resolving as a short task ID prefix or slug reference
    match resolve_task_id_in_graph(graph, stripped) {
        Ok(full_id) => Ok(full_id),
        Err(AikiError::TaskNotFound(_)) if !is_task_only_kind(kind) => {
            // Flexible-target kinds: treat unresolved input as file path
            Ok(format!("file:{}", stripped))
        }
        Err(AikiError::TaskNotFound(_)) => {
            // Task-only kinds: wrap as InvalidLinkTarget for clearer messaging
            Err(AikiError::InvalidLinkTarget {
                kind: kind.to_string(),
                target: stripped.to_string(),
            })
        }
        // AmbiguousTaskId, PrefixTooShort — propagate for all kinds
        Err(e) => Err(e),
    }
}

/// Emit link events for all link flags provided on task add/start.
/// Handles blocking links (blocked-by, depends-on, validates, remediates),
/// supersedes, and other link types. sourced-from and subtask-of are
/// handled by existing codepaths in run_add/run_start.
fn emit_link_flags(
    cwd: &Path,
    graph: &TaskGraph,
    task_id: &str,
    blocked_by: &[String],
    depends_on: &[String],
    validates: &[String],
    remediates: &[String],
    supersedes: &Option<String>,
    implements: &Option<String>,
    orchestrates: &Option<String>,
    fixes: &[String],
    decomposes_plan: &Option<String>,
    adds_plan: &Option<String>,
    autorun: bool,
) -> Result<()> {
    // Blocking links: use autorun variant when --autorun is set
    let autorun_opt = if autorun { Some(true) } else { None };
    for target in blocked_by {
        write_link_event_with_autorun(cwd, graph, "blocked-by", task_id, target, autorun_opt)?;
    }
    for target in depends_on {
        write_link_event_with_autorun(cwd, graph, "depends-on", task_id, target, autorun_opt)?;
    }
    for target in validates {
        write_link_event_with_autorun(cwd, graph, "validates", task_id, target, autorun_opt)?;
    }
    for target in remediates {
        write_link_event_with_autorun(cwd, graph, "remediates", task_id, target, autorun_opt)?;
    }
    // Non-blocking links: autorun not applicable
    if let Some(target) = supersedes {
        write_link_event(cwd, graph, "supersedes", task_id, target)?;
    }
    if let Some(target) = implements {
        write_link_event(cwd, graph, "implements-plan", task_id, target)?;
    }
    if let Some(target) = orchestrates {
        write_link_event(cwd, graph, "orchestrates", task_id, target)?;
    }
    for target in fixes {
        write_link_event(cwd, graph, "fixes", task_id, target)?;
    }
    if let Some(target) = decomposes_plan {
        write_link_event(cwd, graph, "decomposes-plan", task_id, target)?;
    }
    if let Some(target) = adds_plan {
        write_link_event(cwd, graph, "adds-plan", task_id, target)?;
    }
    Ok(())
}

/// Extract the single (kind, target) pair from the link/unlink flags.
/// Returns an error if zero or more than one flag is set.
fn extract_link_flag(
    blocked_by: Option<String>,
    depends_on: Option<String>,
    validates: Option<String>,
    remediates: Option<String>,
    sourced_from: Option<String>,
    subtask_of: Option<String>,
    implements: Option<String>,
    orchestrates: Option<String>,
    supersedes: Option<String>,
    fixes: Option<String>,
    decomposes_plan: Option<String>,
    adds_plan: Option<String>,
) -> Result<(String, String)> {
    let mut pairs: Vec<(&str, String)> = Vec::new();
    if let Some(v) = blocked_by {
        pairs.push(("blocked-by", v));
    }
    if let Some(v) = depends_on {
        pairs.push(("depends-on", v));
    }
    if let Some(v) = validates {
        pairs.push(("validates", v));
    }
    if let Some(v) = remediates {
        pairs.push(("remediates", v));
    }
    if let Some(v) = sourced_from {
        pairs.push(("sourced-from", v));
    }
    if let Some(v) = subtask_of {
        pairs.push(("subtask-of", v));
    }
    if let Some(v) = implements {
        pairs.push(("implements-plan", v));
    }
    if let Some(v) = orchestrates {
        pairs.push(("orchestrates", v));
    }
    if let Some(v) = supersedes {
        pairs.push(("supersedes", v));
    }
    if let Some(v) = fixes {
        pairs.push(("fixes", v));
    }
    if let Some(v) = decomposes_plan {
        pairs.push(("decomposes-plan", v));
    }
    if let Some(v) = adds_plan {
        pairs.push(("adds-plan", v));
    }

    match pairs.len() {
        0 => {
            let msg = "No link kind specified. Use one of: --blocked-by, --depends-on, --validates, --remediates, --sourced-from, --subtask-of, --implements, --orchestrates, --supersedes, --fixes, --decomposes-plan, --adds-plan";
            aiki_print(&MdBuilder::new("link").error().build_error(msg));
            Err(AikiError::Other(anyhow::anyhow!("{}", msg)))
        }
        1 => {
            let (kind, target) = pairs.remove(0);
            Ok((kind.to_string(), target))
        }
        _ => {
            let msg = "Only one link kind flag can be specified at a time";
            aiki_print(&MdBuilder::new("link").error().build_error(msg));
            Err(AikiError::Other(anyhow::anyhow!("{}", msg)))
        }
    }
}

/// Add a link between tasks
fn run_link(
    cwd: &Path,
    id: String,
    blocked_by: Option<String>,
    depends_on: Option<String>,
    validates: Option<String>,
    remediates: Option<String>,
    sourced_from: Option<String>,
    subtask_of: Option<String>,
    implements: Option<String>,
    orchestrates: Option<String>,
    supersedes: Option<String>,
    fixes: Option<String>,
    decomposes_plan: Option<String>,
    adds_plan: Option<String>,
) -> Result<()> {
    let (kind, raw_target) = extract_link_flag(
        blocked_by,
        depends_on,
        validates,
        remediates,
        sourced_from,
        subtask_of,
        implements,
        orchestrates,
        supersedes,
        fixes,
        decomposes_plan,
        adds_plan,
    )?;

    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);

    // Resolve the subject task
    let from_task = find_task_in_graph(&graph, &id)?;
    let from_id = from_task.id.clone();

    // Delegate all validation, cardinality, and writing to write_link_event
    let wrote = write_link_event(cwd, &graph, &kind, &from_id, &raw_target)?;

    if wrote {
        eprintln!(
            "Linked: {} --{} {}",
            short_id(&from_id),
            kind,
            short_id(&raw_target)
        );
    } else {
        eprintln!(
            "Link already exists: {} --{} {}",
            short_id(&from_id),
            kind,
            short_id(&raw_target)
        );
    }
    Ok(())
}

/// Remove a link between tasks
fn run_unlink(
    cwd: &Path,
    id: String,
    blocked_by: Option<String>,
    depends_on: Option<String>,
    validates: Option<String>,
    remediates: Option<String>,
    sourced_from: Option<String>,
    subtask_of: Option<String>,
    implements: Option<String>,
    orchestrates: Option<String>,
    supersedes: Option<String>,
    fixes: Option<String>,
    decomposes_plan: Option<String>,
    adds_plan: Option<String>,
) -> Result<()> {
    let (kind, raw_target) = extract_link_flag(
        blocked_by,
        depends_on,
        validates,
        remediates,
        sourced_from,
        subtask_of,
        implements,
        orchestrates,
        supersedes,
        fixes,
        decomposes_plan,
        adds_plan,
    )?;

    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);

    // Resolve the subject task
    let from_task = find_task_in_graph(&graph, &id)?;
    let from_id = from_task.id.clone();

    // Normalize the target
    let to_id = normalize_link_target(&raw_target, &kind, &graph)?;

    // Check the link exists
    if !graph.edges.has_link(&from_id, &to_id, &kind) {
        eprintln!(
            "No link found: {} --{} {}",
            short_id(&from_id),
            kind,
            short_id(&to_id)
        );
        return Ok(());
    }

    // Emit the LinkRemoved event
    let event = TaskEvent::LinkRemoved {
        from: from_id.clone(),
        to: to_id.clone(),
        kind: kind.clone(),
        reason: None,
        timestamp: chrono::Utc::now(),
    };
    write_event(cwd, &event)?;

    eprintln!(
        "Unlinked: {} --{} {}",
        short_id(&from_id),
        kind,
        short_id(&to_id)
    );
    Ok(())
}

/// Get the current working copy change_id from JJ
///
/// Returns the change_id of the current working copy (`@` in jj terms).
/// This is captured when creating tasks from templates for historical template lookup.
fn get_working_copy_change_id(cwd: &Path) -> Option<String> {
    use crate::jj::jj_cmd;

    let output = jj_cmd()
        .args(["log", "-r", "@", "-T", "change_id", "--no-graph"])
        .current_dir(cwd)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let change_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if change_id.is_empty() {
        None
    } else {
        Some(change_id)
    }
}

/// Reset all non-closed tasks.
///
/// Requires `--confirm reset` to prevent accidental data loss.
fn run_reset(cwd: &Path, confirm: Option<String>) -> Result<()> {
    // Require --confirm reset
    match confirm.as_deref() {
        Some("reset") => {} // confirmed
        Some(other) => {
            let xml = MdBuilder::new("reset")
                .error()
                .build_error(&format!(
                    "Invalid confirmation: '{}'. To confirm, run:\n  aiki task reset --confirm reset",
                    other
                ));
            aiki_print(&xml);
            return Ok(());
        }
        None => {
            let xml = MdBuilder::new("reset")
                .error()
                .build_error(
                    "This will close all tasks as won't-do. To confirm, run:\n  aiki task reset --confirm reset",
                );
            aiki_print(&xml);
            return Ok(());
        }
    }

    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);

    // Collect all non-closed task IDs
    let ids_to_close: Vec<String> = graph
        .tasks
        .values()
        .filter(|t| t.status != TaskStatus::Closed)
        .map(|t| t.id.clone())
        .collect();

    if ids_to_close.is_empty() {
        let xml = MdBuilder::new("reset").build_error("No tasks to reset");
        aiki_print(&xml);
        return Ok(());
    }

    let count = ids_to_close.len();

    // Close all via batch event
    let session_match = crate::session::find_active_session(cwd);
    let turn_id =
        crate::tasks::current_turn_id(session_match.as_ref().map(|m| m.session_id.as_str()));

    let close_event = TaskEvent::Closed {
        task_ids: ids_to_close,
        outcome: TaskOutcome::WontDo,
        summary: Some("Reset".to_string()),
        turn_id,
        timestamp: chrono::Utc::now(),
    };
    write_event(cwd, &close_event)?;

    aiki_print(&format!("Reset {} task(s)", count));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_task_revset_pattern() {
        let pattern = build_task_revset_pattern("abc123");
        assert!(pattern.contains("task=abc123"));
        assert!(pattern.contains("task=abc123."));
    }

    #[test]
    fn test_parse_diff_summary_files_basic() {
        let output = r#"M src/auth.ts
A src/new_file.ts
D src/old_file.ts
"#;
        let files = parse_diff_summary_files(output);

        assert_eq!(files.len(), 3);
        assert_eq!(files[0], "src/auth.ts");
        assert_eq!(files[1], "src/new_file.ts");
        assert_eq!(files[2], "src/old_file.ts");
    }

    #[test]
    fn test_parse_diff_summary_files_single() {
        let output = "M path/to/file.rs\n";
        let files = parse_diff_summary_files(output);

        assert_eq!(files.len(), 1);
        assert_eq!(files[0], "path/to/file.rs");
    }

    #[test]
    fn test_parse_diff_summary_files_empty() {
        let output = "";
        let files = parse_diff_summary_files(output);
        assert!(files.is_empty());
    }

    #[test]
    fn test_parse_diff_summary_with_types_basic() {
        let output = "M src/auth.rs\nA src/new_file.rs\nD src/old_file.rs\n";
        let changes = parse_diff_summary_with_types(output);

        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0], ("M".to_string(), "src/auth.rs".to_string()));
        assert_eq!(changes[1], ("A".to_string(), "src/new_file.rs".to_string()));
        assert_eq!(changes[2], ("D".to_string(), "src/old_file.rs".to_string()));
    }

    #[test]
    fn test_parse_diff_summary_with_types_empty() {
        let output = "";
        let changes = parse_diff_summary_with_types(output);
        assert!(changes.is_empty());
    }

    #[test]
    fn test_parse_diff_summary_with_types_single() {
        let output = "M path/to/file.rs\n";
        let changes = parse_diff_summary_with_types(output);

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0], ("M".to_string(), "path/to/file.rs".to_string()));
    }

    #[test]
    fn test_parse_diff_summary_with_types_rename() {
        let output = "M src/auth.rs\nR src/old_name.rs => src/new_name.rs\nA src/added.rs\n";
        let changes = parse_diff_summary_with_types(output);

        assert_eq!(changes.len(), 4);
        assert_eq!(changes[0], ("M".to_string(), "src/auth.rs".to_string()));
        // Rename is split into D (old) + A (new)
        assert_eq!(changes[1], ("D".to_string(), "src/old_name.rs".to_string()));
        assert_eq!(changes[2], ("A".to_string(), "src/new_name.rs".to_string()));
        assert_eq!(changes[3], ("A".to_string(), "src/added.rs".to_string()));
    }

    #[test]
    fn test_parse_diff_summary_with_types_rename_only() {
        let output = "R old.rs => new.rs\n";
        let changes = parse_diff_summary_with_types(output);

        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0], ("D".to_string(), "old.rs".to_string()));
        assert_eq!(changes[1], ("A".to_string(), "new.rs".to_string()));
    }

    #[test]
    fn test_parse_diff_summary_files_rename() {
        let output = "M src/auth.rs\nR src/old_name.rs => src/new_name.rs\n";
        let files = parse_diff_summary_files(output);

        assert_eq!(files.len(), 3);
        assert_eq!(files[0], "src/auth.rs");
        assert_eq!(files[1], "src/old_name.rs");
        assert_eq!(files[2], "src/new_name.rs");
    }

    #[test]
    fn test_parse_diff_summary_files_rename_only() {
        let output = "R old.rs => new.rs\n";
        let files = parse_diff_summary_files(output);

        assert_eq!(files.len(), 2);
        assert_eq!(files[0], "old.rs");
        assert_eq!(files[1], "new.rs");
    }

    // --- normalize_link_target tests ---

    fn make_task_graph() -> TaskGraph {
        use crate::tasks::graph::EdgeStore;
        use crate::tasks::types::{TaskPriority, TaskStatus};

        let mut tasks = FastHashMap::default();
        let make = |id: &str, name: &str| Task {
            id: id.to_string(),
            name: name.to_string(),
            slug: None,
            task_type: None,
            status: TaskStatus::Open,
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data: HashMap::new(),
            created_at: chrono::Utc::now(),
            started_at: None,
            claimed_by_session: None,
            last_session_id: None,
            stopped_reason: None,
            closed_outcome: None,
            summary: None,
            turn_started: None,
            turn_closed: None,
            turn_stopped: None,
            comments: Vec::new(),
        };
        tasks.insert(
            "klmnopqrstuvwxyzklmnopqrstuvwxyz".to_string(),
            make("klmnopqrstuvwxyzklmnopqrstuvwxyz", "Task A"),
        );
        tasks.insert(
            "xyzxyzxyzxyzxyzxyzxyzxyzxyzxyzxy".to_string(),
            make("xyzxyzxyzxyzxyzxyzxyzxyzxyzxyzxy", "Task B"),
        );
        TaskGraph {
            tasks,
            edges: EdgeStore::new(),
            slug_index: FastHashMap::default(),
        }
    }

    #[test]
    fn test_normalize_link_target_full_task_id() {
        let graph = make_task_graph();
        let result =
            normalize_link_target("klmnopqrstuvwxyzklmnopqrstuvwxyz", "blocked-by", &graph);
        assert_eq!(result.unwrap(), "klmnopqrstuvwxyzklmnopqrstuvwxyz");
    }

    #[test]
    fn test_normalize_link_target_with_task_prefix() {
        let graph = make_task_graph();
        let result = normalize_link_target(
            "task:klmnopqrstuvwxyzklmnopqrstuvwxyz",
            "blocked-by",
            &graph,
        );
        assert_eq!(result.unwrap(), "klmnopqrstuvwxyzklmnopqrstuvwxyz");
    }

    #[test]
    fn test_normalize_link_target_short_prefix() {
        let graph = make_task_graph();
        let result = normalize_link_target("klmno", "blocked-by", &graph);
        assert_eq!(result.unwrap(), "klmnopqrstuvwxyzklmnopqrstuvwxyz");
    }

    #[test]
    fn test_normalize_link_target_external_ref_flexible_kind() {
        let graph = make_task_graph();
        let result = normalize_link_target("file:design.md", "sourced-from", &graph);
        assert_eq!(result.unwrap(), "file:design.md");
    }

    #[test]
    fn test_normalize_link_target_external_ref_task_only_kind_rejected() {
        let graph = make_task_graph();
        let result = normalize_link_target("file:design.md", "blocked-by", &graph);
        assert!(result.is_err());
        match result.unwrap_err() {
            AikiError::InvalidLinkTarget { kind, .. } => assert_eq!(kind, "blocked-by"),
            other => panic!("Expected InvalidLinkTarget, got {:?}", other),
        }
    }

    #[test]
    fn test_normalize_link_target_bare_path_flexible_kind() {
        let graph = make_task_graph();
        let result = normalize_link_target("design.md", "sourced-from", &graph);
        assert_eq!(result.unwrap(), "file:design.md");
    }

    #[test]
    fn test_normalize_link_target_nonexistent_task_only_kind() {
        let graph = make_task_graph();
        let result = normalize_link_target("nonexistent", "blocked-by", &graph);
        assert!(result.is_err());
        match result.unwrap_err() {
            AikiError::InvalidLinkTarget { kind, .. } => assert_eq!(kind, "blocked-by"),
            other => panic!("Expected InvalidLinkTarget, got {:?}", other),
        }
    }

    #[test]
    fn test_normalize_link_target_ambiguous_task_only_kind() {
        use crate::tasks::types::{TaskPriority, TaskStatus};

        let mut graph = make_task_graph();
        // Add a second task sharing the "klmn" prefix to create ambiguity
        let task_c = Task {
            id: "klmnzzzzzzzzzzzzzzzzzzzzzzzzzzzy".to_string(),
            name: "Task C".to_string(),
            slug: None,
            task_type: None,
            status: TaskStatus::Open,
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data: HashMap::new(),
            created_at: chrono::Utc::now(),
            started_at: None,
            claimed_by_session: None,
            last_session_id: None,
            stopped_reason: None,
            closed_outcome: None,
            summary: None,
            turn_started: None,
            turn_closed: None,
            turn_stopped: None,
            comments: Vec::new(),
        };
        graph.tasks.insert(task_c.id.clone(), task_c);

        let result = normalize_link_target("klmn", "blocked-by", &graph);
        assert!(result.is_err());
        match result.unwrap_err() {
            AikiError::AmbiguousTaskId { prefix, .. } => assert_eq!(prefix, "klmn"),
            other => panic!("Expected AmbiguousTaskId, got {:?}", other),
        }
    }

    #[test]
    fn test_normalize_link_target_ambiguous_flexible_kind() {
        use crate::tasks::types::{TaskPriority, TaskStatus};

        let mut graph = make_task_graph();
        let task_c = Task {
            id: "klmnzzzzzzzzzzzzzzzzzzzzzzzzzzzy".to_string(),
            name: "Task C".to_string(),
            slug: None,
            task_type: None,
            status: TaskStatus::Open,
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data: HashMap::new(),
            created_at: chrono::Utc::now(),
            started_at: None,
            claimed_by_session: None,
            last_session_id: None,
            stopped_reason: None,
            closed_outcome: None,
            summary: None,
            turn_started: None,
            turn_closed: None,
            turn_stopped: None,
            comments: Vec::new(),
        };
        graph.tasks.insert(task_c.id.clone(), task_c);

        // Flexible kinds should also error on ambiguous prefixes (not silently file:-prefix)
        let result = normalize_link_target("klmn", "sourced-from", &graph);
        assert!(result.is_err());
        match result.unwrap_err() {
            AikiError::AmbiguousTaskId { prefix, .. } => assert_eq!(prefix, "klmn"),
            other => panic!("Expected AmbiguousTaskId, got {:?}", other),
        }
    }

    // --- has_external_ref_prefix tests ---

    #[test]
    fn test_has_external_ref_prefix() {
        assert!(has_external_ref_prefix("file:foo.md"));
        assert!(has_external_ref_prefix("prompt:abc123"));
        assert!(has_external_ref_prefix("comment:xyz"));
        assert!(has_external_ref_prefix("issue:GH-42"));
        assert!(!has_external_ref_prefix("task:abc"));
        assert!(!has_external_ref_prefix("abc123"));
        assert!(!has_external_ref_prefix("design.md"));
    }

    // --- extract_link_flag tests ---

    #[test]
    fn test_extract_link_flag_single() {
        let result = extract_link_flag(
            Some("target".to_string()),
            None, None, None, None, None, None, None, None, None, None, None,
        );
        let (kind, target) = result.unwrap();
        assert_eq!(kind, "blocked-by");
        assert_eq!(target, "target");
    }

    #[test]
    fn test_extract_link_flag_sourced_from() {
        let result = extract_link_flag(
            None, None, None, None,
            Some("file:design.md".to_string()),
            None, None, None, None, None, None, None,
        );
        let (kind, target) = result.unwrap();
        assert_eq!(kind, "sourced-from");
        assert_eq!(target, "file:design.md");
    }

    #[test]
    fn test_extract_link_flag_implements_emits_implements_plan() {
        let result = extract_link_flag(
            None, None, None, None, None, None,
            Some("file:ops/now/feature.md".to_string()),
            None, None, None, None, None,
        );
        let (kind, target) = result.unwrap();
        assert_eq!(kind, "implements-plan");
        assert_eq!(target, "file:ops/now/feature.md");
    }

    #[test]
    fn test_extract_link_flag_fixes() {
        let result = extract_link_flag(
            None, None, None, None, None, None, None, None, None,
            Some("task:abc123".to_string()),
            None, None,
        );
        let (kind, target) = result.unwrap();
        assert_eq!(kind, "fixes");
        assert_eq!(target, "task:abc123");
    }

    #[test]
    fn test_extract_link_flag_none() {
        let result = extract_link_flag(None, None, None, None, None, None, None, None, None, None, None, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_link_flag_multiple() {
        let result = extract_link_flag(
            Some("a".to_string()),
            Some("b".to_string()),
            None, None, None, None, None, None, None, None, None, None,
        );
        assert!(result.is_err());
    }

    // --- alias resolution tests ---

    #[test]
    fn test_resolve_subtask_of_only_canonical() {
        let result = resolve_subtask_of_alias(Some("parent-id".into()), None).unwrap();
        assert_eq!(result, Some("parent-id".to_string()));
    }

    #[test]
    fn test_resolve_subtask_of_only_alias() {
        let result = resolve_subtask_of_alias(None, Some("parent-id".into())).unwrap();
        assert_eq!(result, Some("parent-id".to_string()));
    }

    #[test]
    fn test_resolve_subtask_of_neither() {
        let result = resolve_subtask_of_alias(None, None).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_subtask_of_both_errors() {
        let result = resolve_subtask_of_alias(Some("a".into()), Some("b".into()));
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_sourced_from_only_canonical() {
        let result = resolve_sourced_from_alias(vec!["file:foo.md".into()], Vec::new()).unwrap();
        assert_eq!(result, vec!["file:foo.md"]);
    }

    #[test]
    fn test_resolve_sourced_from_only_alias() {
        let result = resolve_sourced_from_alias(Vec::new(), vec!["file:bar.md".into()]).unwrap();
        assert_eq!(result, vec!["file:bar.md"]);
    }

    #[test]
    fn test_resolve_sourced_from_neither() {
        let result = resolve_sourced_from_alias(Vec::new(), Vec::new()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_resolve_sourced_from_both_errors() {
        let result = resolve_sourced_from_alias(vec!["file:a.md".into()], vec!["file:b.md".into()]);
        assert!(result.is_err());
    }

    #[test]
    fn test_resolve_sourced_from_option_only_canonical() {
        let result = resolve_sourced_from_option_alias(Some("file:foo.md".into()), None).unwrap();
        assert_eq!(result, Some("file:foo.md".to_string()));
    }

    #[test]
    fn test_resolve_sourced_from_option_only_alias() {
        let result = resolve_sourced_from_option_alias(None, Some("file:bar.md".into())).unwrap();
        assert_eq!(result, Some("file:bar.md".to_string()));
    }

    #[test]
    fn test_resolve_sourced_from_option_neither() {
        let result = resolve_sourced_from_option_alias(None, None).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_sourced_from_option_both_errors() {
        let result =
            resolve_sourced_from_option_alias(Some("file:a.md".into()), Some("file:b.md".into()));
        assert!(result.is_err());
    }
}
