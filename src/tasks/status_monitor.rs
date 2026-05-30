//! Real-time status monitoring for task execution
//!
//! Provides live terminal visualization of task progress during sync execution.
//! Shows subtasks and comments as they're created by the working agent.

use std::io::{stderr, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::{
    cursor::{RestorePosition, SavePosition},
    terminal::{Clear, ClearType},
    ExecutableCommand,
};

use super::graph::{materialize_graph, TaskGraph};
use super::storage::read_events;
use super::types::{Task, TaskStatus};
use crate::agents::MonitoredChild;
use crate::error::Result;

/// Default polling interval in milliseconds
const DEFAULT_POLL_INTERVAL_MS: u64 = 500;

/// Reason for monitor to stop
#[derive(Debug, Clone)]
pub enum MonitorExitReason {
    /// Task reached terminal state (closed or stopped)
    TaskCompleted,
    /// User pressed Ctrl+C to detach
    UserDetached,
    /// Agent process exited without task reaching terminal state
    AgentExited {
        /// Captured stderr output from the agent (if any)
        stderr: String,
    },
}

/// Status symbols for task visualization
const SYMBOL_COMPLETED: &str = "✓";
const SYMBOL_IN_PROGRESS: &str = "●";
const SYMBOL_PENDING: &str = "○";
const SYMBOL_STOPPED: &str = "✗";
const SYMBOL_COMMENT: &str = "💬";

/// Monitor for real-time task status updates
pub struct StatusMonitor {
    /// The root task being monitored
    task_id: String,
    /// Number of events at last poll (to detect changes)
    last_event_count: usize,
    /// Polling interval
    poll_interval: Duration,
    /// When monitoring started (for elapsed time)
    start_time: Instant,
    /// Flag to track if we've already rendered initial state
    has_rendered: bool,
    /// Atomic flag to signal when to stop (for Ctrl+C handling)
    stop_flag: Arc<AtomicBool>,
    /// Optional PID of the agent process to monitor for unexpected exit
    agent_pid: Option<u32>,
}

impl StatusMonitor {
    /// Create a new status monitor for a task
    #[must_use]
    pub fn new(task_id: &str) -> Self {
        let poll_interval_ms = std::env::var("AIKI_STATUS_INTERVAL_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_POLL_INTERVAL_MS);

        Self {
            task_id: task_id.to_string(),
            last_event_count: 0,
            poll_interval: Duration::from_millis(poll_interval_ms),
            start_time: Instant::now(),
            has_rendered: false,
            stop_flag: Arc::new(AtomicBool::new(false)),
            agent_pid: None,
        }
    }

    /// Set the agent process ID to monitor for unexpected exits
    #[must_use]
    pub fn with_agent_pid(mut self, pid: u32) -> Self {
        self.agent_pid = Some(pid);
        self
    }

    /// Get a clone of the stop flag for signal handling
    #[must_use]
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop_flag)
    }

    /// Poll for new events and update display if state changed
    ///
    /// Returns Ok(true) if task reached terminal state (closed/stopped)
    pub fn poll_and_display(&mut self, cwd: &Path) -> Result<bool> {
        let events = read_events(cwd)?;
        let graph = materialize_graph(&events);

        // Find the root task
        let root_task = match graph.tasks.get(&self.task_id) {
            Some(task) => task,
            None => return Ok(false), // Task not found yet, keep waiting
        };

        // Check if we should update display (new events since last poll)
        let should_render = events.len() != self.last_event_count || !self.has_rendered;

        if should_render {
            self.last_event_count = events.len();
            self.render_task_tree(&graph, root_task)?;
            self.has_rendered = true;
        }

        // Check if task reached terminal state
        let is_terminal = matches!(root_task.status, TaskStatus::Closed | TaskStatus::Stopped);

        Ok(is_terminal)
    }

    /// Monitor until task completion, detach, or agent exit
    ///
    /// Returns the reason why monitoring stopped.
    ///
    /// **Note:** This version uses `is_process_alive(pid)` which has a known issue
    /// with zombie processes. Prefer `monitor_until_complete_with_child()` when possible.
    pub fn monitor_until_complete(&mut self, cwd: &Path) -> Result<MonitorExitReason> {
        // Initial render
        let _ = self.poll_and_display(cwd);

        loop {
            // Check stop flag (Ctrl+C)
            if self.stop_flag.load(Ordering::Relaxed) {
                self.render_detach_message()?;
                return Ok(MonitorExitReason::UserDetached);
            }

            // Sleep for poll interval
            std::thread::sleep(self.poll_interval);

            // Check if agent process exited unexpectedly
            if let Some(pid) = self.agent_pid {
                if !is_process_alive(pid) {
                    // Agent exited - do one final poll to check task status
                    // Note: This version can't capture stderr since we only have the PID
                    match self.poll_and_display(cwd) {
                        Ok(true) => return Ok(MonitorExitReason::TaskCompleted),
                        _ => return Ok(MonitorExitReason::AgentExited {
                            stderr: String::new(),
                        }),
                    }
                }
            }

            // Poll and update display
            match self.poll_and_display(cwd) {
                Ok(true) => return Ok(MonitorExitReason::TaskCompleted),
                Ok(false) => continue,
                Err(e) => {
                    // Log error but continue monitoring
                    eprintln!("\nError polling task: {}", e);
                    continue;
                }
            }
        }
    }

    /// Monitor until task completion, detach, or agent exit (using MonitoredChild)
    ///
    /// This version properly handles zombie processes by using `try_wait()` on the
    /// child process instead of checking if the PID is alive with `kill(pid, 0)`.
    ///
    /// Returns the reason why monitoring stopped.
    pub fn monitor_until_complete_with_child(
        &mut self,
        cwd: &Path,
        child: &mut MonitoredChild,
    ) -> Result<MonitorExitReason> {
        // Initial render
        let _ = self.poll_and_display(cwd);

        loop {
            // Check stop flag (Ctrl+C)
            if self.stop_flag.load(Ordering::Relaxed) {
                self.render_detach_message()?;
                return Ok(MonitorExitReason::UserDetached);
            }

            // Sleep for poll interval
            std::thread::sleep(self.poll_interval);

            // Check if agent process exited using try_wait()
            // This properly handles zombie processes by calling wait() internally
            match child.try_wait() {
                Ok(Some(_exit_status)) => {
                    // Agent exited - capture stderr and do one final poll to check task status
                    let stderr = child.read_stderr();
                    match self.poll_and_display(cwd) {
                        Ok(true) => return Ok(MonitorExitReason::TaskCompleted),
                        _ => return Ok(MonitorExitReason::AgentExited { stderr }),
                    }
                }
                Ok(None) => {
                    // Process is still running, continue monitoring
                }
                Err(e) => {
                    // Error checking process status - treat as exited
                    eprintln!("\nError checking agent status: {}", e);
                    let stderr = child.read_stderr();
                    match self.poll_and_display(cwd) {
                        Ok(true) => return Ok(MonitorExitReason::TaskCompleted),
                        _ => return Ok(MonitorExitReason::AgentExited { stderr }),
                    }
                }
            }

            // Poll and update display
            match self.poll_and_display(cwd) {
                Ok(true) => return Ok(MonitorExitReason::TaskCompleted),
                Ok(false) => continue,
                Err(e) => {
                    // Log error but continue monitoring
                    eprintln!("\nError polling task: {}", e);
                    continue;
                }
            }
        }
    }

    /// Render the task tree to stderr
    fn render_task_tree(&mut self, graph: &TaskGraph, root_task: &Task) -> Result<()> {
        let mut stderr = stderr();

        // On re-render, restore cursor to saved position and clear everything below.
        // This avoids counting physical lines (which breaks when lines wrap).
        if self.has_rendered {
            stderr.execute(RestorePosition)?;
            stderr.execute(Clear(ClearType::FromCursorDown))?;
        }

        // Save cursor position before writing so next render can return here
        stderr.execute(SavePosition)?;

        let mut lines = Vec::new();

        // Render root task
        let root_line = self.format_task_line(root_task, "", None);
        lines.push(root_line);

        // Get subtasks (direct children)
        let subtasks = self.get_sorted_subtasks(graph, &root_task.id);
        let subtask_count = subtasks.len();

        for (idx, subtask) in subtasks.iter().enumerate() {
            let is_last = idx == subtask_count - 1;
            let prefix = if is_last { "└─ " } else { "├─ " };
            let child_prefix = if is_last { "   " } else { "│  " };

            let task_line = self.format_task_line(subtask, prefix, Some(idx));
            lines.push(task_line);

            // Show summary for closed tasks, latest comment for in-progress
            let display_text = if subtask.status == TaskStatus::Closed {
                subtask.effective_summary().map(|s| s.to_string())
            } else {
                subtask.comments.last().map(|c| c.text.clone())
            };
            if let Some(text) = display_text {
                let comment_line = format!(
                    "{}   └─ {} {}",
                    child_prefix,
                    SYMBOL_COMMENT,
                    format_comment(&text)
                );
                lines.push(comment_line);
            }
        }

        // Show comments on the parent/root task below the tree
        if !root_task.comments.is_empty() {
            lines.push(String::new());
            for comment in &root_task.comments {
                let comment_line = format!(
                    "{} {}",
                    SYMBOL_COMMENT,
                    format_comment(&comment.text)
                );
                lines.push(comment_line);
            }
        }

        // If root task has data.epic, render the epic task tree below
        if let Some(epic_id) = root_task.data.get("epic") {
            if let Some(epic_task) = graph.tasks.get(epic_id) {
                lines.push(String::new());
                let epic_line = self.format_task_line(epic_task, "", None);
                lines.push(epic_line);

                let epic_subtasks = self.get_sorted_subtasks(graph, epic_id);
                let epic_subtask_count = epic_subtasks.len();

                for (idx, subtask) in epic_subtasks.iter().enumerate() {
                    let is_last = idx == epic_subtask_count - 1;
                    let prefix = if is_last { "└─ " } else { "├─ " };
                    let child_prefix = if is_last { "   " } else { "│  " };

                    let task_line = self.format_task_line(subtask, prefix, Some(idx));
                    lines.push(task_line);

                    let display_text = if subtask.status == TaskStatus::Closed {
                        subtask.effective_summary().map(|s| s.to_string())
                    } else {
                        subtask.comments.last().map(|c| c.text.clone())
                    };
                    if let Some(text) = display_text {
                        let comment_line = format!(
                            "{}   └─ {} {}",
                            child_prefix,
                            SYMBOL_COMMENT,
                            format_comment(&text)
                        );
                        lines.push(comment_line);
                    }
                }
            }
        }

        // Add footer
        lines.push(String::new());
        lines.push("[Ctrl+C to detach]".to_string());

        // Render all lines
        for line in &lines {
            writeln!(stderr, "{}", line)?;
        }
        stderr.flush()?;

        Ok(())
    }

    /// Format a single task line with status symbol and elapsed time
    ///
    /// For root tasks, shows full short ID: `[twxlpqwz]`
    /// For subtasks, shows relative index: `[.1]`, `[.2]`, etc.
    fn format_task_line(&self, task: &Task, prefix: &str, subtask_index: Option<usize>) -> String {
        let symbol = match task.status {
            TaskStatus::Closed => SYMBOL_COMPLETED,
            TaskStatus::InProgress => SYMBOL_IN_PROGRESS,
            TaskStatus::Open => SYMBOL_PENDING,
            TaskStatus::Stopped => SYMBOL_STOPPED,
        };

        let is_root = subtask_index.is_none();
        let elapsed = if is_root || task.status == TaskStatus::InProgress {
            format!(" [{}]", self.format_elapsed())
        } else {
            String::new()
        };

        // Root task: short ID; Subtasks: slug or .N)
        let id_display = match subtask_index {
            None => format!("[{}]", &task.id[..8.min(task.id.len())]),
            Some(num) => {
                if let Some(ref slug) = task.slug {
                    slug.clone()
                } else {
                    format!(".{})", num)
                }
            }
        };

        let name = &task.name;

        format!("{}{} {} {}{}", prefix, symbol, id_display, name, elapsed)
    }

    /// Format elapsed time as human-readable string
    fn format_elapsed(&self) -> String {
        let elapsed = self.start_time.elapsed();
        let secs = elapsed.as_secs();

        if secs < 60 {
            format!("{}s", secs)
        } else if secs < 3600 {
            format!("{}m {}s", secs / 60, secs % 60)
        } else {
            format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
        }
    }

    /// Get sorted subtasks for a parent task (sorted by creation time)
    fn get_sorted_subtasks<'a>(&self, graph: &'a TaskGraph, parent_id: &str) -> Vec<&'a Task> {
        let mut subtasks = graph.children_of(parent_id);
        subtasks.sort_by_key(|t| t.created_at);
        subtasks
    }

    /// Render detach message when Ctrl+C is pressed
    fn render_detach_message(&self) -> Result<()> {
        let mut stderr = stderr();
        writeln!(stderr)?;
        writeln!(
            stderr,
            "Detached. Task {} still running. Use `aiki task show {}` to check status.",
            &self.task_id[..8.min(self.task_id.len())],
            self.task_id
        )?;
        stderr.flush()?;
        Ok(())
    }
}

