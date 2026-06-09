//! Epic-level review history extraction.

use super::issues::{get_issue_comments, has_actionable_issues};
use super::location::parse_locations;
use crate::tasks::graph::TaskGraph;
use crate::tasks::{
    build_task_revset_pattern_with_graph, Task, TaskEvent, TaskOutcome, TaskStatus,
};
use std::path::Path;

/// A single review issue with severity and locations.
#[derive(Debug, Clone)]
pub struct ReviewIssue {
    pub description: String,
    pub severity: String,
    pub locations: Vec<String>,
}

/// A single review iteration for an epic.
#[derive(Debug, Clone)]
pub struct ReviewIteration {
    pub review_task_id: String,
    pub iteration: usize,
    pub outcome: String,
    pub issues: Vec<ReviewIssue>,
    pub fixes: Vec<ReviewFix>,
    #[allow(dead_code)]
    pub fix_task_id: Option<String>,
}

/// Structured metadata about a fix task linked to a review iteration.
#[derive(Debug, Clone)]
pub struct ReviewFix {
    pub task_id: String,
    pub name: String,
    pub outcome: String,
    pub summary: Option<String>,
    pub revset: String,
    pub files_changed: Vec<String>,
    pub diff_stat: Option<String>,
}

/// Extract the full review history for an epic.
///
/// Finds all review tasks that validate the given epic, extracts their issues,
/// and links them to any associated fix tasks via `remediates` edges.
/// Results are ordered by review task creation time (iteration 1, 2, 3...).
pub fn epic_review_history(cwd: &Path, events: &[TaskEvent], graph: &TaskGraph, epic_id: &str) -> Vec<ReviewIteration> {

    // Find all review task IDs that validate this epic
    let review_ids = graph.edges.referrers(epic_id, "validates");

    // Collect review tasks with their creation times for sorting
    let mut reviews: Vec<_> = review_ids
        .iter()
        .filter_map(|rid| graph.tasks.get(rid).map(|task| (rid.clone(), task)))
        .collect();

    // Sort by creation time for iteration ordering
    reviews.sort_by_key(|(_, task)| task.created_at);

    reviews
        .into_iter()
        .enumerate()
        .map(|(i, (review_id, task))| {
            // Extract issues from issue comments
            let issue_comments = get_issue_comments(task);
            let issues: Vec<ReviewIssue> = issue_comments
                .into_iter()
                .map(|comment| {
                    let severity = comment
                        .data
                        .get("severity")
                        .cloned()
                        .unwrap_or_else(|| "medium".to_string());
                    let locations = parse_locations(&comment.data);
                    let location_strings: Vec<String> =
                        locations.iter().map(|l| l.to_string()).collect();
                    ReviewIssue {
                        description: comment.text.clone(),
                        severity,
                        locations: location_strings,
                    }
                })
                .collect();

            // Reviews are only approvals once they are actually closed successfully.
            let outcome = review_outcome(task);

            // Find associated fix tasks via remediates link.
            let fixes = review_fix_history(cwd, &events, graph, &review_id);
            let fix_task_id = fixes.first().map(|fix| fix.task_id.clone());

            ReviewIteration {
                review_task_id: review_id,
                iteration: i + 1,
                outcome,
                issues,
                fixes,
                fix_task_id,
            }
        })
        .collect()
}

fn review_fix_history(
    cwd: &Path,
    events: &[TaskEvent],
    graph: &TaskGraph,
    review_id: &str,
) -> Vec<ReviewFix> {
    let mut fixes: Vec<&Task> = graph
        .edges
        .referrers(review_id, "remediates")
        .iter()
        .filter_map(|task_id| graph.tasks.get(task_id))
        .collect();

    fixes.sort_by_key(|task| task.created_at);
    fixes
        .into_iter()
        .map(|task| build_review_fix(cwd, events, graph, task))
        .collect()
}

fn build_review_fix(_cwd: &Path, _events: &[TaskEvent], graph: &TaskGraph, task: &Task) -> ReviewFix {
    let revset = build_task_revset_pattern_with_graph(&task.id, graph);

    ReviewFix {
        task_id: task.id.clone(),
        name: task.name.clone(),
        outcome: task_outcome(task),
        summary: task.summary.clone(),
        files_changed: vec![],
        diff_stat: None,
        revset,
    }
}

fn task_outcome(task: &Task) -> String {
    task.closed_outcome
        .as_ref()
        .map(|outcome| format!("{outcome:?}"))
        .unwrap_or_else(|| format!("{:?}", task.status))
}

