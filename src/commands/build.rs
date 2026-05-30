//! Build command for decomposing plan files and executing all subtasks
//!
//! This module provides the `aiki build` command which:
//! - Creates an epic from a plan file and automatically executes all subtasks
//! - Supports building from an existing epic ID
//! - Shows build/epic status via the `show` subcommand
//! - Supports async (background) execution

use std::collections::HashMap;
use std::env;
use std::io::IsTerminal;
use std::path::Path;

use clap::Subcommand;

use super::OutputFormat;
use super::epic::find_or_create_epic;
use crate::agents::AgentType;
use crate::config::get_aiki_binary_path;
use crate::error::{AikiError, Result};
use crate::plans::{parse_plan_metadata, PlanGraph};
use crate::tasks::id::{is_task_id, is_task_id_prefix};
use crate::tasks::runner::{task_run, task_run_async, TaskRunOptions};
use crate::tasks::md::MdBuilder;
use crate::tasks::{
    find_task, get_subtasks, materialize_graph, read_events, write_event, Task,
    TaskEvent, TaskOutcome, TaskStatus,
};

/// Build subcommands
#[derive(Subcommand)]
pub enum BuildSubcommands {
    /// Show build/epic status for a plan
    Show {
        /// Plan path to show build status for
        plan_path: String,

        /// Output format (e.g., `id` for bare task ID)
        #[arg(long, short = 'o', value_name = "FORMAT")]
        output: Option<OutputFormat>,
    },
}

/// Arguments for the build command
#[derive(clap::Args)]
pub struct BuildArgs {
    /// Plan path or epic ID (32 lowercase letters)
    pub target: Option<String>,

    /// Run build asynchronously
    #[arg(long = "async")]
    pub run_async: bool,

    /// Ignore existing epic, create new one from scratch
    #[arg(long)]
    pub restart: bool,

    /// Build template to use (default: aiki/build)
    #[arg(long)]
    pub template: Option<String>,

    /// Agent for build orchestration (default: claude-code)
    #[arg(long)]
    pub agent: Option<String>,

    /// Subcommand (show)
    #[command(subcommand)]
    pub subcommand: Option<BuildSubcommands>,
}

/// Run the build command
pub fn run(args: BuildArgs) -> Result<()> {
    let cwd = env::current_dir().map_err(|_| {
        AikiError::InvalidArgument("Failed to get current directory".to_string())
    })?;

    if let Some(subcommand) = args.subcommand {
        return match subcommand {
            BuildSubcommands::Show { plan_path, output } => run_show(&cwd, &plan_path, output),
        };
    }

    let target = args.target.ok_or_else(|| {
        AikiError::InvalidArgument(
            "No plan path or epic ID provided. Usage: aiki build <plan-path-or-epic-id>"
                .to_string(),
        )
    })?;

    if is_task_id(&target) || is_task_id_prefix(&target) {
        run_build_epic(&cwd, &target, args.run_async, args.template, args.agent)
    } else {
        run_build_plan(
            &cwd,
            &target,
            args.restart,
            args.run_async,
            args.template,
            args.agent,
        )
    }
}