/// Check if a process is still alive
///
/// Uses kill with signal 0 on Unix, which checks if the process exists
/// without actually sending a signal.
#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    // SAFETY: kill with signal 0 is safe - it just checks if process exists
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0
}

/// Check if a process is still alive (non-Unix stub)
#[cfg(not(unix))]
fn is_process_alive(_pid: u32) -> bool {
    // On non-Unix platforms, assume process is alive
    // (we can't reliably check without platform-specific code)
    true
}

/// Format a comment for display (first line only)
fn format_comment(text: &str) -> String {
    text.lines().next().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_comment_short() {
        let result = format_comment("Short comment");
        assert_eq!(result, "Short comment");
    }

    #[test]
    fn test_format_comment_long() {
        let long = "A".repeat(80);
        let result = format_comment(&long);
        assert_eq!(result.len(), 80);
    }

    #[test]
    fn test_format_comment_multiline() {
        let multiline = "First line\nSecond line\nThird line";
        let result = format_comment(multiline);
        assert_eq!(result, "First line");
    }

    #[test]
    fn test_status_monitor_new() {
        let monitor = StatusMonitor::new("test-task-id");
        assert_eq!(monitor.task_id, "test-task-id");
        assert_eq!(monitor.last_event_count, 0);
        assert!(!monitor.has_rendered);
        assert!(monitor.agent_pid.is_none());
    }

    #[test]
    fn test_status_monitor_with_agent_pid() {
        let monitor = StatusMonitor::new("test-task-id").with_agent_pid(12345);
        assert_eq!(monitor.agent_pid, Some(12345));
    }

    #[test]
    fn test_monitor_exit_reason_variants() {
        // Test that we can construct all variants
        let _completed = MonitorExitReason::TaskCompleted;
        let _detached = MonitorExitReason::UserDetached;
        let _exited = MonitorExitReason::AgentExited {
            stderr: "test error".to_string(),
        };

        // Test that AgentExited carries stderr
        let exit_reason = MonitorExitReason::AgentExited {
            stderr: "captured error".to_string(),
        };
        if let MonitorExitReason::AgentExited { stderr } = exit_reason {
            assert_eq!(stderr, "captured error");
        } else {
            panic!("Expected AgentExited variant");
        }
    }

    #[test]
    #[cfg(unix)]
    fn test_is_process_alive_current_process() {
        // Current process should be alive
        let pid = std::process::id();
        assert!(is_process_alive(pid));
    }

    #[test]
    #[cfg(unix)]
    fn test_is_process_alive_invalid_pid() {
        // Very high PID that almost certainly doesn't exist
        // (typically PIDs are limited to ~32k or 4M)
        let result = is_process_alive(u32::MAX - 1);
        // Could be false (doesn't exist) or we might get EPERM
        // Just ensure it doesn't panic
        let _ = result;
    }
}
