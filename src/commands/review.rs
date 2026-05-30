//! Review command for creating and running code review tasks
//!
//! This module provides the `aiki review` command which:
//! - Creates a review task with subtasks (digest, review)
//! - Runs the review task (default: completion, --async: async, --start: hand off)
//! - Supports different review scopes (task ID or session)
//! - Lists review tasks (list subcommand)
//! - Shows review task details (show subcommand)

use clap::Subcommand;
use std::collections::HashMap;
use std::env;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::agents::{determine_reviewer, AgentType};
use crate::error::{AikiError, Result};
use crate::session::find_active_session;
use crate::tasks::runner::{task_run, task_run_async, TaskRunOptions};
use crate::tasks::templates::create_review_task_from_template;
use crate::tasks::md::MdBuilder;
use crate::tasks::{
    find_task, get_current_scope_set, get_in_progress, get_ready_queue_for_scope_set,
    materialize_graph, read_events, reassign_task, start_task_core, write_link_event,
    write_link_event_with_autorun, Task, TaskComment, TaskStatus,
};

/// What kind of review scope this is
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewScopeKind {
    Task,
    Plan,
    Code,
    Session,
}

impl ReviewScopeKind {
    /// Convert to string representation for serialization
    pub fn as_str(&self) -> &str {
        match self {
            ReviewScopeKind::Task => "task",
            ReviewScopeKind::Plan => "plan",
            ReviewScopeKind::Code => "code",
            ReviewScopeKind::Session => "session",
        }
    }

    /// Parse from string representation
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "task" => Ok(ReviewScopeKind::Task),
            "plan" => Ok(ReviewScopeKind::Plan),
            "code" => Ok(ReviewScopeKind::Code),
            "session" => Ok(ReviewScopeKind::Session),
            _ => Err(AikiError::UnknownReviewScope(s.to_string())),
        }
    }
}

/// What is being reviewed and how
#[derive(Debug, Clone)]
pub struct ReviewScope {
    pub kind: ReviewScopeKind,
    /// Task ID or file path depending on kind
    pub id: String,
    /// Task IDs for session reviews (empty otherwise)
    pub task_ids: Vec<String>,
}

impl ReviewScope {
    /// Get display name (computed from kind and id)
    pub fn name(&self) -> String {
        match self.kind {
            ReviewScopeKind::Task => format!("Task ({})", &self.id),
            ReviewScopeKind::Plan => {
                let filename = Path::new(&self.id)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&self.id);
                format!("Plan ({})", filename)
            }
            ReviewScopeKind::Code => {
                let filename = Path::new(&self.id)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&self.id);
                format!("Code ({})", filename)
            }
            ReviewScopeKind::Session => "Session".to_string(),
        }
    }

    /// Serialize to task data HashMap for persistence
    pub fn to_data(&self) -> HashMap<String, String> {
        let mut data = HashMap::new();
        data.insert("scope.kind".into(), self.kind.as_str().into());
        data.insert("scope.id".into(), self.id.clone());
        data.insert("scope.name".into(), self.name());
        if !self.task_ids.is_empty() {
            data.insert("scope.task_ids".into(), self.task_ids.join(","));
        }
        data
    }

    /// Deserialize from task data HashMap
    pub fn from_data(data: &HashMap<String, String>) -> Result<Self> {
        let kind_str = data.get("scope.kind").ok_or_else(|| {
            AikiError::InvalidArgument("Missing scope.kind in review task data".into())
        })?;
        let kind = ReviewScopeKind::from_str(kind_str)?;

        // scope.id is required for non-Session scopes (Task, Plan, Code)
        let id = match kind {
            ReviewScopeKind::Session => {
                data.get("scope.id").cloned().unwrap_or_default()
            }
            _ => {
                data.get("scope.id")
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .ok_or_else(|| {
                        AikiError::InvalidArgument(format!(
                            "Missing scope.id in review task data (required for {:?} scope kind)",
                            kind_str
                        ))
                    })?
            }
        };

        Ok(Self {
            kind,
            id,
            task_ids: data
                .get("scope.task_ids")
                .map(|s| s.split(',').map(String::from).collect())
                .unwrap_or_default(),
        })
    }
}

/// Review subcommands (for list, show, and issue management)
#[derive(Subcommand)]
pub enum ReviewSubcommands {
    /// List review tasks
    List {
        /// Show all reviews (not just open)
        #[arg(long)]
        all: bool,
    },

    /// Show review task details
    Show {
        /// Review task ID
        task_id: String,
    },

    /// Manage review issues
    Issue {
        #[command(subcommand)]
        command: ReviewIssueSubcommands,
    },
}

