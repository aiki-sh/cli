//! Epic command for creating and managing epics from plan files
//!
//! This module provides the `aiki epic` command which:
//! - Creates an epic from a plan file via `epic add`
//! - Shows epic status and subtasks via `epic show`
//! - Lists all epics via `epic list`

use std::env;
use std::io::IsTerminal;
use std::path::Path;

use clap::Subcommand;

use super::OutputFormat;
use crate::agents::AgentType;
use crate::config::get_aiki_binary_path;
use crate::error::{AikiError, Result};
use crate::tasks::id::is_task_id;
use crate::tasks::runner::{task_run, TaskRunOptions};
use crate::tasks::templates::get_working_copy_change_id;
use crate::plans::{parse_plan_metadata, PlanGraph};
use crate::tasks::md::MdBuilder;
use crate::tasks::{
    find_task, generate_task_id, get_subtasks, is_task_id_prefix, materialize_graph, read_events,
    write_event, write_link_event, Task, TaskEvent, TaskOutcome, TaskPriority, TaskStatus,
};

/// Epic subcommands
#[derive(Subcommand)]
pub enum EpicCommands {
    /// Create an epic from a plan file
    Add {
        /// Path to plan file (e.g., ops/now/my-feature.md)
        plan_path: String,

        /// Ignore existing epic and create a new one from scratch
        #[arg(long)]
        restart: bool,

        /// Decompose template to use (default: aiki/decompose)
        #[arg(long)]
        template: Option<String>,

        /// Agent for decomposition (default: claude-code)
        #[arg(long)]
        agent: Option<String>,
    },
    /// Show epic status and subtasks
    Show {
        /// Plan path or epic task ID (32 lowercase letters)
        arg: String,

        /// Output format (e.g., `id` for bare task ID)
        #[arg(long, short = 'o', value_name = "FORMAT")]
        output: Option<OutputFormat>,
    },
    /// List all epics
    List,
}

/// Run the epic command
pub fn run(command: EpicCommands) -> Result<()> {
    let cwd = env::current_dir().map_err(|_| {
        AikiError::InvalidArgument("Failed to get current directory".to_string())
    })?;

    match command {
        EpicCommands::Add {
            plan_path,
            restart,
            template,
            agent,
        } => run_add(&cwd, &plan_path, restart, template, agent),
        EpicCommands::Show { arg, output } => run_show(&cwd, &arg, output),
        EpicCommands::List => run_list(&cwd),
    }
}

/// Core add (decompose) implementation — deterministic find-or-create.
///
/// Behavior (no interactive prompts):
/// - `--restart` → always close existing epic and create new
/// - No epic exists → create new epic
/// - Valid incomplete epic exists (has subtasks) → return it
/// - Invalid epic exists (no subtasks, still open) → close as wont_do, create new
/// - Closed epic exists → create new epic
fn run_add(
    cwd: &Path,
    plan_path: &str,
    restart: bool,
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
            "Cannot create epic for draft plan. Remove `draft: true` from frontmatter first."
                .to_string(),
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

    // Load current tasks to check for existing epics
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let plan_graph = PlanGraph::build(&graph);

    // --restart always creates a new epic
    if restart {
        if let Some(epic) = plan_graph.find_epic_for_plan(plan_path, &graph) {
            if epic.status != TaskStatus::Closed {
                undo_completed_subtasks(cwd, &epic.id)?;
                close_epic(cwd, &epic.id)?;
            }
        }
        let epic_id = create_epic(cwd, plan_path, template_name.as_deref(), agent_type)?;
        return output_epic_result(cwd, &epic_id, true);
    }

    // Find-or-create: check for existing epic
    let existing_epic = plan_graph.find_epic_for_plan(plan_path, &graph);

    match existing_epic {
        Some(epic) if epic.status != TaskStatus::Closed => {
            // Epic is open — validate it has subtasks
            let subtasks = get_subtasks(&graph, &epic.id);
            if subtasks.is_empty() {
                // Invalid epic (no subtasks) — decompose agent failed
                close_epic_as_invalid(cwd, &epic.id)?;
                let epic_id = create_epic(cwd, plan_path, template_name.as_deref(), agent_type)?;
                return output_epic_result(cwd, &epic_id, true);
            }

            // Valid incomplete epic — return it (deterministic, no prompt)
            output_epic_resumed(&epic.id, &subtasks)?;
            if !std::io::stdout().is_terminal() {
                println!("<aiki_epic epic_id=\"{}\"/>", epic.id);
            }
            Ok(())
        }
        _ => {
            // No epic, or epic is closed — create new
            let epic_id = create_epic(cwd, plan_path, template_name.as_deref(), agent_type)?;
            output_epic_result(cwd, &epic_id, true)
        }
    }
}

