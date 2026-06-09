//! Review output helpers.

use std::path::Path;

use super::issues::issue_count;
use crate::commands::output::{format_command_output, CommandOutput};
use crate::error::Result;
use crate::output_utils;
use crate::tasks::md::MdBuilder;
use crate::tasks::types::TaskStatus;
use crate::tasks::{find_task, materialize_graph, read_events};

/// Summarize a review task as a short text line (e.g. "Found 3 issues" or "approved").
pub fn review_summary(cwd: &Path, review_id: &str) -> Result<String> {
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let task = find_task(&graph.tasks, review_id)?;
    let ic = issue_count(task);
    if ic > 0 {
        Ok(format!("Found {} issues", ic))
    } else if task.status != TaskStatus::Closed {
        Ok("pending".to_string())
    } else {
        Ok("approved".to_string())
    }
}

/// Output approved message when no issues found.
#[allow(dead_code)]
pub fn output_approved(task_id: &str) -> Result<()> {
    output_utils::emit(|| {
        let output = CommandOutput {
            heading: "Approved",
            task_id,
            scope: None,
            status: "Review approved - no issues found.",
            issues: None,
            hint: None,
        };
        let content = format_command_output(&output);
        MdBuilder::new().build(&content)
    });
    Ok(())
}
