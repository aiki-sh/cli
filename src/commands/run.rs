//! Top-level `aiki run` command
//!
//! Spawns an agent session for a task and returns the session UUID.

use std::path::Path;

use crate::agents::runtime::{discover_session_id, BackgroundHandle};
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

/// Read a task's current status, or `None` if it can't be determined (event
/// read failed or the task is absent).
fn task_status(cwd: &Path, task_id: &str) -> Option<TaskStatus> {
    read_events(cwd)
        .ok()
        .and_then(|events| materialize_graph(&events).tasks.get(task_id).map(|t| t.status))
}

/// Whether a discovery-failed spawn left a process that should be reaped.
///
/// Only a still-`Reserved` task means the agent never registered its session
/// (it hung or died at startup). Any progressed status (`InProgress`, `Closed`)
/// means a live agent owns the work, so the discovery error was transient and
/// the process must NOT be killed. An unreadable status (`None`) is treated as
/// "do not kill" — we cannot confirm the process is an orphan.
fn should_reap_process(status: Option<TaskStatus>) -> bool {
    status == Some(TaskStatus::Reserved)
}

/// Best-effort termination of an orphaned agent process: `SIGTERM`, a short
/// grace period, then `SIGKILL`. `ESRCH` (already exited) is harmless. No-op on
/// non-Unix targets.
#[cfg(unix)]
fn terminate_orphan(pid: u32) {
    use std::time::Duration;
    // SAFETY: kill() with a plain signal is always safe to call; an invalid or
    // already-exited pid simply returns ESRCH.
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    std::thread::sleep(Duration::from_millis(500));
    unsafe { libc::kill(pid as i32, libc::SIGKILL) };
}

#[cfg(not(unix))]
fn terminate_orphan(_pid: u32) {}

/// Reap a spawn whose session never registered: terminate the orphaned process
/// (only when the task is still `Reserved`, to avoid killing a healthy agent
/// after a transient discovery error) and release the reservation so the task
/// returns to the ready queue instead of being stranded in `reserved`.
fn reap_orphan_if_reserved(cwd: &Path, task_id: &str, handle: &BackgroundHandle) {
    if let Some(pid) = handle.pid {
        if should_reap_process(task_status(cwd, task_id)) {
            terminate_orphan(pid);
        }
    }
    // Release the claim (no-op unless still Reserved; conservatively releases
    // when status is unreadable, matching the spawn-failure rollback).
    try_rollback_reserved(
        cwd,
        task_id,
        "Agent session never registered within timeout; terminated orphan process and released reservation",
    );
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
        Err(e) => {
            // The agent never registered its session within the timeout: it hung
            // or died at startup (a healthy agent records `session.started`
            // within seconds). Reap the orphan — terminate the process and
            // release the reservation — so we don't leave a zombie plus a task
            // stranded in `reserved`. Without this, the only recovery is a manual
            // `aiki run --force` and a manual `kill`.
            reap_orphan_if_reserved(cwd, task_id, &handle);

            if output_id {
                // No session ID to output.
                if is_async {
                    return Ok(());
                }
                // Fall through to a blocking task-based run now that the orphan
                // is gone and the reservation released.
                return run_task_with_output(cwd, task_id, TaskRunOptions::new());
            }
            return Err(e);
        }
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

    #[test]
    fn reaps_process_only_when_still_reserved() {
        // A still-Reserved task means the agent never registered — reap it.
        assert!(should_reap_process(Some(TaskStatus::Reserved)));
        // A progressed task is owned by a live agent — never kill it.
        assert!(!should_reap_process(Some(TaskStatus::InProgress)));
        assert!(!should_reap_process(Some(TaskStatus::Closed)));
        assert!(!should_reap_process(Some(TaskStatus::Open)));
        // Unknown status can't be confirmed an orphan — don't kill.
        assert!(!should_reap_process(None));
    }

    #[test]
    fn reap_without_pid_on_bare_dir_is_a_noop_release() {
        use tempfile::tempdir;

        // No pid -> no kill; a bare dir (no task in graph) -> no Released event.
        // Exercises that reap is safe to call when the process/task can't be
        // resolved, without panicking.
        let dir = tempdir().unwrap();
        let task_id = "nonexistent-task";
        let handle = BackgroundHandle {
            thread: ThreadId::single(task_id.to_string()),
            session_id: None,
            agent_type: AgentType::ClaudeCode,
            pid: None,
        };
        reap_orphan_if_reserved(dir.path(), task_id, &handle);
        let events = read_events(dir.path()).unwrap_or_default();
        assert!(events.is_empty(), "absent task should produce no events");
    }
}
