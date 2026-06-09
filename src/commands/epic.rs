//! Epic command for creating and managing epics from plan files
//!
//! This module provides the `aiki epic` command which:
//! - Creates an epic from a plan file via `epic add`
//! - Shows epic status and subtasks via `epic show`
//! - Lists all epics via `epic list`

use std::env;
use std::path::Path;

use clap::Subcommand;

use super::OutputFormat;
use crate::agents::AgentType;
use crate::epic::{close_epic, create_epic_with_decompose, undo_completed_subtasks};
use crate::error::{AikiError, Result};
use crate::output_utils;
use crate::plans::{parse_plan_metadata, PlanGraph};
use crate::tasks::md::MdBuilder;
use crate::tasks::{
    find_task, get_subtasks, looks_like_task_id, materialize_graph, read_events, Task, TaskStatus,
};
use crate::workflow::steps::plan::validate_plan_path;

/// Epic subcommands
#[derive(Subcommand)]
#[command(disable_help_subcommand = true)]
pub enum EpicCommands {
    /// Create an epic from a plan file
    Add {
        /// Path to plan file (e.g., ops/now/my-feature.md)
        plan_path: String,

        /// Ignore existing epic and create a new one from scratch
        #[arg(long)]
        restart: bool,

        /// Decompose template to use (default: decompose)
        #[arg(long)]
        template: Option<String>,

        /// Agent for decomposition (default: claude-code)
        #[arg(long)]
        agent: Option<String>,

        /// Shorthand for --agent claude-code
        #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
        claude: bool,
        /// Shorthand for --agent codex
        #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
        codex: bool,
        /// Shorthand for --agent cursor
        #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
        cursor: bool,
        /// Shorthand for --agent gemini
        #[arg(long, group = "agent_shorthand", conflicts_with = "agent")]
        gemini: bool,

        /// Output format (e.g., `id` for bare task ID on stdout)
        #[arg(long, short = 'o', value_name = "FORMAT")]
        output: Option<OutputFormat>,
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
    List {
        /// Limit the number of results shown
        #[arg(long, short = 'n')]
        number: Option<usize>,
    },
}

/// Run the epic command
pub fn run(command: EpicCommands) -> Result<()> {
    let cwd = env::current_dir()
        .map_err(|_| AikiError::InvalidArgument("Failed to get current directory".to_string()))?;

    match command {
        EpicCommands::Add {
            plan_path,
            restart,
            template,
            agent,
            claude,
            codex,
            cursor,
            gemini,
            output,
        } => {
            use crate::session::flags::resolve_agent_shorthand;
            let agent = resolve_agent_shorthand(agent, claude, codex, cursor, gemini);
            run_add(&cwd, &plan_path, restart, template, agent, output)
        }
        EpicCommands::Show { arg, output } => run_show(&cwd, &arg, output),
        EpicCommands::List { number } => run_list(&cwd, number),
    }
}

/// Core add (decompose) implementation — deterministic create-or-error.
///
/// Behavior (no interactive prompts):
/// - `--restart` → always close existing epic and create new
/// - No epic exists → create new epic
/// - Open epic exists → error (user must use `--restart` to replace)
/// - Closed epic exists → create new epic
fn run_add(
    cwd: &Path,
    plan_path: &str,
    restart: bool,
    template_name: Option<String>,
    agent_type: Option<AgentType>,
    output_format: Option<OutputFormat>,
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

    // Load current tasks to check for existing epics
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let plan_graph = PlanGraph::build(&graph);

    // --restart always creates a new epic
    if restart {
        if let Some(epic) = plan_graph.resolve_epic_for_plan(plan_path, &graph)? {
            if epic.status != TaskStatus::Closed {
                undo_completed_subtasks(cwd, &epic.id)?;
                close_epic(cwd, &epic.id)?;
            }
        }
        let epic_id =
            create_epic_with_decompose(cwd, plan_path, template_name.as_deref(), agent_type, false)?;
        return output_epic_result(cwd, &epic_id, output_format);
    }

    // Find-or-create: check for existing epic
    let existing_epic = plan_graph.resolve_epic_for_plan(plan_path, &graph)?;

    match existing_epic {
        Some(epic) if epic.status != TaskStatus::Closed => {
            // Epic is open — error out; user must use --restart to replace it
            return Err(AikiError::InvalidArgument(format!(
                "An open epic already exists for this plan: {} ({}). Use --restart to replace it.",
                &epic.id[..epic.id.len().min(8)],
                epic.name
            )));
        }
        _ => {
            // No epic, or epic is closed — create new
            let epic_id =
                create_epic_with_decompose(cwd, plan_path, template_name.as_deref(), agent_type, false)?;
            output_epic_result(cwd, &epic_id, output_format)
        }
    }
}

/// Show epic status and subtasks
fn run_show(cwd: &Path, arg: &str, output_format: Option<OutputFormat>) -> Result<()> {
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);