/// Build from a plan path — deterministic find-or-create.
///
/// 1. Validate plan file exists and is .md
/// 2. Clean up stale builds for this plan
/// 3. Check for existing epic (deterministic: no interactive prompts)
/// 4. Create build task
/// 5. Run build task (sync or async)
/// 6. Output results
fn run_build_plan(
    cwd: &Path,
    plan_path: &str,
    restart: bool,
    run_async: bool,
    template_name: Option<String>,
    agent: Option<String>,
) -> Result<()> {
    // Validate plan file exists and is .md
    validate_plan_path(cwd, plan_path)?;

    // Check if plan is a draft
    let full_path = if plan_path.starts_with('/') {
        std::path::PathBuf::from(plan_path)
    } else {
        cwd.join(plan_path)
    };
    let metadata = parse_plan_metadata(&full_path);
    if metadata.draft {
        return Err(AikiError::InvalidArgument(
            "Cannot build draft plan. Remove `draft: true` from frontmatter first.".to_string(),
        ));
    }

    // Parse agent if provided
    let agent_type = if let Some(ref agent_str) = agent {
        Some(
            AgentType::from_str(agent_str)
                .ok_or_else(|| AikiError::UnknownAgentType(agent_str.clone()))?,
        )
    } else {
        None
    };

    // Clean up stale builds for this plan
    cleanup_stale_builds(cwd, plan_path)?;

    // Load current tasks to check for existing epics
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let plan_graph = PlanGraph::build(&graph);

    // Deterministic epic lookup (no interactive prompts)
    let existing_epic = plan_graph.find_epic_for_plan(plan_path, &graph);

    let epic_id = if restart {
        // --restart: close existing epic and create fresh
        if let Some(epic) = existing_epic {
            if epic.status != TaskStatus::Closed {
                undo_completed_subtasks(cwd, &epic.id)?;
                close_epic(cwd, &epic.id)?;
            }
        }
        None
    } else {
        match existing_epic {
            Some(epic) if epic.status != TaskStatus::Closed => {
                // Valid incomplete epic — use it (deterministic, no prompt)
                let subtasks = get_subtasks(&graph, &epic.id);
                if subtasks.is_empty() {
                    // Invalid epic (no subtasks) — close and create new
                    close_epic_as_invalid(cwd, &epic.id)?;
                    None
                } else {
                    Some(epic.id.clone())
                }
            }
            _ => None, // No epic or closed epic — create new
        }
    };

    // Ensure we always have an epic before creating the build task.
    // If no existing epic was found, create one via the decompose agent.
    let epic_id = match epic_id {
        Some(id) => id,
        None => find_or_create_epic(cwd, plan_path)?,
    };

    // Check if epic is blocked before creating build task
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    check_epic_blockers(&graph, &epic_id)?;

    // Create build task
    let template = template_name.as_deref().unwrap_or("aiki/build");
    let assignee = agent_type
        .as_ref()
        .map(|a| a.as_str().to_string())
        .or_else(|| Some("claude-code".to_string()));

    let build_task_id =
        create_build_task(cwd, plan_path, Some(&epic_id), template, assignee)?;

    let display_epic_id = epic_id.as_str();

    // Run build task
    if run_async {
        let options = if let Some(agent) = agent_type {
            TaskRunOptions::new().with_agent(agent)
        } else {
            TaskRunOptions::new()
        };
        let _handle = task_run_async(cwd, &build_task_id, options)?;
        output_build_async(&build_task_id, display_epic_id)?;

        // Output machine-readable to stdout if piped
        if !std::io::stdout().is_terminal() {
            println!(
                "<aiki_build build_id=\"{}\" epic_id=\"{}\"/>",
                build_task_id, display_epic_id
            );
        }
    } else {
        output_build_started(&build_task_id, display_epic_id)?;

        let options = if let Some(agent) = agent_type {
            TaskRunOptions::new().with_agent(agent)
        } else {
            TaskRunOptions::new()
        };
        task_run(cwd, &build_task_id, options)?;

        // After build completes, re-read tasks to get final state
        let events = read_events(cwd)?;
        let graph = materialize_graph(&events);
        let plan_graph = PlanGraph::build(&graph);

        // Find the epic task (may have been created during the build)
        let final_epic = plan_graph.find_epic_for_plan(plan_path, &graph);
        let final_epic_id = final_epic
            .map(|p| p.id.as_str())
            .unwrap_or(display_epic_id);

        let subtasks = final_epic
            .map(|p| get_subtasks(&graph, &p.id))
            .unwrap_or_default();
        let subtask_refs: Vec<&Task> = subtasks.into_iter().collect();
        output_build_completed(&build_task_id, final_epic_id, &subtask_refs)?;

        // Output machine-readable to stdout if piped
        if !std::io::stdout().is_terminal() {
            println!(
                "<aiki_build build_id=\"{}\" epic_id=\"{}\"/>",
                build_task_id, final_epic_id
            );
        }
    }

    Ok(())
}

