//! Task manager for materializing views and calculating ready queue

use std::collections::HashSet;

use super::types::{FastHashMap, Task, TaskStatus};
use crate::agents::{AgentType, Assignee};
use crate::error::{AikiError, Result};

/// Represents the set of active scopes based on in-progress tasks
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeSet {
    /// Whether root-level tasks should be included (a root task is in-progress)
    pub include_root: bool,
    /// Parent IDs of in-progress child tasks (sorted, deduplicated)
    pub scopes: Vec<String>,
}

impl ScopeSet {
    /// Check if this scope set is empty (no scopes and root not included)
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.include_root && self.scopes.is_empty()
    }

    /// Get scope list for XML output, including "root" when include_root=true
    #[must_use]
    pub fn to_xml_scopes(&self) -> Vec<String> {
        let mut result = Vec::new();
        if self.include_root {
            result.push("root".to_string());
        }
        result.extend(self.scopes.iter().cloned());
        result
    }
}


/// Get the ready queue (open, unblocked tasks sorted by priority)
///
/// Ready queue contains:
/// - Open status tasks that are not blocked
/// - Sorted by priority (P0 first, then P1, P2, P3)
/// - Then by creation time (oldest first)
#[must_use]
pub fn get_ready_queue<'a>(graph: &'a super::graph::TaskGraph) -> Vec<&'a Task> {
    let mut ready: Vec<&Task> = graph
        .tasks
        .values()
        .filter(|t| t.status == TaskStatus::Open)
        .filter(|t| !graph.is_blocked(&t.id))
        .collect();

    // Sort by priority (P0 < P1 < P2 < P3), then by creation time (oldest first)
    ready.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    ready
}

/// Get tasks currently in progress
#[must_use]
pub fn get_in_progress(tasks: &FastHashMap<String, Task>) -> Vec<&Task> {
    tasks
        .values()
        .filter(|t| t.status == TaskStatus::InProgress)
        .collect()
}

/// Get task IDs that are in-progress and claimed by a specific session
///
/// This is used to include task context in provenance records when
/// changes are made during a task.
///
/// Returns task IDs ordered by most recently started first.
#[must_use]
pub fn get_in_progress_task_ids_for_session(
    tasks: &FastHashMap<String, Task>,
    session_id: &str,
) -> Vec<String> {
    let mut result: Vec<&Task> = tasks
        .values()
        .filter(|t| {
            t.status == TaskStatus::InProgress
                && t.claimed_by_session.as_deref() == Some(session_id)
        })
        .collect();

    // Sort by start time descending (most recently started first)
    // Falls back to creation time if started_at is not set (shouldn't happen for in-progress tasks)
    result.sort_by(|a, b| {
        let a_time = a.started_at.unwrap_or(a.created_at);
        let b_time = b.started_at.unwrap_or(b.created_at);
        b_time.cmp(&a_time)
    });

    result.into_iter().map(|t| t.id.clone()).collect()
}

/// Get stopped tasks
#[must_use]
#[allow(dead_code)] // Part of task manager API
pub fn get_stopped(tasks: &FastHashMap<String, Task>) -> Vec<&Task> {
    tasks
        .values()
        .filter(|t| t.status == TaskStatus::Stopped)
        .collect()
}

/// Get closed tasks
#[must_use]
#[allow(dead_code)] // Part of task manager API
pub fn get_closed(tasks: &FastHashMap<String, Task>) -> Vec<&Task> {
    tasks
        .values()
        .filter(|t| t.status == TaskStatus::Closed)
        .collect()
}

/// Find a task by ID or prefix
///
/// Accepts full IDs or unique prefixes. Returns the task or an error
/// (TaskNotFound, AmbiguousTaskId, SubtaskNotFound, PrefixTooShort).
pub fn find_task<'a>(tasks: &'a FastHashMap<String, Task>, id_or_prefix: &str) -> Result<&'a Task> {
    // Fast path: exact match
    if let Some(task) = tasks.get(id_or_prefix) {
        return Ok(task);
    }

    // Try prefix resolution (without slug support — use find_task_in_graph for slug resolution)
    let full_id = resolve_task_id_internal(tasks, None, id_or_prefix)?;
    tasks.get(&full_id).ok_or_else(|| AikiError::TaskNotFound(full_id))
}

/// Find a task by ID, prefix, or slug notation (parent:slug).
///
/// Like `find_task`, but accepts a `TaskGraph` to enable slug resolution.
pub fn find_task_in_graph<'a>(graph: &'a super::graph::TaskGraph, id_or_prefix: &str) -> Result<&'a Task> {
    // Fast path: exact match
    if let Some(task) = graph.tasks.get(id_or_prefix) {
        return Ok(task);
    }

    // Try resolution with slug support
    let full_id = resolve_task_id_internal(&graph.tasks, Some(&graph.slug_index), id_or_prefix)?;
    graph.tasks.get(&full_id).ok_or_else(|| AikiError::TaskNotFound(full_id))
}

/// Resolve a task ID prefix to a full ID
///
/// Use this when you need the resolved ID string (e.g., for batch validation
/// or before the task map is available). Most call sites should use `find_task`.
pub fn resolve_task_id(tasks: &FastHashMap<String, Task>, prefix: &str) -> Result<String> {
    resolve_task_id_internal(tasks, None, prefix)
}

/// Resolve a task ID prefix to a full ID, with slug support.
pub fn resolve_task_id_in_graph(graph: &super::graph::TaskGraph, prefix: &str) -> Result<String> {
    resolve_task_id_internal(&graph.tasks, Some(&graph.slug_index), prefix)
}

/// Internal helper for prefix resolution
fn resolve_task_id_internal(
    tasks: &FastHashMap<String, Task>,
    slug_index: Option<&FastHashMap<(String, String), String>>,
    prefix: &str,
) -> Result<String> {
    // Fast path: exact match
    if tasks.contains_key(prefix) {
        return Ok(prefix.to_string());
    }

    // Colon notation: "parent_ref:slug"
    if let Some((parent_ref, slug)) = prefix.split_once(':') {
        if let Some(slug_idx) = slug_index {
            let parent_id = resolve_task_id_internal(tasks, Some(slug_idx), parent_ref)?;
            let key = (parent_id.clone(), slug.to_string());
            if let Some(child_id) = slug_idx.get(&key) {
                return Ok(child_id.clone());
            }
            return Err(AikiError::SubtaskNotFound {
                root: parent_id,
                subtask: slug.to_string(),
            });
        }
        // No slug index available — fall through to prefix resolution
    }

    // Subtask prefix: "mvslrsp.1"
    if let Some((root_prefix, suffix)) = prefix.split_once('.') {
        let full_root = resolve_root_prefix(tasks, root_prefix)?;
        let full_id = format!("{}.{}", full_root, suffix);

        // Verify subtask exists
        if tasks.contains_key(&full_id) {
            Ok(full_id)
        } else {
            Err(AikiError::SubtaskNotFound {
                root: full_root,
                subtask: suffix.to_string(),
            })
        }
    } else {
        // Root prefix: "mvslrsp"
        resolve_root_prefix(tasks, prefix)
    }
}

/// Resolve a root task prefix (no dots)
fn resolve_root_prefix(tasks: &FastHashMap<String, Task>, prefix: &str) -> Result<String> {
    // Enforce minimum prefix length (3 chars)
    if prefix.len() < 3 {
        return Err(AikiError::PrefixTooShort { prefix: prefix.to_string() });
    }

    // Collect unique root IDs matching the prefix
    let mut matches: Vec<String> = tasks
        .keys()
        .filter_map(|id| {
            let root = id.split('.').next().unwrap();
            if root.starts_with(prefix) {
                Some(root.to_string())
            } else {
                None
            }
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    matches.sort();

    match matches.len() {
        0 => Err(AikiError::TaskNotFound(prefix.to_string())),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => {
            let match_list = matches
                .iter()
                .filter_map(|id| tasks.get(id).map(|t| format!("  {} — {}", &id[..id.len().min(8)], t.name)))
                .collect::<Vec<_>>()
                .join("\n");

            Err(AikiError::AmbiguousTaskId {
                prefix: prefix.to_string(),
                count: matches.len(),
                matches: match_list,
            })
        }
    }
}

/// Check if a task has any subtasks (using graph edge lookups)
#[must_use]
pub fn has_subtasks(graph: &super::graph::TaskGraph, parent_id: &str) -> bool {
    !graph.edges.referrers(parent_id, "subtask-of").is_empty()
}

/// Get direct subtasks of a task (using graph edge lookups)
#[must_use]
pub fn get_subtasks<'a>(graph: &'a super::graph::TaskGraph, parent_id: &str) -> Vec<&'a Task> {
    graph
        .edges
        .referrers(parent_id, "subtask-of")
        .iter()
        .filter_map(|id| graph.tasks.get(id))
        .collect()
}

/// Get the ready queue filtered by scope (using graph edge lookups)
///
/// When scope is None, returns only root-level tasks (no parent).
/// When scope is Some(parent_id), returns only direct subtasks of that parent.
#[must_use]
pub fn get_scoped_ready_queue<'a>(
    graph: &'a super::graph::TaskGraph,
    scope: Option<&str>,
) -> Vec<&'a Task> {
    let mut ready: Vec<&Task> = graph
        .tasks
        .values()
        .filter(|t| t.status == TaskStatus::Open)
        .filter(|t| !graph.is_blocked(&t.id))
        .filter(|t| match scope {
            None => graph.edges.target(&t.id, "subtask-of").is_none(), // Root-level tasks only
            Some(parent_id) => graph.edges.target(&t.id, "subtask-of") == Some(parent_id),
        })
        .collect();

    ready.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    ready
}

/// Determine the current scope set based on in-progress tasks (using graph edge lookups)
///
/// Returns a `ScopeSet` containing:
/// - `include_root`: true if any root task is in-progress
/// - `scopes`: unique parent IDs of in-progress child tasks
#[must_use]
pub fn get_current_scope_set(graph: &super::graph::TaskGraph) -> ScopeSet {
    let in_progress = get_in_progress(&graph.tasks);

    let mut include_root = false;
    let mut scopes: Vec<String> = Vec::new();

    for task in in_progress {
        if let Some(parent_id) = graph.edges.target(&task.id, "subtask-of") {
            scopes.push(parent_id.to_string());
        } else {
            include_root = true;
        }
    }

    scopes.sort();
    scopes.dedup();

    ScopeSet {
        include_root,
        scopes,
    }
}

/// Get current scopes as a Vec (for backward compatibility)
#[must_use]
#[allow(dead_code)] // Part of task manager API
pub fn get_current_scopes(graph: &super::graph::TaskGraph) -> Vec<String> {
    get_current_scope_set(graph).scopes
}

/// Check if all subtasks of a parent are closed (using graph edge lookups)
#[must_use]
pub fn all_subtasks_closed(graph: &super::graph::TaskGraph, parent_id: &str) -> bool {
    let subtasks = get_subtasks(graph, parent_id);
    !subtasks.is_empty() && subtasks.iter().all(|t| t.status == TaskStatus::Closed)
}

/// Get unclosed subtasks of a parent (using graph edge lookups)
#[must_use]
#[allow(dead_code)] // Part of task manager API
pub fn get_unclosed_subtasks<'a>(
    graph: &'a super::graph::TaskGraph,
    parent_id: &str,
) -> Vec<&'a Task> {
    get_subtasks(graph, parent_id)
        .into_iter()
        .filter(|t| t.status != TaskStatus::Closed)
        .collect()
}

/// Get all unclosed descendants of a parent (recursive, using graph edge lookups)
///
/// Returns all descendants (subtasks, grandsubtasks, etc.) that are not closed,
/// in depth-first order (deepest first, so they can be closed bottom-up).
#[must_use]
pub fn get_all_unclosed_descendants<'a>(
    graph: &'a super::graph::TaskGraph,
    parent_id: &str,
) -> Vec<&'a Task> {
    let mut result = Vec::new();
    collect_unclosed_descendants(graph, parent_id, &mut result);
    result
}

/// Helper for recursive descent - collects descendants depth-first (using graph edge lookups)
fn collect_unclosed_descendants<'a>(
    graph: &'a super::graph::TaskGraph,
    parent_id: &str,
    result: &mut Vec<&'a Task>,
) {
    for subtask in get_subtasks(graph, parent_id) {
        collect_unclosed_descendants(graph, &subtask.id, result);
        if subtask.status != TaskStatus::Closed {
            result.push(subtask);
        }
    }
}

