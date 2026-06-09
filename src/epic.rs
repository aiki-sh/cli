//! Epic domain module — lifecycle management and plan↔epic resolution.
//!
//! Consolidates epic-related logic that was previously spread across
//! `plans::graph` (resolution functions) and `workflow::steps::decompose`
//! (lifecycle helpers).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::agents::{determine_default_coder, AgentType};
use crate::config::get_aiki_binary_path;
use crate::error::{AikiError, Result as AikiResult};
use crate::plans::parse_plan_metadata;
use crate::plans::PlanGraph;
use crate::session::isolation::find_repo_root_from_workspace;
use crate::tasks::graph::TaskGraph;
use crate::tasks::id::generate_task_id;
use crate::tasks::types::Task;
use crate::tasks::{
    get_subtasks, materialize_graph, read_events, write_event,
    TaskEvent, TaskOutcome, TaskPriority, TaskStatus,
};
use crate::workflow::steps::plan::resolve_plan_path;

// ---------------------------------------------------------------------------
// Plan↔epic resolution (moved from plans::graph)
// ---------------------------------------------------------------------------

/// Resolve the on-disk path for a plan linked from an epic task.
///
/// Relative paths resolve from the current working directory first. When
/// running inside an isolated workspace, absolute repo-root paths are mapped
/// back into the workspace when the corresponding file exists there.
pub fn resolve_linked_plan_path(cwd: &Path, graph: &TaskGraph, epic_id: &str) -> Option<PathBuf> {
    let targets = graph.edges.targets(epic_id, "implements-plan");
    let target = targets.iter().find(|t| t.starts_with("file:"))?;
    let linked_path = PathBuf::from(target.strip_prefix("file:")?);

    if linked_path.is_relative() {
        let candidate = cwd.join(&linked_path);
        if candidate.exists() {
            return Some(candidate);
        }

        if let Some(repo_root) = find_repo_root_from_workspace(cwd) {
            let repo_candidate = repo_root.join(&linked_path);
            if repo_candidate.exists() {
                return Some(repo_candidate);
            }
        }

        return None;
    }

    if let Some(repo_root) = find_repo_root_from_workspace(cwd) {
        if let Ok(relative) = linked_path.strip_prefix(&repo_root) {
            let workspace_candidate = cwd.join(relative);
            if workspace_candidate.exists() {
                return Some(workspace_candidate);
            }
        }
    }

    if linked_path.exists() {
        return Some(linked_path);
    }

    None
}

/// Read the plan file content for an epic task.
///
/// Finds the `implements-plan` link target for the given epic, resolves it in
/// the current workspace context, and reads the file from disk.
/// Returns `None` if no link exists or the file doesn't exist.
pub fn read_plan_for_epic(cwd: &Path, graph: &TaskGraph, epic_id: &str) -> Option<String> {
    let path = resolve_linked_plan_path(cwd, graph, epic_id)?;
    fs::read_to_string(path).ok()
}

/// Resolve the epic task that implements a given plan path.
///
/// Uses `PlanGraph::resolve_epic_for_plan()` to find the epic. Returns an error
/// if no epic is found for the given path or when the plan path is ambiguous.
pub fn resolve_epic_from_plan_path<'a>(graph: &'a TaskGraph, path: &str) -> Result<&'a Task> {
    let plan_graph = PlanGraph::build(graph);
    match plan_graph.resolve_epic_for_plan(path, graph)? {
        Some(task) => Ok(task),
        None => bail!("No epic found that implements {}", path),
    }
}

// ---------------------------------------------------------------------------
// Epic lifecycle (moved from workflow::steps::decompose)
// ---------------------------------------------------------------------------