/// Create a new epic by running the decompose agent.
///
/// 1. Creates the epic task (container for subtasks)
/// 2. Creates and runs the decompose agent task with `data.epic` pointing to it
/// 3. Returns the epic task ID
fn create_epic(
    cwd: &Path,
    plan_path: &str,
    template_name: Option<&str>,
    agent_type: Option<AgentType>,
) -> Result<String> {
    // Create the epic task first so the decompose agent can add subtasks to it
    let epic_id = create_epic_task(cwd, plan_path)?;

    let template = template_name.unwrap_or("aiki/decompose");
    let assignee = agent_type
        .as_ref()
        .map(|a| a.as_str().to_string())
        .or_else(|| Some("claude-code".to_string()));

    let decompose_task_id = create_decompose_task(cwd, plan_path, &epic_id, template, assignee)?;

    // Epic task depends on decompose task — blocked until decomposition finishes
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    write_link_event(cwd, &graph, "depends-on", &epic_id, &decompose_task_id)?;

    // Run the decompose task to completion
    let options = if let Some(agent) = agent_type {
        TaskRunOptions::new().with_agent(agent)
    } else {
        TaskRunOptions::new()
    };
    task_run(cwd, &decompose_task_id, options)?;

    Ok(epic_id)
}

/// Create the epic task — the container that holds subtasks.
///
/// Extracts the plan title from the H1 heading (or filename as fallback).
/// Sets `data.plan`, `implements` link, and source.
fn create_epic_task(cwd: &Path, plan_path: &str) -> Result<String> {
    let full_path = if plan_path.starts_with('/') {
        std::path::PathBuf::from(plan_path)
    } else {
        cwd.join(plan_path)
    };
    let metadata = parse_plan_metadata(&full_path);

    // Use H1 title from plan, or fall back to filename without extension
    let plan_title = metadata.title.unwrap_or_else(|| {
        Path::new(plan_path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    });

    let epic_name = format!("Epic: {}", plan_title);
    let epic_id = generate_task_id(&epic_name);
    let timestamp = chrono::Utc::now();
    let working_copy = get_working_copy_change_id(cwd);

    let mut data = std::collections::HashMap::new();
    data.insert("plan".to_string(), plan_path.to_string());

    let event = TaskEvent::Created {
        task_id: epic_id.clone(),
        name: epic_name,
        slug: None,
        task_type: None,
        priority: TaskPriority::P2,
        assignee: None,
        sources: vec![format!("file:{}", plan_path)],
        template: None,
        working_copy,
        instructions: None,
        data,
        timestamp,
    };
    write_event(cwd, &event)?;

    // Emit implements link
    let spec_target = if plan_path.starts_with("file:") {
        plan_path.to_string()
    } else {
        format!("file:{}", plan_path)
    };
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    write_link_event(cwd, &graph, "implements-plan", &epic_id, &spec_target)?;

    Ok(epic_id)
}

/// Find an existing epic or create a new one for the given plan.
///
/// Deterministic behavior (no interactive prompts):
/// - Valid incomplete epic exists (has subtasks) → return its ID
/// - Invalid epic exists (no subtasks, still open) → close as wont_do, create new
/// - No epic or closed epic → create new via decompose agent
///
/// Returns the epic task ID.
pub fn find_or_create_epic(cwd: &Path, plan_path: &str) -> Result<String> {
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let plan_graph = PlanGraph::build(&graph);

    let existing_epic = plan_graph.find_epic_for_plan(plan_path, &graph);

    match existing_epic {
        Some(epic) if epic.status != TaskStatus::Closed => {
            let subtasks = get_subtasks(&graph, &epic.id);
            if subtasks.is_empty() {
                close_epic_as_invalid(cwd, &epic.id)?;
                create_epic(cwd, plan_path, None, None)
            } else {
                Ok(epic.id.clone())
            }
        }
        _ => create_epic(cwd, plan_path, None, None),
    }
}

/// Close an epic as invalid (no subtasks — decompose agent failed).
fn close_epic_as_invalid(cwd: &Path, epic_id: &str) -> Result<()> {
    let timestamp = chrono::Utc::now();
    let close_event = TaskEvent::Closed {
        task_ids: vec![epic_id.to_string()],
        outcome: TaskOutcome::WontDo,
        summary: Some("No subtasks created — epic invalid".to_string()),
        turn_id: None,
        timestamp,
    };
    write_event(cwd, &close_event)?;
    Ok(())
}

/// Show epic status and subtasks
fn run_show(cwd: &Path, arg: &str, output_format: Option<OutputFormat>) -> Result<()> {
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);

    // Determine if arg is a task ID or plan path
    let epic = if is_task_id(arg) || is_task_id_prefix(arg) {
        // Task ID or prefix lookup
        find_task(&graph.tasks, arg)?
    } else {
        // Plan path lookup via PlanGraph
        let plan_graph = PlanGraph::build(&graph);
        plan_graph.find_epic_for_plan(arg, &graph).ok_or_else(|| {
            AikiError::InvalidArgument(format!("No epic found for plan: {}", arg))
        })?
    };

    match output_format {
        Some(OutputFormat::Id) => {
            println!("{}", epic.id);
        }
        None => {
            let subtasks = get_subtasks(&graph, &epic.id);
            output_epic_show(epic, &subtasks)?;
        }
    }

    Ok(())
}