/// Subcommands for managing review issues
#[derive(Subcommand)]
pub enum ReviewIssueSubcommands {
    /// Add an issue to a review
    Add {
        /// The review task ID
        review_id: String,
        /// Description of the issue
        text: String,
    },
    /// List issues on a review
    List {
        /// The review task ID
        review_id: String,
    },
}

/// Arguments for the review command (top-level create args)
#[derive(clap::Args)]
pub struct ReviewArgs {
    /// Target to review: task ID, file path (.md), or nothing for session review
    pub target: Option<String>,

    /// Review the codebase implementation described in a plan (only with file targets)
    #[arg(long)]
    pub code: bool,

    /// Review and auto-fix issues in one command
    #[arg(long)]
    pub fix: bool,

    /// Run review asynchronously (return immediately)
    #[arg(long = "async")]
    pub run_async: bool,

    /// Start review and return control to calling agent
    #[arg(long)]
    pub start: bool,

    /// Task template to use (default: aiki/review)
    #[arg(long)]
    pub template: Option<String>,

    /// Agent for review assignment (default: opposite of task worker)
    #[arg(long)]
    pub agent: Option<String>,

    /// Enable autorun (auto-start this review when its target closes)
    #[arg(long)]
    pub autorun: bool,

    /// Subcommand (list or show)
    #[command(subcommand)]
    pub subcommand: Option<ReviewSubcommands>,
}

/// Run the review command
pub fn run(args: ReviewArgs) -> Result<()> {
    let cwd = env::current_dir()
        .map_err(|_| AikiError::InvalidArgument("Failed to get current directory".to_string()))?;

    // If a subcommand is provided, dispatch to it
    if let Some(subcommand) = args.subcommand {
        return match subcommand {
            ReviewSubcommands::List { all } => list_reviews(&cwd, all),
            ReviewSubcommands::Show { task_id } => show_review(&cwd, &task_id),
            ReviewSubcommands::Issue { command } => match command {
                ReviewIssueSubcommands::Add { review_id, text } => {
                    run_issue_add(&cwd, &review_id, &text)
                }
                ReviewIssueSubcommands::List { review_id } => run_issue_list(&cwd, &review_id),
            },
        };
    }

    // Otherwise, run the create/review flow with top-level args
    run_review(
        &cwd,
        args.target,
        args.code,
        args.fix,
        args.run_async,
        args.start,
        args.template,
        args.agent,
        args.autorun,
    )
}

/// Parameters for creating a review task
#[derive(Debug, Clone)]
pub struct CreateReviewParams {
    /// Pre-resolved review scope (caller detects target type)
    pub scope: ReviewScope,
    /// Override the reviewer agent
    pub agent_override: Option<String>,
    /// Template to use (default: aiki/review)
    pub template: Option<String>,
    /// Whether to auto-fix issues (sets data.options.fix)
    pub fix: bool,
    /// Enable autorun on the validates link (default: false, opt-in only)
    pub autorun: bool,
}

/// Result of creating a review task
#[derive(Debug, Clone)]
pub struct CreateReviewResult {
    /// The created review task ID
    pub review_task_id: String,
    /// The review scope (typed, replaces loose scope_name/scope_id)
    pub scope: ReviewScope,
    /// The assigned reviewer
    pub assignee: Option<String>,
}