/// Create the epic task — the container that holds subtasks.
///
/// Extracts the plan title from the H1 heading (or filename as fallback).
/// Sets `data.plan` and source. The `implements-plan` link is written by
/// `run_decompose()` which is called after this function.
pub(crate) fn create_epic_task(
    cwd: &Path,
    plan_path: &str,
    agent_override: Option<AgentType>,
) -> AikiResult<String> {
    let full_path = resolve_plan_path(cwd, plan_path);
    let metadata = parse_plan_metadata(&full_path);

    let plan_title = metadata.title.unwrap_or(metadata.path);

    // Resolve assignee: explicit --agent override, or default coder.
    let assignee = agent_override
        .or_else(|| determine_default_coder().ok())
        .map(|a| a.as_str().to_string());

    let epic_name = format!("Epic: {}", plan_title);
    let epic_id = generate_task_id(&epic_name);
    let timestamp = chrono::Utc::now();
    let mut data = std::collections::HashMap::new();
    data.insert("plan".to_string(), plan_path.to_string());

    let event = TaskEvent::Created {
        task_id: epic_id.clone(),
        name: epic_name,
        slug: None,
        task_type: Some("epic".to_string()),
        priority: TaskPriority::P2,
        assignee,
        sources: vec![format!("file:{}", plan_path)],
        template: None,
        instructions: None,
        data,
        timestamp,
    };
    write_event(cwd, &event)?;

    Ok(epic_id)
}

/// Undo file changes made by completed subtasks of an epic.
///
/// Invokes `aiki task undo <epic-id> --completed` to revert changes before
/// closing the epic. If no completed subtasks exist, this is a no-op.
pub(crate) fn undo_completed_subtasks(cwd: &Path, epic_id: &str) -> AikiResult<()> {
    let output = std::process::Command::new(get_aiki_binary_path())
        .current_dir(cwd)
        .args(["task", "undo", epic_id, "--completed"])
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to run task undo: {}", e)))?;

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

/// Close an existing epic as wont_do.
pub(crate) fn close_epic(cwd: &Path, epic_id: &str) -> AikiResult<()> {
    crate::tasks::close_task_as_wont_do(cwd, epic_id, "Closed by --restart")
}

/// Restart an epic by stopping it and re-starting via `aiki task start`.
///
/// `aiki task start` on a parent with subtasks stops any stale in-progress
/// subtasks, giving the new orchestrator a clean slate.
pub(crate) fn restart_epic(cwd: &Path, epic_id: &str) -> AikiResult<()> {
    // Stop the epic to record why it was restarted
    let stop_event = TaskEvent::Stopped {
        task_ids: vec![epic_id.to_string()],
        reason: Some("Restarted by new build".to_string()),
        session_id: None,
        turn_id: None,
        timestamp: chrono::Utc::now(),
    };
    write_event(cwd, &stop_event)?;

    // Re-start via `aiki task start` which handles stopping stale subtasks
    let output = std::process::Command::new(get_aiki_binary_path())
        .current_dir(cwd)
        .args(["task", "start", epic_id])
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to restart epic: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::JjCommandFailed(format!(
            "Failed to restart epic: {}",
            stderr.trim()
        )));
    }

    Ok(())
}

/// Close an epic as invalid (no subtasks created).
pub(crate) fn close_epic_as_invalid(cwd: &Path, epic_id: &str) -> AikiResult<()> {
    crate::tasks::close_task_as_wont_do(cwd, epic_id, "No subtasks created — epic invalid")
}

/// Check if an epic is blocked by unresolved dependencies.
///
/// An epic is blocked if any of its `depends-on` targets are not closed with
/// outcome `Done`.
#[allow(dead_code)]
pub(crate) fn check_epic_blockers(graph: &TaskGraph, epic_id: &str) -> AikiResult<()> {
    let blocker_ids: Vec<&str> = graph
        .edges
        .targets(epic_id, "depends-on")
        .iter()
        .filter(|tid| {
            graph.tasks.get(tid.as_str()).map_or(true, |t| {
                !(t.status == TaskStatus::Closed && t.closed_outcome == Some(TaskOutcome::Done))
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
            "Epic {} is blocked by unresolved dependencies: {}. Rerun with --restart to start over",
            &epic_id[..epic_id.len().min(8)],
            blocker_names.join(", ")
        )));
    }

    Ok(())
}

