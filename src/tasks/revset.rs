//! Revset pattern builders for task-related JJ queries.
//!
//! These functions build JJ revset expressions that match commits belonging
//! to tasks, filtering out task lifecycle events on the `aiki/tasks` branch.

use crate::tasks::TaskGraph;

/// Matches changes whose description contains `task=<id>` (provenance metadata).
/// Excludes `::aiki/tasks` to filter out task lifecycle events (which contain
/// `stopped_task=<id>`, `task_id=<id>`, etc.) that live on a separate branch.
///
/// NOTE: For link-based subtasks (connected via `subtask-of` edges),
/// use `build_task_revset_pattern_with_graph`.
pub fn build_task_revset_pattern(task_id: &str) -> String {
    format!("description(substring:\"task={}\") ~ ::aiki/tasks", task_id)
}

/// Build revset pattern for a task including all descendants via `subtask-of` links.
///
/// Like `build_task_revset_pattern` but also includes link-based subtasks
/// (tasks connected via `subtask-of` edges in the graph). This is needed for
/// epics where fix-parent tasks are linked as subtasks with independent IDs.
pub fn build_task_revset_pattern_with_graph(task_id: &str, graph: &TaskGraph) -> String {
    let mut patterns = vec![build_task_revset_pattern(task_id)];

    // Collect all descendant task IDs via subtask-of links (BFS)
    let mut queue: Vec<String> = graph
        .edges
        .referrers(task_id, "subtask-of")
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut visited = std::collections::HashSet::new();
    visited.insert(task_id.to_string());

    while let Some(child_id) = queue.pop() {
        if !visited.insert(child_id.clone()) {
            continue;
        }
        patterns.push(build_task_revset_pattern(&child_id));
        for grandchild in graph.edges.referrers(&child_id, "subtask-of") {
            queue.push(grandchild.to_string());
        }
    }

    if patterns.len() == 1 {
        patterns.into_iter().next().unwrap()
    } else {
        format!("({})", patterns.join(" | "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::graph::EdgeStore;
    use crate::tasks::types::{FastHashMap, Task, TaskPriority, TaskStatus};
    use std::collections::HashMap;

    #[test]
    fn test_build_task_revset_pattern() {
        let pattern = build_task_revset_pattern("abc123");
        assert!(pattern.contains("task=abc123"));
        assert!(!pattern.contains("task=abc123."));
    }

    #[test]
    fn test_build_task_revset_pattern_with_graph_no_subtasks() {
        let mut tasks = FastHashMap::default();
        let task = Task {
            id: "epic".to_string(),
            name: "Epic".to_string(),
            slug: None,
            task_type: None,
            status: TaskStatus::Open,
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            instructions: None,
            data: HashMap::new(),
            created_at: chrono::Utc::now(),
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
        };
        tasks.insert("epic".to_string(), task);
        let graph = TaskGraph {
            tasks,
            edges: EdgeStore::new(),
            slug_index: FastHashMap::default(),
        };

        // Without subtasks, should be same as build_task_revset_pattern
        let pattern = build_task_revset_pattern_with_graph("epic", &graph);
        assert_eq!(pattern, build_task_revset_pattern("epic"));
    }

    #[test]
    fn test_build_task_revset_pattern_with_graph_includes_link_subtasks() {
        let make = |id: &str| Task {
            id: id.to_string(),
            name: id.to_string(),
            slug: None,
            task_type: None,
            status: TaskStatus::Open,
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            instructions: None,
            data: HashMap::new(),
            created_at: chrono::Utc::now(),
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
        };

        let mut tasks = FastHashMap::default();
        tasks.insert("epic".to_string(), make("epic"));
        tasks.insert("fixparent".to_string(), make("fixparent"));
        tasks.insert("fixchild1".to_string(), make("fixchild1"));

        let mut edges = EdgeStore::new();
        edges.add("fixparent", "epic", "subtask-of");
        edges.add("fixchild1", "fixparent", "subtask-of");

        let graph = TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        };

        let pattern = build_task_revset_pattern_with_graph("epic", &graph);

        // Should include patterns for epic, fixparent, and fixchild1
        assert!(pattern.contains("task=epic"));
        assert!(pattern.contains("task=fixparent"));
        assert!(pattern.contains("task=fixchild1"));
    }
}