/// Check if a string looks like it could be a task ID, prefix, or subtask ID.
///
/// Task IDs are 32 lowercase letters (a-z only). Prefixes are shorter
/// but follow the same pattern. Subtask IDs append `.N` suffixes
/// (e.g., `abcdef.1`, `abcdef.1.2`). This is a heuristic used by
/// detect_target to distinguish task IDs from file paths.
fn looks_like_task_id(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // Split off optional subtask suffix (e.g., "abc.1.2" → root "abc")
    let root = s.split('.').next().unwrap_or(s);
    // Root must be non-empty lowercase letters
    if root.is_empty() || !root.chars().all(|c| c.is_ascii_lowercase()) {
        return false;
    }
    // Every part after the root must be a non-empty digit sequence
    let mut parts = s.split('.');
    parts.next(); // skip root
    parts.all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Detect the review target from the CLI argument and flags.
///
/// Returns a `ReviewScope` and optionally a worker agent string (for task targets).
/// The `cwd` is needed to resolve file paths and load tasks.
pub fn detect_target(
    cwd: &Path,
    arg: Option<&str>,
    code: bool,
) -> Result<(ReviewScope, Option<String>)> {
    match arg {
        None => {
            if code {
                return Err(AikiError::InvalidArgument(
                    "--code flag only applies to file targets".to_string(),
                ));
            }

            // Session scope — collect closed tasks from current session
            let events = read_events(cwd)?;
            let tasks = materialize_graph(&events).tasks;
            let session = find_active_session(cwd);

            let (session_id, session_agent) = match &session {
                Some(s) => (
                    Some(s.session_id.clone()),
                    Some(s.agent_type.as_str().to_string()),
                ),
                None => (None, None),
            };

            let closed_tasks: Vec<Task> = tasks
                .values()
                .filter(|t| {
                    t.status == TaskStatus::Closed
                        && match (&t.last_session_id, &session_id) {
                            (Some(task_session), Some(current_session)) => {
                                task_session == current_session
                            }
                            (_, None) => true,
                            (None, Some(_)) => false,
                        }
                })
                .cloned()
                .collect();

            if closed_tasks.is_empty() {
                output_nothing_to_review()?;
                return Err(AikiError::NothingToReview);
            }

            let task_ids: Vec<String> = closed_tasks.iter().map(|t| t.id.clone()).collect();
            let fallback_id = {
                let mut ids = task_ids.clone();
                ids.sort();
                let hash_input = ids.join(",");
                uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, hash_input.as_bytes()).to_string()
            };
            let scope = ReviewScope {
                kind: ReviewScopeKind::Session,
                id: session_id.unwrap_or(fallback_id),
                task_ids,
            };
            Ok((scope, session_agent))
        }

        Some(s) if s.ends_with(".md") && PathBuf::from(s).exists() => {
            let kind = if code {
                ReviewScopeKind::Code
            } else {
                ReviewScopeKind::Plan
            };
            Ok((
                ReviewScope {
                    kind,
                    id: s.to_string(),
                    task_ids: vec![],
                },
                None,
            ))
        }

        Some(s) if s.ends_with(".md") => {
            Err(AikiError::InvalidArgument(format!("File not found: {}", s)))
        }

        Some(s) if looks_like_task_id(s) => {
            if code {
                return Err(AikiError::InvalidArgument(
                    "--code flag only applies to file targets".to_string(),
                ));
            }

            let events = read_events(cwd)?;
            let tasks = materialize_graph(&events).tasks;
            let task = find_task(&tasks, s)?;
            let worker = task.assignee.as_deref().map(|s| s.to_string());
            let scope = ReviewScope {
                kind: ReviewScopeKind::Task,
                id: task.id.clone(),
                task_ids: vec![],
            };
            Ok((scope, worker))
        }

        Some(s) if Path::new(s).exists() => {
            Err(AikiError::InvalidArgument(
                "File review only supports .md files currently".to_string(),
            ))
        }

        Some(s) => {
            Err(AikiError::InvalidArgument(format!("Target not found: {}", s)))
        }
    }
}

/// Core review creation logic. Used by both CLI and flow action.
///
/// This function creates the review task with subtasks but does NOT
/// start or run the task. The caller is responsible for the execution mode.
/// The scope must be pre-resolved by the caller (via `detect_target()` for CLI,
/// or directly constructed for flow actions).
pub fn create_review(cwd: &Path, params: CreateReviewParams) -> Result<CreateReviewResult> {
    let scope = params.scope;

    // Determine worker for reviewer assignment (for task scope)
    let worker = match scope.kind {
        ReviewScopeKind::Task => {
            let events = read_events(cwd)?;
            let tasks = materialize_graph(&events).tasks;
            let task = find_task(&tasks, &scope.id)?;
            task.assignee.as_deref().map(|s| s.to_string())
        }
        ReviewScopeKind::Session => {
            find_active_session(cwd)
                .map(|s| s.agent_type.as_str().to_string())
        }
        _ => None,
    };

    // Determine assignee for review task
    let assignee = params
        .agent_override
        .or_else(|| Some(determine_reviewer(worker.as_deref())));

    // Create review task with subtasks from template
    let template = params.template.as_deref().unwrap_or("aiki/review");
    let mut scope_data = scope.to_data();

    // Add options data
    if params.fix {
        scope_data.insert("options.fix".to_string(), "true".to_string());
    }

    // Build sources for lineage (not routing)
    let sources = match scope.kind {
        ReviewScopeKind::Task => vec![format!("task:{}", scope.id)],
        ReviewScopeKind::Plan | ReviewScopeKind::Code => {
            vec![format!("file:{}", scope.id)]
        }
        _ => vec![],
    };

    let review_id = create_review_task_from_template(
        cwd,
        &scope_data,
        &sources,
        &assignee,
        template,
    )?;

    // Emit validates link for task-scoped reviews: review validates the original task
    // Autorun is opt-in only (--autorun flag); default is no autorun
    if scope.kind == ReviewScopeKind::Task {
        let events = read_events(cwd)?;
        let graph = materialize_graph(&events);
        let autorun = if params.autorun { Some(true) } else { None };
        write_link_event_with_autorun(cwd, &graph, "validates", &review_id, &scope.id, autorun)?;
    }

    Ok(CreateReviewResult {
        review_task_id: review_id,
        scope,
        assignee,
    })
}