/// Get ready queue based on a ScopeSet (using graph edge lookups)
///
/// When include_root is true, includes root-level tasks.
/// When scopes has entries, includes tasks from those scopes.
/// When scope_set is empty (no in-progress tasks), defaults to root-level tasks.
/// Merges and deduplicates when multiple sources are active.
#[must_use]
pub fn get_ready_queue_for_scope_set<'a>(
    graph: &'a super::graph::TaskGraph,
    scope_set: &ScopeSet,
) -> Vec<&'a Task> {
    use std::collections::HashSet;

    let mut seen: HashSet<&str> = HashSet::new();
    let mut ready: Vec<&Task> = Vec::new();

    if scope_set.include_root || scope_set.is_empty() {
        for task in get_scoped_ready_queue(graph, None) {
            if seen.insert(&task.id) {
                ready.push(task);
            }
        }
    }

    for scope in &scope_set.scopes {
        for task in get_scoped_ready_queue(graph, Some(scope)) {
            if seen.insert(&task.id) {
                ready.push(task);
            }
        }
    }

    ready.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    ready
}

/// Get ready queue filtered for a specific agent
///
/// Returns open, unblocked tasks that are visible to the given agent:
/// - Unassigned tasks (visible to all)
/// - Tasks assigned to this specific agent
///
/// Excludes:
/// - Tasks assigned to "human"
/// - Tasks assigned to other agents
/// - Blocked tasks
#[must_use]
pub fn get_ready_queue_for_agent<'a>(
    graph: &'a super::graph::TaskGraph,
    agent: &AgentType,
) -> Vec<&'a Task> {
    let mut ready: Vec<&Task> = graph
        .tasks
        .values()
        .filter(|t| t.status == TaskStatus::Open)
        .filter(|t| !graph.is_blocked(&t.id))
        .filter(|t| {
            let assignee = t
                .assignee
                .as_ref()
                .and_then(|s| Assignee::from_str(s))
                .unwrap_or(Assignee::Unassigned);
            assignee.is_visible_to(agent)
        })
        .collect();

    ready.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    ready
}

/// Get ready queue filtered for human visibility
///
/// Returns open, unblocked tasks that are visible to humans:
/// - Unassigned tasks
/// - Tasks assigned to "human"
///
/// Excludes:
/// - Tasks assigned to any agent
/// - Blocked tasks
#[must_use]
#[allow(dead_code)] // Part of task manager API
pub fn get_ready_queue_for_human(graph: &super::graph::TaskGraph) -> Vec<&'_ Task> {
    let mut ready: Vec<&Task> = graph
        .tasks
        .values()
        .filter(|t| t.status == TaskStatus::Open)
        .filter(|t| !graph.is_blocked(&t.id))
        .filter(|t| {
            let assignee = t
                .assignee
                .as_ref()
                .and_then(|s| Assignee::from_str(s))
                .unwrap_or(Assignee::Unassigned);
            assignee.is_visible_to_human()
        })
        .collect();

    ready.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    ready
}

/// Get ready queue for a scope set, filtered for a specific agent
///
/// Combines scope filtering with agent visibility filtering.
#[must_use]
pub fn get_ready_queue_for_agent_scoped<'a>(
    graph: &'a super::graph::TaskGraph,
    scope_set: &ScopeSet,
    agent: &AgentType,
) -> Vec<&'a Task> {
    let scoped = get_ready_queue_for_scope_set(graph, scope_set);
    scoped
        .into_iter()
        .filter(|t| {
            let assignee = t
                .assignee
                .as_ref()
                .and_then(|s| Assignee::from_str(s))
                .unwrap_or(Assignee::Unassigned);
            assignee.is_visible_to(agent)
        })
        .collect()
}