/// Build from an existing epic ID
///
/// 1. Find epic task, verify it exists
/// 2. Get plan path from epic's data
/// 3. Create build task with data.epic and data.plan
/// 4. Run build task (sync or async)
/// 5. Output results
fn run_build_epic(
    cwd: &Path,
    epic_id: &str,
    run_async: bool,
    template_name: Option<String>,
    agent: Option<String>,
) -> Result<()> {
    // Parse agent if provided
    let agent_type = if let Some(ref agent_str) = agent {
        Some(
            AgentType::from_str(agent_str)
                .ok_or_else(|| AikiError::UnknownAgentType(agent_str.clone()))?,
        )
    } else {
        None
    };

    // Find epic task (resolve prefix to canonical ID)
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let epic = find_task(&graph.tasks, epic_id)?;
    let epic_id = epic.id.as_str();

    // Check if epic is blocked before creating build task
    check_epic_blockers(&graph, epic_id)?;

    let plan_path = epic
        .data
        .get("plan")
        .cloned()
        .ok_or_else(|| {
            AikiError::InvalidArgument(format!(
                "Epic task {} missing data.plan. Cannot create build task without a plan path.",
                epic_id
            ))
        })?;

    // Create build task
    let template = template_name.as_deref().unwrap_or("aiki/build");
    let assignee = agent_type
        .as_ref()
        .map(|a| a.as_str().to_string())
        .or_else(|| Some("claude-code".to_string()));

    let build_task_id = create_build_task(
        cwd,
        &plan_path,
        Some(epic_id),
        template,
        assignee,
    )?;

    // Run build task
    if run_async {
        let options = if let Some(agent) = agent_type {
            TaskRunOptions::new().with_agent(agent)
        } else {
            TaskRunOptions::new()
        };
        let _handle = task_run_async(cwd, &build_task_id, options)?;
        output_build_async(&build_task_id, epic_id)?;

        // Output machine-readable to stdout if piped
        if !std::io::stdout().is_terminal() {
            println!(
                "<aiki_build build_id=\"{}\" epic_id=\"{}\"/>",
                build_task_id, epic_id
            );
        }
    } else {
        output_build_started(&build_task_id, epic_id)?;

        let options = if let Some(agent) = agent_type {
            TaskRunOptions::new().with_agent(agent)
        } else {
            TaskRunOptions::new()
        };
        task_run(cwd, &build_task_id, options)?;

        // After build completes, re-read tasks to get final state
        let events = read_events(cwd)?;
        let graph = materialize_graph(&events);

        let subtasks = get_subtasks(&graph, epic_id);
        output_build_completed(&build_task_id, epic_id, &subtasks)?;

        // Output machine-readable to stdout if piped
        if !std::io::stdout().is_terminal() {
            println!(
                "<aiki_build build_id=\"{}\" epic_id=\"{}\"/>",
                build_task_id, epic_id
            );
        }
    }

    Ok(())
}

/// Show build/epic status for a plan
fn run_show(cwd: &Path, plan_path: &str, output_format: Option<OutputFormat>) -> Result<()> {
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let plan_graph = PlanGraph::build(&graph);

    // Find epic via PlanGraph
    let epic = plan_graph.find_epic_for_plan(plan_path, &graph).ok_or_else(|| {
        AikiError::InvalidArgument(format!("No epic found for plan: {}", plan_path))
    })?;

    // Find build tasks associated with this plan
    let build_tasks: Vec<&Task> = graph
        .tasks
        .values()
        .filter(|t| {
            t.task_type.as_deref() == Some("orchestrator")
                && t.data.get("plan").map(|s| s.as_str()) == Some(plan_path)
        })
        .collect();

    match output_format {
        Some(OutputFormat::Id) => {
            if build_tasks.is_empty() {
                // No builds yet -- emit the epic ID as fallback
                println!("{}", epic.id);
            } else {
                for build in &build_tasks {
                    println!("{}", build.id);
                }
            }
        }
        None => {
            let subtasks = get_subtasks(&graph, &epic.id);
            output_build_show(epic, &subtasks, &build_tasks)?;
        }
    }

    Ok(())
}

/// Validate that the plan path is a .md file and exists
fn validate_plan_path(cwd: &Path, plan_path: &str) -> Result<()> {
    if !plan_path.ends_with(".md") {
        return Err(AikiError::InvalidArgument(
            "Plan file must be markdown (.md)".to_string(),
        ));
    }

    let full_path = if plan_path.starts_with('/') {
        std::path::PathBuf::from(plan_path)
    } else {
        cwd.join(plan_path)
    };

    if !full_path.exists() {
        return Err(AikiError::InvalidArgument(format!(
            "Plan file not found: {}",
            plan_path
        )));
    }

    if !full_path.is_file() {
        return Err(AikiError::InvalidArgument(format!(
            "Not a file: {}",
            plan_path
        )));
    }

    Ok(())
}

/// Clean up stale build tasks for this plan.
///
/// Finds any in_progress or open build tasks with `data.plan` matching the plan path
/// and closes them as wont_do with a comment.
fn cleanup_stale_builds(cwd: &Path, plan_path: &str) -> Result<()> {
    let events = read_events(cwd)?;
    let tasks = materialize_graph(&events).tasks;

    let stale_builds: Vec<String> = tasks
        .values()
        .filter(|t| {
            t.task_type.as_deref() == Some("orchestrator")
                && t.data.get("plan").map(|s| s.as_str()) == Some(plan_path)
                && (t.status == TaskStatus::InProgress || t.status == TaskStatus::Open)
        })
        .map(|t| t.id.clone())
        .collect();

    for build_id in &stale_builds {
        let comment_event = TaskEvent::CommentAdded {
            task_ids: vec![build_id.clone()],
            text: "Stale build cleaned up".to_string(),
            data: std::collections::HashMap::new(),
            timestamp: chrono::Utc::now(),
        };
        write_event(cwd, &comment_event)?;

        let close_event = TaskEvent::Closed {
            task_ids: vec![build_id.clone()],
            outcome: TaskOutcome::WontDo,
            summary: Some("Stale build cleaned up".to_string()),
            turn_id: None,
            timestamp: chrono::Utc::now(),
        };
        write_event(cwd, &close_event)?;
    }

    Ok(())
}