/// Find an existing epic or create a new one for the given plan.
///
/// Deterministic behavior (no interactive prompts):
/// - Valid incomplete epic exists (has subtasks) → return its ID
/// - Invalid epic exists (no subtasks, still open) → close as wont_do, create new
/// - No epic or closed epic → create new via decompose agent
///
/// Returns the epic task ID.
#[allow(dead_code)]
pub fn find_or_create_epic(
    cwd: &Path,
    plan_path: &str,
    decompose_template: Option<&str>,
    show_tui: bool,
) -> AikiResult<String> {
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let plan_graph = PlanGraph::build(&graph);

    let existing_epic = plan_graph
        .resolve_epic_for_plan(plan_path, &graph)
        .map_err(|e| AikiError::Other(e))?;

    match existing_epic {
        Some(epic) if epic.status != TaskStatus::Closed => {
            let subtasks = get_subtasks(&graph, &epic.id);
            if subtasks.is_empty() {
                close_epic_as_invalid(cwd, &epic.id)?;
                create_epic_with_decompose(cwd, plan_path, decompose_template, None, show_tui)
            } else {
                Ok(epic.id.clone())
            }
        }
        _ => create_epic_with_decompose(cwd, plan_path, decompose_template, None, show_tui),
    }
}

/// Create a new epic by running the decompose agent.
///
/// 1. Creates the epic task (container for subtasks)
/// 2. Calls `run_decompose()` which handles implements-plan link, decompose task,
///    decomposes-plan link, depends-on link, and running the decompose agent
/// 3. Returns the epic task ID
pub(crate) fn create_epic_with_decompose(
    cwd: &Path,
    plan_path: &str,
    template_name: Option<&str>,
    agent_type: Option<crate::agents::AgentType>,
    show_tui: bool,
) -> AikiResult<String> {
    use crate::commands::decompose::{run_decompose, DecomposeOptions};

    let epic_id = create_epic_task(cwd, plan_path, agent_type)?;

    let options = DecomposeOptions {
        template: template_name.map(|s| s.to_string()),
        agent: agent_type,
    };
    run_decompose(cwd, plan_path, &epic_id, options, show_tui, None)?;

    Ok(epic_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::graph::{materialize_graph, EdgeStore};
    use crate::tasks::types::{FastHashMap, TaskEvent, TaskPriority, TaskStatus};
    use chrono::Utc;
    use std::collections::HashMap;

    fn make_task_graph(tasks: FastHashMap<String, Task>, edges: EdgeStore) -> TaskGraph {
        TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        }
    }

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
            created_at: Utc::now(),
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

    fn make_created(id: &str, name: &str) -> TaskEvent {
        TaskEvent::Created {
            task_id: id.to_string(),
            name: name.to_string(),
            slug: None,
            task_type: None,
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            instructions: None,
            data: HashMap::new(),
            timestamp: Utc::now(),
        }
    }

    fn make_link(from: &str, to: &str, kind: &str) -> TaskEvent {
        TaskEvent::LinkAdded {
            from: from.to_string(),
            to: to.to_string(),
            kind: kind.to_string(),
            autorun: None,
            timestamp: Utc::now(),
        }
    }

    // --- resolve_epic_from_plan_path tests ---

    #[test]
    fn test_resolve_epic_from_plan_path_returns_not_found_error() {
        let tg = make_task_graph(FastHashMap::default(), EdgeStore::new());

        let err = resolve_epic_from_plan_path(&tg, "ops/now/missing.md").unwrap_err();
        assert_eq!(
            err.to_string(),
            "No epic found that implements ops/now/missing.md"
        );
    }

    #[test]
    fn test_resolve_epic_from_plan_path_returns_ambiguity_error() {
        let mut tasks = FastHashMap::default();
        tasks.insert(
            "epic1".to_string(),
            make_task("epic1", "Epic One", TaskStatus::Closed),
        );
        tasks.insert(
            "epic2".to_string(),
            make_task("epic2", "Epic Two", TaskStatus::Open),
        );

        let mut edges = EdgeStore::new();
        edges.add("epic1", "file:ops/now/feature.md", "implements-plan");
        edges.add("epic2", "file:ops/now/feature.md", "implements-plan");

        let tg = make_task_graph(tasks, edges);

        let err = resolve_epic_from_plan_path(&tg, "ops/now/feature.md").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Multiple epics implement file:ops/now/feature.md"));
        assert!(msg.contains("epic1 (Epic One)"));
        assert!(msg.contains("epic2 (Epic Two)"));
    }

    // --- resolve_linked_plan_path / read_plan_for_epic tests ---

    #[test]
    fn test_resolve_linked_plan_path_relative_to_cwd() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("ops/now")).unwrap();
        let plan = temp.path().join("ops/now/feature.md");
        fs::write(&plan, "# plan").unwrap();

        let mut tasks = FastHashMap::default();
        tasks.insert(
            "epic1".to_string(),
            make_task("epic1", "Epic", TaskStatus::Closed),
        );
        let mut edges = EdgeStore::new();
        edges.add("epic1", "file:ops/now/feature.md", "implements-plan");

        let tg = make_task_graph(tasks, edges);
        let resolved = resolve_linked_plan_path(temp.path(), &tg, "epic1").unwrap();
        assert_eq!(resolved, plan);
        assert_eq!(
            read_plan_for_epic(temp.path(), &tg, "epic1").unwrap(),
            "# plan"
        );
    }

    #[test]
    fn test_resolve_linked_plan_path_maps_repo_absolute_path_into_workspace() {
        let repo_root = tempfile::tempdir().unwrap();
        let workspace_root = tempfile::tempdir().unwrap();

        fs::create_dir_all(repo_root.path().join("ops/now")).unwrap();
        fs::create_dir_all(workspace_root.path().join("ops/now")).unwrap();
        fs::create_dir_all(workspace_root.path().join(".jj")).unwrap();

        let repo_plan = repo_root.path().join("ops/now/feature.md");
        let workspace_plan = workspace_root.path().join("ops/now/feature.md");
        fs::write(&repo_plan, "# repo plan").unwrap();
        fs::write(&workspace_plan, "# workspace plan").unwrap();
        fs::write(
            workspace_root.path().join(".jj/repo"),
            repo_root.path().join(".jj/repo").to_string_lossy().as_ref(),
        )
        .unwrap();

        let mut tasks = FastHashMap::default();
        tasks.insert(
            "epic1".to_string(),
            make_task("epic1", "Epic", TaskStatus::Closed),
        );
        let mut edges = EdgeStore::new();
        edges.add(
            "epic1",
            &format!("file:{}", repo_plan.to_string_lossy()),
            "implements-plan",
        );

        let tg = make_task_graph(tasks, edges);
        let resolved = resolve_linked_plan_path(workspace_root.path(), &tg, "epic1").unwrap();
        assert_eq!(resolved, workspace_plan);
        assert_eq!(
            read_plan_for_epic(workspace_root.path(), &tg, "epic1").unwrap(),
            "# workspace plan"
        );
    }

    // --- build from events (integration) ---

    #[test]
    fn test_build_from_events_with_link() {
        let events = vec![
            make_created("epic1", "Epic: Feature"),
            make_link("epic1", "file:ops/now/feature.md", "implements-plan"),
        ];

        let tg = materialize_graph(&events);
        let result = resolve_epic_from_plan_path(&tg, "ops/now/feature.md");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().id, "epic1");
    }
}
