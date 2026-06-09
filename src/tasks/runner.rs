//! Task execution runner
//!
//! This module provides functions for executing tasks via agent sessions,
//! including blocking and background (async) execution modes.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;

use crate::agents::{
    detect_agent_from_process_tree, get_runtime, AgentRuntime, AgentSessionResult,
    AgentSpawnOptions, AgentType, Assignee, BackgroundHandle,
};
use crate::error::{AikiError, Result};
use crate::session::find_active_session;
use crate::tasks::lanes::ThreadId;
use crate::tasks::{
    find_task, materialize_graph,
    md::MdBuilder,
    read_events,
    types::{Task, TaskEvent, TaskStatus},
    write_event, TaskGraph,
};
use crate::tui::app::{Effect, Model, Screen, WindowState};
use crate::tui::components::ChildLine;
use crate::tui::render::render_to_string_ex;
use crate::tui::theme;

/// Options for running a task
#[derive(Debug, Clone)]
pub struct TaskRunOptions {
    /// Override the task's assignee agent
    pub agent_override: Option<AgentType>,
    /// Suppress real-time status updates
    pub quiet: bool,
    /// Thread to run (overrides single-task default when set)
    pub thread: Option<ThreadId>,
}

impl Default for TaskRunOptions {
    fn default() -> Self {
        Self {
            agent_override: None,
            quiet: false,
            thread: None,
        }
    }
}

impl TaskRunOptions {
    /// Create new task run options
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set an agent override
    #[must_use]
    pub fn with_agent(mut self, agent: AgentType) -> Self {
        self.agent_override = Some(agent);
        self
    }

    /// Suppress informational output (spawning, completion messages)
    #[must_use]
    #[allow(dead_code)]
    pub fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    /// Set the thread for multi-task (needs-context) session execution
    #[must_use]
    pub fn with_thread(mut self, thread: ThreadId) -> Self {
        self.thread = Some(thread);
        self
    }
}

/// Resolve which agent type to use for running a task.
///
/// Resolution order:
/// 1. Explicit `--agent` override from options
/// 2. Task's assignee field (if set to an agent)
/// 3. Active session's agent type (cheap file lookup, authoritative)
/// 4. Process tree detection (expensive, fallback for non-session contexts)
fn resolve_agent_type(
    cwd: &Path,
    task_id: &str,
    task: &Task,
    options: &TaskRunOptions,
) -> Result<AgentType> {
    // 1. Explicit override
    if let Some(agent) = options.agent_override {
        return Ok(agent);
    }

    // 2. Task assignee
    if let Some(ref assignee_str) = task.assignee {
        match Assignee::from_str(assignee_str) {
            Some(Assignee::Agent(agent)) => return Ok(agent),
            Some(Assignee::Human) => {
                return Err(AikiError::TaskNoAssignee(format!(
                    "Task '{}' is assigned to human, use --agent to specify an agent",
                    task_id
                )));
            }
            Some(Assignee::Unassigned) | None => {}
        }
    }

    // 3. Active session (cheap file lookup, authoritative for aiki-managed sessions)
    if let Some(session) = find_active_session(cwd) {
        return Ok(session.agent_type);
    }

    // 4. Process tree detection (fallback for non-session contexts)
    if let Some(agent) = detect_agent_from_process_tree() {
        return Ok(agent);
    }

    Err(AikiError::TaskNoAssignee(task_id.to_string()))
}

// ---------------------------------------------------------------------------
// Extracted helpers for task_run / task_run_on_session
// ---------------------------------------------------------------------------

/// Validated and prepared state for running a task.
pub(crate) struct PreparedTaskRun {
    pub(crate) task_id: String,
    pub(crate) agent_type: AgentType,
    pub(crate) runtime: Box<dyn AgentRuntime>,
    pub(crate) spawn_options: AgentSpawnOptions,
}

/// Roll back a task claim if spawn failed and the task is still in Reserved status.
///
/// Re-reads the event log to check the current status rather than relying on a
/// stale boolean captured at prepare time. This avoids emitting spurious Released
/// events when the task has already transitioned to InProgress.
///
/// If reading the event log fails, assumes the task may still be Reserved and
/// emits a Released event as a safe default (a spurious Released on a
/// non-Reserved task is a harmless no-op).
pub(crate) fn rollback_if_still_reserved(cwd: &Path, task_id: &str, error: &crate::error::AikiError) {
    let reason = format!("Spawn failed: {}", error);
    crate::commands::run::try_rollback_reserved(cwd, task_id, &reason);
}

/// Loading phases shown during task run preparation.
///
/// Maps to spec states 1.0a–1.0d in screen-states.md.
pub enum LoadingPhase {
    /// 1.0a: Reading JJ event log.
    ReadingGraph,
    /// 1.0b: Resolving which agent to use.
    ResolvingAgent,
    /// 1.0c: Emitting Started event and building spawn options.
    CreatingWorkspace { agent: String },
    /// 1.0d: About to spawn the agent process.
    StartingSession { agent: String },
}

impl LoadingPhase {
    /// Status text shown as the child line beneath the spinner.
    pub fn status_text(&self) -> &str {
        match self {
            Self::ReadingGraph => "Reading task graph...",
            Self::ResolvingAgent => "Resolving agent...",
            Self::CreatingWorkspace { .. } => "creating isolated workspace...",
            Self::StartingSession { .. } => "starting session...",
        }
    }

    /// Agent label (shown after phase name once known).
    pub fn agent_label(&self) -> Option<&str> {
        match self {
            Self::ReadingGraph | Self::ResolvingAgent => None,
            Self::CreatingWorkspace { agent } | Self::StartingSession { agent } => Some(agent),
        }
    }

    /// Tick index for spinner frame progression (one frame per phase).
    pub fn tick(&self) -> u64 {
        match self {
            Self::ReadingGraph => 0,
            Self::ResolvingAgent => 1,
            Self::CreatingWorkspace { .. } => 2,
            Self::StartingSession { .. } => 3,
        }
    }
}