fn review_outcome(task: &Task) -> String {
    if has_actionable_issues(task) {
        return "issues_found".to_string();
    }

    match (task.status, task.closed_outcome) {
        (TaskStatus::Closed, Some(TaskOutcome::Done)) => "approved".to_string(),
        _ => "incomplete".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::graph::EdgeStore;
    use crate::tasks::types::{FastHashMap, TaskComment, TaskPriority};
    use chrono::Utc;
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

    fn issue_comment(text: &str) -> TaskComment {
        let mut data = HashMap::new();
        data.insert("issue".to_string(), "true".to_string());
        data.insert("severity".to_string(), "high".to_string());
        TaskComment {
            id: None,
            text: text.to_string(),
            timestamp: Utc::now(),
            data,
        }
    }

    fn review_history_for(review: Task) -> Vec<ReviewIteration> {
        let epic = make_task("epic", "Epic", TaskStatus::Closed);
        let review_id = review.id.clone();

        let mut tasks = FastHashMap::default();
        tasks.insert(epic.id.clone(), epic);
        tasks.insert(review_id.clone(), review);

        let mut edges = EdgeStore::new();
        edges.add(&review_id, "epic", "validates");

        let graph = TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        };

        epic_review_history(std::env::temp_dir().as_path(), &[], &graph, "epic")
    }

    #[test]
    fn epic_review_history_marks_closed_done_reviews_as_approved() {
        let mut review = make_task("review-1", "Review", TaskStatus::Closed);
        review.closed_outcome = Some(TaskOutcome::Done);

        let history = review_history_for(review);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].outcome, "approved");
    }

    #[test]
    fn epic_review_history_marks_reviews_with_actionable_issues() {
        let mut review = make_task("review-1", "Review", TaskStatus::Closed);
        review.closed_outcome = Some(TaskOutcome::Done);
        review.comments.push(issue_comment("Fix the edge case"));

        let history = review_history_for(review);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].outcome, "issues_found");
        assert_eq!(history[0].issues.len(), 1);
    }

    #[test]
    fn epic_review_history_marks_unfinished_clean_reviews_as_incomplete() {
        let review = make_task("review-1", "Review", TaskStatus::Stopped);

        let history = review_history_for(review);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].outcome, "incomplete");
    }

    #[test]
    fn epic_review_history_preserves_multiple_fix_tasks_with_structured_metadata() {
        let epic = make_task("epic", "Epic", TaskStatus::Closed);
        let review = make_task("review-1", "Review", TaskStatus::Closed);

        let mut fix_one = make_task("fix-1", "Fix parser", TaskStatus::Closed);
        fix_one.closed_outcome = Some(TaskOutcome::Done);
        fix_one.summary = Some("Adjusted parser edge handling".to_string());

        let mut fix_two = make_task("fix-2", "Fix serializer", TaskStatus::Closed);
        fix_two.closed_outcome = Some(TaskOutcome::Done);
        fix_two.summary = Some("Updated serializer shape".to_string());

        let mut tasks = FastHashMap::default();
        tasks.insert(epic.id.clone(), epic);
        tasks.insert(review.id.clone(), review);
        tasks.insert(fix_one.id.clone(), fix_one);
        tasks.insert(fix_two.id.clone(), fix_two);

        let mut edges = EdgeStore::new();
        edges.add("review-1", "epic", "validates");
        edges.add("fix-1", "review-1", "remediates");
        edges.add("fix-2", "review-1", "remediates");

        let graph = TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        };

        let history = epic_review_history(std::env::temp_dir().as_path(), &[], &graph, "epic");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].fix_task_id.as_deref(), Some("fix-1"));
        assert_eq!(history[0].fixes.len(), 2);
        assert_eq!(history[0].fixes[0].task_id, "fix-1");
        assert_eq!(history[0].fixes[0].name, "Fix parser");
        assert_eq!(history[0].fixes[0].outcome, "Done");
        assert_eq!(
            history[0].fixes[0].summary.as_deref(),
            Some("Adjusted parser edge handling")
        );
        assert!(history[0].fixes[0].revset.contains("task=fix-1"));
        assert_eq!(history[0].fixes[1].task_id, "fix-2");
        assert!(history[0].fixes[1].revset.contains("task=fix-2"));
    }

    #[test]
    fn epic_review_history_preserves_multiple_iterations_with_fix_metadata() {
        let epic = make_task("epic", "Epic", TaskStatus::Closed);

        let mut review_one = make_task("review-1", "Review One", TaskStatus::Closed);
        review_one.created_at = Utc::now() - chrono::Duration::hours(2);
        review_one.comments.push(issue_comment("First issue"));

        let mut review_two = make_task("review-2", "Review Two", TaskStatus::Closed);
        review_two.created_at = Utc::now() - chrono::Duration::hours(1);
        review_two.closed_outcome = Some(TaskOutcome::Done);

        let mut fix_one = make_task("fix-1", "Fix round one", TaskStatus::Closed);
        fix_one.closed_outcome = Some(TaskOutcome::Done);
        fix_one.summary = Some("Patched the first issue".to_string());

        let mut fix_two = make_task("fix-2", "Fix round two", TaskStatus::Closed);
        fix_two.closed_outcome = Some(TaskOutcome::Done);
        fix_two.summary = Some("Addressed review follow-up".to_string());

        let mut tasks = FastHashMap::default();
        tasks.insert(epic.id.clone(), epic);
        tasks.insert(review_one.id.clone(), review_one);
        tasks.insert(review_two.id.clone(), review_two);
        tasks.insert(fix_one.id.clone(), fix_one);
        tasks.insert(fix_two.id.clone(), fix_two);

        let mut edges = EdgeStore::new();
        edges.add("review-1", "epic", "validates");
        edges.add("review-2", "epic", "validates");
        edges.add("fix-1", "review-1", "remediates");
        edges.add("fix-2", "review-2", "remediates");

        let graph = TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        };

        let history = epic_review_history(std::env::temp_dir().as_path(), &[], &graph, "epic");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].iteration, 1);
        assert_eq!(history[0].review_task_id, "review-1");
        assert_eq!(history[0].outcome, "issues_found");
        assert_eq!(history[0].issues[0].description, "First issue");
        assert_eq!(history[0].fix_task_id.as_deref(), Some("fix-1"));
        assert_eq!(history[0].fixes[0].task_id, "fix-1");
        assert_eq!(
            history[0].fixes[0].summary.as_deref(),
            Some("Patched the first issue")
        );

        assert_eq!(history[1].iteration, 2);
        assert_eq!(history[1].review_task_id, "review-2");
        assert_eq!(history[1].outcome, "approved");
        assert!(history[1].issues.is_empty());
        assert_eq!(history[1].fix_task_id.as_deref(), Some("fix-2"));
        assert_eq!(history[1].fixes[0].task_id, "fix-2");
        assert_eq!(
            history[1].fixes[0].summary.as_deref(),
            Some("Addressed review follow-up")
        );
    }

}
