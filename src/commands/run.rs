//! Top-level `aiki run` command
//!
//! Spawns an agent session for a task and returns the session UUID.

use std::path::Path;

use crate::agents::runtime::discover_session_id;
use crate::agents::AgentType;
use crate::commands::task::{
    create_from_template, get_blocker_short_ids, parse_data_flags, TemplateTaskParams,
};
use crate::commands::OutputFormat;
use crate::error::{AikiError, Result};
use crate::tasks::{
    lanes::ThreadId,
    manager::{find_task, resolve_task_id_in_graph},
    materialize_graph,
    md::short_id,
    runner::{
        resolve_next_thread, resolve_next_thread_in_lane, run_task_with_output, TaskRunOptions,
        ThreadResolution,
    },
    storage::{read_events, write_event},
    types::{TaskEvent, TaskStatus},
    MdBuilder,
};

/// Run the top-level `aiki run` command.
#[allow(clippy::too_many_arguments)]
pub fn run(
    id: Option<String>,
    run_async: bool,
    force: bool,
    next_thread: bool,
    lane: Option<String>,
    agent: Option<AgentType>,
    template: Option<String>,
    data: Option<Vec<String>>,
    output: Option<OutputFormat>,
) -> Result<()> {
    let cwd = std::env::current_dir().map_err(|e| AikiError::Other(e.into()))?;
    run_impl(
        &cwd,
        id,
        run_async,
        force,
        next_thread,
        lane,
        agent,
        template,
        data,
        output,
    )
}