/// List all epics
fn run_list(cwd: &Path) -> Result<()> {
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);

    // Collect all unique epics by finding tasks with implements-plan edges
    let mut epics: Vec<&Task> = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    for task in graph.tasks.values() {
        let targets = graph.edges.targets(&task.id, "implements-plan");
        if targets.is_empty() {
            continue;
        }
        if seen_ids.insert(&task.id) {
            epics.push(task);
        }
    }

    // Sort by created_at (newest first)
    epics.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    if epics.is_empty() {
        eprintln!("No epics found.");
        return Ok(());
    }

    let mut content = String::from("## Epics\n\n| ID | Status | Plan | Progress | Name |\n|-----|--------|------|----------|------|\n");

    for epic in &epics {
        let status_str = match epic.status {
            TaskStatus::Open => "open",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Stopped => "stopped",
            TaskStatus::Closed => "closed",
        };

        let plan_str = epic
            .data
            .get("plan")
            .map(|s| s.as_str())
            .unwrap_or("-");

        let subtasks = get_subtasks(&graph, &epic.id);
        let completed = subtasks.iter().filter(|t| t.status == TaskStatus::Closed).count();
        let total = subtasks.len();
        let progress = if total > 0 {
            format!("{}/{}", completed, total)
        } else {
            "-".to_string()
        };

        let short_id = &epic.id[..epic.id.len().min(8)];
        content.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            short_id, status_str, plan_str, progress, &epic.name
        ));
    }

    let md = MdBuilder::new("epic-list").build(&content, &[], &[]);
    eprintln!("{}", md);

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

/// Undo file changes made by completed subtasks of an epic.
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
        if stderr.contains("no completed subtasks") || stderr.contains("NoCompletedSubtasks") {
            return Ok(());
        }
        return Err(AikiError::JjCommandFailed(format!(
            "task undo failed: {}",
            stderr.trim()
        )));
    }

    let stderr_output = String::from_utf8_lossy(&output.stderr);
    if !stderr_output.is_empty() {
        eprint!("{}", stderr_output);
    }

    Ok(())
}

