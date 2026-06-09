//! Helpers for selecting task snapshot baselines from Started events.

use chrono::{DateTime, Utc};

use super::{manager::get_all_descendants, TaskEvent, TaskGraph};

/// Return the first recorded immutable start snapshot for a task.
#[must_use]
pub fn first_started_snapshot(
    events: &[TaskEvent],
    task_id: &str,
) -> Option<(DateTime<Utc>, String)> {
    events.iter().find_map(|event| {
        if let TaskEvent::Started {
            task_ids,
            working_copy,
            timestamp,
            ..
        } = event
        {
            if task_ids.iter().any(|started_id| started_id == task_id) {
                return working_copy.clone().map(|snapshot| (*timestamp, snapshot));
            }
        }
        None
    })
}

/// Choose the snapshot baseline that should anchor a task's diff.
///
/// Normal tasks use their own `Started.working_copy` snapshot. Parent tasks that
/// only auto-start after descendant subtasks finish need an earlier baseline, so
/// compare the parent's first immutable snapshot to the earliest descendant
/// snapshot and prefer whichever happened first.
#[must_use]
pub fn select_task_snapshot_baseline(
    events: &[TaskEvent],
    graph: &TaskGraph,
    task_id: &str,
) -> Option<String> {
    let task_snapshot = first_started_snapshot(events, task_id);
    let descendant_snapshot = get_all_descendants(graph, task_id)
        .into_iter()
        .filter_map(|descendant| first_started_snapshot(events, &descendant.id))
        .min_by_key(|(timestamp, _)| *timestamp);

    match (task_snapshot, descendant_snapshot) {
        (Some((task_ts, task_snapshot)), Some((desc_ts, desc_snapshot))) => {
            if task_ts > desc_ts {
                Some(desc_snapshot)
            } else {
                Some(task_snapshot)
            }
        }
        (Some((_, task_snapshot)), None) => Some(task_snapshot),
        (None, Some((_, desc_snapshot))) => Some(desc_snapshot),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{materialize_graph, TaskEvent, TaskPriority};
    use chrono::TimeZone;
    use std::collections::HashMap;

    #[test]
    fn prefers_earliest_descendant_snapshot_for_auto_started_parent() {
        let timestamp = |seconds| Utc.timestamp_opt(seconds, 0).single().unwrap();

        let events = vec![
            TaskEvent::Created {
                task_id: "parent".to_string(),
                name: "Parent".to_string(),
                slug: None,
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: timestamp(0),
            },
            TaskEvent::Created {
                task_id: "child".to_string(),
                name: "Child".to_string(),
                slug: None,
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: timestamp(1),
            },
            TaskEvent::LinkAdded {
                from: "child".to_string(),
                to: "parent".to_string(),
                kind: "subtask-of".to_string(),
                autorun: None,
                timestamp: timestamp(2),
            },
            TaskEvent::Started {
                task_ids: vec!["child".to_string()],
                agent_type: "codex".to_string(),
                session_id: None,
                turn_id: None,
                working_copy: Some("child-snapshot".to_string()),
                instructions: None,
                timestamp: timestamp(3),
            },
            TaskEvent::Started {
                task_ids: vec!["parent".to_string()],
                agent_type: "codex".to_string(),
                session_id: None,
                turn_id: None,
                working_copy: Some("parent-snapshot".to_string()),
                instructions: None,
                timestamp: timestamp(4),
            },
        ];

        let graph = materialize_graph(&events);

        assert_eq!(
            select_task_snapshot_baseline(&events, &graph, "parent"),
            Some("child-snapshot".to_string())
        );
    }
}