/// Validate that all non-closed tasks in a thread have instructions.
///
/// Walks the needs-context chain for the thread head. If the chain is empty or
/// doesn't include the head, falls back to `[head]`. Returns an error listing
/// every non-closed task that is missing instructions.
fn validate_thread_instructions(graph: &TaskGraph, thread: &ThreadId) -> Result<()> {
    let full_chain = graph.get_needs_context_chain(&thread.head);

    // Slice the chain to only include tasks between thread.head and thread.tail.
    let chain = if let Some(head_idx) = full_chain.iter().position(|id| id == &thread.head) {
        let tail_idx = full_chain
            .iter()
            .position(|id| id == &thread.tail)
            .unwrap_or(head_idx);
        full_chain[head_idx..=tail_idx].to_vec()
    } else {
        vec![thread.head.clone()]
    };

    let mut missing: Vec<(String, String)> = Vec::new();
    for id in &chain {
        if let Some(task) = graph.tasks.get(id) {
            if task.status == TaskStatus::Closed {
                continue;
            }
            let has_instructions = task
                .instructions
                .as_ref()
                .is_some_and(|s| !s.trim().is_empty());
            if !has_instructions {
                missing.push((id.clone(), task.name.clone()));
            }
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(AikiError::ThreadMissingInstructions { missing })
    }
}

/// Validate a task, resolve the agent, emit a Started event, and build spawn options.
///
/// Calls `on_phase` at each preparation step so callers can show loading progress.
pub(crate) fn prepare_task_run(
    cwd: &Path,
    task_id: &str,
    options: &TaskRunOptions,
    mut on_phase: impl FnMut(&LoadingPhase),
) -> Result<PreparedTaskRun> {
    let thread = options.thread.clone();

    // Phase 1.0a: Reading task graph
    on_phase(&LoadingPhase::ReadingGraph);
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);

    // Find the task
    let task = find_task(&graph.tasks, task_id)?;
    let task_id = &task.id; // rebind to canonical ID

    // Validate task can be run
    if task.status == TaskStatus::Closed {
        return Err(AikiError::TaskAlreadyClosed(task_id.to_string()));
    }

    // Validate all non-closed tasks in the thread have instructions
    let spawn_thread = thread.unwrap_or_else(|| ThreadId::single(task_id.to_string()));
    validate_thread_instructions(&graph, &spawn_thread)?;

    // Phase 1.0b: Resolving agent
    on_phase(&LoadingPhase::ResolvingAgent);
    let agent_type = resolve_agent_type(cwd, task_id, &task, options)?;

    // Verify the agent CLI is actually installed
    if !agent_type.is_installed() {
        return Err(AikiError::AgentNotInstalled {
            agent: agent_type.as_str().to_string(),
            hint: agent_type.install_hint().to_string(),
        });
    }

    // Get runtime for the agent
    let runtime = get_runtime(agent_type).ok_or_else(|| AikiError::AgentNotInstalled {
        agent: agent_type.as_str().to_string(),
        hint: agent_type.install_hint().to_string(),
    })?;

    // Phase 1.0c: Creating workspace
    let agent_name = agent_type.display_name().to_string();
    on_phase(&LoadingPhase::CreatingWorkspace {
        agent: agent_name.clone(),
    });

    // Emit Reserved event before spawning to lock the task (Open → Reserved).
    // The agent's hook (via `aiki task start`) emits Started with session_id,
    // transitioning Reserved → InProgress.
    if task.status == TaskStatus::Open {
        let reserve = TaskEvent::Reserved {
            task_ids: vec![task_id.to_string()],
            agent_type: agent_type.as_str().to_string(),
            timestamp: chrono::Utc::now(),
        };
        write_event(cwd, &reserve)?;
    }

    // Build spawn options with parent session UUID for workspace isolation chaining
    let parent_uuid = find_active_session(cwd).map(|s| s.session_id);
    let spawn_options =
        AgentSpawnOptions::new(cwd, spawn_thread)
            .with_parent_session_uuid(parent_uuid)
            .with_event_pipe(crate::tasks::storage::event_pipe_path());

    // Phase 1.0d: Starting session
    on_phase(&LoadingPhase::StartingSession { agent: agent_name });

    Ok(PreparedTaskRun {
        task_id: task_id.to_string(),
        agent_type,
        runtime,
        spawn_options,
    })
}

/// Convert a TUI `Effect` into an `AgentSessionResult`.
///
/// Reads the final task state from JJ to determine the appropriate result.
fn map_tui_effect(cwd: &Path, task_id: &str, effect: Effect) -> Result<AgentSessionResult> {
    match effect {
        Effect::Detached => Ok(AgentSessionResult::detached()),
        Effect::Done => {
            let events = read_events(cwd)?;
            let tasks = materialize_graph(&events).tasks;

            if let Some(task) = tasks.get(task_id) {
                match task.status {
                    TaskStatus::Closed => {
                        let summary = task.effective_summary().unwrap_or_default().to_string();
                        Ok(AgentSessionResult::Completed { summary })
                    }
                    TaskStatus::Stopped => {
                        let reason = task
                            .stopped_reason
                            .clone()
                            .unwrap_or_else(|| "Task stopped".to_string());
                        Ok(AgentSessionResult::Stopped { reason })
                    }
                    _ => Ok(AgentSessionResult::Failed {
                        error: "Agent process exited without completing task".to_string(),
                    }),
                }
            } else {
                Ok(AgentSessionResult::Failed {
                    error: "Task not found after completion".to_string(),
                })
            }
        }
        Effect::Continue => unreachable!("tui::app::run() should not return Continue"),
    }
}