/// Close an existing epic as wont_do
fn close_epic(cwd: &Path, epic_id: &str) -> Result<()> {
    let timestamp = chrono::Utc::now();

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

/// Create the decompose task from template.
fn create_decompose_task(
    cwd: &Path,
    plan_path: &str,
    epic_id: &str,
    template_name: &str,
    assignee: Option<String>,
) -> Result<String> {
    use super::task::{create_from_template, TemplateTaskParams};

    let mut data = std::collections::HashMap::new();
    data.insert("plan".to_string(), plan_path.to_string());
    data.insert("epic".to_string(), epic_id.to_string());

    let params = TemplateTaskParams {
        template_name: template_name.to_string(),
        data,
        sources: vec![format!("file:{}", plan_path)],
        assignee: assignee.or_else(|| Some("claude-code".to_string())),
        ..Default::default()
    };

    let task_id = create_from_template(cwd, params)?;

    let spec_target = if plan_path.starts_with("file:") {
        plan_path.to_string()
    } else {
        format!("file:{}", plan_path)
    };
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    write_link_event(cwd, &graph, "decomposes-plan", &task_id, &spec_target)?;

    Ok(task_id)
}

/// Output epic result (created or found) to stderr and stdout.
fn output_epic_result(cwd: &Path, epic_id: &str, created: bool) -> Result<()> {
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let subtasks = get_subtasks(&graph, epic_id);

    if created {
        output_epic_created(epic_id, &subtasks)?;
    } else {
        output_epic_resumed(epic_id, &subtasks)?;
    }

    if !std::io::stdout().is_terminal() {
        println!("<aiki_epic epic_id=\"{}\"/>", epic_id);
    }

    Ok(())
}

/// Output epic created message to stderr
fn output_epic_created(epic_id: &str, subtasks: &[&Task]) -> Result<()> {
    let mut content = format!("## Epic Created\n- **ID:** {}\n\n", epic_id);
    for (i, subtask) in subtasks.iter().enumerate() {
        content.push_str(&format!("{}. {}\n", i + 1, &subtask.name));
    }
    content.push_str(&format!(
        "\n- Review:  `aiki epic show {}`\n- Execute: `aiki build {}`\n",
        epic_id, epic_id
    ));

    let md = MdBuilder::new("epic").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

/// Output epic resumed message to stderr
fn output_epic_resumed(epic_id: &str, subtasks: &[&Task]) -> Result<()> {
    let completed = subtasks
        .iter()
        .filter(|t| t.status == TaskStatus::Closed)
        .count();
    let total = subtasks.len();

    let mut content = format!(
        "## Epic Resumed\n- **ID:** {}\n- Resuming existing epic ({}/{} subtasks done).\n\n",
        epic_id, completed, total
    );
    for (i, subtask) in subtasks.iter().enumerate() {
        let status_mark = if subtask.status == TaskStatus::Closed {
            "done"
        } else {
            "pending"
        };
        content.push_str(&format!("{}. [{}] {}\n", i + 1, status_mark, &subtask.name));
    }
    content.push_str(&format!(
        "\n- Review:  `aiki epic show {}`\n- Execute: `aiki build {}`\n",
        epic_id, epic_id
    ));

    let md = MdBuilder::new("epic").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

/// Output epic show (detailed status display)
fn output_epic_show(epic: &Task, subtasks: &[&Task]) -> Result<()> {
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

    content.push_str(&format!("- **Progress:** {}/{}\n", completed, total));

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

    if !epic.sources.is_empty() {
        content.push_str("\n### Sources\n");
        for source in &epic.sources {
            content.push_str(&format!("- {}\n", source));
        }
    }

    let md = MdBuilder::new("epic-show").build(&content, &[], &[]);
    eprintln!("{}", md);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::graph::{EdgeStore, TaskGraph};
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

    fn make_graph(tasks: FastHashMap<String, Task>, edges: EdgeStore) -> TaskGraph {
        TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        }
    }

    fn find_epic_for_plan_via_graph<'a>(
        graph: &'a TaskGraph,
        plan_path: &str,
    ) -> Option<&'a Task> {
        let sg = PlanGraph::build(graph);
        sg.find_epic_for_plan(plan_path, graph)
    }

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
}
