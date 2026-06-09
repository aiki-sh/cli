//! Resolve command for resolving JJ merge conflicts
//!
//! This module provides the `aiki resolve` command which:
//! - Takes a change ID that has JJ merge conflicts
//! - Creates a task from the `resolve` template
//! - Supports run modes: blocking (default), --async, --start

use std::collections::HashMap;
use std::env;
use std::path::Path;

use crate::commands::OutputFormat;
use crate::error::{AikiError, Result};
use crate::jj;
use crate::output_utils;
use crate::session::find_active_session;
use crate::tasks::md::MdBuilder;
use crate::tasks::runner::{task_run, task_run_async, TaskRunOptions};
use crate::tasks::{
    get_current_scope_set, get_in_progress, get_ready_queue_for_scope_set, materialize_graph,
    read_events, reassign_task, start_task_core, Task,
};

use super::task::{create_from_template, TemplateTaskParams};

/// Arguments for the resolve command
#[derive(clap::Args)]
pub struct ResolveArgs {
    /// JJ change ID with merge conflicts to resolve
    pub change_id: String,

    /// Run resolve asynchronously (return immediately)
    #[arg(long = "async")]
    pub run_async: bool,

    /// Start resolve and return control to calling agent
    #[arg(long)]
    pub start: bool,

    /// Agent for resolve assignment
    #[arg(long)]
    pub agent: Option<String>,

    /// Shorthand for --agent claude-code
    #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
    pub claude: bool,
    /// Shorthand for --agent codex
    #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
    pub codex: bool,
    /// Shorthand for --agent cursor
    #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
    pub cursor: bool,
    /// Shorthand for --agent gemini
    #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
    pub gemini: bool,

    /// Output format (e.g., `id` for bare task ID on stdout)
    #[arg(long, short = 'o', value_name = "FORMAT")]
    pub output: Option<OutputFormat>,
}

/// Check if a JJ change has conflicts.
///
/// Uses `jj resolve --list` to check whether the given change has any
/// unresolved conflicts. Returns true if conflicts exist.
fn has_jj_conflicts(cwd: &Path, change_id: &str) -> bool {
    let output = jj::jj_cmd()
        .current_dir(cwd)
        .args(["resolve", "--list", "-r", change_id])
        .args(["--no-pager", "--ignore-working-copy"])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            // If there are conflicts, jj resolve --list outputs them (non-empty)
            let stdout = String::from_utf8_lossy(&out.stdout);
            !stdout.trim().is_empty()
        }
        _ => false,
    }
}

/// Run the resolve command
pub fn run(args: ResolveArgs) -> Result<()> {
    use crate::session::flags::resolve_agent_shorthand;
    let agent_type = resolve_agent_shorthand(args.agent, args.claude, args.codex, args.cursor, args.gemini);

    let cwd = env::current_dir()
        .map_err(|_| AikiError::InvalidArgument("Failed to get current directory".to_string()))?;

    // Validate that the change has conflicts
    if !has_jj_conflicts(&cwd, &args.change_id) {
        return Err(AikiError::InvalidArgument(format!(
            "Change {} has no JJ conflicts to resolve",
            args.change_id
        )));
    }

    // Resolve agent override to canonical string for assignee
    let agent_override = agent_type.map(|a| a.as_str().to_string());

    // Determine assignee
    let assignee = agent_override
        .or_else(|| find_active_session(&cwd).map(|s| s.agent_type.as_str().to_string()));

    // Create resolve task from resolve template
    let mut data = HashMap::new();
    data.insert("conflict_id".to_string(), args.change_id.clone());

    let params = TemplateTaskParams {
        template_name: "resolve".to_string(),
        data,
        sources: vec![format!("conflict:{}", args.change_id)],
        assignee,
        ..Default::default()
    };

    let resolve_id = create_from_template(&cwd, params)?;

    // Re-read tasks to include newly created resolve task
    let events = read_events(&cwd)?;
    let graph = materialize_graph(&events);
    let tasks = &graph.tasks;
    let scope_set = get_current_scope_set(&graph);
    let in_progress: Vec<&Task> = get_in_progress(tasks).into_iter().collect();
    let ready = get_ready_queue_for_scope_set(&graph, &scope_set);

    let output_id = matches!(args.output, Some(OutputFormat::Id));

    // Handle execution mode
    if args.start {
        // Reassign task to current agent (caller takes over)
        if let Some(session) = find_active_session(&cwd) {
            reassign_task(&cwd, &resolve_id, session.agent_type.as_str())?;
        }
        // Start task
        start_task_core(&cwd, &[resolve_id.clone()])?;
        if !output_id {
            output_resolve_started(&resolve_id, &args.change_id, &in_progress, &ready)?;
        }
    } else if args.run_async {
        let options = TaskRunOptions::new();
        task_run_async(&cwd, &resolve_id, options)?;
        if !output_id {
            output_resolve_async(&resolve_id, &args.change_id)?;
        }
    } else {
        // Run to completion (default)
        let options = TaskRunOptions::new();
        task_run(&cwd, &resolve_id, options)?;
        if !output_id {
            output_resolve_completed(&resolve_id, &args.change_id)?;
        }
    }

    if output_id {
        println!("{}", resolve_id);
    }

    Ok(())
}

/// Output resolve started message (for --start mode)
fn output_resolve_started(
    resolve_id: &str,
    change_id: &str,
    _in_progress: &[&Task],
    _ready: &[&Task],
) -> Result<()> {
    use super::output::{format_command_output, CommandOutput};
    output_utils::emit(|| {
        let output = CommandOutput {
            heading: "Resolve Started",
            task_id: resolve_id,
            scope: None,
            status: &format!("Resolve task started for conflict in {}.", change_id),
            issues: None,
            hint: None,
        };
        let content = format_command_output(&output);
        MdBuilder::new().build(&content)
    });
    Ok(())
}

/// Output resolve async message (for --async mode)
fn output_resolve_async(resolve_id: &str, change_id: &str) -> Result<()> {
    use super::output::{format_command_output, CommandOutput};
    output_utils::emit(|| {
        let output = CommandOutput {
            heading: "Resolve Started",
            task_id: resolve_id,
            scope: None,
            status: &format!(
                "Resolve started in background for conflict in {}.",
                change_id
            ),
            issues: None,
            hint: None,
        };
        let content = format_command_output(&output);
        MdBuilder::new().build(&content)
    });
    Ok(())
}

/// Output resolve completed message (for blocking mode)
fn output_resolve_completed(resolve_id: &str, change_id: &str) -> Result<()> {
    use super::output::{format_command_output, CommandOutput};
    output_utils::emit(|| {
        let output = CommandOutput {
            heading: "Resolve Completed",
            task_id: resolve_id,
            scope: None,
            status: &format!("Conflict in {} resolved.", change_id),
            issues: None,
            hint: None,
        };
        let content = format_command_output(&output);
        MdBuilder::new().build(&content)
    });
    Ok(())
}