    // Determine if arg is a task ID or plan path
    let epic = if looks_like_task_id(arg) {
        // Task ID or prefix lookup
        find_task(&graph.tasks, arg)?
    } else {
        // Plan path lookup via PlanGraph
        let plan_graph = PlanGraph::build(&graph);
        plan_graph
            .resolve_epic_for_plan(arg, &graph)?
            .ok_or_else(|| AikiError::InvalidArgument(format!("No epic found for plan: {}", arg)))?
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
fn run_list(cwd: &Path, number: Option<usize>) -> Result<()> {
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

    // Apply --number truncation
    if let Some(n) = number {
        epics.truncate(n);
    }

    if epics.is_empty() {
        output_utils::emit(|| "No epics found.".to_string());
        return Ok(());
    }

    let mut content = String::from("## Epics\n\n| ID | Status | Plan | Progress | Name |\n|-----|--------|------|----------|------|\n");

    for epic in &epics {
        let status_str = match epic.status {
            TaskStatus::Open => "open",
            TaskStatus::Reserved => "reserved",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Stopped => "stopped",
            TaskStatus::Closed => "closed",
        };

        let plan_str = epic.data.get("plan").map(|s| s.as_str()).unwrap_or("-");

        let subtasks = get_subtasks(&graph, &epic.id);
        let completed = subtasks
            .iter()
            .filter(|t| t.status == TaskStatus::Closed)
            .count();
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

    output_utils::emit(|| MdBuilder::new().build(&content));

    Ok(())
}

/// Output epic result (created) to stderr and stdout.
fn output_epic_result(
    cwd: &Path,
    epic_id: &str,
    output_format: Option<OutputFormat>,
) -> Result<()> {
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let subtasks = get_subtasks(&graph, epic_id);

    if matches!(output_format, Some(OutputFormat::Id)) {
        println!("{}", epic_id);
    } else {
        output_epic_created(epic_id, &subtasks)?;
    }

    Ok(())
}

/// Output epic created message to stderr
fn output_epic_created(epic_id: &str, subtasks: &[&Task]) -> Result<()> {
    output_utils::emit(|| {
        let mut content = format!("## Epic Created\n- **ID:** {}\n\n", epic_id);
        for (i, subtask) in subtasks.iter().enumerate() {
            content.push_str(&format!("{}. {}\n", i + 1, &subtask.name));
        }
        content.push_str(&format!(
            "\n- Review:  `aiki epic show {}`\n- Execute: `aiki build {}`\n",
            epic_id, epic_id
        ));
        MdBuilder::new().build(&content)
    });
    Ok(())
}


/// Output epic show (detailed status display)
fn output_epic_show(epic: &Task, subtasks: &[&Task]) -> Result<()> {
    output_utils::emit(|| {
        let completed = subtasks
            .iter()
            .filter(|t| t.status == TaskStatus::Closed)
            .count();
        let total = subtasks.len();

        let status_str = match epic.status {
            TaskStatus::Open => "open",
            TaskStatus::Reserved => "reserved",
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
                    TaskStatus::Reserved => "reserved",
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
                    i + 1,
                    &subtask.id,
                    sub_status,
                    sub_outcome,
                    &subtask.name
                ));
            }
        }

        if !epic.sources.is_empty() {
            content.push_str("\n### Sources\n");
            for source in &epic.sources {
                content.push_str(&format!("- {}\n", source));
            }
        }

        MdBuilder::new().build(&content)
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::graph::{EdgeStore, TaskGraph};
    use crate::tasks::types::FastHashMap;
    use crate::tasks::TaskPriority;
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
            instructions: None,
            data: HashMap::new(),
            created_at: chrono::Utc::now(),
            started_at: None,
            claimed_by_session: None,
            last_session_id: None,
            stopped_reason: None,
            closed_outcome: None,
            confidence: None,
            summary: None,
            turn_started: None,
            closed_at: None,
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
    ) -> anyhow::Result<Option<&'a Task>> {
        let sg = PlanGraph::build(graph);
        sg.resolve_epic_for_plan(plan_path, graph)
    }

    #[test]
    fn test_find_epic_for_plan_none() {
        let graph = make_graph(FastHashMap::default(), EdgeStore::new());
        assert!(find_epic_for_plan_via_graph(&graph, "ops/now/feature.md")
            .unwrap()
            .is_none());
    }

    #[test]
    fn test_find_epic_for_plan_via_implements_link() {
        let mut tasks = FastHashMap::default();
        let task = make_task("epic1", "Epic: Feature", TaskStatus::Open);
        tasks.insert("epic1".to_string(), task);

        let mut edges = EdgeStore::new();
        edges.add("epic1", "file:ops/now/feature.md", "implements-plan");

        let graph = make_graph(tasks, edges);
        let result = find_epic_for_plan_via_graph(&graph, "ops/now/feature.md").unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "epic1");
    }

    #[test]
    fn test_find_epic_for_plan_returns_ambiguity_error() {
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
        let err = find_epic_for_plan_via_graph(&graph, "ops/now/feature.md").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Multiple epics implement file:ops/now/feature.md"));
        assert!(msg.contains("epic_old (Epic: Old)"));
        assert!(msg.contains("epic_new (Epic: New)"));
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