/// Run a task by spawning an agent session
///
/// This function:
/// 1. Loads the task from the aiki/tasks branch
/// 2. Validates the task can be run (not closed)
/// 3. Determines which agent to use (from options or task assignee)
/// 4. Spawns the agent session with task context
/// 5. Shows real-time TUI status updates while waiting (if TTY)
/// 6. Handles the result and updates task state
pub fn task_run(cwd: &Path, task_id: &str, options: TaskRunOptions) -> Result<()> {
    let quiet = options.quiet;
    let show_tui = std::io::stdout().is_terminal() && !quiet;

    // Show loading spinner during prepare (TTY only)
    let (width, _) = if show_tui {
        crossterm::terminal::size().unwrap_or((80, 24))
    } else {
        (80, 24)
    };
    let tui_theme = if show_tui {
        Some(theme::Theme::from_mode(theme::detect_mode()))
    } else {
        None
    };
    let mut loading_lines_printed = 0u16;

    let prepared = prepare_task_run(cwd, task_id, &options, |phase| {
        if let Some(ref theme) = tui_theme {
            let mut lines = crate::tui::components::phase(
                0,
                "task",
                phase.agent_label(),
                true,
                vec![ChildLine::active(phase.status_text())],
            );
            let ansi = render_to_string_ex(&mut lines, theme, width, phase.tick());
            // Overwrite previous loading output
            if loading_lines_printed > 0 {
                for _ in 0..loading_lines_printed {
                    eprint!("\x1b[A\x1b[2K"); // stderr-ok: loading spinner
                }
            }
            let line_count = ansi.matches('\n').count() as u16 + 1;
            eprint!("{}", ansi); // stderr-ok: loading spinner
            eprintln!(); // stderr-ok: trailing newline
            loading_lines_printed = line_count;
        }
    });

    // Clear loading output before TUI takes over (or on error)
    if loading_lines_printed > 0 {
        for _ in 0..loading_lines_printed {
            eprint!("\x1b[A\x1b[2K"); // stderr-ok: clear loading
        }
    }

    let prepared = prepared?;
    let task_id = &prepared.task_id;

    // Print status for non-TTY path
    if !show_tui && !quiet {
        eprintln!(
            // stderr-ok: non-TTY path
            "Spawning {} agent session for task {}...",
            prepared.agent_type.display_name(),
            task_id
        );
    }

    // Spawn agent session and optionally monitor with TUI
    let result = if show_tui {
        // Spawn agent in background, then run TUI to monitor via JJ events
        let _handle = match prepared.runtime.spawn_background(&prepared.spawn_options) {
            Ok(handle) => handle,
            Err(e) => {
                rollback_if_still_reserved(cwd, task_id, &e);
                return Err(e);
            }
        };

        let events = read_events(cwd)?;
        let graph = materialize_graph(&events);
        let model = Model {
            graph: Arc::new(graph),
            screen: Screen::TaskRun {
                task_id: task_id.to_string(),
            },
            window: WindowState::new(width),
            entries: Vec::new(),
            finished: false,
            detached: false,
        };

        let effect = crate::tui::app::run(model, cwd)?;
        map_tui_effect(cwd, task_id, effect)?
    } else {
        match prepared.runtime.spawn_blocking(&prepared.spawn_options) {
            Ok(result) => result,
            Err(e) => {
                rollback_if_still_reserved(cwd, task_id, &e);
                return Err(e);
            }
        }
    };

    handle_session_result(cwd, task_id, result, quiet)?;

    Ok(())
}

/// Run a task with optional TUI monitoring.
///
/// Like `task_run` but:
/// - Does NOT print "Spawning..." or "Task run complete" messages
/// - Returns `AgentSessionResult` instead of `()`
/// - When `show_tui` is true, runs the Elm TUI event loop for monitoring
/// - When `show_tui` is false, blocks until the agent exits
pub fn task_run_on_session(
    cwd: &Path,
    task_id: &str,
    options: TaskRunOptions,
    show_tui: bool,
) -> Result<AgentSessionResult> {
    let prepared = prepare_task_run(cwd, task_id, &options, |_| {})?;
    let task_id = &prepared.task_id;

    if show_tui {
        // Spawn agent in background, then run TUI to monitor via JJ events
        let _handle = match prepared.runtime.spawn_background(&prepared.spawn_options) {
            Ok(handle) => handle,
            Err(e) => {
                rollback_if_still_reserved(cwd, task_id, &e);
                return Err(e);
            }
        };

        let events = read_events(cwd)?;
        let graph = materialize_graph(&events);
        let (width, _) = crossterm::terminal::size().unwrap_or((80, 24));
        let model = Model {
            graph: Arc::new(graph),
            screen: Screen::TaskRun {
                task_id: task_id.to_string(),
            },
            window: WindowState::new(width),
            entries: Vec::new(),
            finished: false,
            detached: false,
        };

        let effect = crate::tui::app::run(model, cwd)?;
        map_tui_effect(cwd, task_id, effect)
    } else {
        // Non-TUI: block until the agent exits
        match prepared.runtime.spawn_blocking(&prepared.spawn_options) {
            Ok(result) => Ok(result),
            Err(e) => {
                rollback_if_still_reserved(cwd, task_id, &e);
                Err(e)
            }
        }
    }
}

