//! Review issue helpers.

use crate::tasks::{Task, TaskComment};

/// Get all issue comments from a task (comments where data.issue == "true").
///
/// This is the canonical function for filtering issue comments — used by both
/// `aiki review issue list` and `aiki fix`.
pub fn get_issue_comments(task: &Task) -> Vec<&TaskComment> {
    task.comments
        .iter()
        .filter(|c| c.data.get("issue").map(|v| v == "true").unwrap_or(false))
        .collect()
}

/// Count issues on a review task.
///
/// Prefers the explicit `issue_count` data field (set at close time); falls back
/// to counting issue-tagged comments.
pub fn issue_count(task: &Task) -> usize {
    task.data
        .get("issue_count")
        .and_then(|ic| ic.parse::<usize>().ok())
        .unwrap_or_else(|| get_issue_comments(task).len())
}

/// Check if a review task has actionable issues.
pub fn has_actionable_issues(task: &Task) -> bool {
    issue_count(task) > 0
}