#[allow(clippy::too_many_arguments)]
fn run_impl(
    cwd: &Path,
    id: Option<String>,
    run_async: bool,
    force: bool,
    next_thread: bool,
    lane: Option<String>,
    agent: Option<AgentType>,
    template: Option<String>,
    data: Option<Vec<String>>,
    output: Option<OutputFormat>,
) -> Result<()> {
    let output_id = output.as_ref() == Some(&OutputFormat::Id);

    let agent_override = agent;

    // Handle template creation if --template provided
    let id = if let Some(template_name) = template {
        let data_map = parse_data_flags(&data.unwrap_or_default(), true)?;

        let params = TemplateTaskParams {
            template_name: template_name.clone(),
            data: data_map,
            sources: vec![],
            assignee: None,
            priority: None,
            parent_id: None,
            parent_name: None,
            source_data: std::collections::HashMap::new(),
            builtins: std::collections::HashMap::new(),
            task_id: None,
        };

        let task_id = create_from_template(cwd, params)?;
        if !output_id {
            eprintln!(
                "Added: {} — (created from template {})",
                task_id, template_name
            );
        }

        Some(task_id)
    } else if let Some(id_val) = id {
        Some(id_val)
    } else if !next_thread {
        return Err(AikiError::Other(anyhow::anyhow!(
            "Either task ID or --template must be provided"
        )));
    } else {
        None
    };

    // Track whether we claimed a subtask (for rollback on failure)
    let mut claimed_id: Option<String> = None;
    let mut thread: Option<ThreadId> = None;

    let actual_id = if next_thread {
        let id = id.ok_or_else(|| {
            AikiError::InvalidArgument("--next-thread requires a parent task ID".to_string())
        })?;

        let events = read_events(cwd)?;
        let graph = materialize_graph(&events);

        let parent_id = resolve_task_id_in_graph(&graph, &id)?;

        let parent = find_task(&graph.tasks, &parent_id)?;
        if parent.status == TaskStatus::Closed {
            return Err(AikiError::TaskAlreadyClosed(parent_id));
        }

        let resolution = if let Some(ref lane_prefix) = lane {
            resolve_next_thread_in_lane(&graph, &parent_id, lane_prefix)?
        } else {
            resolve_next_thread(&graph, &parent_id)
        };

        match resolution {
            ThreadResolution::Standalone(task) => {
                if !output_id {
                    eprintln!("Running subtask {} ({})...", short_id(&task.id), task.name);
                }

                let reserved_event = TaskEvent::Reserved {
                    task_ids: vec![task.id.clone()],
                    agent_type: agent.map(|a| a.as_str().to_string()).unwrap_or_default(),
                    timestamp: chrono::Utc::now(),
                };
                write_event(cwd, &reserved_event)?;
                claimed_id = Some(task.id.clone());

                task.id.clone()
            }
            ThreadResolution::Chain(chain) => {
                let head_id = chain[0].clone();
                if !output_id {
                    let head_name = {
                        let events2 = read_events(cwd)?;
                        let graph2 = materialize_graph(&events2);
                        graph2
                            .tasks
                            .get(&head_id)
                            .map(|t| t.name.clone())
                            .unwrap_or_else(|| "?".to_string())
                    };
                    eprintln!(
                        "Running needs-context chain ({} tasks, head: {} ({}))...",
                        chain.len(),
                        short_id(&head_id),
                        head_name,
                    );
                }

                let reserved_event = TaskEvent::Reserved {
                    task_ids: vec![head_id.clone()],
                    agent_type: agent.map(|a| a.as_str().to_string()).unwrap_or_default(),
                    timestamp: chrono::Utc::now(),
                };
                write_event(cwd, &reserved_event)?;
                claimed_id = Some(head_id.clone());
                let tail_id = chain.last().unwrap().clone();
                thread = Some(ThreadId {
                    head: head_id.clone(),
                    tail: tail_id,
                });

                head_id
            }
            ThreadResolution::AllComplete => {
                if output_id {
                    // No output at all for -o id
                    std::process::exit(2);
                }
                let md = MdBuilder::new().build(&format!(
                    "All subtasks complete for {}\n",
                    short_id(&parent_id)
                ));
                println!("{}", md);
                std::process::exit(2);
            }
            ThreadResolution::Blocked(unclosed) => {
                if output_id {
                    // No output for -o id on error
                    std::process::exit(1);
                }
                let mut msg = format!(
                    "No ready subtasks for {} ({} subtasks blocked)\n",
                    short_id(&parent_id),
                    unclosed.len()
                );
                for t in &unclosed {
                    let blocker_ids = get_blocker_short_ids(&graph, &t.id);
                    let status_str = match t.status {
                        TaskStatus::InProgress => "in progress".to_string(),
                        TaskStatus::Reserved => "reserved".to_string(),
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
                let md = MdBuilder::new().build_error(&msg);
                println!("{}", md);
                return Err(AikiError::InvalidArgument(format!(
                    "No ready subtasks for {}",
                    short_id(&parent_id),
                )));
            }
            ThreadResolution::NoSubtasks => {
                if output_id {
                    std::process::exit(1);
                }
                let msg = format!("Task {} has no subtasks", short_id(&parent_id));
                let md = MdBuilder::new().build_error(&msg);
                println!("{}", md);
                return Err(AikiError::InvalidArgument(msg));
            }
        }
    } else {
        let target_id = id.expect("id must be Some after validation");
        let events = read_events(cwd)?;
        let graph = materialize_graph(&events);

        let target_task_id = resolve_task_id_in_graph(&graph, &target_id)?;
        let task = find_task(&graph.tasks, &target_task_id)?;

        match task.status {
            TaskStatus::Reserved => {
                if force {
                    let released = TaskEvent::Released {
                        task_ids: vec![target_task_id.clone()],
                        reason: Some("Force-released by aiki run --force".to_string()),
                        timestamp: chrono::Utc::now(),
                    };
                    write_event(cwd, &released)?;
                } else {
                    return Err(AikiError::InvalidArgument(format!(
                        "Task '{}' is reserved and already pending a run. Use --force to override and re-run it.",
                        target_task_id
                    )));
                }
            }
            TaskStatus::InProgress => {
                if force {
                    let stopped = TaskEvent::Stopped {
                        task_ids: vec![target_task_id.clone()],
                        reason: Some("Force-stopped by aiki run --force".to_string()),
                        session_id: None,
                        turn_id: None,
                        timestamp: chrono::Utc::now(),
                    };
                    write_event(cwd, &stopped)?;
                } else {
                    return Err(AikiError::InvalidArgument(format!(
                        "Task '{}' is already in progress. Use --force to override and re-run it.",
                        target_task_id
                    )));
                }
            }
            _ => {}
        }

        target_task_id
    };

    // Build options
    let mut options = TaskRunOptions::new();
    if let Some(agent_type) = agent_override {
        options = options.with_agent(agent_type);
    }
    if let Some(t) = thread {
        options = options.with_thread(t);
    }

    // Spawn the task
    let result = if run_async {
        spawn_and_discover(cwd, &actual_id, options, output_id, true)
    } else {
        spawn_and_discover(cwd, &actual_id, options, output_id, false)
    };

    // Rollback claim on spawn failure
    rollback_on_spawn_failure(cwd, &claimed_id, &result);

    result
}

/// Roll back a task claim if spawn failed and the task is still in Reserved status.
///
/// Re-reads the event log to check if the agent already started the task before
/// emitting a Released event. If reading the event log itself fails, emits a
/// Released event unconditionally to avoid stranding the task in Reserved.
pub(crate) fn rollback_on_spawn_failure(
    cwd: &Path,
    claimed_id: &Option<String>,
    result: &Result<()>,
) {
    if let Err(ref spawn_err) = result {
        if let Some(ref cid) = claimed_id {
            let reason = format!("Spawn failed: {spawn_err}");
            try_rollback_reserved(cwd, cid, &reason);
        }
    }
}

/// Re-read events, check the task's current status, and emit a Released event
/// if the task is still Reserved. If the task is not found in the graph,
/// returns without emitting any event. If reading events fails, assumes
/// Reserved to avoid stranding the task.
///
/// This is the shared rollback logic used by both `rollback_on_spawn_failure`
/// (in run.rs) and `rollback_if_still_reserved` (in runner.rs).
pub(crate) fn try_rollback_reserved(cwd: &Path, task_id: &str, reason: &str) {
    let current_status = match read_events(cwd) {
        Ok(events) => match materialize_graph(&events)
            .tasks
            .get(task_id)
            .map(|t| t.status)
        {
            Some(status) => status,
            None => return, // Task not in graph — nothing to roll back
        },
        Err(_) => {
            // Cannot determine status — assume Reserved to avoid stranding
            TaskStatus::Reserved
        }
    };

    if let Some(event) = rollback_claim_if_reserved(task_id, current_status, Some(reason)) {
        let _ = write_event(cwd, &event);
    }
}

/// Build a Released rollback event if the task is still Reserved.
///
/// Returns `Some(Released)` when the current status is `Reserved` (the agent
/// hasn't started yet, so we need to release the claim). Returns `None` for any
/// other status — e.g. `InProgress` (agent already started) or `Closed`.
///
/// The `reason` parameter is included in the Released event so that task history
/// preserves the concrete failure cause (e.g. the spawn error).
pub(crate) fn rollback_claim_if_reserved(
    task_id: &str,
    current_status: TaskStatus,
    reason: Option<&str>,
) -> Option<TaskEvent> {
    if current_status == TaskStatus::Reserved {
        Some(TaskEvent::Released {
            task_ids: vec![task_id.to_string()],
            reason: Some(
                reason
                    .unwrap_or("Spawn failed, rolling back claim")
                    .to_string(),
            ),
            timestamp: chrono::Utc::now(),
        })
    } else {
        None
    }
}

/// Spawn an agent session, discover the session UUID, and optionally wait.
fn spawn_and_discover(
    cwd: &Path,
    task_id: &str,
    options: TaskRunOptions,
    output_id: bool,
    is_async: bool,
) -> Result<()> {
    use crate::tasks::runner::task_run_async;

    // Always spawn async first to get the handle
    let handle = task_run_async(cwd, task_id, options)?;

    let head_id = &handle.thread.head;

    // Discover session UUID (works for all agents with native hooks)
    let session_id = match discover_session_id(&handle.thread) {
        Ok(sid) => sid,
        Err(_) if output_id => {
            // Session discovery failed — no session ID to output
            if is_async {
                return Ok(());
            }
            // Fall through to blocking wait using task-based approach
            return run_task_with_output(cwd, task_id, TaskRunOptions::new());
        }
        Err(e) => return Err(e),
    };

    if is_async {
        // --async: print session UUID and return
        if output_id {
            println!("{}", session_id);
        } else {
            let md = MdBuilder::new().build(&format!(
                "## Run Started\n- **Session:** {}\n- **Task:** {}\n\n**Tip:** Use `aiki session wait {}` to block until complete.\n",
                session_id,
                short_id(head_id),
                session_id,
            ));
            println!("{}", md);
        }
        Ok(())
    } else {
        // Blocking: print session ID, then wait for task completion
        if !output_id {
            let md = MdBuilder::new().build(&format!(
                "## Running\n- **Session:** {}\n- **Task:** {}\n",
                session_id,
                short_id(head_id),
            ));
            eprintln!("{}", md);
        }
        // Wait for the task to reach terminal status
        wait_for_task_completion(cwd, head_id, Some(&session_id))
    }
}

/// Poll until task reaches a terminal status (Closed).
fn wait_for_task_completion(cwd: &Path, task_id: &str, session_id: Option<&str>) -> Result<()> {
    use std::thread;
    use std::time::Duration;

    let poll_interval = Duration::from_secs(2);

    loop {
        let events = read_events(cwd)?;
        let graph = materialize_graph(&events);

        if let Some(task) = graph.tasks.get(task_id) {
            if task.status == TaskStatus::Closed {
                let session_line = session_id
                    .map(|s| format!("- **Session:** {}\n", s))
                    .unwrap_or_default();
                let md = MdBuilder::new().build(&format!(
                    "## Run Completed\n{}- **Task:** {}\n- **Summary:** {}\n\n**Tip:** Use `aiki task show {}` for full details.\n",
                    session_line,
                    short_id(task_id),
                    task.display_summary(),
                    short_id(task_id),
                ));
                println!("{}", md);
                return Ok(());
            }
        }

        thread::sleep(poll_interval);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_when_still_reserved() {
        let result = rollback_claim_if_reserved("task-123", TaskStatus::Reserved, None);
        assert!(
            result.is_some(),
            "Should return Released event when Reserved"
        );
        if let Some(TaskEvent::Released {
            task_ids, reason, ..
        }) = result
        {
            assert_eq!(task_ids, vec!["task-123"]);
            assert!(reason.unwrap().contains("rolling back"));
        } else {
            panic!("Expected Released event");
        }
    }

    #[test]
    fn rollback_with_custom_reason() {
        let result = rollback_claim_if_reserved(
            "task-123",
            TaskStatus::Reserved,
            Some("Spawn failed: connection refused"),
        );
        assert!(
            result.is_some(),
            "Should return Released event when Reserved"
        );
        if let Some(TaskEvent::Released { reason, .. }) = result {
            assert_eq!(reason.unwrap(), "Spawn failed: connection refused");
        } else {
            panic!("Expected Released event");
        }
    }

    #[test]
    fn no_rollback_when_in_progress() {
        let result = rollback_claim_if_reserved("task-123", TaskStatus::InProgress, None);
        assert!(
            result.is_none(),
            "Should not rollback when agent already started"
        );
    }

    #[test]
    fn no_rollback_when_closed() {
        let result = rollback_claim_if_reserved("task-123", TaskStatus::Closed, None);
        assert!(result.is_none(), "Should not rollback when task is closed");
    }

    #[test]
    fn no_rollback_when_open() {
        let result = rollback_claim_if_reserved("task-123", TaskStatus::Open, None);
        assert!(result.is_none(), "Should not rollback when task is Open");
    }

    #[test]
    fn no_rollback_when_task_absent_from_graph() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        try_rollback_reserved(dir.path(), "nonexistent-task", "test reason");
        // Verify no events were written
        let events = read_events(dir.path()).unwrap_or_default();
        assert!(
            events.is_empty(),
            "Should not write events when task is absent from graph"
        );
    }
}