/// Handle an `AgentSessionResult` with the same semantics as `task_run()`:
/// emit stop events, cascade-close orchestrator subtasks, and propagate errors.
///
/// Set `quiet` to suppress "Task run complete" and summary messages (useful for
/// callers operating inside a shared screen session).
pub fn handle_session_result(
    cwd: &Path,
    task_id: &str,
    result: AgentSessionResult,
    quiet: bool,
) -> Result<()> {
    match &result {
        AgentSessionResult::Completed { summary } => {
            // Agent completed successfully - it should have closed the task itself
            if !quiet {
                eprintln!("Task run complete"); // stderr-ok: post-TUI
                if !summary.is_empty() {
                    eprintln!("Summary: {}", summary); // stderr-ok: post-TUI
                }
            }
        }
        AgentSessionResult::Stopped { reason } => {
            // Agent stopped - emit Stopped event if task is not already closed
            let refreshed_events = read_events(cwd)?;
            let mut refreshed_graph = materialize_graph(&refreshed_events);
            if let Ok(refreshed_task) = find_task(&refreshed_graph.tasks, task_id) {
                if refreshed_task.status != TaskStatus::Closed {
                    let is_orchestrator = refreshed_task.is_orchestrator();
                    let stop_event = TaskEvent::Stopped {
                        task_ids: vec![task_id.to_string()],
                        reason: Some(reason.clone()),
                        session_id: None,
                        turn_id: None,
                        timestamp: chrono::Utc::now(),
                    };
                    write_event(cwd, &stop_event)?;

                    // Cascade-close subtasks if this is an orchestrator task
                    if is_orchestrator {
                        use crate::commands::task::cascade_close_tasks;
                        use crate::tasks::manager::get_all_unclosed_descendants;
                        let unclosed = get_all_unclosed_descendants(&refreshed_graph, task_id);
                        if !unclosed.is_empty() {
                            let cascade_ids: Vec<String> =
                                unclosed.iter().map(|t| t.id.clone()).collect();
                            cascade_close_tasks(
                                cwd,
                                &mut refreshed_graph.tasks,
                                &cascade_ids,
                                crate::tasks::types::TaskOutcome::WontDo,
                                "Parent orchestrator stopped",
                            )?;
                        }
                    }
                }
            }
            eprintln!("Task {} stopped: {}", task_id, reason); // stderr-ok: post-TUI
        }
        AgentSessionResult::Detached => {
            // User detached via Ctrl+C - agent continues running in background
            // Do NOT emit TaskEvent::Stopped since the agent is still working
            eprintln!(
                // stderr-ok: post-TUI
                "Detached. Task {} still running. Use `aiki task show {}` to check status.",
                &task_id[..8.min(task_id.len())],
                task_id
            );
        }
        AgentSessionResult::Failed { error } => {
            // Agent process exited abnormally. Check actual task state before
            // declaring failure — the agent may have completed the work but
            // exited with a signal (common with codex exec).
            let refreshed_events = read_events(cwd)?;
            let mut refreshed_graph = materialize_graph(&refreshed_events);
            if let Ok(refreshed_task) = find_task(&refreshed_graph.tasks, task_id) {
                if refreshed_task.status == TaskStatus::Closed {
                    // Task completed despite unclean exit — trust the task state
                    return Ok(());
                }

                let is_orchestrator = refreshed_task.is_orchestrator();
                let stop_event = TaskEvent::Stopped {
                    task_ids: vec![task_id.to_string()],
                    reason: Some(format!("Session failed: {}", error)),
                    session_id: None,
                    turn_id: None,
                    timestamp: chrono::Utc::now(),
                };
                write_event(cwd, &stop_event)?;

                // Cascade-close subtasks if this is an orchestrator task
                if is_orchestrator {
                    use crate::commands::task::cascade_close_tasks;
                    use crate::tasks::manager::get_all_unclosed_descendants;
                    let unclosed = get_all_unclosed_descendants(&refreshed_graph, task_id);
                    if !unclosed.is_empty() {
                        let cascade_ids: Vec<String> =
                            unclosed.iter().map(|t| t.id.clone()).collect();
                        cascade_close_tasks(
                            cwd,
                            &mut refreshed_graph.tasks,
                            &cascade_ids,
                            crate::tasks::types::TaskOutcome::WontDo,
                            "Parent orchestrator failed",
                        )?;
                    }
                }
            }
            return Err(AikiError::AgentSpawnFailed(error.clone()));
        }
    }

    Ok(())
}

/// Derive AgentSessionResult from final task state and delegate to
/// handle_session_result(). Must be called after the drain loop exits
/// (i.e., after the agent process has been reaped by try_wait()).
///
/// This is the non-TUI equivalent of map_tui_effect(Effect::Done) +
/// handle_session_result() — it reads the task's final state from JJ
/// to determine success/stopped/failed, then runs the same cleanup
/// logic (Stopped event emission, cascade-close) that the TUI path uses.
pub fn finalize_agent_run(cwd: &Path, task_id: &str) -> Result<()> {
    let events = read_events(cwd)?;
    let tasks = materialize_graph(&events).tasks;

    let result = if let Some(task) = tasks.get(task_id) {
        match task.status {
            TaskStatus::Closed => {
                let summary = task.effective_summary().unwrap_or_default().to_string();
                AgentSessionResult::Completed { summary }
            }
            TaskStatus::Stopped => {
                let reason = task
                    .stopped_reason
                    .clone()
                    .unwrap_or_else(|| "Task stopped".to_string());
                AgentSessionResult::Stopped { reason }
            }
            _ => AgentSessionResult::Failed {
                error: "Agent process exited without completing task".to_string(),
            },
        }
    } else {
        AgentSessionResult::Failed {
            error: "Task not found after completion".to_string(),
        }
    };

    // quiet=true: the workflow step handles its own output via ctx.emit(),
    // so suppress handle_session_result()'s stderr printing.
    handle_session_result(cwd, task_id, result, /* quiet */ true)
}

/// Run a task and output result
///
/// Wrapper around `task_run` that outputs formatted results.
pub fn run_task_with_output(cwd: &Path, task_id: &str, options: TaskRunOptions) -> Result<()> {
    match task_run(cwd, task_id, options) {
        Ok(()) => {
            let md =
                MdBuilder::new().build(&format!("## Run Completed\n- **Task:** {}\n", task_id));
            println!("{}", md);
            Ok(())
        }
        Err(e) => {
            let md = MdBuilder::new().build_error(&e.to_string());
            println!("{}", md);
            Err(e)
        }
    }
}

