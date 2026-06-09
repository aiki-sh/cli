//! Task management system for Aiki
//!
//! Provides an AI-first task tracking system with:
//! - Event-sourced storage on `aiki/tasks` branch
//! - Markdown output format for agent consumption
//! - Ready queue calculation with priority sorting
//! - Task execution via agent runtimes
//! - Template-based task creation

use std::path::Path;

pub mod baseline;
pub mod graph;
pub mod id;
pub mod lanes;
pub mod listener;
pub mod manager;
pub mod md;
pub mod revset;
pub mod runner;
pub mod spawner;
pub mod storage;
pub mod templates;
pub mod types;

pub use baseline::select_task_snapshot_baseline;
pub use graph::{materialize_graph, materialize_graph_with_ids, TaskGraph};
pub use id::{generate_task_id, is_task_id, is_task_id_prefix, is_valid_slug, looks_like_task_id};
#[cfg(test)]
pub use id::TaskRef;
pub use manager::{
    find_task, get_current_scope_set, get_in_progress, get_ready_queue_for_scope_set, get_subtasks,
};
pub use md::MdBuilder;
pub use revset::build_task_revset_pattern_with_graph;
pub use storage::{
    read_events, read_events_with_ids, write_event, write_link_event, write_link_event_with_autorun,
};
pub use types::{
    Task, TaskActivity, TaskComment, TaskEvent, TaskOutcome, TaskPriority, TaskStatus,
};

use crate::error::{AikiError, Result};
use crate::events::{AikiEvent, AikiTaskStartedPayload, TaskEventPayload};
use crate::session::find_active_session;

/// Get the current turn ID for the active session, if available.
///
/// Returns `None` when running outside a session (e.g., from a terminal)
/// or if the session/turn lookup fails.
pub fn current_turn_id(session_id: Option<&str>) -> Option<String> {
    let sid = session_id?;
    let (turn_number, _) =
        crate::history::get_current_turn_info(&crate::global::global_aiki_dir(), sid).ok()?;
    Some(crate::session::turn_state::generate_turn_id(
        sid,
        turn_number,
    ))
}

/// Result of starting tasks via `start_task_core`
#[derive(Debug, Clone)]
pub struct StartTaskResult;

/// Core task start logic. Validates tasks and emits Started events.
///
/// This is the canonical implementation used by `aiki task start`, `aiki review --start`,
/// and `aiki fix --start`. All start operations should go through this function to ensure
/// consistent behavior.
///
/// # What this function does:
/// - Validates that tasks exist and are not closed
/// - Creates TaskEvent::Started
/// - Emits task.started flow events via event_bus
///
/// # What this function does NOT do:
/// - Quick-start (description → new task) - caller should create task first
/// - Template creation - caller should create from template first
/// - Reopen logic - caller should reopen before calling this
/// - Parent/subtask handling - caller should handle stale subtask cleanup
///
/// # Arguments
/// * `cwd` - Working directory
/// * `task_ids` - Task IDs to start
///
/// # Returns
/// `StartTaskResult` with the started tasks
pub fn start_task_core(cwd: &Path, task_ids: &[String]) -> Result<StartTaskResult> {
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let tasks = graph.tasks.clone();

    // Validate all tasks exist and are not closed
    for id in task_ids {
        let task = find_task(&tasks, id)?;

        if task.status == TaskStatus::Closed {
            return Err(AikiError::InvalidArgument(format!(
                "Task '{}' is closed. Use --reopen --reason to reopen it.",
                id
            )));
        }
    }

    // Detect current session early - needed for start event
    let session_match = find_active_session(cwd);
    let our_session_id = session_match.as_ref().map(|m| m.session_id.clone());

    // Query current turn ID from session
    let turn_id = current_turn_id(our_session_id.as_deref());

    // Reuse session detected earlier for start event
    let agent_type_str = session_match
        .as_ref()
        .map(|m| m.agent_type.as_str().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let session_id = our_session_id;

    // Create started event
    let timestamp = chrono::Utc::now();
    let start_event = TaskEvent::Started {
        task_ids: task_ids.to_vec(),
        agent_type: agent_type_str,
        session_id,
        turn_id,
        working_copy: crate::jj::get_working_copy_snapshot_rev(cwd),
        instructions: None,
        timestamp,
    };
    write_event(cwd, &start_event)?;

    // Emit task.started flow events for each started task
    for task_id in task_ids {
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

    Ok(StartTaskResult)
}

/// Reopen a task if it is closed. No-op if the task is not closed or not found.
///
/// Used when adding subtasks to a closed parent — the parent must be reopened
/// before new children can be created.
pub fn reopen_if_closed(
    cwd: &Path,
    task_id: &str,
    tasks: &types::FastHashMap<String, Task>,
    reason: &str,
) -> Result<()> {
    if let Some(task) = tasks.get(task_id) {
        if task.status == TaskStatus::Closed {
            let reopen_event = TaskEvent::Reopened {
                task_id: task_id.to_string(),
                reason: reason.to_string(),
                timestamp: chrono::Utc::now(),
            };
            write_event(cwd, &reopen_event)?;
        }
    }
    Ok(())
}

/// Close a task as won't-do with a summary comment.
pub fn close_task_as_wont_do(cwd: &Path, task_id: &str, summary: &str) -> Result<()> {
    let timestamp = chrono::Utc::now();

    let comment_event = TaskEvent::CommentAdded {
        task_ids: vec![task_id.to_string()],
        text: summary.to_string(),
        data: std::collections::HashMap::new(),
        timestamp: timestamp - chrono::Duration::milliseconds(1),
    };
    write_event(cwd, &comment_event)?;

    let close_event = TaskEvent::Closed {
        task_ids: vec![task_id.to_string()],
        outcome: TaskOutcome::WontDo,
        confidence: None,
        summary: Some(summary.to_string()),
        session_id: None,
        turn_id: None,
        timestamp,
    };
    write_event(cwd, &close_event)?;
    Ok(())
}

/// Reassign a task to a new agent.
///
/// Creates an Updated event to change the task's assignee field.
pub fn reassign_task(cwd: &Path, task_id: &str, new_assignee: &str) -> Result<()> {
    let update_event = TaskEvent::Updated {
        task_id: task_id.to_string(),
        name: None,
        priority: None,
        assignee: Some(new_assignee.to_string()),
        data: None,
        instructions: None,
        timestamp: chrono::Utc::now(),
    };
    write_event(cwd, &update_event)?;
    Ok(())
}

/// Infer task type from task name and sources.
///
/// Looks at task name and sources to determine type:
/// - "review" if task type is explicitly set to "review"
/// - "bug" if task name contains "fix" or "bug"
/// - "feature" otherwise (default)
fn infer_task_type(task: &Task) -> String {
    // Check explicit task_type first
    if let Some(ref task_type) = task.task_type {
        return task_type.clone();
    }

    let name_lower = task.name.to_lowercase();

    // Check for review indicators
    if name_lower.contains("review") {
        return "review".to_string();
    }

    // Check for bug/fix indicators
    if name_lower.contains("fix") || name_lower.contains("bug") {
        return "bug".to_string();
    }

    // Default to feature
    "feature".to_string()
}