/// Core review implementation
fn run_review(
    cwd: &Path,
    target: Option<String>,
    code: bool,
    fix: bool,
    run_async: bool,
    start: bool,
    template_name: Option<String>,
    agent: Option<String>,
    autorun: bool,
) -> Result<()> {
    // Parse agent if provided
    let agent_override = if let Some(ref agent_str) = agent {
        let agent_type = AgentType::from_str(agent_str)
            .ok_or_else(|| AikiError::UnknownAgentType(agent_str.clone()))?;
        Some(agent_type.as_str().to_string())
    } else {
        None
    };

    // Detect target and resolve scope at CLI layer
    let (scope, _worker) = match detect_target(cwd, target.as_deref(), code) {
        Ok(r) => r,
        Err(AikiError::NothingToReview) => {
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    // --fix is not supported for session reviews
    if fix && scope.kind == ReviewScopeKind::Session {
        return Err(AikiError::InvalidArgument(
            "--fix is not supported for session reviews".to_string(),
        ));
    }

    // Create review task using shared logic
    let result = match create_review(
        cwd,
        CreateReviewParams {
            scope,
            agent_override,
            template: template_name,
            fix,
            autorun,
        },
    ) {
        Ok(r) => r,
        Err(AikiError::NothingToReview) => {
            return Ok(());
        }
        Err(e) => return Err(e),
    };

    let review_id = result.review_task_id;

    // Re-read tasks to include newly created review task
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let tasks = &graph.tasks;
    let scope_set = get_current_scope_set(&graph);
    let in_progress: Vec<&Task> = get_in_progress(tasks).into_iter().collect();
    let ready = get_ready_queue_for_scope_set(&graph, &scope_set);

    let scope = &result.scope;

    // Handle execution mode
    if start {
        // Reassign task to current agent (caller takes over)
        if let Some(session) = find_active_session(cwd) {
            reassign_task(cwd, &review_id, session.agent_type.as_str())?;
        }
        // Start task using core logic (validates, auto-stops, emits events)
        start_task_core(cwd, &[review_id.clone()])?;
        output_review_started(&review_id, scope, &in_progress, &ready)?;
    } else if run_async {
        // Run async and return immediately
        let options = TaskRunOptions::new();
        task_run_async(cwd, &review_id, options)?;
        output_review_async(&review_id, scope)?;
        // Output task ID to stdout if piped
        if !std::io::stdout().is_terminal() {
            println!("{}", review_id);
        }
    } else {
        // Run to completion (default)
        let options = TaskRunOptions::new();
        task_run(cwd, &review_id, options)?;
        output_review_completed(&review_id, scope)?;
        // Output task ID to stdout if piped
        if !std::io::stdout().is_terminal() {
            println!("{}", review_id);
        }
    }

    Ok(())
}

/// Output message when there's nothing to review
fn output_nothing_to_review() -> Result<()> {
    use super::output::{CommandOutput, format_command_output};
    let output = CommandOutput {
        heading: "Approved",
        task_id: "",
        scope: None,
        status: "Nothing to review - no closed tasks in session.",
        issues: None,
        hint: None,
    };
    let content = format_command_output(&output);
    let md = MdBuilder::new("review").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

/// Output review started message (for --start mode)
fn output_review_started(review_id: &str, scope: &ReviewScope, in_progress: &[&Task], ready: &[&Task]) -> Result<()> {
    use super::output::{CommandOutput, format_command_output};
    let output = CommandOutput {
        heading: "Review Started",
        task_id: review_id,
        scope: Some(scope),
        status: "Review task started. You are now reviewing.",
        issues: None,
        hint: None,
    };
    let content = format_command_output(&output);
    let md = MdBuilder::new("review").build(&content, in_progress, ready);
    eprintln!("{}", md);

    // Output task ID to stdout if piped
    if !std::io::stdout().is_terminal() {
        println!("{}", review_id);
    }

    Ok(())
}

/// Output review async message (for --async mode)
fn output_review_async(review_id: &str, scope: &ReviewScope) -> Result<()> {
    use super::output::{CommandOutput, format_command_output};
    let output = CommandOutput {
        heading: "Review Started",
        task_id: review_id,
        scope: Some(scope),
        status: "Review started in background.",
        issues: None,
        hint: None,
    };
    let content = format_command_output(&output);
    let md = MdBuilder::new("review").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

/// Output review completed message (for blocking mode)
fn output_review_completed(review_id: &str, scope: &ReviewScope) -> Result<()> {
    use super::output::{CommandOutput, format_command_output};
    // Only show fix hint for scopes that support fixing
    let hint = if scope.kind == ReviewScopeKind::Session {
        None
    } else {
        Some(format!("Run `aiki fix {}` to remediate.", review_id))
    };
    let output = CommandOutput {
        heading: "Review Completed",
        task_id: review_id,
        scope: Some(scope),
        status: "Review completed.",
        issues: None,
        hint,
    };
    let content = format_command_output(&output);
    let md = MdBuilder::new("review").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

/// Get all issue comments from a task (comments where data.issue == "true").
///
/// This is the canonical function for filtering issue comments — used by both
/// `aiki review issue list` and `aiki fix`.
pub fn get_issue_comments(task: &Task) -> Vec<&TaskComment> {
    task.comments
        .iter()
        .filter(|c| c.data.get("issue").map(|v| v == "true").unwrap_or(false))
        .collect()
}

/// Add an issue to a review task
fn run_issue_add(cwd: &Path, review_id: &str, text: &str) -> Result<()> {
    let events = read_events(cwd)?;
    let tasks = materialize_graph(&events).tasks;
    let task = find_task(&tasks, review_id)?;

    // Validate it's a review task
    if !super::fix::is_review_task(task) {
        return Err(AikiError::InvalidArgument(format!(
            "Task {} is not a review task.",
            review_id
        )));
    }

    // Validate it's not closed
    if task.status == TaskStatus::Closed {
        return Err(AikiError::InvalidArgument(format!(
            "Review task {} is already closed.",
            review_id
        )));
    }

    // Use shared comment codepath with issue data
    let mut data = HashMap::new();
    data.insert("issue".to_string(), "true".to_string());

    super::task::comment_on_task(cwd, &task.id, text, data)?;

    eprintln!("Added issue to review {}", review_id);
    Ok(())
}

/// List issues on a review task
fn run_issue_list(cwd: &Path, review_id: &str) -> Result<()> {
    let events = read_events(cwd)?;
    let tasks = materialize_graph(&events).tasks;
    let task = find_task(&tasks, review_id)?;

    // Validate it's a review task
    if !super::fix::is_review_task(task) {
        return Err(AikiError::InvalidArgument(format!(
            "Task {} is not a review task.",
            review_id
        )));
    }

    let issues = get_issue_comments(task);

    if issues.is_empty() {
        eprintln!("No issues found on review {}", review_id);
        return Ok(());
    }

    for comment in &issues {
        let id = comment.id.as_deref().unwrap_or("unknown");
        eprintln!("{}\t{}", id, &comment.text);
    }

    Ok(())
}

/// List review tasks
fn list_reviews(cwd: &Path, all: bool) -> Result<()> {
    let events = read_events(cwd)?;
    let tasks = materialize_graph(&events).tasks;

    // Filter to tasks with task_type == "review"
    let mut reviews: Vec<&Task> = tasks
        .values()
        .filter(|t| t.task_type.as_deref() == Some("review"))
        .filter(|t| {
            // If not --all, only show open reviews (not closed)
            all || t.status != TaskStatus::Closed
        })
        .collect();

    // Sort by created_at (most recent first)
    reviews.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    if reviews.is_empty() {
        let content = if all {
            "No review tasks found.\n"
        } else {
            "No open review tasks. Use --all to see closed reviews.\n"
        };
        let md = MdBuilder::new("review-list").build(content, &[], &[]);
        eprintln!("{}", md);
        return Ok(());
    }

    // Format review list
    let mut content = String::from("## Reviews\n| ID | Status | Outcome | Issues | Name |\n|----|--------|---------|--------|------|\n");
    for review in &reviews {
        let status_str = match review.status {
            TaskStatus::Open => "open",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Stopped => "stopped",
            TaskStatus::Closed => "closed",
        };

        let outcome_str = review
            .closed_outcome
            .as_ref()
            .map(|o| format!("{:?}", o).to_lowercase())
            .unwrap_or_default();

        let issue_count = if let Some(count) = review.data.get("issues_found") {
            count.parse::<usize>().unwrap_or(review.comments.len())
        } else {
            // Backward compat: fall back to comment count
            review.comments.len()
        };

        content.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            &review.id,
            status_str,
            outcome_str,
            issue_count,
            &review.name
        ));
    }

    let md = MdBuilder::new("review-list").build(&content, &[], &[]);
    eprintln!("{}", md);

    Ok(())
}

/// Show review task details
fn show_review(cwd: &Path, task_id: &str) -> Result<()> {
    let events = read_events(cwd)?;
    let tasks = materialize_graph(&events).tasks;

    let task = find_task(&tasks, task_id)?;

    // Verify it's a review task
    if task.task_type.as_deref() != Some("review") {
        return Err(AikiError::InvalidArgument(format!(
            "Task {} is not a review task (type: {:?})",
            task_id, task.task_type
        )));
    }

    let status_str = match task.status {
        TaskStatus::Open => "open",
        TaskStatus::InProgress => "in_progress",
        TaskStatus::Stopped => "stopped",
        TaskStatus::Closed => "closed",
    };

    let outcome_str = task
        .closed_outcome
        .as_ref()
        .map(|o| format!("{:?}", o).to_lowercase())
        .unwrap_or_default();

    let assignee_str = task
        .assignee
        .as_ref()
        .map(|a| format!("- **Assignee:** {}\n", a))
        .unwrap_or_default();

    // Build content
    let mut content = format!(
        "## Review: {}\n- **ID:** {}\n- **Status:** {}\n",
        &task.name, &task.id, status_str
    );
    if !outcome_str.is_empty() {
        content.push_str(&format!("- **Outcome:** {}\n", outcome_str));
    }
    content.push_str(&assignee_str);

    // Add sources if any
    if !task.sources.is_empty() {
        content.push_str("\n### Sources\n");
        for source in &task.sources {
            content.push_str(&format!("- {}\n", source));
        }
    }

    // Add issues and comments
    if task.data.contains_key("issues_found") {
        // Structured: show issues separately from regular comments
        let issues = get_issue_comments(task);
        if !issues.is_empty() {
            content.push_str("\n### Issues\n");
            for (idx, comment) in issues.iter().enumerate() {
                content.push_str(&format!("{}. {}\n", idx + 1, &comment.text));
            }
        }
        let regular: Vec<&TaskComment> = task
            .comments
            .iter()
            .filter(|c| c.data.get("issue").map(|v| v != "true").unwrap_or(true))
            .collect();
        if !regular.is_empty() {
            content.push_str("\n### Comments\n");
            for comment in &regular {
                content.push_str(&format!("- {}\n", &comment.text));
            }
        }
    } else if !task.comments.is_empty() {
        // Backward compat: show all comments as issues
        content.push_str("\n### Issues\n");
        for (idx, comment) in task.comments.iter().enumerate() {
            content.push_str(&format!("{}. {}\n", idx + 1, &comment.text));
        }
    }

    // Find followup tasks (tasks sourced from this review's comments)
    let followups: Vec<&Task> = tasks
        .values()
        .filter(|t| {
            t.sources.iter().any(|s| {
                s.starts_with(&format!("comment:{}", task.id))
                    || s.starts_with(&format!("task:{}", task.id))
            })
        })
        .collect();

    if !followups.is_empty() {
        content.push_str("\n### Followups\n");
        for followup in &followups {
            let fu_status = match followup.status {
                TaskStatus::Open => "open",
                TaskStatus::InProgress => "in_progress",
                TaskStatus::Stopped => "stopped",
                TaskStatus::Closed => "closed",
            };
            content.push_str(&format!(
                "- **{}** [{}] {}\n",
                &followup.id, fu_status, &followup.name
            ));
        }
    }

    let md = MdBuilder::new("review-show").build(&content, &[], &[]);
    eprintln!("{}", md);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_determine_reviewer_opposite_claude() {
        // Worker is claude-code, reviewer should be codex
        let result = determine_reviewer(Some("claude-code"));
        assert_eq!(result, "codex".to_string());
    }

    #[test]
    fn test_determine_reviewer_opposite_codex() {
        // Worker is codex, reviewer should be claude-code
        let result = determine_reviewer(Some("codex"));
        assert_eq!(result, "claude-code".to_string());
    }

    #[test]
    fn test_determine_reviewer_default() {
        // No worker, default to codex
        let result = determine_reviewer(None);
        assert_eq!(result, "codex".to_string());
    }

    #[test]
    fn test_determine_reviewer_unknown_agent() {
        // Unknown worker, default to codex
        let result = determine_reviewer(Some("unknown-agent"));
        assert_eq!(result, "codex".to_string());
    }

    // ReviewScopeKind tests

    #[test]
    fn test_scope_kind_as_str() {
        assert_eq!(ReviewScopeKind::Task.as_str(), "task");
        assert_eq!(ReviewScopeKind::Plan.as_str(), "plan");
        assert_eq!(ReviewScopeKind::Code.as_str(), "code");
        assert_eq!(ReviewScopeKind::Session.as_str(), "session");
    }

    #[test]
    fn test_scope_kind_from_str() {
        assert_eq!(ReviewScopeKind::from_str("task").unwrap(), ReviewScopeKind::Task);
        assert_eq!(ReviewScopeKind::from_str("plan").unwrap(), ReviewScopeKind::Plan);
        assert_eq!(ReviewScopeKind::from_str("code").unwrap(), ReviewScopeKind::Code);
        assert_eq!(ReviewScopeKind::from_str("session").unwrap(), ReviewScopeKind::Session);
    }

    #[test]
    fn test_scope_kind_from_str_unknown() {
        let result = ReviewScopeKind::from_str("unknown");
        assert!(result.is_err());
    }

    // ReviewScope tests

    #[test]
    fn test_scope_name_task() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Task,
            id: "abc123".to_string(),
            task_ids: vec![],
        };
        assert_eq!(scope.name(), "Task (abc123)");
    }

    #[test]
    fn test_scope_name_spec() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Plan,
            id: "ops/now/feature.md".to_string(),
            task_ids: vec![],
        };
        assert_eq!(scope.name(), "Plan (feature.md)");
    }

    #[test]
    fn test_scope_name_code() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Code,
            id: "ops/now/feature.md".to_string(),
            task_ids: vec![],
        };
        assert_eq!(scope.name(), "Code (feature.md)");
    }

    #[test]
    fn test_scope_name_session() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Session,
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            task_ids: vec![],
        };
        assert_eq!(scope.name(), "Session");
    }

    #[test]
    fn test_scope_to_data_task() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Task,
            id: "abc123".to_string(),
            task_ids: vec![],
        };
        let data = scope.to_data();
        assert_eq!(data.get("scope.kind").unwrap(), "task");
        assert_eq!(data.get("scope.id").unwrap(), "abc123");
        assert_eq!(data.get("scope.name").unwrap(), "Task (abc123)");
        assert!(data.get("scope.task_ids").is_none());
    }

    #[test]
    fn test_scope_to_data_session_with_task_ids() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Session,
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            task_ids: vec!["t1".to_string(), "t2".to_string()],
        };
        let data = scope.to_data();
        assert_eq!(data.get("scope.kind").unwrap(), "session");
        assert_eq!(data.get("scope.task_ids").unwrap(), "t1,t2");
    }

    #[test]
    fn test_scope_roundtrip_task() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Task,
            id: "abc123".to_string(),
            task_ids: vec![],
        };
        let data = scope.to_data();
        let restored = ReviewScope::from_data(&data).unwrap();
        assert_eq!(restored.kind, ReviewScopeKind::Task);
        assert_eq!(restored.id, "abc123");
        assert!(restored.task_ids.is_empty());
    }

    #[test]
    fn test_scope_roundtrip_session() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Session,
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            task_ids: vec!["t1".to_string(), "t2".to_string()],
        };
        let data = scope.to_data();
        let restored = ReviewScope::from_data(&data).unwrap();
        assert_eq!(restored.kind, ReviewScopeKind::Session);
        assert_eq!(restored.id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(restored.task_ids, vec!["t1", "t2"]);
    }

    #[test]
    fn test_scope_roundtrip_spec() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Plan,
            id: "ops/now/feature.md".to_string(),
            task_ids: vec![],
        };
        let data = scope.to_data();
        let restored = ReviewScope::from_data(&data).unwrap();
        assert_eq!(restored.kind, ReviewScopeKind::Plan);
        assert_eq!(restored.id, "ops/now/feature.md");
    }

    #[test]
    fn test_scope_roundtrip_code() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Code,
            id: "ops/now/feature.md".to_string(),
            task_ids: vec![],
        };
        let data = scope.to_data();
        let restored = ReviewScope::from_data(&data).unwrap();
        assert_eq!(restored.kind, ReviewScopeKind::Code);
        assert_eq!(restored.id, "ops/now/feature.md");
    }

    #[test]
    fn test_scope_from_data_missing_type() {
        let data = HashMap::new();
        let result = ReviewScope::from_data(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_scope_from_data_unknown_type() {
        let mut data = HashMap::new();
        data.insert("scope.kind".to_string(), "bogus".to_string());
        let result = ReviewScope::from_data(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_scope_from_data_missing_id_for_task_scope() {
        let mut data = HashMap::new();
        data.insert("scope.kind".to_string(), "task".to_string());
        // No scope.id — should fail for Task scope
        let result = ReviewScope::from_data(&data);
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("Missing scope.id"),
            "Error should mention missing scope.id"
        );
    }

    #[test]
    fn test_scope_from_data_empty_id_for_task_scope() {
        let mut data = HashMap::new();
        data.insert("scope.kind".to_string(), "task".to_string());
        data.insert("scope.id".to_string(), "".to_string());
        // Empty scope.id — should also fail for Task scope
        let result = ReviewScope::from_data(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_scope_from_data_missing_id_ok_for_session() {
        let mut data = HashMap::new();
        data.insert("scope.kind".to_string(), "session".to_string());
        // No scope.id — should be fine for Session scope
        let result = ReviewScope::from_data(&data);
        assert!(result.is_ok());
    }

    // detect_target tests

    #[test]
    fn test_detect_target_md_file_spec() {
        let dir = tempfile::tempdir().unwrap();
        let md_path = dir.path().join("feature.md");
        std::fs::write(&md_path, "# Feature\n").unwrap();
        let path_str = md_path.to_str().unwrap();

        let (scope, worker) = detect_target(dir.path(), Some(path_str), false).unwrap();
        assert_eq!(scope.kind, ReviewScopeKind::Plan);
        assert_eq!(scope.id, path_str);
        assert!(scope.task_ids.is_empty());
        assert!(worker.is_none());
    }

    #[test]
    fn test_detect_target_md_file_code() {
        let dir = tempfile::tempdir().unwrap();
        let md_path = dir.path().join("feature.md");
        std::fs::write(&md_path, "# Feature\n").unwrap();
        let path_str = md_path.to_str().unwrap();

        let (scope, worker) = detect_target(dir.path(), Some(path_str), true).unwrap();
        assert_eq!(scope.kind, ReviewScopeKind::Code);
        assert_eq!(scope.id, path_str);
        assert!(worker.is_none());
    }

    #[test]
    fn test_detect_target_md_file_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let result = detect_target(dir.path(), Some("nonexistent.md"), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("File not found"));
    }

    #[test]
    fn test_detect_target_code_flag_no_target() {
        let dir = tempfile::tempdir().unwrap();
        let result = detect_target(dir.path(), None, true);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("--code flag only applies to file targets"));
    }

    #[test]
    fn test_detect_target_code_flag_task_id() {
        let dir = tempfile::tempdir().unwrap();
        let result = detect_target(dir.path(), Some("abcdefgh"), true);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("--code flag only applies to file targets"));
    }

    #[test]
    fn test_detect_target_non_md_file() {
        let dir = tempfile::tempdir().unwrap();
        let txt_path = dir.path().join("file.txt");
        std::fs::write(&txt_path, "content").unwrap();
        let path_str = txt_path.to_str().unwrap();

        let result = detect_target(dir.path(), Some(path_str), false);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("File review only supports .md files"));
    }

    #[test]
    fn test_detect_target_unknown_target() {
        let dir = tempfile::tempdir().unwrap();
        // Not a file, not a task ID (has digits and hyphen)
        let result = detect_target(dir.path(), Some("not-a-target-123"), false);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Target not found"));
    }

    // looks_like_task_id tests

    #[test]
    fn test_looks_like_task_id_valid() {
        assert!(looks_like_task_id("abcdefghijklmnopqrstuvwxyzabcdef"));
        assert!(looks_like_task_id("abc")); // prefix
        assert!(looks_like_task_id("x"));
        assert!(looks_like_task_id("abcdefghijklmnopqrstuvwxyzabcdef.1")); // subtask
        assert!(looks_like_task_id("abcdefghijklmnopqrstuvwxyzabcdef.1.2")); // nested subtask
        assert!(looks_like_task_id("abc.3")); // prefix with subtask
    }

    #[test]
    fn test_looks_like_task_id_invalid() {
        assert!(!looks_like_task_id("")); // empty
        assert!(!looks_like_task_id("ABC")); // uppercase
        assert!(!looks_like_task_id("abc123")); // digits in root
        assert!(!looks_like_task_id("ops/now/feature.md")); // path
        assert!(!looks_like_task_id("hello-world")); // hyphen
        assert!(!looks_like_task_id("has spaces")); // spaces
        assert!(!looks_like_task_id(".1")); // no root
        assert!(!looks_like_task_id("abc.")); // trailing dot, empty suffix
    }
}