/// Undo file changes made by completed subtasks of an epic.
///
/// Invokes `aiki task undo <epic-id> --completed` to revert changes before
/// closing the epic. If no completed subtasks exist, this is a no-op.
fn undo_completed_subtasks(cwd: &Path, epic_id: &str) -> Result<()> {
    let output = std::process::Command::new(get_aiki_binary_path())
        .current_dir(cwd)
        .args(["task", "undo", epic_id, "--completed"])
        .output()
        .map_err(|e| {
            AikiError::JjCommandFailed(format!("Failed to run task undo: {}", e))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // If there are no completed subtasks, that's fine - nothing to undo
        if stderr.contains("no completed subtasks") || stderr.contains("NoCompletedSubtasks") {
            return Ok(());
        }
        return Err(AikiError::JjCommandFailed(format!(
            "task undo failed: {}",
            stderr.trim()
        )));
    }

    // Forward undo output to stderr so user sees what was reverted
    let stderr_output = String::from_utf8_lossy(&output.stderr);
    if !stderr_output.is_empty() {
        eprint!("{}", stderr_output);
    }

    Ok(())
}

/// Close an existing epic as wont_do
fn close_epic(cwd: &Path, epic_id: &str) -> Result<()> {
    let timestamp = chrono::Utc::now();

    // Add comment before closing
    let comment_event = TaskEvent::CommentAdded {
        task_ids: vec![epic_id.to_string()],
        text: "Closed by --restart".to_string(),
        data: std::collections::HashMap::new(),
        timestamp: timestamp - chrono::Duration::milliseconds(1),
    };
    write_event(cwd, &comment_event)?;

    let close_event = TaskEvent::Closed {
        task_ids: vec![epic_id.to_string()],
        outcome: TaskOutcome::WontDo,
        summary: Some("Closed by --restart".to_string()),
        turn_id: None,
        timestamp,
    };
    write_event(cwd, &close_event)?;
    Ok(())
}

/// Create a build task from template.
///
/// The build task orchestrates epic execution (if needed) and execution of subtasks.
/// It stores `data.plan` and optionally `data.epic` to link back to the plan and epic.
fn create_build_task(
    cwd: &Path,
    plan_path: &str,
    epic_id: Option<&str>,
    template_name: &str,
    assignee: Option<String>,
) -> Result<String> {
    use super::task::{create_from_template, TemplateTaskParams};

    let mut data = HashMap::new();
    data.insert("plan".to_string(), plan_path.to_string());
    if let Some(epic) = epic_id {
        data.insert("epic".to_string(), epic.to_string());
    }

    let params = TemplateTaskParams {
        template_name: template_name.to_string(),
        data,
        sources: vec![format!("file:{}", plan_path)],
        assignee: assignee.or_else(|| Some("claude-code".to_string())),
        ..Default::default()
    };

    let task_id = create_from_template(cwd, params)?;

    // Emit link events for the relationships (dual-write with data attributes)
    let events = crate::tasks::storage::read_events(cwd)?;
    let graph = crate::tasks::graph::materialize_graph(&events);

    // orchestrator orchestrates the epic (if one exists)
    if let Some(epic) = epic_id {
        crate::tasks::storage::write_link_event(cwd, &graph, "orchestrates", &task_id, epic)?;
    }

    Ok(task_id)
}

/// Check if an epic is blocked by unresolved dependencies.
///
/// Returns an error if the epic has `depends-on` links to tasks that are not yet
/// Closed with Done outcome. This prevents starting a build on a blocked epic.
fn check_epic_blockers(
    graph: &crate::tasks::graph::TaskGraph,
    epic_id: &str,
) -> Result<()> {
    let blocker_ids: Vec<&str> = graph
        .edges
        .targets(epic_id, "depends-on")
        .iter()
        .filter(|tid| {
            graph.tasks.get(tid.as_str()).map_or(true, |t| {
                !(t.status == TaskStatus::Closed
                    && t.closed_outcome == Some(TaskOutcome::Done))
            })
        })
        .map(|s| s.as_str())
        .collect();

    if !blocker_ids.is_empty() {
        let blocker_names: Vec<String> = blocker_ids
            .iter()
            .map(|id| {
                let name = graph
                    .tasks
                    .get(*id)
                    .map(|t| t.name.as_str())
                    .unwrap_or("unknown");
                let short = &id[..id.len().min(8)];
                format!("{} ({})", short, name)
            })
            .collect();
        return Err(AikiError::InvalidArgument(format!(
            "Epic {} is blocked by unresolved dependencies: {}",
            &epic_id[..epic_id.len().min(8)],
            blocker_names.join(", ")
        )));
    }

    Ok(())
}

/// Close an epic as invalid (no subtasks created).
fn close_epic_as_invalid(cwd: &Path, epic_id: &str) -> Result<()> {
    let close_event = TaskEvent::Closed {
        task_ids: vec![epic_id.to_string()],
        outcome: TaskOutcome::WontDo,
        summary: Some("No subtasks created — epic invalid".to_string()),
        turn_id: None,
        timestamp: chrono::Utc::now(),
    };
    write_event(cwd, &close_event)?;
    Ok(())
}

/// Output build started message to stderr
fn output_build_started(build_id: &str, epic_id: &str) -> Result<()> {
    let content = format!(
        "## Build Started\n- **Build ID:** {}\n- **Epic ID:** {}\n",
        build_id, epic_id
    );
    let md = MdBuilder::new("build").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

/// Output build completed message to stderr
fn output_build_completed(build_id: &str, epic_id: &str, subtasks: &[&Task]) -> Result<()> {
    let mut content = format!(
        "## Build Completed\n- **Build ID:** {}\n- **Epic ID:** {}\n- **Subtasks:** {}\n\n",
        build_id, epic_id, subtasks.len()
    );

    for (i, subtask) in subtasks.iter().enumerate() {
        let status = if subtask.status == TaskStatus::Closed {
            "done"
        } else {
            "pending"
        };
        content.push_str(&format!("{}. {} ({})\n", i + 1, &subtask.name, status));
    }

    let md = MdBuilder::new("build").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

/// Output build async started message to stderr
fn output_build_async(build_id: &str, epic_id: &str) -> Result<()> {
    let content = format!(
        "## Build Started\n- **Build ID:** {}\n- **Epic ID:** {}\n- Build started in background.\n",
        build_id, epic_id
    );
    let md = MdBuilder::new("build").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

/// Output build show (detailed status display)
fn output_build_show(epic: &Task, subtasks: &[&Task], build_tasks: &[&Task]) -> Result<()> {
    let completed = subtasks
        .iter()
        .filter(|t| t.status == TaskStatus::Closed)
        .count();
    let total = subtasks.len();

    let status_str = match epic.status {
        TaskStatus::Open => "open",
        TaskStatus::InProgress => "in_progress",
        TaskStatus::Stopped => "stopped",
        TaskStatus::Closed => "closed",
    };

    let outcome_str = epic
        .closed_outcome
        .as_ref()
        .map(|o| format!("- **Outcome:** {}\n", o))
        .unwrap_or_default();

    let plan_str = epic
        .data
        .get("plan")
        .map(|s| format!("- **Plan:** {}\n", s))
        .unwrap_or_default();

    let mut content = format!(
        "## Epic: {}\n- **ID:** {}\n- **Status:** {}\n{}{}",
        &epic.name, &epic.id, status_str, outcome_str, plan_str
    );

    // Add progress summary
    content.push_str(&format!("- **Progress:** {}/{}\n", completed, total));

    // Add subtask list
    if !subtasks.is_empty() {
        content.push_str("\n### Subtasks\n| # | ID | Status | Outcome | Name |\n|---|-----|--------|---------|------|\n");
        for (i, subtask) in subtasks.iter().enumerate() {
            let sub_status = match subtask.status {
                TaskStatus::Open => "open",
                TaskStatus::InProgress => "in_progress",
                TaskStatus::Stopped => "stopped",
                TaskStatus::Closed => "closed",
            };

            let sub_outcome = subtask
                .closed_outcome
                .as_ref()
                .map(|o| o.to_string())
                .unwrap_or_default();

            content.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                i + 1, &subtask.id, sub_status, sub_outcome, &subtask.name
            ));
        }
    }

    // Add build history
    if !build_tasks.is_empty() {
        content.push_str("\n### Builds\n| ID | Status | Outcome | Name |\n|-----|--------|---------|------|\n");
        for build in build_tasks {
            let build_status = match build.status {
                TaskStatus::Open => "open",
                TaskStatus::InProgress => "in_progress",
                TaskStatus::Stopped => "stopped",
                TaskStatus::Closed => "closed",
            };

            let build_outcome = build
                .closed_outcome
                .as_ref()
                .map(|o| o.to_string())
                .unwrap_or_default();

            content.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                &build.id, build_status, build_outcome, &build.name
            ));
        }
    }

    // Add sources
    if !epic.sources.is_empty() {
        content.push_str("\n### Sources\n");
        for source in &epic.sources {
            content.push_str(&format!("- {}\n", source));
        }
    }

    let md = MdBuilder::new("build-show").build(&content, &[], &[]);
    eprintln!("{}", md);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::graph::{EdgeStore, TaskGraph};
    use crate::tasks::TaskPriority;
    use crate::tasks::types::FastHashMap;
    use std::collections::HashMap;

    fn make_task(id: &str, name: &str, status: TaskStatus) -> Task {
        Task {
            id: id.to_string(),
            name: name.to_string(),
            slug: None,
            task_type: None,
            status,
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
        }
    }

    fn make_task_with_data(
        id: &str,
        name: &str,
        status: TaskStatus,
        data: HashMap<String, String>,
    ) -> Task {
        let mut task = make_task(id, name, status);
        task.data = data;
        task
    }

    fn make_graph(tasks: FastHashMap<String, Task>, edges: EdgeStore) -> TaskGraph {
        TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        }
    }

    /// Helper: find epic for plan via PlanGraph
    fn find_epic_for_plan_via_graph<'a>(
        graph: &'a TaskGraph,
        plan_path: &str,
    ) -> Option<&'a Task> {
        let sg = PlanGraph::build(graph);
        sg.find_epic_for_plan(plan_path, graph)
    }

    // --- find_epic_for_plan tests ---

    #[test]
    fn test_find_epic_for_plan_none() {
        let graph = make_graph(FastHashMap::default(), EdgeStore::new());
        assert!(find_epic_for_plan_via_graph(&graph, "ops/now/feature.md").is_none());
    }

    #[test]
    fn test_find_epic_for_plan_via_implements_link() {
        let mut tasks = FastHashMap::default();
        let task = make_task("epic1", "Epic: Feature", TaskStatus::Open);
        tasks.insert("epic1".to_string(), task);

        let mut edges = EdgeStore::new();
        edges.add("epic1", "file:ops/now/feature.md", "implements-plan");

        let graph = make_graph(tasks, edges);
        let result = find_epic_for_plan_via_graph(&graph, "ops/now/feature.md");
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "epic1");
    }

    #[test]
    fn test_find_epic_for_plan_wrong_plan() {
        let mut tasks = FastHashMap::default();
        let task = make_task("epic1", "Epic: Other", TaskStatus::Open);
        tasks.insert("epic1".to_string(), task);

        let mut edges = EdgeStore::new();
        edges.add("epic1", "file:ops/now/other.md", "implements-plan");

        let graph = make_graph(tasks, edges);
        assert!(find_epic_for_plan_via_graph(&graph, "ops/now/feature.md").is_none());
    }

    #[test]
    fn test_find_epic_for_plan_most_recent() {
        let mut tasks = FastHashMap::default();

        let mut task1 = make_task("epic_old", "Epic: Old", TaskStatus::Closed);
        task1.created_at = chrono::Utc::now() - chrono::Duration::hours(1);
        tasks.insert("epic_old".to_string(), task1);

        let task2 = make_task("epic_new", "Epic: New", TaskStatus::Open);
        tasks.insert("epic_new".to_string(), task2);

        let mut edges = EdgeStore::new();
        edges.add("epic_old", "file:ops/now/feature.md", "implements-plan");
        edges.add("epic_new", "file:ops/now/feature.md", "implements-plan");

        let graph = make_graph(tasks, edges);
        let result = find_epic_for_plan_via_graph(&graph, "ops/now/feature.md");
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "epic_new");
    }

    // --- cleanup_stale_builds helper logic tests ---

    #[test]
    fn test_stale_build_detection_in_progress() {
        let mut tasks = FastHashMap::default();
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/feature.md".to_string());

        let mut task = make_task_with_data(
            "build1",
            "Build: feature",
            TaskStatus::InProgress,
            data,
        );
        task.task_type = Some("orchestrator".to_string());
        tasks.insert("build1".to_string(), task);

        // Verify the stale build detection logic
        let stale_builds: Vec<String> = tasks
            .values()
            .filter(|t| {
                t.task_type.as_deref() == Some("orchestrator")
                    && t.data.get("plan").map(|s| s.as_str()) == Some("ops/now/feature.md")
                    && (t.status == TaskStatus::InProgress || t.status == TaskStatus::Open)
            })
            .map(|t| t.id.clone())
            .collect();

        assert_eq!(stale_builds.len(), 1);
        assert_eq!(stale_builds[0], "build1");
    }

    #[test]
    fn test_stale_build_detection_open() {
        let mut tasks = FastHashMap::default();
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/feature.md".to_string());

        let mut task = make_task_with_data(
            "build2",
            "Build: feature",
            TaskStatus::Open,
            data,
        );
        task.task_type = Some("orchestrator".to_string());
        tasks.insert("build2".to_string(), task);

        let stale_builds: Vec<String> = tasks
            .values()
            .filter(|t| {
                t.task_type.as_deref() == Some("orchestrator")
                    && t.data.get("plan").map(|s| s.as_str()) == Some("ops/now/feature.md")
                    && (t.status == TaskStatus::InProgress || t.status == TaskStatus::Open)
            })
            .map(|t| t.id.clone())
            .collect();

        assert_eq!(stale_builds.len(), 1);
        assert_eq!(stale_builds[0], "build2");
    }

    #[test]
    fn test_stale_build_not_detected_when_closed() {
        let mut tasks = FastHashMap::default();
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/feature.md".to_string());

        let mut task = make_task_with_data(
            "build3",
            "Build: feature",
            TaskStatus::Closed,
            data,
        );
        task.task_type = Some("orchestrator".to_string());
        tasks.insert("build3".to_string(), task);

        let stale_builds: Vec<String> = tasks
            .values()
            .filter(|t| {
                t.task_type.as_deref() == Some("orchestrator")
                    && t.data.get("plan").map(|s| s.as_str()) == Some("ops/now/feature.md")
                    && (t.status == TaskStatus::InProgress || t.status == TaskStatus::Open)
            })
            .map(|t| t.id.clone())
            .collect();

        assert!(stale_builds.is_empty());
    }

    #[test]
    fn test_stale_build_not_detected_wrong_plan() {
        let mut tasks = FastHashMap::default();
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/other.md".to_string());

        let mut task = make_task_with_data(
            "build4",
            "Build: other",
            TaskStatus::InProgress,
            data,
        );
        task.task_type = Some("orchestrator".to_string());
        tasks.insert("build4".to_string(), task);

        let stale_builds: Vec<String> = tasks
            .values()
            .filter(|t| {
                t.task_type.as_deref() == Some("orchestrator")
                    && t.data.get("plan").map(|s| s.as_str()) == Some("ops/now/feature.md")
                    && (t.status == TaskStatus::InProgress || t.status == TaskStatus::Open)
            })
            .map(|t| t.id.clone())
            .collect();

        assert!(stale_builds.is_empty());
    }

    #[test]
    fn test_stale_build_not_detected_wrong_type() {
        let mut tasks = FastHashMap::default();
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/feature.md".to_string());

        // Not a build task (no task_type or different type)
        let task = make_task_with_data(
            "not_build",
            "Something else",
            TaskStatus::InProgress,
            data,
        );
        tasks.insert("not_build".to_string(), task);

        let stale_builds: Vec<String> = tasks
            .values()
            .filter(|t| {
                t.task_type.as_deref() == Some("orchestrator")
                    && t.data.get("plan").map(|s| s.as_str()) == Some("ops/now/feature.md")
                    && (t.status == TaskStatus::InProgress || t.status == TaskStatus::Open)
            })
            .map(|t| t.id.clone())
            .collect();

        assert!(stale_builds.is_empty());
    }

    // --- Argument detection tests ---

    #[test]
    fn test_argument_detection_plan_path() {
        assert!(!is_task_id("ops/now/feature.md"));
        assert!(!is_task_id("simple.md"));
        assert!(!is_task_id("/absolute/path/to/plan.md"));
        assert!(!is_task_id("not-a-task-id"));
        assert!(!is_task_id(""));
    }

    #[test]
    fn test_argument_detection_task_id() {
        // 32 lowercase k-z letters
        assert!(is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls"));
        assert!(is_task_id("xtuttnyvykpulsxzqnznsxylrzkkqssy"));
    }

    #[test]
    fn test_argument_detection_not_task_id() {
        // Too short
        assert!(!is_task_id("klmnop"));
        // Too long
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztklsx"));
        // Contains letters outside k-z range
        assert!(!is_task_id("abcdefghijklmnopqrstuvwxyzabcdef"));
        // Contains numbers
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvz1234"));
        // Contains uppercase
        assert!(!is_task_id("Mvslrspmoynoxyyywqyutmovxpvztkls"));
    }

    // --- validate_plan_path tests ---

    #[test]
    fn test_validate_plan_path_not_md() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let result = validate_plan_path(temp_dir.path(), "not-markdown.txt");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must be markdown"));
    }

    #[test]
    fn test_validate_plan_path_not_found() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let result = validate_plan_path(temp_dir.path(), "nonexistent.md");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Plan file not found"));
    }

    #[test]
    fn test_validate_plan_path_exists() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let plan_file = temp_dir.path().join("my-plan.md");
        std::fs::write(&plan_file, "# My Plan").unwrap();
        let result = validate_plan_path(temp_dir.path(), "my-plan.md");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_plan_path_absolute() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let plan_file = temp_dir.path().join("absolute-plan.md");
        std::fs::write(&plan_file, "# Plan").unwrap();
        let result = validate_plan_path(temp_dir.path(), &plan_file.to_string_lossy());
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_plan_path_directory() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let dir_path = temp_dir.path().join("subdir.md");
        std::fs::create_dir_all(&dir_path).unwrap();
        let result = validate_plan_path(temp_dir.path(), "subdir.md");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Not a file"));
    }

    // --- Build show output formatting tests ---

    #[test]
    fn test_output_build_started_format() {
        // Just verify it does not panic
        let result = output_build_started("build123", "epic456");
        assert!(result.is_ok());
    }

    #[test]
    fn test_output_build_completed_no_subtasks() {
        let subtasks: Vec<&Task> = vec![];
        let result = output_build_completed("build123", "epic456", &subtasks);
        assert!(result.is_ok());
    }

    #[test]
    fn test_output_build_completed_with_subtasks() {
        let task1 = make_task("sub1", "Implement auth", TaskStatus::Closed);
        let task2 = make_task("sub2", "Add tests", TaskStatus::Open);
        let subtasks: Vec<&Task> = vec![&task1, &task2];
        let result = output_build_completed("build123", "epic456", &subtasks);
        assert!(result.is_ok());
    }

    #[test]
    fn test_output_build_async_format() {
        let result = output_build_async("build123", "epic456");
        assert!(result.is_ok());
    }

    #[test]
    fn test_output_build_show_basic() {
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/feature.md".to_string());
        let epic = make_task_with_data("epic1", "Epic: Feature", TaskStatus::Open, data);
        let subtasks: Vec<&Task> = vec![];
        let build_tasks: Vec<&Task> = vec![];
        let result = output_build_show(&epic, &subtasks, &build_tasks);
        assert!(result.is_ok());
    }

    #[test]
    fn test_output_build_show_with_subtasks_and_builds() {
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/feature.md".to_string());
        let mut epic = make_task_with_data("epic1", "Epic: Feature", TaskStatus::InProgress, data);
        epic.sources = vec!["file:ops/now/feature.md".to_string()];

        let sub1 = make_task("sub1", "Step 1", TaskStatus::Closed);
        let sub2 = make_task("sub2", "Step 2", TaskStatus::InProgress);
        let subtasks: Vec<&Task> = vec![&sub1, &sub2];

        let mut build_data = HashMap::new();
        build_data.insert("plan".to_string(), "ops/now/feature.md".to_string());
        let mut build = make_task_with_data(
            "build1",
            "Build: feature",
            TaskStatus::Closed,
            build_data,
        );
        build.task_type = Some("orchestrator".to_string());
        build.closed_outcome = Some(TaskOutcome::Done);
        let build_tasks: Vec<&Task> = vec![&build];

        let result = output_build_show(&epic, &subtasks, &build_tasks);
        assert!(result.is_ok());
    }

    #[test]
    fn test_output_build_show_closed_epic_with_outcome() {
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/feature.md".to_string());
        let mut epic = make_task_with_data("epic1", "Epic: Feature", TaskStatus::Closed, data);
        epic.closed_outcome = Some(TaskOutcome::Done);

        let subtasks: Vec<&Task> = vec![];
        let build_tasks: Vec<&Task> = vec![];
        let result = output_build_show(&epic, &subtasks, &build_tasks);
        assert!(result.is_ok());
    }

    #[test]
    fn test_xml_escaping_in_output() {
        // Verify XML special characters are properly escaped
        let task = make_task("sub1", "Fix <angle> & \"quote\" 'apos'", TaskStatus::Open);
        let subtasks: Vec<&Task> = vec![&task];
        let result = output_build_completed("build<1>", "epic&2", &subtasks);
        assert!(result.is_ok());
    }

    // --- OutputFormat tests ---

    #[test]
    fn test_output_format_id_variant() {
        let fmt = OutputFormat::Id;
        assert!(matches!(fmt, OutputFormat::Id));
    }

    #[test]
    fn test_output_format_clap_parse() {
        use clap::ValueEnum;
        let parsed = OutputFormat::from_str("id", false);
        assert!(parsed.is_ok());
        assert!(matches!(parsed.unwrap(), OutputFormat::Id));
    }

    #[test]
    fn test_output_format_clap_rejects_unknown() {
        use clap::ValueEnum;
        let parsed = OutputFormat::from_str("unknown_format", false);
        assert!(parsed.is_err());
    }
}