/// Spawn a task in the background and return immediately
///
/// This function:
/// 1. Validates the task can be run
/// 2. Determines which agent to use
/// 3. Spawns the agent process in the background (with AIKI_THREAD env var)
/// 4. Returns the background handle
///
/// The spawned agent creates a session with task field, which allows
/// `terminate_background_task` to find and terminate it later.
pub fn task_run_async(
    cwd: &Path,
    task_id: &str,
    options: TaskRunOptions,
) -> Result<BackgroundHandle> {
    // Load task from events
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);

    // Find the task
    let task = find_task(&graph.tasks, task_id)?;
    let task_id = &task.id; // rebind to canonical ID

    // Validate task can be run
    if task.status == TaskStatus::Closed {
        return Err(AikiError::TaskAlreadyClosed(task_id.to_string()));
    }

    // Validate all non-closed tasks in the thread have instructions
    let spawn_thread = options
        .thread
        .clone()
        .unwrap_or_else(|| ThreadId::single(task_id.to_string()));
    validate_thread_instructions(&graph, &spawn_thread)?;

    // Determine which agent to use
    let agent_type = resolve_agent_type(cwd, task_id, &task, &options)?;

    // Verify the agent CLI is actually installed
    if !agent_type.is_installed() {
        return Err(AikiError::AgentNotInstalled {
            agent: agent_type.as_str().to_string(),
            hint: agent_type.install_hint().to_string(),
        });
    }

    // Get runtime for the agent
    let runtime = get_runtime(agent_type).ok_or_else(|| AikiError::AgentNotInstalled {
        agent: agent_type.as_str().to_string(),
        hint: agent_type.install_hint().to_string(),
    })?;

    // Emit Reserved event before spawning to lock the task (Open → Reserved).
    // The agent's hook (via `aiki task start`) emits Started with session_id,
    // transitioning Reserved → InProgress.
    if task.status == TaskStatus::Open {
        let reserve = TaskEvent::Reserved {
            task_ids: vec![task_id.to_string()],
            agent_type: agent_type.as_str().to_string(),
            timestamp: chrono::Utc::now(),
        };
        write_event(cwd, &reserve)?;
    }

    // Build spawn options with parent session UUID for workspace isolation chaining
    let parent_uuid = find_active_session(cwd).map(|s| s.session_id);
    let spawn_options =
        AgentSpawnOptions::new(cwd, spawn_thread)
            .with_parent_session_uuid(parent_uuid)
            .with_event_pipe(crate::tasks::storage::event_pipe_path());

    // Spawn agent session in background
    // The agent inherits AIKI_THREAD env var which gets recorded in its session file
    // This allows terminate_background_task to find and kill it later
    let handle = match runtime.spawn_background(&spawn_options) {
        Ok(h) => h,
        Err(e) => {
            // Compensate: emit Released so the task doesn't get stuck in Reserved
            rollback_if_still_reserved(cwd, task_id, &e);
            return Err(e);
        }
    };

    Ok(handle)
}

/// Run a task asynchronously and output XML result
///
/// Wrapper around `task_run_async` that outputs formatted results.
#[allow(dead_code)]
pub fn run_task_async_with_output(
    cwd: &Path,
    task_id: &str,
    options: TaskRunOptions,
) -> Result<()> {
    match task_run_async(cwd, task_id, options) {
        Ok(handle) => {
            let md = MdBuilder::new().build(&format!(
                "## Run Started\n- **Task:** {}\n- Task started asynchronously.\n",
                handle.thread
            ));
            println!("{}", md);
            Ok(())
        }
        Err(e) => {
            let md = MdBuilder::new().build_error(&e.to_string());
            println!("{}", md);
            Err(e)
        }
    }
}

/// Result of resolving the next ready subtask
pub enum SubtaskResolution<'a> {
    /// A ready subtask was found
    Ready(&'a Task),
    /// All subtasks are closed — nothing left to do
    AllComplete,
    /// Subtasks exist but none are ready (all blocked or in-progress)
    Blocked(Vec<&'a Task>),
    /// Parent task has no subtasks
    NoSubtasks,
}

/// Resolve the next ready subtask of a parent task.
///
/// Returns the first ready (open + unblocked) subtask sorted by priority then
/// creation time.
///
/// Returns:
/// - `Ready(task)` if a ready subtask is found
/// - `AllComplete` if all subtasks are closed
/// - `Blocked(unclosed)` if subtasks exist but none are ready
/// - `NoSubtasks` if the parent has no subtasks
pub fn resolve_next_subtask<'a>(graph: &'a TaskGraph, parent_id: &str) -> SubtaskResolution<'a> {
    use crate::tasks::manager::get_subtasks;

    let subtasks: Vec<&Task> = get_subtasks(graph, parent_id);

    if subtasks.is_empty() {
        return SubtaskResolution::NoSubtasks;
    }

    // Filter to ready subtasks (open + unblocked)
    let mut ready: Vec<&Task> = subtasks
        .iter()
        .copied()
        .filter(|t| t.status == TaskStatus::Open)
        .filter(|t| !graph.is_blocked(&t.id))
        .collect();

    // Sort by priority (P0 first), then by creation time (oldest first)
    ready.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    if let Some(first) = ready.first() {
        return SubtaskResolution::Ready(first);
    }

    // No ready subtasks — check if all are closed vs some are blocked/in-progress
    let unclosed: Vec<&Task> = subtasks
        .into_iter()
        .filter(|t| t.status != TaskStatus::Closed)
        .collect();

    if unclosed.is_empty() {
        SubtaskResolution::AllComplete
    } else {
        SubtaskResolution::Blocked(unclosed)
    }
}

/// Result of resolving the next session (needs-context aware)
pub enum ThreadResolution<'a> {
    /// A single standalone task (no needs-context chain)
    Standalone(&'a Task),
    /// A needs-context chain (ordered task IDs from head to tail)
    Chain(Vec<String>),
    /// All subtasks are closed — nothing left to do
    AllComplete,
    /// Subtasks exist but none are ready (all blocked or in-progress)
    Blocked(Vec<&'a Task>),
    /// Parent task has no subtasks
    NoSubtasks,
}

/// Resolve the next session to run for a parent task.
///
/// Like `resolve_next_subtask`, but needs-context aware: if the next ready
/// subtask is the head of a `needs-context` chain, returns `Chain` with the
/// full ordered list of chain task IDs. For standalone tasks, returns
/// `Standalone`.
pub fn resolve_next_thread<'a>(graph: &'a TaskGraph, parent_id: &str) -> ThreadResolution<'a> {
    match resolve_next_subtask(graph, parent_id) {
        SubtaskResolution::Ready(task) => {
            if graph.is_needs_context_head(&task.id) {
                let chain = graph.get_needs_context_chain(&task.id);
                ThreadResolution::Chain(chain)
            } else {
                ThreadResolution::Standalone(task)
            }
        }
        SubtaskResolution::AllComplete => ThreadResolution::AllComplete,
        SubtaskResolution::Blocked(unclosed) => ThreadResolution::Blocked(unclosed),
        SubtaskResolution::NoSubtasks => ThreadResolution::NoSubtasks,
    }
}

