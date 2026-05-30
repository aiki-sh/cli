//! Wait command for blocking until a task reaches terminal state
//!
//! This module provides the `aiki wait` command which:
//! - Blocks until a task reaches terminal state (closed or stopped)
//! - Uses exponential backoff polling (100ms initial, 2x multiplier, 2000ms max)
//! - Supports reading task ID from stdin for piping
//! - Outputs task ID to stdout for piping support

use std::env;
use std::io::{self, BufRead};
use std::path::Path;
use std::thread;
use std::time::Duration;

use crate::error::{AikiError, Result};
use crate::tasks::{find_task, materialize_graph, read_events, TaskStatus};

/// Exponential backoff configuration for polling
const INITIAL_DELAY_MS: u64 = 100;
const MAX_DELAY_MS: u64 = 2000;
const MULTIPLIER: u64 = 2;

/// Run the wait command
///
/// Blocks until the specified task reaches a terminal state (closed or stopped).
/// If task_id is None, reads from stdin (for piping support).
///
/// Returns Ok(()) with exit code 0 if task completed successfully (closed with done outcome).
/// Returns an error (exit code 1) if task failed, was stopped, or closed with wont_do.
pub fn run(task_id: Option<String>) -> Result<()> {
    let current_dir = env::current_dir().map_err(|_| {
        AikiError::InvalidArgument("Failed to get current directory".to_string())
    })?;

    // Get task ID from argument or stdin
    let task_id = match task_id {
        Some(id) => extract_task_id(&id),
        None => read_task_id_from_stdin()?,
    };

    // Poll until terminal state
    let (final_status, outcome) = poll_task_status(&current_dir, &task_id)?;

    // Output task ID to stdout (passthrough for piping)
    println!("{}", task_id);

    // Determine exit based on final state
    match final_status {
        TaskStatus::Closed => {
            // Check outcome for done vs wont_do
            if let Some(crate::tasks::TaskOutcome::WontDo) = outcome {
                Err(AikiError::InvalidArgument(format!(
                    "Task '{}' was closed as won't-do",
                    task_id
                )))
            } else {
                Ok(())
            }
        }
        TaskStatus::Stopped => Err(AikiError::InvalidArgument(format!(
            "Task '{}' was stopped",
            task_id
        ))),
        _ => unreachable!("poll_task_status only returns terminal states"),
    }
}

/// Extract task ID from input, handling XML output format
///
/// Supports:
/// - Plain task ID: "xqrmnpst"
/// - XML output with task_id attribute: `<started task_id="xqrmnpst" async="true">`
fn extract_task_id(input: &str) -> String {
    let trimmed = input.trim();

    // Try to extract from XML task_id attribute
    if let Some(start) = trimmed.find("task_id=\"") {
        let after_quote = &trimmed[start + 9..]; // Skip `task_id="`
        if let Some(end) = after_quote.find('"') {
            return after_quote[..end].to_string();
        }
    }

    // Return as-is (plain task ID)
    trimmed.to_string()
}

/// Read task ID from stdin
///
/// Reads all available input and extracts the task ID.
fn read_task_id_from_stdin() -> Result<String> {
    let stdin = io::stdin();
    let mut input = String::new();

    // Read all lines from stdin
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| {
            AikiError::InvalidArgument(format!("Failed to read from stdin: {}", e))
        })?;
        input.push_str(&line);
        input.push('\n');
    }

    if input.trim().is_empty() {
        return Err(AikiError::InvalidArgument(
            "No task ID provided. Pass as argument or pipe from another command.".to_string(),
        ));
    }

    Ok(extract_task_id(&input))
}

/// Poll task status until it reaches a terminal state
///
/// Uses exponential backoff: 100ms -> 200ms -> 400ms -> ... -> 2000ms max
///
/// Returns the final status and outcome (if closed).
fn poll_task_status(
    cwd: &Path,
    task_id: &str,
) -> Result<(TaskStatus, Option<crate::tasks::TaskOutcome>)> {
    let mut delay_ms = INITIAL_DELAY_MS;

    loop {
        // Load current task state
        let events = read_events(cwd)?;
        let tasks = materialize_graph(&events).tasks;

        // Find the task
        let task = find_task(&tasks, task_id)?;

        // Check if terminal state
        match task.status {
            TaskStatus::Closed => {
                return Ok((TaskStatus::Closed, task.closed_outcome));
            }
            TaskStatus::Stopped => {
                return Ok((TaskStatus::Stopped, None));
            }
            TaskStatus::Open | TaskStatus::InProgress => {
                // Not terminal, continue polling
            }
        }

        // Sleep with exponential backoff
        thread::sleep(Duration::from_millis(delay_ms));

        // Increase delay for next iteration (capped at max)
        delay_ms = (delay_ms * MULTIPLIER).min(MAX_DELAY_MS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_task_id_plain() {
        assert_eq!(extract_task_id("xqrmnpst"), "xqrmnpst");
        assert_eq!(extract_task_id("  xqrmnpst  "), "xqrmnpst");
    }

    #[test]
    fn test_extract_task_id_xml_started() {
        let xml = r#"<aiki_task cmd="run" status="ok">
  <started task_id="xqrmnpst" async="true">
    Task started asynchronously.
  </started>
</aiki_task>"#;
        assert_eq!(extract_task_id(xml), "xqrmnpst");
    }

    #[test]
    fn test_extract_task_id_xml_completed() {
        let xml = r#"<aiki_task cmd="run" status="ok">
  <completed task_id="abcdefgh"/>
</aiki_task>"#;
        assert_eq!(extract_task_id(xml), "abcdefgh");
    }

    #[test]
    fn test_extract_task_id_no_xml() {
        // If no task_id attribute found, return as-is
        let input = "some random text";
        assert_eq!(extract_task_id(input), "some random text");
    }

    #[test]
    fn test_exponential_backoff_values() {
        // Verify our constants make sense
        assert_eq!(INITIAL_DELAY_MS, 100);
        assert_eq!(MAX_DELAY_MS, 2000);
        assert_eq!(MULTIPLIER, 2);

        // Verify sequence: 100 -> 200 -> 400 -> 800 -> 1600 -> 2000 (capped)
        let mut delay = INITIAL_DELAY_MS;
        assert_eq!(delay, 100);

        delay = (delay * MULTIPLIER).min(MAX_DELAY_MS);
        assert_eq!(delay, 200);

        delay = (delay * MULTIPLIER).min(MAX_DELAY_MS);
        assert_eq!(delay, 400);

        delay = (delay * MULTIPLIER).min(MAX_DELAY_MS);
        assert_eq!(delay, 800);

        delay = (delay * MULTIPLIER).min(MAX_DELAY_MS);
        assert_eq!(delay, 1600);

        delay = (delay * MULTIPLIER).min(MAX_DELAY_MS);
        assert_eq!(delay, 2000); // Capped at max

        delay = (delay * MULTIPLIER).min(MAX_DELAY_MS);
        assert_eq!(delay, 2000); // Stays at max
    }
}