/// Get task activity during a specific turn.
///
/// Returns a TaskActivity with tasks that transitioned during the turn.
/// Uses the materialized task graph — turn_started/turn_closed/turn_stopped
/// are captured during event replay.
pub fn get_task_activity_by_turn(
    graph: &super::graph::TaskGraph,
    turn_id: &str,
) -> super::types::TaskActivity {
    use super::types::{TaskActivity, TaskReference};

    let mut activity = TaskActivity::default();

    for task in graph.tasks.values() {
        if task.turn_closed.as_deref() == Some(turn_id) {
            activity.closed.push(TaskReference {
                id: task.id.clone(),
                name: task.name.clone(),
                task_type: task.task_type.clone(),
            });
        }
        if task.turn_started.as_deref() == Some(turn_id) {
            activity.started.push(TaskReference {
                id: task.id.clone(),
                name: task.name.clone(),
                task_type: task.task_type.clone(),
            });
        }
        if task.turn_stopped.as_deref() == Some(turn_id) {
            activity.stopped.push(TaskReference {
                id: task.id.clone(),
                name: task.name.clone(),
                task_type: task.task_type.clone(),
            });
        }
    }

    activity
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::graph::{materialize_graph, TaskGraph};
    use crate::tasks::types::{TaskEvent, TaskOutcome, TaskPriority};
    use chrono::Utc;

    /// Helper: materialize a graph from events (tests that need edge lookups)
    fn make_graph(events: &[TaskEvent]) -> TaskGraph {
        materialize_graph(events)
    }

    fn make_created_event(
        task_id: &str,
        name: &str,
        priority: TaskPriority,
        hours_ago: i64,
    ) -> TaskEvent {
        TaskEvent::Created {
            task_id: task_id.to_string(),
            name: name.to_string(),
            slug: None,
            task_type: None,
            priority,
            assignee: None,
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data: std::collections::HashMap::new(),
            timestamp: Utc::now() - chrono::Duration::hours(hours_ago),
        }
    }

    fn make_started_event(task_id: &str) -> TaskEvent {
        TaskEvent::Started {
            task_ids: vec![task_id.to_string()],
            agent_type: "claude-code".to_string(),
            session_id: None,
            turn_id: None,
            timestamp: Utc::now(),
        }
    }

    fn make_stopped_event(task_id: &str, reason: Option<&str>) -> TaskEvent {
        TaskEvent::Stopped {
            task_ids: vec![task_id.to_string()],
            reason: reason.map(|s| s.to_string()),
            turn_id: None,
            timestamp: Utc::now(),
        }
    }

    fn make_closed_event(task_id: &str, outcome: TaskOutcome) -> TaskEvent {
        TaskEvent::Closed {
            task_ids: vec![task_id.to_string()],
            outcome,
            summary: None,
            turn_id: None,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_materialize_single_task() {
        let events = vec![make_created_event("a1b2", "Test task", TaskPriority::P2, 1)];

        let tasks = materialize_graph(&events).tasks;

        assert_eq!(tasks.len(), 1);
        let task = tasks.get("a1b2").unwrap();
        assert_eq!(task.name, "Test task");
        assert_eq!(task.status, TaskStatus::Open);
        assert_eq!(task.priority, TaskPriority::P2);
    }

    #[test]
    fn test_materialize_task_lifecycle() {
        let events = vec![
            make_created_event("a1b2", "Test task", TaskPriority::P2, 1),
            make_started_event("a1b2"),
        ];

        let tasks = materialize_graph(&events).tasks;
        let task = tasks.get("a1b2").unwrap();
        assert_eq!(task.status, TaskStatus::InProgress);

        // Add stop event
        let events = vec![
            make_created_event("a1b2", "Test task", TaskPriority::P2, 1),
            make_started_event("a1b2"),
            make_stopped_event("a1b2", Some("Need info")),
        ];

        let tasks = materialize_graph(&events).tasks;
        let task = tasks.get("a1b2").unwrap();
        assert_eq!(task.status, TaskStatus::Stopped);
        assert_eq!(task.stopped_reason, Some("Need info".to_string()));

        // Add close event
        let events = vec![
            make_created_event("a1b2", "Test task", TaskPriority::P2, 1),
            make_started_event("a1b2"),
            make_stopped_event("a1b2", Some("Need info")),
            make_started_event("a1b2"),
            make_closed_event("a1b2", TaskOutcome::Done),
        ];

        let tasks = materialize_graph(&events).tasks;
        let task = tasks.get("a1b2").unwrap();
        assert_eq!(task.status, TaskStatus::Closed);
        assert_eq!(task.closed_outcome, Some(TaskOutcome::Done));
    }

    #[test]
    fn test_ready_queue_priority_sorting() {
        let events = vec![
            make_created_event("low", "Low priority", TaskPriority::P3, 1),
            make_created_event("high", "High priority", TaskPriority::P1, 1),
            make_created_event("critical", "Critical", TaskPriority::P0, 1),
            make_created_event("normal", "Normal", TaskPriority::P2, 1),
        ];

        let graph = make_graph(&events);
        let ready = get_ready_queue(&graph);

        assert_eq!(ready.len(), 4);
        assert_eq!(ready[0].id, "critical"); // P0 first
        assert_eq!(ready[1].id, "high"); // P1 second
        assert_eq!(ready[2].id, "normal"); // P2 third
        assert_eq!(ready[3].id, "low"); // P3 last
    }

    #[test]
    fn test_ready_queue_time_sorting_same_priority() {
        let events = vec![
            make_created_event("newer", "Newer task", TaskPriority::P2, 1),
            make_created_event("older", "Older task", TaskPriority::P2, 3),
        ];

        let graph = make_graph(&events);
        let ready = get_ready_queue(&graph);

        assert_eq!(ready.len(), 2);
        assert_eq!(ready[0].id, "older"); // Older first (created 3 hours ago)
        assert_eq!(ready[1].id, "newer"); // Newer second (created 1 hour ago)
    }

    #[test]
    fn test_ready_queue_excludes_non_open() {
        let events = vec![
            make_created_event("open", "Open task", TaskPriority::P2, 1),
            make_created_event("started", "Started task", TaskPriority::P2, 1),
            make_started_event("started"),
            make_created_event("closed", "Closed task", TaskPriority::P2, 1),
            make_closed_event("closed", TaskOutcome::Done),
        ];

        let graph = make_graph(&events);
        let ready = get_ready_queue(&graph);

        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, "open");
    }

    #[test]
    fn test_get_in_progress() {
        let events = vec![
            make_created_event("task1", "Task 1", TaskPriority::P2, 1),
            make_created_event("task2", "Task 2", TaskPriority::P2, 1),
            make_started_event("task1"),
        ];

        let tasks = materialize_graph(&events).tasks;
        let in_progress = get_in_progress(&tasks);

        assert_eq!(in_progress.len(), 1);
        assert_eq!(in_progress[0].id, "task1");
    }

    #[test]
    fn test_find_task() {
        let events = vec![make_created_event("a1b2", "Test", TaskPriority::P2, 1)];

        let tasks = materialize_graph(&events).tasks;

        assert!(find_task(&tasks, "a1b2").is_ok());
        assert!(find_task(&tasks, "nonexistent").is_err());
    }

    #[test]
    fn test_has_subtasks() {
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 1),
            make_created_event("parent.1", "Child 1", TaskPriority::P2, 1),
            make_created_event("parent.2", "Child 2", TaskPriority::P2, 1),
            make_created_event("other", "Other", TaskPriority::P2, 1),
        ];

        let graph = make_graph(&events);

        assert!(has_subtasks(&graph, "parent"));
        assert!(!has_subtasks(&graph, "parent.1"));
        assert!(!has_subtasks(&graph, "other"));
        assert!(!has_subtasks(&graph, "nonexistent"));
    }

    #[test]
    fn test_get_subtasks() {
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 1),
            make_created_event("parent.1", "Child 1", TaskPriority::P2, 1),
            make_created_event("parent.2", "Child 2", TaskPriority::P2, 1),
            make_created_event("parent.1.1", "Grandsubtask", TaskPriority::P2, 1),
            make_created_event("other", "Other", TaskPriority::P2, 1),
        ];

        let graph = make_graph(&events);
        let subtasks = get_subtasks(&graph, "parent");

        // Should only get direct subtasks, not grandsubtasks
        assert_eq!(subtasks.len(), 2);
        let subtask_ids: Vec<_> = subtasks.iter().map(|t| t.id.as_str()).collect();
        assert!(subtask_ids.contains(&"parent.1"));
        assert!(subtask_ids.contains(&"parent.2"));
        assert!(!subtask_ids.contains(&"parent.1.1"));
    }

    #[test]
    fn test_get_scoped_ready_queue_no_scope() {
        let events = vec![
            make_created_event("root1", "Root 1", TaskPriority::P2, 1),
            make_created_event("root2", "Root 2", TaskPriority::P1, 1),
            make_created_event("root1.1", "Child", TaskPriority::P0, 1),
        ];

        let graph = make_graph(&events);
        let ready = get_scoped_ready_queue(&graph, None);

        // Should only get root-level tasks, not subtasks
        assert_eq!(ready.len(), 2);
        assert_eq!(ready[0].id, "root2"); // P1 first
        assert_eq!(ready[1].id, "root1"); // P2 second
    }

    #[test]
    fn test_get_scoped_ready_queue_with_scope() {
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 1),
            make_created_event("parent.1", "Child 1", TaskPriority::P2, 1),
            make_created_event("parent.2", "Child 2", TaskPriority::P0, 1),
            make_created_event("parent.1.1", "Grandchild", TaskPriority::P0, 1),
            make_created_event("other", "Other root", TaskPriority::P0, 1),
        ];

        let graph = make_graph(&events);
        let ready = get_scoped_ready_queue(&graph, Some("parent"));

        // Should only get direct subtasks of parent
        assert_eq!(ready.len(), 2);
        assert_eq!(ready[0].id, "parent.2"); // P0 first
        assert_eq!(ready[1].id, "parent.1"); // P2 second
    }

    #[test]
    fn test_get_current_scopes() {
        // No in-progress tasks -> no scopes
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 1),
            make_created_event("parent.1", "Child", TaskPriority::P2, 1),
        ];
        let graph = make_graph(&events);
        assert!(get_current_scopes(&graph).is_empty());

        // In-progress root task -> no scopes
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 1),
            make_started_event("parent"),
        ];
        let graph = make_graph(&events);
        assert!(get_current_scopes(&graph).is_empty());

        // In-progress child task -> scope is parent
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 1),
            make_created_event("parent.1", "Child", TaskPriority::P2, 1),
            make_started_event("parent.1"),
        ];
        let graph = make_graph(&events);
        assert_eq!(get_current_scopes(&graph), vec!["parent".to_string()]);
    }

    #[test]
    fn test_get_current_scopes_multi_parent() {
        // Two in-progress subtasks from different parents -> two scopes
        let events = vec![
            make_created_event("parent1", "Parent 1", TaskPriority::P2, 1),
            make_created_event("parent2", "Parent 2", TaskPriority::P2, 1),
            make_created_event("parent1.1", "Child 1.1", TaskPriority::P2, 1),
            make_created_event("parent2.1", "Child 2.1", TaskPriority::P2, 1),
            TaskEvent::Started {
                task_ids: vec!["parent1.1".to_string(), "parent2.1".to_string()],
                agent_type: "claude-code".to_string(),
                session_id: None,
                turn_id: None,
                timestamp: Utc::now(),
                },
        ];
        let graph = make_graph(&events);
        let scopes = get_current_scopes(&graph);
        assert_eq!(scopes.len(), 2);
        assert!(scopes.contains(&"parent1".to_string()));
        assert!(scopes.contains(&"parent2".to_string()));
    }

    #[test]
    fn test_get_current_scopes_same_parent() {
        // Two in-progress subtasks from same parent -> one scope (deduplicated)
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 1),
            make_created_event("parent.1", "Child 1", TaskPriority::P2, 1),
            make_created_event("parent.2", "Child 2", TaskPriority::P2, 1),
            TaskEvent::Started {
                task_ids: vec!["parent.1".to_string(), "parent.2".to_string()],
                agent_type: "claude-code".to_string(),
                session_id: None,
                turn_id: None,
                timestamp: Utc::now(),
                },
        ];
        let graph = make_graph(&events);
        let scopes = get_current_scopes(&graph);
        assert_eq!(scopes, vec!["parent".to_string()]);
    }

    #[test]
    fn test_all_subtasks_closed() {
        // No subtasks -> returns false
        let events = vec![make_created_event("parent", "Parent", TaskPriority::P2, 1)];
        let graph = make_graph(&events);
        assert!(!all_subtasks_closed(&graph, "parent"));

        // Some subtasks open -> returns false
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 1),
            make_created_event("parent.1", "Child 1", TaskPriority::P2, 1),
            make_created_event("parent.2", "Child 2", TaskPriority::P2, 1),
            make_closed_event("parent.1", TaskOutcome::Done),
        ];
        let graph = make_graph(&events);
        assert!(!all_subtasks_closed(&graph, "parent"));

        // All subtasks closed -> returns true
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 1),
            make_created_event("parent.1", "Child 1", TaskPriority::P2, 1),
            make_created_event("parent.2", "Child 2", TaskPriority::P2, 1),
            make_closed_event("parent.1", TaskOutcome::Done),
            make_closed_event("parent.2", TaskOutcome::Done),
        ];
        let graph = make_graph(&events);
        assert!(all_subtasks_closed(&graph, "parent"));
    }

    #[test]
    fn test_get_unclosed_subtasks() {
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 1),
            make_created_event("parent.1", "Child 1", TaskPriority::P2, 1),
            make_created_event("parent.2", "Child 2", TaskPriority::P2, 1),
            make_created_event("parent.3", "Child 3", TaskPriority::P2, 1),
            make_closed_event("parent.1", TaskOutcome::Done),
        ];

        let graph = make_graph(&events);
        let unclosed = get_unclosed_subtasks(&graph, "parent");

        assert_eq!(unclosed.len(), 2);
        let ids: Vec<_> = unclosed.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"parent.2"));
        assert!(ids.contains(&"parent.3"));
    }

    #[test]
    fn test_get_all_unclosed_descendants_flat() {
        // Test with only direct subtasks (no grandsubtasks)
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 4),
            make_created_event("parent.1", "Child 1", TaskPriority::P2, 3),
            make_created_event("parent.2", "Child 2", TaskPriority::P2, 2),
            make_created_event("parent.3", "Child 3", TaskPriority::P2, 1),
            make_closed_event("parent.1", TaskOutcome::Done),
        ];

        let graph = make_graph(&events);
        let descendants = get_all_unclosed_descendants(&graph, "parent");

        assert_eq!(descendants.len(), 2);
        let ids: Vec<_> = descendants.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"parent.2"));
        assert!(ids.contains(&"parent.3"));
    }

    #[test]
    fn test_get_all_unclosed_descendants_nested() {
        // Test with grandsubtasks - should return depth-first (deepest first)
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 6),
            make_created_event("parent.1", "Child 1", TaskPriority::P2, 5),
            make_created_event("parent.2", "Child 2", TaskPriority::P2, 4),
            make_created_event("parent.1.1", "Grandchild 1.1", TaskPriority::P2, 3),
            make_created_event("parent.1.2", "Grandchild 1.2", TaskPriority::P2, 2),
            make_created_event("parent.2.1", "Grandchild 2.1", TaskPriority::P2, 1),
        ];

        let graph = make_graph(&events);
        let descendants = get_all_unclosed_descendants(&graph, "parent");

        // Should have all 5 descendants
        assert_eq!(descendants.len(), 5);
        let ids: Vec<_> = descendants.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"parent.1"));
        assert!(ids.contains(&"parent.2"));
        assert!(ids.contains(&"parent.1.1"));
        assert!(ids.contains(&"parent.1.2"));
        assert!(ids.contains(&"parent.2.1"));

        // Grandsubtasks should come before their parents (depth-first)
        let pos_1 = ids.iter().position(|id| *id == "parent.1").unwrap();
        let pos_1_1 = ids.iter().position(|id| *id == "parent.1.1").unwrap();
        let pos_1_2 = ids.iter().position(|id| *id == "parent.1.2").unwrap();
        assert!(
            pos_1_1 < pos_1,
            "Grandsubtask 1.1 should come before Child 1"
        );
        assert!(
            pos_1_2 < pos_1,
            "Grandsubtask 1.2 should come before Child 1"
        );
    }

    #[test]
    fn test_get_all_unclosed_descendants_some_closed() {
        // Test with some descendants already closed
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 5),
            make_created_event("parent.1", "Child 1", TaskPriority::P2, 4),
            make_created_event("parent.2", "Child 2", TaskPriority::P2, 3),
            make_created_event("parent.1.1", "Grandchild 1.1", TaskPriority::P2, 2),
            make_closed_event("parent.1", TaskOutcome::Done),
            make_closed_event("parent.1.1", TaskOutcome::Done),
        ];

        let graph = make_graph(&events);
        let descendants = get_all_unclosed_descendants(&graph, "parent");

        // Only parent.2 should be unclosed
        assert_eq!(descendants.len(), 1);
        assert_eq!(descendants[0].id, "parent.2");
    }

    #[test]
    fn test_get_all_unclosed_descendants_empty() {
        // Test with no subtasks
        let events = vec![make_created_event("parent", "Parent", TaskPriority::P2, 1)];

        let graph = make_graph(&events);
        let descendants = get_all_unclosed_descendants(&graph, "parent");

        assert!(descendants.is_empty());
    }

    #[test]
    fn test_scope_set_only_root() {
        // Root task in-progress → include_root=true, scopes=[]
        let events = vec![
            make_created_event("root1", "Root task", TaskPriority::P2, 1),
            make_started_event("root1"),
        ];
        let graph = make_graph(&events);
        let scope_set = get_current_scope_set(&graph);

        assert!(scope_set.include_root);
        assert!(scope_set.scopes.is_empty());
    }

    #[test]
    fn test_scope_set_root_and_child() {
        // Root task + child task both in-progress
        // → include_root=true, scopes=[parent_of_child]
        let events = vec![
            make_created_event("root1", "Root task", TaskPriority::P2, 1),
            make_created_event("parent", "Parent task", TaskPriority::P2, 2),
            make_created_event("parent.1", "Child task", TaskPriority::P2, 3),
            make_started_event("root1"),
            make_started_event("parent.1"),
        ];
        let graph = make_graph(&events);
        let scope_set = get_current_scope_set(&graph);

        assert!(scope_set.include_root);
        assert_eq!(scope_set.scopes, vec!["parent".to_string()]);
    }

    #[test]
    fn test_scope_set_only_child() {
        // Only child task in-progress → include_root=false, scopes=[parent]
        let events = vec![
            make_created_event("parent", "Parent task", TaskPriority::P2, 1),
            make_created_event("parent.1", "Child task", TaskPriority::P2, 2),
            make_started_event("parent.1"),
        ];
        let graph = make_graph(&events);
        let scope_set = get_current_scope_set(&graph);

        assert!(!scope_set.include_root);
        assert_eq!(scope_set.scopes, vec!["parent".to_string()]);
    }

    #[test]
    fn test_scope_set_is_empty() {
        // No in-progress tasks → is_empty() = true
        let events = vec![make_created_event("task1", "Task 1", TaskPriority::P2, 1)];
        let graph = make_graph(&events);
        let scope_set = get_current_scope_set(&graph);

        assert!(!scope_set.include_root);
        assert!(scope_set.scopes.is_empty());
        assert!(scope_set.is_empty());
    }

    #[test]
    fn test_scope_set_deeply_nested() {
        // Deeply nested task in-progress (3 levels: parent.1.1)
        // Should set scope to "parent.1", not "parent"
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 4),
            make_created_event("parent.1", "Child", TaskPriority::P2, 3),
            make_created_event("parent.1.1", "Grandchild", TaskPriority::P2, 2),
            make_started_event("parent.1.1"),
        ];
        let graph = make_graph(&events);
        let scope_set = get_current_scope_set(&graph);

        assert!(!scope_set.include_root);
        // Scope should be the DIRECT parent of the in-progress task
        assert_eq!(scope_set.scopes, vec!["parent.1".to_string()]);
    }

    #[test]
    fn test_scope_set_multiple_depths() {
        // Multiple in-progress tasks at different hierarchy depths
        // root task + grandchild task
        let events = vec![
            make_created_event("root", "Root", TaskPriority::P2, 5),
            make_created_event("parent", "Parent", TaskPriority::P2, 4),
            make_created_event("parent.1", "Child", TaskPriority::P2, 3),
            make_created_event("parent.1.1", "Grandchild", TaskPriority::P2, 2),
            make_started_event("root"),
            make_started_event("parent.1.1"),
        ];
        let graph = make_graph(&events);
        let scope_set = get_current_scope_set(&graph);

        assert!(scope_set.include_root); // root task is in-progress
        assert_eq!(scope_set.scopes, vec!["parent.1".to_string()]); // grandchild's parent
    }

    #[test]
    fn test_scope_set_deduplication() {
        // Multiple subtasks of same parent in-progress → only one scope entry
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 4),
            make_created_event("parent.1", "Child 1", TaskPriority::P2, 3),
            make_created_event("parent.2", "Child 2", TaskPriority::P2, 2),
            make_created_event("parent.3", "Child 3", TaskPriority::P2, 1),
            make_started_event("parent.1"),
            make_started_event("parent.2"),
            make_started_event("parent.3"),
        ];
        let graph = make_graph(&events);
        let scope_set = get_current_scope_set(&graph);

        assert!(!scope_set.include_root);
        // Should deduplicate to single "parent" scope
        assert_eq!(scope_set.scopes, vec!["parent".to_string()]);
    }

    #[test]
    fn test_materialize_empty_events() {
        let events: Vec<TaskEvent> = vec![];
        let tasks = materialize_graph(&events).tasks;
        assert!(tasks.is_empty());
    }

    #[test]
    fn test_ready_queue_empty_tasks() {
        let events: Vec<TaskEvent> = vec![];
        let graph = make_graph(&events);
        let ready = get_ready_queue(&graph);
        assert!(ready.is_empty());
    }

    #[test]
    fn test_scoped_ready_queue_empty_tasks() {
        let events: Vec<TaskEvent> = vec![];
        let graph = make_graph(&events);
        let ready = get_scoped_ready_queue(&graph, None);
        assert!(ready.is_empty());

        let ready = get_scoped_ready_queue(&graph, Some("parent"));
        assert!(ready.is_empty());
    }

    #[test]
    fn test_ready_queue_same_priority_same_time() {
        // All tasks with same priority and creation time
        // Should return in deterministic order (by ID as tiebreaker)
        let now = Utc::now();
        let events = vec![
            TaskEvent::Created {
                task_type: None,
                task_id: "task_c".to_string(),
                name: "Task C".to_string(),
                slug: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp: now,
            },
            TaskEvent::Created {
                task_type: None,
                task_id: "task_a".to_string(),
                name: "Task A".to_string(),
                slug: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp: now,
            },
            TaskEvent::Created {
                task_type: None,
                task_id: "task_b".to_string(),
                name: "Task B".to_string(),
                slug: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp: now,
            },
        ];
        let graph = make_graph(&events);
        let ready = get_ready_queue(&graph);

        assert_eq!(ready.len(), 3);
        // With same priority and time, order is deterministic (HashMap iteration order + sort stability)
        // The key thing is that all 3 appear and sorting doesn't crash
        let ids: Vec<_> = ready.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"task_a"));
        assert!(ids.contains(&"task_b"));
        assert!(ids.contains(&"task_c"));
    }

    #[test]
    fn test_task_lifecycle_complete() {
        // Test complete lifecycle: Open → InProgress → Stopped → InProgress → Closed
        let events = vec![
            make_created_event("task1", "Task 1", TaskPriority::P2, 5),
            make_started_event("task1"),
            make_stopped_event("task1", Some("need info")),
            make_started_event("task1"),
            make_closed_event("task1", TaskOutcome::Done),
        ];
        let tasks = materialize_graph(&events).tasks;

        let task = tasks.get("task1").unwrap();
        assert_eq!(task.status, TaskStatus::Closed);
        assert_eq!(task.closed_outcome, Some(TaskOutcome::Done));
    }

    #[test]
    fn test_find_task_nonexistent() {
        let events = vec![make_created_event("task1", "Task 1", TaskPriority::P2, 1)];
        let tasks = materialize_graph(&events).tasks;

        assert!(find_task(&tasks, "nonexistent").is_err());
        assert!(find_task(&tasks, "").is_err());
    }

    #[test]
    fn test_get_subtasks_excludes_grandsubtasks() {
        // Verify get_subtasks only returns direct subtasks, not grandsubtasks
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 4),
            make_created_event("parent.1", "Child", TaskPriority::P2, 3),
            make_created_event("parent.1.1", "Grandsubtask", TaskPriority::P2, 2),
            make_created_event("parent.2", "Child 2", TaskPriority::P2, 1),
        ];
        let graph = make_graph(&events);

        let subtasks = get_subtasks(&graph, "parent");
        let ids: Vec<_> = subtasks.iter().map(|t| t.id.as_str()).collect();

        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"parent.1"));
        assert!(ids.contains(&"parent.2"));
        assert!(!ids.contains(&"parent.1.1")); // Grandsubtask excluded
    }

    #[test]
    fn test_has_subtasks_with_only_grandsubtasks() {
        // Parent has grandsubtasks but no direct subtasks
        // This is an edge case - should return false
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 3),
            make_created_event("parent.1", "Child", TaskPriority::P2, 2),
            make_created_event("parent.1.1", "Grandsubtask", TaskPriority::P2, 1),
        ];
        let graph = make_graph(&events);

        // parent.1 has subtasks (grandsubtask of parent)
        assert!(has_subtasks(&graph, "parent.1"));
        // parent has subtasks (direct subtask parent.1)
        assert!(has_subtasks(&graph, "parent"));
        // parent.1.1 has no subtasks
        assert!(!has_subtasks(&graph, "parent.1.1"));
    }

    #[test]
    fn test_in_progress_excludes_stopped() {
        // Stopped tasks should not be in in_progress list
        let events = vec![
            make_created_event("task1", "Task 1", TaskPriority::P2, 3),
            make_created_event("task2", "Task 2", TaskPriority::P2, 2),
            make_started_event("task1"),
            make_started_event("task2"),
            make_stopped_event("task1", None),
        ];
        let tasks = materialize_graph(&events).tasks;

        let in_progress = get_in_progress(&tasks);
        let ids: Vec<_> = in_progress.iter().map(|t| t.id.as_str()).collect();

        assert_eq!(ids.len(), 1);
        assert!(ids.contains(&"task2"));
        assert!(!ids.contains(&"task1"));
    }

    // Tests for get_ready_queue_for_scope_set

    #[test]
    fn test_scope_set_queue_empty_scope_set_defaults_to_root() {
        // Empty ScopeSet (no in-progress tasks) should default to root-level tasks
        // This ensures `task list` works when nothing is in progress
        let events = vec![
            make_created_event("root1", "Root 1", TaskPriority::P2, 2),
            make_created_event("root2", "Root 2", TaskPriority::P1, 1),
            make_created_event("parent.1", "Child 1", TaskPriority::P0, 1),
        ];
        let graph = make_graph(&events);

        let scope_set = ScopeSet {
            include_root: false,
            scopes: vec![],
        };

        let ready = get_ready_queue_for_scope_set(&graph, &scope_set);
        let ids: Vec<_> = ready.iter().map(|t| t.id.as_str()).collect();

        // Should return root-level tasks only (not subtasks)
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"root1"));
        assert!(ids.contains(&"root2"));
        assert!(!ids.contains(&"parent.1")); // Child excluded
    }

    #[test]
    fn test_scope_set_queue_root_only() {
        // include_root=true should return root-level tasks only
        let events = vec![
            make_created_event("root1", "Root 1", TaskPriority::P2, 2),
            make_created_event("root2", "Root 2", TaskPriority::P1, 1),
            make_created_event("parent.1", "Child 1", TaskPriority::P0, 1),
        ];
        let graph = make_graph(&events);

        let scope_set = ScopeSet {
            include_root: true,
            scopes: vec![],
        };

        let ready = get_ready_queue_for_scope_set(&graph, &scope_set);
        let ids: Vec<_> = ready.iter().map(|t| t.id.as_str()).collect();

        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"root1"));
        assert!(ids.contains(&"root2"));
        assert!(!ids.contains(&"parent.1")); // Child not included
    }

    #[test]
    fn test_scope_set_queue_scoped_only() {
        // Single scope should return only that scope's subtasks
        let events = vec![
            make_created_event("root1", "Root 1", TaskPriority::P2, 3),
            make_created_event("parent", "Parent", TaskPriority::P2, 3),
            make_created_event("parent.1", "Child 1", TaskPriority::P0, 2),
            make_created_event("parent.2", "Child 2", TaskPriority::P2, 1),
        ];
        let graph = make_graph(&events);

        let scope_set = ScopeSet {
            include_root: false,
            scopes: vec!["parent".to_string()],
        };

        let ready = get_ready_queue_for_scope_set(&graph, &scope_set);
        let ids: Vec<_> = ready.iter().map(|t| t.id.as_str()).collect();

        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"parent.1"));
        assert!(ids.contains(&"parent.2"));
        assert!(!ids.contains(&"root1")); // Root not included
                                          // Verify priority sorting (P0 first)
        assert_eq!(ready[0].id, "parent.1");
    }

    #[test]
    fn test_scope_set_queue_root_and_scopes() {
        // Both root and scopes should merge results
        // Note: "parent" is also a root-level task so gets included with include_root=true
        let events = vec![
            make_created_event("root1", "Root 1", TaskPriority::P1, 4),
            make_created_event("parent", "Parent", TaskPriority::P3, 3), // P3 so it's last
            make_created_event("parent.1", "Child 1", TaskPriority::P0, 2),
            make_created_event("parent.2", "Child 2", TaskPriority::P2, 1),
        ];
        let graph = make_graph(&events);

        let scope_set = ScopeSet {
            include_root: true,
            scopes: vec!["parent".to_string()],
        };

        let ready = get_ready_queue_for_scope_set(&graph, &scope_set);
        let ids: Vec<_> = ready.iter().map(|t| t.id.as_str()).collect();

        // 4 tasks: root1, parent (root), parent.1, parent.2
        assert_eq!(ids.len(), 4);
        assert!(ids.contains(&"root1"));
        assert!(ids.contains(&"parent"));
        assert!(ids.contains(&"parent.1"));
        assert!(ids.contains(&"parent.2"));
        // Verify priority sorting (P0 first, then P1, then P2, then P3)
        assert_eq!(ready[0].id, "parent.1"); // P0
        assert_eq!(ready[1].id, "root1"); // P1
        assert_eq!(ready[2].id, "parent.2"); // P2
        assert_eq!(ready[3].id, "parent"); // P3
    }

    #[test]
    fn test_scope_set_queue_multiple_scopes() {
        // Multiple scopes should merge and deduplicate
        let events = vec![
            make_created_event("p1", "Parent 1", TaskPriority::P2, 5),
            make_created_event("p2", "Parent 2", TaskPriority::P2, 4),
            make_created_event("p1.1", "P1 Child", TaskPriority::P1, 3),
            make_created_event("p2.1", "P2 Child", TaskPriority::P0, 2),
        ];
        let graph = make_graph(&events);

        let scope_set = ScopeSet {
            include_root: false,
            scopes: vec!["p1".to_string(), "p2".to_string()],
        };

        let ready = get_ready_queue_for_scope_set(&graph, &scope_set);
        let ids: Vec<_> = ready.iter().map(|t| t.id.as_str()).collect();

        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"p1.1"));
        assert!(ids.contains(&"p2.1"));
        // Verify priority sorting
        assert_eq!(ready[0].id, "p2.1"); // P0 first
        assert_eq!(ready[1].id, "p1.1"); // P1 second
    }

    #[test]
    fn test_scope_set_queue_excludes_non_open() {
        // Should only include Open tasks, not InProgress/Stopped/Closed
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 5),
            make_created_event("parent.1", "Open", TaskPriority::P2, 4),
            make_created_event("parent.2", "InProgress", TaskPriority::P2, 3),
            make_created_event("parent.3", "Closed", TaskPriority::P2, 2),
            make_started_event("parent.2"),
            make_closed_event("parent.3", TaskOutcome::Done),
        ];
        let graph = make_graph(&events);

        let scope_set = ScopeSet {
            include_root: false,
            scopes: vec!["parent".to_string()],
        };

        let ready = get_ready_queue_for_scope_set(&graph, &scope_set);
        let ids: Vec<_> = ready.iter().map(|t| t.id.as_str()).collect();

        assert_eq!(ids.len(), 1);
        assert!(ids.contains(&"parent.1"));
        assert!(!ids.contains(&"parent.2")); // InProgress excluded
        assert!(!ids.contains(&"parent.3")); // Closed excluded
    }

    // Tests for ScopeSet::to_xml_scopes

    #[test]
    fn test_to_xml_scopes_empty() {
        let scope_set = ScopeSet {
            include_root: false,
            scopes: vec![],
        };
        assert!(scope_set.to_xml_scopes().is_empty());
    }

    #[test]
    fn test_to_xml_scopes_root_only() {
        let scope_set = ScopeSet {
            include_root: true,
            scopes: vec![],
        };
        assert_eq!(scope_set.to_xml_scopes(), vec!["root".to_string()]);
    }

    #[test]
    fn test_to_xml_scopes_scopes_only() {
        let scope_set = ScopeSet {
            include_root: false,
            scopes: vec!["parent1".to_string(), "parent2".to_string()],
        };
        assert_eq!(
            scope_set.to_xml_scopes(),
            vec!["parent1".to_string(), "parent2".to_string()]
        );
    }

    #[test]
    fn test_to_xml_scopes_root_and_scopes() {
        let scope_set = ScopeSet {
            include_root: true,
            scopes: vec!["parent1".to_string()],
        };
        assert_eq!(
            scope_set.to_xml_scopes(),
            vec!["root".to_string(), "parent1".to_string()]
        );
    }

    #[test]
    fn test_materialize_reopened_event() {
        let base_time = Utc::now();
        let events = vec![
            TaskEvent::Created {
                task_type: None,
                task_id: "a1b2".to_string(),
                name: "Test task".to_string(),
                slug: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp: base_time,
            },
            TaskEvent::Closed {
                task_ids: vec!["a1b2".to_string()],
                outcome: TaskOutcome::Done,
                summary: None,
                turn_id: None,
                timestamp: base_time + chrono::Duration::seconds(1),
            },
            TaskEvent::Reopened {
                task_id: "a1b2".to_string(),
                reason: "Found another bug".to_string(),
                timestamp: base_time + chrono::Duration::seconds(2),
            },
        ];

        let tasks = materialize_graph(&events).tasks;
        let task = tasks.get("a1b2").expect("Task should exist");

        assert_eq!(task.status, TaskStatus::Open);
        // Closed outcome should be cleared when reopened
        assert!(task.closed_outcome.is_none());
    }

    #[test]
    fn test_materialize_comment_added_event() {
        let base_time = Utc::now();
        let events = vec![
            TaskEvent::Created {
                task_type: None,
                task_id: "a1b2".to_string(),
                name: "Test task".to_string(),
                slug: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp: base_time,
            },
            TaskEvent::CommentAdded {
                task_ids: vec!["a1b2".to_string()],
                text: "First comment".to_string(),
                data: std::collections::HashMap::new(),
                timestamp: base_time + chrono::Duration::seconds(1),
            },
            TaskEvent::CommentAdded {
                task_ids: vec!["a1b2".to_string()],
                text: "Second comment".to_string(),
                data: std::collections::HashMap::new(),
                timestamp: base_time + chrono::Duration::seconds(2),
            },
        ];

        let tasks = materialize_graph(&events).tasks;
        let task = tasks.get("a1b2").expect("Task should exist");

        assert_eq!(task.comments.len(), 2);
        assert_eq!(task.comments[0].text, "First comment");
        assert_eq!(task.comments[1].text, "Second comment");
    }

    #[test]
    fn test_materialize_updated_event_name() {
        let base_time = Utc::now();
        let events = vec![
            TaskEvent::Created {
                task_type: None,
                task_id: "a1b2".to_string(),
                name: "Original name".to_string(),
                slug: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp: base_time,
            },
            TaskEvent::Updated {
                task_id: "a1b2".to_string(),
                name: Some("Updated name".to_string()),
                priority: None,
                assignee: None,
                data: None,
                instructions: None,
                timestamp: base_time + chrono::Duration::seconds(1),
            },
        ];

        let tasks = materialize_graph(&events).tasks;
        let task = tasks.get("a1b2").expect("Task should exist");

        assert_eq!(task.name, "Updated name");
        assert_eq!(task.priority, TaskPriority::P2); // Priority unchanged
    }

    #[test]
    fn test_materialize_updated_event_priority() {
        let base_time = Utc::now();
        let events = vec![
            TaskEvent::Created {
                task_type: None,
                task_id: "a1b2".to_string(),
                name: "Test task".to_string(),
                slug: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp: base_time,
            },
            TaskEvent::Updated {
                task_id: "a1b2".to_string(),
                name: None,
                priority: Some(TaskPriority::P0),
                assignee: None,
                data: None,
                instructions: None,
                timestamp: base_time + chrono::Duration::seconds(1),
            },
        ];

        let tasks = materialize_graph(&events).tasks;
        let task = tasks.get("a1b2").expect("Task should exist");

        assert_eq!(task.name, "Test task"); // Name unchanged
        assert_eq!(task.priority, TaskPriority::P0);
    }

    #[test]
    fn test_materialize_updated_event_both_fields() {
        let base_time = Utc::now();
        let events = vec![
            TaskEvent::Created {
                task_type: None,
                task_id: "a1b2".to_string(),
                name: "Original".to_string(),
                slug: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp: base_time,
            },
            TaskEvent::Updated {
                task_id: "a1b2".to_string(),
                name: Some("New name".to_string()),
                priority: Some(TaskPriority::P1),
                assignee: None,
                data: None,
                instructions: None,
                timestamp: base_time + chrono::Duration::seconds(1),
            },
        ];

        let tasks = materialize_graph(&events).tasks;
        let task = tasks.get("a1b2").expect("Task should exist");

        assert_eq!(task.name, "New name");
        assert_eq!(task.priority, TaskPriority::P1);
    }

    #[test]
    fn test_materialize_full_task_lifecycle_with_reopen() {
        let base_time = Utc::now();
        let events = vec![
            // Create task
            TaskEvent::Created {
                task_type: None,
                task_id: "a1b2".to_string(),
                name: "Test task".to_string(),
                slug: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp: base_time,
            },
            // Start task
            TaskEvent::Started {
                task_ids: vec!["a1b2".to_string()],
                agent_type: "claude-code".to_string(),
                session_id: None,
                turn_id: None,
                timestamp: base_time + chrono::Duration::seconds(1),
            },
            // Add comment
            TaskEvent::CommentAdded {
                task_ids: vec!["a1b2".to_string()],
                text: "Working on this".to_string(),
                data: std::collections::HashMap::new(),
                timestamp: base_time + chrono::Duration::seconds(2),
            },
            // Close task
            TaskEvent::Closed {
                task_ids: vec!["a1b2".to_string()],
                outcome: TaskOutcome::Done,
                summary: None,
                turn_id: None,
                timestamp: base_time + chrono::Duration::seconds(3),
            },
            // Reopen task
            TaskEvent::Reopened {
                task_id: "a1b2".to_string(),
                reason: "Bug still exists".to_string(),
                timestamp: base_time + chrono::Duration::seconds(4),
            },
            // Update task
            TaskEvent::Updated {
                task_id: "a1b2".to_string(),
                name: Some("Fix critical bug".to_string()),
                priority: Some(TaskPriority::P0),
                assignee: None,
                data: None,
                instructions: None,
                timestamp: base_time + chrono::Duration::seconds(5),
            },
        ];

        let tasks = materialize_graph(&events).tasks;
        let task = tasks.get("a1b2").expect("Task should exist");

        assert_eq!(task.status, TaskStatus::Open);
        assert_eq!(task.name, "Fix critical bug");
        assert_eq!(task.priority, TaskPriority::P0);
        assert_eq!(task.comments.len(), 1);
        assert!(task.closed_outcome.is_none());
    }

    #[test]
    fn test_reopened_task_appears_in_ready_queue() {
        let base_time = Utc::now();
        let events = vec![
            TaskEvent::Created {
                task_type: None,
                task_id: "a1b2".to_string(),
                name: "Test task".to_string(),
                slug: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp: base_time,
            },
            TaskEvent::Closed {
                task_ids: vec!["a1b2".to_string()],
                outcome: TaskOutcome::Done,
                summary: None,
                turn_id: None,
                timestamp: base_time + chrono::Duration::seconds(1),
            },
        ];

        let graph = make_graph(&events);
        let ready = get_ready_queue(&graph);
        assert!(ready.is_empty(), "Closed task should not be in ready queue");

        // Now add reopened event
        let events_with_reopen = vec![
            TaskEvent::Created {
                task_type: None,
                task_id: "a1b2".to_string(),
                name: "Test task".to_string(),
                slug: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp: base_time,
            },
            TaskEvent::Closed {
                task_ids: vec!["a1b2".to_string()],
                outcome: TaskOutcome::Done,
                summary: None,
                turn_id: None,
                timestamp: base_time + chrono::Duration::seconds(1),
            },
            TaskEvent::Reopened {
                task_id: "a1b2".to_string(),
                reason: "Found more issues".to_string(),
                timestamp: base_time + chrono::Duration::seconds(2),
            },
        ];

        let graph = make_graph(&events_with_reopen);
        let ready = get_ready_queue(&graph);
        assert_eq!(ready.len(), 1, "Reopened task should be in ready queue");
        assert_eq!(ready[0].id, "a1b2");
    }

    #[test]
    fn test_update_nonexistent_task_ignored() {
        let events = vec![TaskEvent::Updated {
            task_id: "nonexistent".to_string(),
            name: Some("New name".to_string()),
            priority: None,
            assignee: None,
            data: None,
            instructions: None,
            timestamp: Utc::now(),
        }];

        let tasks = materialize_graph(&events).tasks;
        assert!(
            tasks.is_empty(),
            "No task should be created from Update event alone"
        );
    }

    #[test]
    fn test_materialize_updated_event_instructions() {
        let base_time = Utc::now();
        let events = vec![
            TaskEvent::Created {
                task_type: None,
                task_id: "a1b2".to_string(),
                name: "Test task".to_string(),
                slug: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp: base_time,
            },
            TaskEvent::Updated {
                task_id: "a1b2".to_string(),
                name: None,
                priority: None,
                assignee: None,
                data: None,
                instructions: Some("Step 1: do X\nStep 2: do Y".to_string()),
                timestamp: base_time + chrono::Duration::seconds(1),
            },
        ];

        let tasks = materialize_graph(&events).tasks;
        let task = tasks.get("a1b2").expect("Task should exist");

        assert_eq!(task.instructions, Some("Step 1: do X\nStep 2: do Y".to_string()));
        assert_eq!(task.name, "Test task"); // Name unchanged
    }

    #[test]
    fn test_comment_on_nonexistent_task_ignored() {
        let events = vec![TaskEvent::CommentAdded {
            task_ids: vec!["nonexistent".to_string()],
            text: "Comment".to_string(),
            data: std::collections::HashMap::new(),
            timestamp: Utc::now(),
        }];

        let tasks = materialize_graph(&events).tasks;
        assert!(
            tasks.is_empty(),
            "No task should be created from CommentAdded event alone"
        );
    }

    #[test]
    fn test_reopen_nonexistent_task_ignored() {
        let events = vec![TaskEvent::Reopened {
            task_id: "nonexistent".to_string(),
            reason: "Reason".to_string(),
            timestamp: Utc::now(),
        }];

        let tasks = materialize_graph(&events).tasks;
        assert!(
            tasks.is_empty(),
            "No task should be created from Reopened event alone"
        );
    }

    // Tests for assignee-based filtering

    fn make_created_event_with_assignee(
        task_id: &str,
        name: &str,
        priority: TaskPriority,
        hours_ago: i64,
        assignee: Option<&str>,
    ) -> TaskEvent {
        TaskEvent::Created {
            task_id: task_id.to_string(),
            name: name.to_string(),
            slug: None,
            task_type: None,
            priority,
            assignee: assignee.map(|s| s.to_string()),
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data: std::collections::HashMap::new(),
            timestamp: Utc::now() - chrono::Duration::hours(hours_ago),
        }
    }

    #[test]
    fn test_get_ready_queue_for_agent_unassigned() {
        // Unassigned tasks are visible to all agents
        let events = vec![make_created_event_with_assignee(
            "task1",
            "Unassigned",
            TaskPriority::P2,
            1,
            None,
        )];
        let graph = make_graph(&events);

        let claude_queue = get_ready_queue_for_agent(&graph, &AgentType::ClaudeCode);
        let codex_queue = get_ready_queue_for_agent(&graph, &AgentType::Codex);

        assert_eq!(claude_queue.len(), 1);
        assert_eq!(codex_queue.len(), 1);
    }

    #[test]
    fn test_get_ready_queue_for_agent_assigned_to_self() {
        // Tasks assigned to an agent are visible only to that agent
        let events = vec![make_created_event_with_assignee(
            "task1",
            "For Claude",
            TaskPriority::P2,
            1,
            Some("claude-code"),
        )];
        let graph = make_graph(&events);

        let claude_queue = get_ready_queue_for_agent(&graph, &AgentType::ClaudeCode);
        let codex_queue = get_ready_queue_for_agent(&graph, &AgentType::Codex);

        assert_eq!(claude_queue.len(), 1);
        assert_eq!(codex_queue.len(), 0); // Not visible to Codex
    }

    #[test]
    fn test_get_ready_queue_for_agent_human_task() {
        // Tasks assigned to human are not visible to agents
        let events = vec![make_created_event_with_assignee(
            "task1",
            "For Human",
            TaskPriority::P2,
            1,
            Some("human"),
        )];
        let graph = make_graph(&events);

        let claude_queue = get_ready_queue_for_agent(&graph, &AgentType::ClaudeCode);
        let codex_queue = get_ready_queue_for_agent(&graph, &AgentType::Codex);

        assert_eq!(claude_queue.len(), 0);
        assert_eq!(codex_queue.len(), 0);
    }

    #[test]
    fn test_get_ready_queue_for_agent_mixed() {
        // Mixed assignees - agent should see only relevant tasks
        let events = vec![
            make_created_event_with_assignee("unassigned", "Unassigned", TaskPriority::P3, 4, None),
            make_created_event_with_assignee(
                "for_claude",
                "For Claude",
                TaskPriority::P2,
                3,
                Some("claude-code"),
            ),
            make_created_event_with_assignee(
                "for_codex",
                "For Codex",
                TaskPriority::P1,
                2,
                Some("codex"),
            ),
            make_created_event_with_assignee(
                "for_human",
                "For Human",
                TaskPriority::P0,
                1,
                Some("human"),
            ),
        ];
        let graph = make_graph(&events);

        let claude_queue = get_ready_queue_for_agent(&graph, &AgentType::ClaudeCode);
        let codex_queue = get_ready_queue_for_agent(&graph, &AgentType::Codex);

        // Claude sees: unassigned, for_claude (sorted by priority)
        assert_eq!(claude_queue.len(), 2);
        assert_eq!(claude_queue[0].id, "for_claude"); // P2
        assert_eq!(claude_queue[1].id, "unassigned"); // P3

        // Codex sees: unassigned, for_codex (sorted by priority)
        assert_eq!(codex_queue.len(), 2);
        assert_eq!(codex_queue[0].id, "for_codex"); // P1
        assert_eq!(codex_queue[1].id, "unassigned"); // P3
    }

    #[test]
    fn test_get_ready_queue_for_human() {
        let events = vec![
            make_created_event_with_assignee("unassigned", "Unassigned", TaskPriority::P3, 4, None),
            make_created_event_with_assignee(
                "for_claude",
                "For Claude",
                TaskPriority::P2,
                3,
                Some("claude-code"),
            ),
            make_created_event_with_assignee(
                "for_human",
                "For Human",
                TaskPriority::P0,
                1,
                Some("human"),
            ),
        ];
        let graph = make_graph(&events);

        let human_queue = get_ready_queue_for_human(&graph);

        // Human sees: unassigned, for_human (sorted by priority)
        assert_eq!(human_queue.len(), 2);
        assert_eq!(human_queue[0].id, "for_human"); // P0
        assert_eq!(human_queue[1].id, "unassigned"); // P3
    }

    #[test]
    fn test_get_ready_queue_for_agent_scoped() {
        let events = vec![
            make_created_event_with_assignee("parent", "Parent", TaskPriority::P2, 5, None),
            make_created_event_with_assignee(
                "parent.1",
                "Child Unassigned",
                TaskPriority::P2,
                4,
                None,
            ),
            make_created_event_with_assignee(
                "parent.2",
                "Child For Claude",
                TaskPriority::P1,
                3,
                Some("claude-code"),
            ),
            make_created_event_with_assignee(
                "parent.3",
                "Child For Human",
                TaskPriority::P0,
                2,
                Some("human"),
            ),
        ];
        let graph = make_graph(&events);

        let scope_set = ScopeSet {
            include_root: false,
            scopes: vec!["parent".to_string()],
        };

        let scoped_queue =
            get_ready_queue_for_agent_scoped(&graph, &scope_set, &AgentType::ClaudeCode);

        // Claude sees subtasks: unassigned (P2), for_claude (P1)
        // Sorted by priority: P1 first
        assert_eq!(scoped_queue.len(), 2);
        assert_eq!(scoped_queue[0].id, "parent.2"); // P1 for claude
        assert_eq!(scoped_queue[1].id, "parent.1"); // P2 unassigned
    }

    // Tests for get_in_progress_task_ids_for_session

    fn make_started_event_with_session(task_id: &str, session_id: &str) -> TaskEvent {
        TaskEvent::Started {
            task_ids: vec![task_id.to_string()],
            agent_type: "claude-code".to_string(),
            session_id: Some(session_id.to_string()),
            turn_id: None,
            timestamp: Utc::now(),
        }
    }

    fn make_started_event_with_time(task_id: &str, session_id: &str, hours_ago: i64) -> TaskEvent {
        TaskEvent::Started {
            task_ids: vec![task_id.to_string()],
            agent_type: "claude-code".to_string(),
            session_id: Some(session_id.to_string()),
            turn_id: None,
            timestamp: Utc::now() - chrono::Duration::hours(hours_ago),
        }
    }

    #[test]
    fn test_get_in_progress_task_ids_for_session_empty() {
        let events = vec![make_created_event("task1", "Task 1", TaskPriority::P2, 1)];
        let tasks = materialize_graph(&events).tasks;

        let task_ids = get_in_progress_task_ids_for_session(&tasks, "session-123");
        assert!(task_ids.is_empty());
    }

    #[test]
    fn test_get_in_progress_task_ids_for_session_single() {
        let events = vec![
            make_created_event("task1", "Task 1", TaskPriority::P2, 1),
            make_started_event_with_session("task1", "session-123"),
        ];
        let tasks = materialize_graph(&events).tasks;

        let task_ids = get_in_progress_task_ids_for_session(&tasks, "session-123");
        assert_eq!(task_ids, vec!["task1"]);
    }

    #[test]
    fn test_get_in_progress_task_ids_for_session_multiple() {
        let events = vec![
            make_created_event("task1", "Task 1", TaskPriority::P2, 3),
            make_created_event("task2", "Task 2", TaskPriority::P2, 2),
            make_created_event("task3", "Task 3", TaskPriority::P2, 1),
            // task1 started 3 hours ago (oldest start)
            make_started_event_with_time("task1", "session-123", 3),
            // task2 started 2 hours ago
            make_started_event_with_time("task2", "session-123", 2),
            // task3 started 1 hour ago (most recent start)
            make_started_event_with_time("task3", "session-123", 1),
        ];
        let tasks = materialize_graph(&events).tasks;

        let task_ids = get_in_progress_task_ids_for_session(&tasks, "session-123");
        // Should be sorted by start time descending (most recently started first)
        assert_eq!(task_ids.len(), 3);
        assert_eq!(task_ids[0], "task3"); // Started 1 hour ago (most recent)
        assert_eq!(task_ids[1], "task2"); // Started 2 hours ago
        assert_eq!(task_ids[2], "task1"); // Started 3 hours ago (oldest)
    }

    #[test]
    fn test_get_in_progress_task_ids_for_session_different_sessions() {
        let events = vec![
            make_created_event("task1", "Task 1", TaskPriority::P2, 2),
            make_created_event("task2", "Task 2", TaskPriority::P2, 1),
            make_started_event_with_session("task1", "session-123"),
            make_started_event_with_session("task2", "session-456"),
        ];
        let tasks = materialize_graph(&events).tasks;

        let session_123_tasks = get_in_progress_task_ids_for_session(&tasks, "session-123");
        let session_456_tasks = get_in_progress_task_ids_for_session(&tasks, "session-456");

        assert_eq!(session_123_tasks, vec!["task1"]);
        assert_eq!(session_456_tasks, vec!["task2"]);
    }

    #[test]
    fn test_get_in_progress_task_ids_for_session_excludes_stopped() {
        let events = vec![
            make_created_event("task1", "Task 1", TaskPriority::P2, 2),
            make_created_event("task2", "Task 2", TaskPriority::P2, 1),
            make_started_event_with_session("task1", "session-123"),
            make_started_event_with_session("task2", "session-123"),
            make_stopped_event("task1", None),
        ];
        let tasks = materialize_graph(&events).tasks;

        let task_ids = get_in_progress_task_ids_for_session(&tasks, "session-123");
        // task1 was stopped, so only task2 remains in-progress
        assert_eq!(task_ids, vec!["task2"]);
    }

    #[test]
    fn test_get_in_progress_task_ids_for_session_excludes_closed() {
        let events = vec![
            make_created_event("task1", "Task 1", TaskPriority::P2, 2),
            make_created_event("task2", "Task 2", TaskPriority::P2, 1),
            make_started_event_with_session("task1", "session-123"),
            make_started_event_with_session("task2", "session-123"),
            make_closed_event("task1", TaskOutcome::Done),
        ];
        let tasks = materialize_graph(&events).tasks;

        let task_ids = get_in_progress_task_ids_for_session(&tasks, "session-123");
        // task1 was closed, so only task2 remains in-progress
        assert_eq!(task_ids, vec!["task2"]);
    }

    // Tests for find_task with prefix resolution

    fn make_tasks_for_prefix_tests() -> FastHashMap<String, Task> {
        let events = vec![
            make_created_event("mvslrspmoynoxyyywqyutmovxpvztkls", "Task Alpha", TaskPriority::P2, 3),
            make_created_event("mvslrspmoynoxyyywqyutmovxpvztkls.1", "Subtask 1", TaskPriority::P2, 2),
            make_created_event("mvslrspmoynoxyyywqyutmovxpvztkls.2", "Subtask 2", TaskPriority::P2, 1),
            make_created_event("nrqklspxopmwtryzyzkqnlmsqvpwtkls", "Task Beta", TaskPriority::P2, 3),
            make_created_event("mvslxyymoynoxyyywqyutmovxpvztkls", "Task Gamma", TaskPriority::P2, 3),
        ];
        materialize_graph(&events).tasks
    }

    #[test]
    fn test_find_task_exact_match() {
        let tasks = make_tasks_for_prefix_tests();
        let task = find_task(&tasks, "mvslrspmoynoxyyywqyutmovxpvztkls").unwrap();
        assert_eq!(task.name, "Task Alpha");
    }

    #[test]
    fn test_find_task_unique_prefix() {
        let tasks = make_tasks_for_prefix_tests();
        // "nrqkl" uniquely matches nrqklspxopmwtryzyzkqnlmsqvpwtkls
        let task = find_task(&tasks, "nrqkl").unwrap();
        assert_eq!(task.name, "Task Beta");
    }

    #[test]
    fn test_find_task_ambiguous_prefix() {
        let tasks = make_tasks_for_prefix_tests();
        // "mvsl" matches both mvslrsp... and mvslxyy...
        let err = find_task(&tasks, "mvsl").unwrap_err();
        match err {
            AikiError::AmbiguousTaskId { prefix, count, matches } => {
                assert_eq!(prefix, "mvsl");
                assert_eq!(count, 2);
                assert!(matches.contains("Task Alpha"));
                assert!(matches.contains("Task Gamma"));
            }
            _ => panic!("Expected AmbiguousTaskId, got {:?}", err),
        }
    }

    #[test]
    fn test_find_task_not_found() {
        let tasks = make_tasks_for_prefix_tests();
        let err = find_task(&tasks, "zzzzz").unwrap_err();
        match err {
            AikiError::TaskNotFound(id) => assert_eq!(id, "zzzzz"),
            _ => panic!("Expected TaskNotFound, got {:?}", err),
        }
    }

    #[test]
    fn test_find_task_subtask_prefix() {
        let tasks = make_tasks_for_prefix_tests();
        // "mvslrsp.1" — prefix of root + subtask number
        // "mvslrsp" uniquely matches mvslrspmoynoxyyywqyutmovxpvztkls (since dedup removes subtask variants)
        let task = find_task(&tasks, "mvslrsp.1").unwrap();
        assert_eq!(task.name, "Subtask 1");
    }

    #[test]
    fn test_find_task_subtask_not_found() {
        let tasks = make_tasks_for_prefix_tests();
        let err = find_task(&tasks, "mvslrsp.99").unwrap_err();
        match err {
            AikiError::SubtaskNotFound { root, subtask } => {
                assert_eq!(root, "mvslrspmoynoxyyywqyutmovxpvztkls");
                assert_eq!(subtask, "99");
            }
            _ => panic!("Expected SubtaskNotFound, got {:?}", err),
        }
    }

    #[test]
    fn test_find_task_prefix_too_short() {
        let tasks = make_tasks_for_prefix_tests();
        let err = find_task(&tasks, "mv").unwrap_err();
        match err {
            AikiError::PrefixTooShort { prefix } => assert_eq!(prefix, "mv"),
            _ => panic!("Expected PrefixTooShort, got {:?}", err),
        }
    }

    #[test]
    fn test_find_task_deduplication() {
        let tasks = make_tasks_for_prefix_tests();
        // "mvslrsp" matches mvslrspmoynoxyyywqyutmovxpvztkls, mvslrspmoynoxyyywqyutmovxpvztkls.1,
        // and mvslrspmoynoxyyywqyutmovxpvztkls.2 — but they all share the same root, so should
        // resolve to the root ID (not ambiguous).
        let task = find_task(&tasks, "mvslrsp").unwrap();
        assert_eq!(task.name, "Task Alpha");
    }

    // Tests for resolve_task_id

    #[test]
    fn test_resolve_task_id_exact() {
        let tasks = make_tasks_for_prefix_tests();
        let id = resolve_task_id(&tasks, "mvslrspmoynoxyyywqyutmovxpvztkls").unwrap();
        assert_eq!(id, "mvslrspmoynoxyyywqyutmovxpvztkls");
    }

    #[test]
    fn test_resolve_task_id_prefix() {
        let tasks = make_tasks_for_prefix_tests();
        let id = resolve_task_id(&tasks, "nrqkl").unwrap();
        assert_eq!(id, "nrqklspxopmwtryzyzkqnlmsqvpwtkls");
    }

    #[test]
    fn test_resolve_task_id_subtask() {
        let tasks = make_tasks_for_prefix_tests();
        let id = resolve_task_id(&tasks, "mvslrsp.2").unwrap();
        assert_eq!(id, "mvslrspmoynoxyyywqyutmovxpvztkls.2");
    }

    #[test]
    fn test_resolve_task_id_ambiguous() {
        let tasks = make_tasks_for_prefix_tests();
        let err = resolve_task_id(&tasks, "mvsl").unwrap_err();
        assert!(matches!(err, AikiError::AmbiguousTaskId { .. }));
    }

    // Tests for slug resolution (find_task_in_graph / resolve_task_id_in_graph)

    fn make_graph_with_slugs() -> crate::tasks::graph::TaskGraph {
        let events = vec![
            make_created_event("mvslrspmoynoxyyywqyutmovxpvztkls", "Task Alpha", TaskPriority::P2, 3),
            TaskEvent::Created {
                task_id: "mvslrspmoynoxyyywqyutmovxpvztkls.1".to_string(),
                name: "Build step".to_string(),
                slug: Some("build".to_string()),
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp: Utc::now(),
            },
            TaskEvent::Created {
                task_id: "mvslrspmoynoxyyywqyutmovxpvztkls.2".to_string(),
                name: "Test step".to_string(),
                slug: Some("test".to_string()),
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: std::collections::HashMap::new(),
                timestamp: Utc::now(),
            },
            make_created_event("nrqklspxopmwtryzyzkqnlmsqvpwtkls", "Task Beta", TaskPriority::P2, 3),
        ];
        materialize_graph(&events)
    }

    #[test]
    fn test_find_task_in_graph_slug_with_prefix() {
        let graph = make_graph_with_slugs();
        // Resolve using parent prefix + slug
        let task = find_task_in_graph(&graph, "mvslrsp:build").unwrap();
        assert_eq!(task.name, "Build step");
    }

    #[test]
    fn test_find_task_in_graph_slug_with_full_id() {
        let graph = make_graph_with_slugs();
        // Resolve using full parent ID + slug
        let task = find_task_in_graph(&graph, "mvslrspmoynoxyyywqyutmovxpvztkls:test").unwrap();
        assert_eq!(task.name, "Test step");
    }

    #[test]
    fn test_find_task_in_graph_slug_not_found() {
        let graph = make_graph_with_slugs();
        let err = find_task_in_graph(&graph, "mvslrsp:nonexistent").unwrap_err();
        match err {
            AikiError::SubtaskNotFound { root, subtask } => {
                assert_eq!(root, "mvslrspmoynoxyyywqyutmovxpvztkls");
                assert_eq!(subtask, "nonexistent");
            }
            _ => panic!("Expected SubtaskNotFound, got {:?}", err),
        }
    }

    #[test]
    fn test_find_task_in_graph_parent_not_found() {
        let graph = make_graph_with_slugs();
        let err = find_task_in_graph(&graph, "zzzzz:build").unwrap_err();
        assert!(matches!(err, AikiError::TaskNotFound(_)));
    }

    #[test]
    fn test_find_task_in_graph_dot_notation_still_works() {
        let graph = make_graph_with_slugs();
        let task = find_task_in_graph(&graph, "mvslrsp.1").unwrap();
        assert_eq!(task.name, "Build step");
    }

    #[test]
    fn test_find_task_in_graph_exact_id_still_works() {
        let graph = make_graph_with_slugs();
        let task = find_task_in_graph(&graph, "mvslrspmoynoxyyywqyutmovxpvztkls").unwrap();
        assert_eq!(task.name, "Task Alpha");
    }

    #[test]
    fn test_resolve_task_id_in_graph_slug() {
        let graph = make_graph_with_slugs();
        let id = resolve_task_id_in_graph(&graph, "mvslrsp:build").unwrap();
        assert_eq!(id, "mvslrspmoynoxyyywqyutmovxpvztkls.1");
    }

    fn make_created_event_with_type(
        task_id: &str,
        name: &str,
        task_type: &str,
        priority: TaskPriority,
        hours_ago: i64,
    ) -> TaskEvent {
        TaskEvent::Created {
            task_id: task_id.to_string(),
            name: name.to_string(),
            slug: None,
            task_type: Some(task_type.to_string()),
            priority,
            assignee: None,
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data: std::collections::HashMap::new(),
            timestamp: Utc::now() - chrono::Duration::hours(hours_ago),
        }
    }

    #[test]
    fn test_orchestrator_is_orchestrator() {
        let events = vec![make_created_event_with_type(
            "parent",
            "Build: plan.md",
            "orchestrator",
            TaskPriority::P2,
            1,
        )];
        let tasks = materialize_graph(&events).tasks;
        let task = tasks.get("parent").unwrap();
        assert!(task.is_orchestrator());
    }

    #[test]
    fn test_non_orchestrator_is_not_orchestrator() {
        let events = vec![make_created_event(
            "parent",
            "Regular task",
            TaskPriority::P2,
            1,
        )];
        let tasks = materialize_graph(&events).tasks;
        let task = tasks.get("parent").unwrap();
        assert!(!task.is_orchestrator());
    }

    #[test]
    fn test_orchestrator_unclosed_descendants_for_cascade() {
        // Simulates the cascade-stop scenario: orchestrator has subtasks, some open
        let events = vec![
            make_created_event_with_type(
                "parent",
                "Build: plan.md",
                "orchestrator",
                TaskPriority::P2,
                5,
            ),
            make_created_event("parent.1", "Decompose", TaskPriority::P2, 4),
            make_created_event("parent.2", "Execute", TaskPriority::P2, 3),
            make_created_event("parent.2.1", "Step 1", TaskPriority::P2, 2),
            make_created_event("parent.2.2", "Step 2", TaskPriority::P2, 1),
            make_closed_event("parent.1", TaskOutcome::Done),
            make_started_event("parent.2"),
            make_started_event("parent.2.1"),
        ];

        let graph = make_graph(&events);
        let parent = graph.tasks.get("parent").unwrap();
        assert!(parent.is_orchestrator());

        let unclosed = get_all_unclosed_descendants(&graph, "parent");
        // parent.1 is closed, so only parent.2, parent.2.1, parent.2.2
        assert_eq!(unclosed.len(), 3);
        let ids: Vec<_> = unclosed.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"parent.2"));
        assert!(ids.contains(&"parent.2.1"));
        assert!(ids.contains(&"parent.2.2"));
    }

    #[test]
    fn test_non_orchestrator_stop_should_not_cascade() {
        // Non-orchestrator tasks have subtasks but stop should NOT trigger cascade
        // (cascade is only triggered by the calling code when is_orchestrator() is true)
        let events = vec![
            make_created_event("parent", "Regular parent", TaskPriority::P2, 3),
            make_created_event("parent.1", "Child 1", TaskPriority::P2, 2),
            make_created_event("parent.2", "Child 2", TaskPriority::P2, 1),
        ];

        let graph = make_graph(&events);
        let parent = graph.tasks.get("parent").unwrap();
        assert!(!parent.is_orchestrator());

        // The unclosed descendants exist, but the calling code won't cascade-close them
        // because is_orchestrator() returns false
        let unclosed = get_all_unclosed_descendants(&graph, "parent");
        assert_eq!(unclosed.len(), 2);
    }

    #[test]
    fn test_orchestrator_stop_cascade_closes_descendants_as_wont_do() {
        // Simulates the full orchestrator stop flow:
        // 1. Orchestrator parent with subtasks (some in-progress, some open)
        // 2. Parent gets stopped
        // 3. cascade_close_tasks writes Closed events for all unclosed descendants
        // 4. Verify descendants are closed with WontDo and correct summary
        let summary = "Parent orchestrator stopped";
        let events = vec![
            make_created_event_with_type("parent", "Build: plan.md", "orchestrator", TaskPriority::P2, 5),
            make_created_event("parent.1", "Decompose", TaskPriority::P2, 4),
            make_created_event("parent.2", "Execute", TaskPriority::P2, 3),
            make_created_event("parent.2.1", "Step 1", TaskPriority::P2, 2),
            make_created_event("parent.2.2", "Step 2", TaskPriority::P2, 1),
            // parent.1 is closed (done)
            make_closed_event("parent.1", TaskOutcome::Done),
            // parent.2 and parent.2.1 are in-progress
            make_started_event("parent"),
            make_started_event("parent.2"),
            make_started_event("parent.2.1"),
            // Simulate run_stop: stop the parent
            make_stopped_event("parent", Some("User stopped")),
            // Simulate cascade_close_tasks: close all unclosed descendants as WontDo
            TaskEvent::Closed {
                task_ids: vec![
                    "parent.2".to_string(),
                    "parent.2.1".to_string(),
                    "parent.2.2".to_string(),
                ],
                outcome: TaskOutcome::WontDo,
                summary: Some(summary.to_string()),
                turn_id: None,
                timestamp: Utc::now(),
            },
        ];

        let tasks = materialize_graph(&events).tasks;

        // Parent should be stopped (not closed)
        let parent = tasks.get("parent").unwrap();
        assert_eq!(parent.status, TaskStatus::Stopped);
        assert!(parent.is_orchestrator());

        // parent.1 was already closed as Done — should remain unchanged
        let child1 = tasks.get("parent.1").unwrap();
        assert_eq!(child1.status, TaskStatus::Closed);
        assert_eq!(child1.closed_outcome, Some(TaskOutcome::Done));

        // parent.2 should be cascade-closed as WontDo
        let child2 = tasks.get("parent.2").unwrap();
        assert_eq!(child2.status, TaskStatus::Closed);
        assert_eq!(child2.closed_outcome, Some(TaskOutcome::WontDo));
        assert_eq!(child2.summary.as_deref(), Some(summary));

        // parent.2.1 should be cascade-closed as WontDo
        let grandchild1 = tasks.get("parent.2.1").unwrap();
        assert_eq!(grandchild1.status, TaskStatus::Closed);
        assert_eq!(grandchild1.closed_outcome, Some(TaskOutcome::WontDo));
        assert_eq!(grandchild1.summary.as_deref(), Some(summary));

        // parent.2.2 should be cascade-closed as WontDo
        let grandchild2 = tasks.get("parent.2.2").unwrap();
        assert_eq!(grandchild2.status, TaskStatus::Closed);
        assert_eq!(grandchild2.closed_outcome, Some(TaskOutcome::WontDo));
        assert_eq!(grandchild2.summary.as_deref(), Some(summary));
    }

    #[test]
    fn test_orchestrator_failure_cascade_closes_descendants() {
        // Simulates runner failure path: orchestrator agent fails, descendants are
        // cascade-closed with "Parent orchestrator failed" summary
        let summary = "Parent orchestrator failed";
        let events = vec![
            make_created_event_with_type("orch", "Build: feature", "orchestrator", TaskPriority::P2, 4),
            make_created_event("orch.1", "Subtask A", TaskPriority::P2, 3),
            make_created_event("orch.2", "Subtask B", TaskPriority::P2, 2),
            make_started_event("orch"),
            make_started_event("orch.1"),
            // Simulate runner failure: stop the parent, cascade-close descendants
            make_stopped_event("orch", Some("Session failed: agent crashed")),
            TaskEvent::Closed {
                task_ids: vec!["orch.1".to_string(), "orch.2".to_string()],
                outcome: TaskOutcome::WontDo,
                summary: Some(summary.to_string()),
                turn_id: None,
                timestamp: Utc::now(),
            },
        ];

        let tasks = materialize_graph(&events).tasks;

        // Both subtasks should be closed as WontDo with failure summary
        let sub_a = tasks.get("orch.1").unwrap();
        assert_eq!(sub_a.status, TaskStatus::Closed);
        assert_eq!(sub_a.closed_outcome, Some(TaskOutcome::WontDo));
        assert_eq!(sub_a.summary.as_deref(), Some(summary));

        let sub_b = tasks.get("orch.2").unwrap();
        assert_eq!(sub_b.status, TaskStatus::Closed);
        assert_eq!(sub_b.closed_outcome, Some(TaskOutcome::WontDo));
        assert_eq!(sub_b.summary.as_deref(), Some(summary));
    }

    #[test]
    fn test_orchestrator_cascade_does_not_affect_already_closed() {
        // If a descendant is already closed (Done), cascade-close should not re-close it
        // because get_all_unclosed_descendants filters out closed tasks
        let events = vec![
            make_created_event_with_type("p", "Build: x", "orchestrator", TaskPriority::P2, 5),
            make_created_event("p.1", "Done task", TaskPriority::P2, 4),
            make_created_event("p.2", "Open task", TaskPriority::P2, 3),
            make_closed_event("p.1", TaskOutcome::Done),
            make_started_event("p"),
        ];

        let graph = make_graph(&events);
        let unclosed = get_all_unclosed_descendants(&graph, "p");

        // Only p.2 should be in the unclosed list (p.1 is already closed)
        assert_eq!(unclosed.len(), 1);
        assert_eq!(unclosed[0].id, "p.2");

        // After cascade close, p.1 should retain its original Done outcome
        let events_with_cascade = vec![
            make_created_event_with_type("p", "Build: x", "orchestrator", TaskPriority::P2, 5),
            make_created_event("p.1", "Done task", TaskPriority::P2, 4),
            make_created_event("p.2", "Open task", TaskPriority::P2, 3),
            make_closed_event("p.1", TaskOutcome::Done),
            make_started_event("p"),
            make_stopped_event("p", None),
            // Only p.2 gets cascade-closed (p.1 already closed)
            TaskEvent::Closed {
                task_ids: vec!["p.2".to_string()],
                outcome: TaskOutcome::WontDo,
                summary: Some("Parent orchestrator stopped".to_string()),
                turn_id: None,
                timestamp: Utc::now(),
            },
        ];

        let tasks = materialize_graph(&events_with_cascade).tasks;
        let done_task = tasks.get("p.1").unwrap();
        assert_eq!(done_task.closed_outcome, Some(TaskOutcome::Done));
        assert!(done_task.summary.is_none()); // Original close had no summary

        let cascade_task = tasks.get("p.2").unwrap();
        assert_eq!(cascade_task.closed_outcome, Some(TaskOutcome::WontDo));
        assert_eq!(cascade_task.summary.as_deref(), Some("Parent orchestrator stopped"));
    }

    // --- blocking integration tests ---

    #[test]
    fn test_ready_queue_excludes_blocked_tasks() {
        let events = vec![
            make_created_event("blocker", "Blocker", TaskPriority::P2, 2),
            make_created_event("blocked", "Blocked task", TaskPriority::P2, 1),
            make_created_event("free", "Free task", TaskPriority::P2, 0),
            TaskEvent::LinkAdded {
                from: "blocked".to_string(),
                to: "blocker".to_string(),
                kind: "blocked-by".to_string(),
                autorun: None,
                timestamp: Utc::now(),
            },
        ];

        let graph = make_graph(&events);
        let ready = get_ready_queue(&graph);
        let ready_ids: Vec<&str> = ready.iter().map(|t| t.id.as_str()).collect();

        assert!(ready_ids.contains(&"blocker"));
        assert!(ready_ids.contains(&"free"));
        assert!(!ready_ids.contains(&"blocked"));
    }

    #[test]
    fn test_ready_queue_unblocks_when_blocker_closed() {
        let events = vec![
            make_created_event("blocker", "Blocker", TaskPriority::P2, 2),
            make_created_event("blocked", "Blocked task", TaskPriority::P2, 1),
            TaskEvent::LinkAdded {
                from: "blocked".to_string(),
                to: "blocker".to_string(),
                kind: "blocked-by".to_string(),
                autorun: None,
                timestamp: Utc::now(),
            },
            TaskEvent::Closed {
                task_ids: vec!["blocker".to_string()],
                outcome: TaskOutcome::Done,
                summary: None,
                turn_id: None,
                timestamp: Utc::now(),
            },
        ];

        let graph = make_graph(&events);
        let ready = get_ready_queue(&graph);
        let ready_ids: Vec<&str> = ready.iter().map(|t| t.id.as_str()).collect();

        assert!(ready_ids.contains(&"blocked"));
    }

    #[test]
    fn test_scoped_ready_queue_excludes_blocked_subtask() {
        // Auto-start-next should skip blocked subtasks
        let events = vec![
            make_created_event("parent", "Parent", TaskPriority::P2, 4),
            make_created_event("parent.1", "Child 1 (blocked)", TaskPriority::P2, 3),
            make_created_event("parent.2", "Child 2 (free)", TaskPriority::P2, 2),
            make_created_event("blocker", "Blocker task", TaskPriority::P2, 1),
            TaskEvent::LinkAdded {
                from: "parent.1".to_string(),
                to: "blocker".to_string(),
                kind: "blocked-by".to_string(),
                autorun: None,
                timestamp: Utc::now(),
            },
        ];

        let graph = make_graph(&events);
        let ready = get_scoped_ready_queue(&graph, Some("parent"));
        let ready_ids: Vec<&str> = ready.iter().map(|t| t.id.as_str()).collect();

        // parent.2 should be in the ready queue, parent.1 should be excluded (blocked)
        assert!(ready_ids.contains(&"parent.2"));
        assert!(!ready_ids.contains(&"parent.1"));
    }

    // Tests for get_task_activity_by_turn

    #[test]
    fn test_activity_empty_when_no_matching_turn() {
        let events = vec![
            make_created_event("t1", "Task 1", TaskPriority::P2, 1),
            TaskEvent::Started {
                task_ids: vec!["t1".to_string()],
                agent_type: "claude-code".to_string(),
                session_id: None,
                turn_id: Some("turn-aaa".to_string()),
                timestamp: Utc::now(),
            },
        ];

        let graph = make_graph(&events);
        let activity = get_task_activity_by_turn(&graph, "turn-zzz");
        assert!(activity.is_empty());
    }

    #[test]
    fn test_activity_started_tasks() {
        let events = vec![
            make_created_event("t1", "Task 1", TaskPriority::P2, 1),
            make_created_event("t2", "Task 2", TaskPriority::P1, 1),
            TaskEvent::Started {
                task_ids: vec!["t1".to_string()],
                agent_type: "claude-code".to_string(),
                session_id: None,
                turn_id: Some("turn-aaa".to_string()),
                timestamp: Utc::now(),
            },
            TaskEvent::Started {
                task_ids: vec!["t2".to_string()],
                agent_type: "claude-code".to_string(),
                session_id: None,
                turn_id: Some("turn-bbb".to_string()),
                timestamp: Utc::now(),
            },
        ];

        let graph = make_graph(&events);
        let activity = get_task_activity_by_turn(&graph, "turn-aaa");
        assert_eq!(activity.started.len(), 1);
        assert_eq!(activity.started[0].id, "t1");
        assert_eq!(activity.started[0].name, "Task 1");
        assert!(activity.closed.is_empty());
        assert!(activity.stopped.is_empty());
    }

    #[test]
    fn test_activity_closed_tasks() {
        let events = vec![
            make_created_event("t1", "Task 1", TaskPriority::P2, 1),
            TaskEvent::Started {
                task_ids: vec!["t1".to_string()],
                agent_type: "claude-code".to_string(),
                session_id: None,
                turn_id: Some("turn-aaa".to_string()),
                timestamp: Utc::now(),
            },
            TaskEvent::Closed {
                task_ids: vec!["t1".to_string()],
                outcome: TaskOutcome::Done,
                summary: None,
                turn_id: Some("turn-aaa".to_string()),
                timestamp: Utc::now(),
            },
        ];

        let graph = make_graph(&events);
        let activity = get_task_activity_by_turn(&graph, "turn-aaa");
        // Started and closed in the same turn
        assert_eq!(activity.started.len(), 1);
        assert_eq!(activity.closed.len(), 1);
        assert_eq!(activity.closed[0].id, "t1");
    }

    #[test]
    fn test_activity_stopped_tasks() {
        let events = vec![
            make_created_event("t1", "Task 1", TaskPriority::P2, 1),
            TaskEvent::Started {
                task_ids: vec!["t1".to_string()],
                agent_type: "claude-code".to_string(),
                session_id: None,
                turn_id: Some("turn-aaa".to_string()),
                timestamp: Utc::now(),
            },
            TaskEvent::Stopped {
                task_ids: vec!["t1".to_string()],
                reason: Some("blocked".to_string()),
                turn_id: Some("turn-bbb".to_string()),
                timestamp: Utc::now(),
            },
        ];

        let graph = make_graph(&events);
        let activity = get_task_activity_by_turn(&graph, "turn-bbb");
        assert_eq!(activity.stopped.len(), 1);
        assert_eq!(activity.stopped[0].id, "t1");
        assert!(activity.started.is_empty()); // t1 was started in turn-aaa, not turn-bbb
    }

    #[test]
    fn test_activity_multiple_tasks_same_turn() {
        let events = vec![
            make_created_event("t1", "Task 1", TaskPriority::P2, 1),
            make_created_event("t2", "Task 2", TaskPriority::P1, 1),
            make_created_event("t3", "Task 3", TaskPriority::P0, 1),
            TaskEvent::Started {
                task_ids: vec!["t1".to_string()],
                agent_type: "claude-code".to_string(),
                session_id: None,
                turn_id: Some("turn-x".to_string()),
                timestamp: Utc::now(),
            },
            TaskEvent::Closed {
                task_ids: vec!["t2".to_string()],
                outcome: TaskOutcome::Done,
                summary: None,
                turn_id: Some("turn-x".to_string()),
                timestamp: Utc::now(),
            },
            TaskEvent::Stopped {
                task_ids: vec!["t3".to_string()],
                reason: None,
                turn_id: Some("turn-x".to_string()),
                timestamp: Utc::now(),
            },
        ];

        let graph = make_graph(&events);
        let activity = get_task_activity_by_turn(&graph, "turn-x");
        assert_eq!(activity.started.len(), 1);
        assert_eq!(activity.closed.len(), 1);
        assert_eq!(activity.stopped.len(), 1);
        assert!(!activity.is_empty());
    }
}