/// Resolve the next session to run within a specific lane.
///
/// Like `resolve_next_thread`, but restricted to subtasks within the
/// lane identified by `lane_prefix` (head task ID prefix matching).
pub fn resolve_next_thread_in_lane<'a>(
    graph: &'a TaskGraph,
    parent_id: &str,
    lane_prefix: &str,
) -> crate::error::Result<ThreadResolution<'a>> {
    use crate::tasks::lanes::{derive_lanes, get_lane_task_ids, resolve_lane_prefix};
    use crate::tasks::manager::get_subtasks;
    use crate::tasks::md::short_id;

    let decomp = derive_lanes(graph, parent_id);

    // Resolve the lane prefix to a full lane head ID
    let lane_head = resolve_lane_prefix(&decomp, lane_prefix, short_id(parent_id))
        .map_err(|msg| crate::error::AikiError::InvalidArgument(msg))?;

    // Get task IDs in the lane
    let lane_task_ids = get_lane_task_ids(&decomp, &lane_head).ok_or_else(|| {
        crate::error::AikiError::InvalidArgument(format!("Lane '{}' not found", lane_head))
    })?;

    // Get subtasks filtered to this lane
    let subtasks: Vec<&Task> = get_subtasks(graph, parent_id)
        .into_iter()
        .filter(|t| lane_task_ids.contains(&t.id))
        .collect();

    if subtasks.is_empty() {
        return Ok(ThreadResolution::NoSubtasks);
    }

    // Filter to ready subtasks within the lane (open + unblocked)
    let mut ready: Vec<&Task> = subtasks
        .iter()
        .copied()
        .filter(|t| t.status == TaskStatus::Open)
        .filter(|t| !graph.is_blocked(&t.id))
        .collect();

    ready.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    if let Some(first) = ready.first() {
        if graph.is_needs_context_head(&first.id) {
            let chain = graph.get_needs_context_chain(&first.id);
            return Ok(ThreadResolution::Chain(chain));
        } else {
            return Ok(ThreadResolution::Standalone(first));
        }
    }

    // No ready subtasks in this lane
    let unclosed: Vec<&Task> = subtasks
        .into_iter()
        .filter(|t| t.status != TaskStatus::Closed)
        .collect();

    if unclosed.is_empty() {
        Ok(ThreadResolution::AllComplete)
    } else {
        Ok(ThreadResolution::Blocked(unclosed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_run_options_default() {
        let options = TaskRunOptions::default();
        assert!(options.agent_override.is_none());
        assert!(!options.quiet);
    }

    #[test]
    fn test_task_run_options_with_agent() {
        let options = TaskRunOptions::new().with_agent(AgentType::ClaudeCode);
        assert_eq!(options.agent_override, Some(AgentType::ClaudeCode));
    }

    #[test]
    fn test_task_run_options_with_thread() {
        let thread = ThreadId {
            head: "A".to_string(),
            tail: "C".to_string(),
        };
        let options = TaskRunOptions::new().with_thread(thread.clone());
        assert_eq!(options.thread, Some(thread));
    }

    // --- resolve_next_thread tests ---
    // These require building a TaskGraph with subtask-of links

    use crate::tasks::graph::materialize_graph;
    use crate::tasks::types::{TaskEvent, TaskPriority};
    use chrono::Utc;
    use std::collections::HashMap;

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

    fn make_closed(id: &str) -> TaskEvent {
        TaskEvent::Closed {
            session_id: None,
            task_ids: vec![id.to_string()],
            outcome: crate::tasks::types::TaskOutcome::Done,
            confidence: None,
            summary: None,
            turn_id: None,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_resolve_next_thread_standalone() {
        // Parent P with subtask A (no needs-context) → Standalone
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Task A"),
            make_link("A", "P", "subtask-of"),
        ];
        let graph = materialize_graph(&events);
        match resolve_next_thread(&graph, "P") {
            ThreadResolution::Standalone(task) => {
                assert_eq!(task.id, "A");
            }
            other => panic!(
                "Expected Standalone, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn test_resolve_next_thread_chain_head() {
        // Parent P with subtasks A→B→C (needs-context chain)
        // A is ready and is the chain head → Chain([A, B, C])
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Explore"),
            make_created("B", "Plan"),
            make_created("C", "Implement"),
            make_link("A", "P", "subtask-of"),
            make_link("B", "P", "subtask-of"),
            make_link("C", "P", "subtask-of"),
            make_link("B", "A", "needs-context"),
            make_link("C", "B", "needs-context"),
        ];
        let graph = materialize_graph(&events);
        match resolve_next_thread(&graph, "P") {
            ThreadResolution::Chain(chain) => {
                assert_eq!(chain, vec!["A", "B", "C"]);
            }
            other => panic!("Expected Chain, got {:?}", std::mem::discriminant(&other)),
        }
    }

    #[test]
    fn test_resolve_next_thread_all_complete() {
        // Parent P with all subtasks closed → AllComplete
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Task A"),
            make_link("A", "P", "subtask-of"),
            make_closed("A"),
        ];
        let graph = materialize_graph(&events);
        assert!(matches!(
            resolve_next_thread(&graph, "P"),
            ThreadResolution::AllComplete
        ));
    }

    #[test]
    fn test_resolve_next_thread_no_subtasks() {
        // Parent P with no subtasks → NoSubtasks
        let events = vec![make_created("P", "Parent")];
        let graph = materialize_graph(&events);
        assert!(matches!(
            resolve_next_thread(&graph, "P"),
            ThreadResolution::NoSubtasks
        ));
    }

    // --- stop_stale_subtasks tests ---

    #[test]
    fn test_resolve_next_thread_non_head_chain_member() {
        // Parent P with subtasks: A→B (needs-context chain) + C (standalone)
        // A is done, B is ready but is NOT a chain head → Standalone(B)
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Explore"),
            make_created("B", "Plan"),
            make_created("C", "Review"),
            make_link("A", "P", "subtask-of"),
            make_link("B", "P", "subtask-of"),
            make_link("C", "P", "subtask-of"),
            make_link("B", "A", "needs-context"),
            make_closed("A"),
        ];
        let graph = materialize_graph(&events);
        // B is now ready (A is done). B has a predecessor in needs-context, so it's NOT a head.
        // C is also ready (standalone). Both are open+unblocked.
        // resolve_next_subtask picks by priority then creation time.
        // B was created before C, so B is picked first.
        // B is not a chain head (it has a predecessor A), so → Standalone(B)
        match resolve_next_thread(&graph, "P") {
            ThreadResolution::Standalone(task) => {
                assert_eq!(task.id, "B");
            }
            other => panic!(
                "Expected Standalone(B), got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    // --- Reserved status tests for resolve_next_thread ---

    fn make_reserved(ids: &[&str]) -> TaskEvent {
        TaskEvent::Reserved {
            task_ids: ids.iter().map(|s| s.to_string()).collect(),
            agent_type: "claude-code".to_string(),
            timestamp: Utc::now(),
        }
    }

    fn make_released(ids: &[&str]) -> TaskEvent {
        TaskEvent::Released {
            task_ids: ids.iter().map(|s| s.to_string()).collect(),
            reason: None,
            timestamp: Utc::now(),
        }
    }

    fn make_started(id: &str) -> TaskEvent {
        TaskEvent::Started {
            task_ids: vec![id.to_string()],
            agent_type: "claude-code".to_string(),
            session_id: None,
            turn_id: None,
            working_copy: None,
            instructions: None,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_resolve_next_thread_skips_reserved_tasks() {
        // Parent P with subtasks A (Reserved) and B (Open).
        // resolve_next_thread should skip A and return B.
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Task A"),
            make_created("B", "Task B"),
            make_link("A", "P", "subtask-of"),
            make_link("B", "P", "subtask-of"),
            make_reserved(&["A"]),
        ];
        let graph = materialize_graph(&events);
        match resolve_next_thread(&graph, "P") {
            ThreadResolution::Standalone(task) => {
                assert_eq!(task.id, "B", "Should pick Open task B, not Reserved task A");
            }
            other => panic!(
                "Expected Standalone(B), got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn test_resolve_next_thread_all_reserved_returns_blocked() {
        // Parent P with only Reserved subtasks → Blocked (none are Open)
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Task A"),
            make_link("A", "P", "subtask-of"),
            make_reserved(&["A"]),
        ];
        let graph = materialize_graph(&events);
        assert!(matches!(
            resolve_next_thread(&graph, "P"),
            ThreadResolution::Blocked(_)
        ));
    }

    #[test]
    fn test_resolve_next_thread_released_task_becomes_ready() {
        // Parent P with subtask A that was Reserved then Released → back to Open → ready
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Task A"),
            make_link("A", "P", "subtask-of"),
            make_reserved(&["A"]),
            make_released(&["A"]),
        ];
        let graph = materialize_graph(&events);
        match resolve_next_thread(&graph, "P") {
            ThreadResolution::Standalone(task) => {
                assert_eq!(task.id, "A", "Released task should be ready again");
            }
            other => panic!(
                "Expected Standalone(A), got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn test_resolve_next_thread_started_reserved_task_is_in_progress() {
        // Parent P with subtask A: Reserved → Started (InProgress) → not in ready pool
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Task A"),
            make_created("B", "Task B"),
            make_link("A", "P", "subtask-of"),
            make_link("B", "P", "subtask-of"),
            make_reserved(&["A"]),
            make_started("A"),
        ];
        let graph = materialize_graph(&events);
        match resolve_next_thread(&graph, "P") {
            ThreadResolution::Standalone(task) => {
                assert_eq!(task.id, "B", "A is InProgress, should pick B");
            }
            other => panic!(
                "Expected Standalone(B), got {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    // Status-based rollback logic (try_rollback_reserved) is tested in
    // commands::run::tests — covers Reserved (with default and custom reason),
    // InProgress, Closed, Open, and absent-from-graph (None path).
    // rollback_if_still_reserved is a thin wrapper that delegates to
    // try_rollback_reserved in run.rs.

    // --- validate_thread_instructions tests ---

    use crate::tasks::lanes::ThreadId;

    fn make_created_with_instructions(id: &str, name: &str, instructions: Option<&str>) -> TaskEvent {
        TaskEvent::Created {
            task_id: id.to_string(),
            name: name.to_string(),
            slug: None,
            task_type: None,
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            instructions: instructions.map(|s| s.to_string()),
            data: HashMap::new(),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_validate_thread_single_task_with_instructions() {
        let events = vec![make_created_with_instructions("A", "Task A", Some("Do the thing"))];
        let graph = materialize_graph(&events);
        let thread = ThreadId { head: "A".to_string(), tail: "A".to_string() };
        assert!(validate_thread_instructions(&graph, &thread).is_ok());
    }

    #[test]
    fn test_validate_thread_single_task_missing_instructions() {
        let events = vec![make_created("A", "Task A")];
        let graph = materialize_graph(&events);
        let thread = ThreadId { head: "A".to_string(), tail: "A".to_string() };
        let err = validate_thread_instructions(&graph, &thread).unwrap_err();
        match err {
            AikiError::ThreadMissingInstructions { missing } => {
                assert_eq!(missing.len(), 1);
                assert_eq!(missing[0].0, "A");
            }
            other => panic!("Expected ThreadMissingInstructions, got: {other}"),
        }
    }

    #[test]
    fn test_validate_thread_single_task_empty_instructions() {
        let events = vec![make_created_with_instructions("A", "Task A", Some("   "))];
        let graph = materialize_graph(&events);
        let thread = ThreadId { head: "A".to_string(), tail: "A".to_string() };
        let err = validate_thread_instructions(&graph, &thread).unwrap_err();
        match err {
            AikiError::ThreadMissingInstructions { missing } => {
                assert_eq!(missing.len(), 1);
                assert_eq!(missing[0].0, "A");
            }
            other => panic!("Expected ThreadMissingInstructions, got: {other}"),
        }
    }

    #[test]
    fn test_validate_thread_multi_task_all_have_instructions() {
        let events = vec![
            make_created_with_instructions("A", "Task A", Some("Step 1")),
            make_created_with_instructions("B", "Task B", Some("Step 2")),
            make_created_with_instructions("C", "Task C", Some("Step 3")),
            make_link("B", "A", "needs-context"),
            make_link("C", "B", "needs-context"),
        ];
        let graph = materialize_graph(&events);
        let thread = ThreadId { head: "A".to_string(), tail: "C".to_string() };
        assert!(validate_thread_instructions(&graph, &thread).is_ok());
    }

    #[test]
    fn test_validate_thread_multi_task_one_missing() {
        let events = vec![
            make_created_with_instructions("A", "Task A", Some("Step 1")),
            make_created("B", "Task B"), // no instructions
            make_created_with_instructions("C", "Task C", Some("Step 3")),
            make_link("B", "A", "needs-context"),
            make_link("C", "B", "needs-context"),
        ];
        let graph = materialize_graph(&events);
        let thread = ThreadId { head: "A".to_string(), tail: "C".to_string() };
        let err = validate_thread_instructions(&graph, &thread).unwrap_err();
        match err {
            AikiError::ThreadMissingInstructions { missing } => {
                assert_eq!(missing.len(), 1);
                assert_eq!(missing[0].0, "B");
            }
            other => panic!("Expected ThreadMissingInstructions, got: {other}"),
        }
    }

    #[test]
    fn test_validate_thread_multi_task_all_missing() {
        let events = vec![
            make_created("A", "Task A"),
            make_created("B", "Task B"),
            make_created("C", "Task C"),
            make_link("B", "A", "needs-context"),
            make_link("C", "B", "needs-context"),
        ];
        let graph = materialize_graph(&events);
        let thread = ThreadId { head: "A".to_string(), tail: "C".to_string() };
        let err = validate_thread_instructions(&graph, &thread).unwrap_err();
        match err {
            AikiError::ThreadMissingInstructions { missing } => {
                assert_eq!(missing.len(), 3);
                let ids: Vec<&str> = missing.iter().map(|(id, _)| id.as_str()).collect();
                assert!(ids.contains(&"A"));
                assert!(ids.contains(&"B"));
                assert!(ids.contains(&"C"));
            }
            other => panic!("Expected ThreadMissingInstructions, got: {other}"),
        }
    }

    #[test]
    fn test_validate_thread_closed_task_missing_instructions_skipped() {
        let events = vec![
            make_created("A", "Task A"), // no instructions, but will be closed
            make_closed("A"),
        ];
        let graph = materialize_graph(&events);
        let thread = ThreadId { head: "A".to_string(), tail: "A".to_string() };
        assert!(validate_thread_instructions(&graph, &thread).is_ok());
    }

    #[test]
    fn test_validate_thread_mix_closed_no_instructions_and_open_with_instructions() {
        let events = vec![
            make_created("A", "Task A"),                                     // no instructions, closed
            make_created_with_instructions("B", "Task B", Some("Step 2")),   // has instructions, open
            make_created("C", "Task C"),                                     // no instructions, closed
            make_link("B", "A", "needs-context"),
            make_link("C", "B", "needs-context"),
            make_closed("A"),
            make_closed("C"),
        ];
        let graph = materialize_graph(&events);
        let thread = ThreadId { head: "A".to_string(), tail: "C".to_string() };
        assert!(validate_thread_instructions(&graph, &thread).is_ok());
    }

    #[test]
    fn test_validate_thread_single_task_in_chain_ignores_siblings() {
        // Chain: A → B → C. Only B is in the thread. A and C lack instructions
        // but should not be validated.
        let events = vec![
            make_created("A", "Task A"),                                     // no instructions
            make_created_with_instructions("B", "Task B", Some("Step 2")),   // has instructions
            make_created("C", "Task C"),                                     // no instructions
            make_link("B", "A", "needs-context"),
            make_link("C", "B", "needs-context"),
        ];
        let graph = materialize_graph(&events);
        let thread = ThreadId { head: "B".to_string(), tail: "B".to_string() };
        assert!(validate_thread_instructions(&graph, &thread).is_ok());
    }

    #[test]
    fn test_validate_thread_subrange_of_chain() {
        // Chain: A → B → C → D → E. Thread covers B..D only.
        // A and E lack instructions but should not be validated.
        // C lacks instructions and IS in the thread, so it should fail.
        let events = vec![
            make_created("A", "Task A"),                                     // no instructions (outside thread)
            make_created_with_instructions("B", "Task B", Some("Step 2")),
            make_created("C", "Task C"),                                     // no instructions (inside thread)
            make_created_with_instructions("D", "Task D", Some("Step 4")),
            make_created("E", "Task E"),                                     // no instructions (outside thread)
            make_link("B", "A", "needs-context"),
            make_link("C", "B", "needs-context"),
            make_link("D", "C", "needs-context"),
            make_link("E", "D", "needs-context"),
        ];
        let graph = materialize_graph(&events);
        let thread = ThreadId { head: "B".to_string(), tail: "D".to_string() };
        let err = validate_thread_instructions(&graph, &thread).unwrap_err();
        match err {
            AikiError::ThreadMissingInstructions { missing } => {
                assert_eq!(missing.len(), 1);
                assert_eq!(missing[0].0, "C");
            }
            other => panic!("Expected ThreadMissingInstructions, got: {other}"),
        }
    }

    #[test]
    fn test_validate_thread_subrange_all_have_instructions() {
        // Chain: A → B → C → D → E. Thread covers B..D, all have instructions.
        let events = vec![
            make_created("A", "Task A"),                                     // no instructions (outside)
            make_created_with_instructions("B", "Task B", Some("Step 2")),
            make_created_with_instructions("C", "Task C", Some("Step 3")),
            make_created_with_instructions("D", "Task D", Some("Step 4")),
            make_created("E", "Task E"),                                     // no instructions (outside)
            make_link("B", "A", "needs-context"),
            make_link("C", "B", "needs-context"),
            make_link("D", "C", "needs-context"),
            make_link("E", "D", "needs-context"),
        ];
        let graph = materialize_graph(&events);
        let thread = ThreadId { head: "B".to_string(), tail: "D".to_string() };
        assert!(validate_thread_instructions(&graph, &thread).is_ok());
    }

    #[test]
    fn test_validate_thread_task_not_in_graph_skipped() {
        // Thread references a task ID that doesn't exist in the graph.
        // The validation should skip it (not panic) and pass.
        let events = vec![
            make_created_with_instructions("A", "Task A", Some("Step 1")),
            make_link("GHOST", "A", "needs-context"),
        ];
        let graph = materialize_graph(&events);
        let thread = ThreadId { head: "A".to_string(), tail: "GHOST".to_string() };
        assert!(validate_thread_instructions(&graph, &thread).is_ok());
    }
}
