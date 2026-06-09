//! Lane derivation from the subtask DAG
//!
//! A **lane** is a sequence of threads derived from `needs-context` and
//! `depends-on` edges.  Lanes are independent of each other and can run
//! concurrently.  Lane structure is a query-time derivation — nothing is
//! persisted.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

use anyhow::{bail, Result};

use super::graph::TaskGraph;
use super::manager::{get_subtasks, resolve_task_id_in_graph};
use super::md::short_id;
use super::types::{TaskOutcome, TaskStatus};

// ── Types ───────────────────────────────────────────────────────────

/// Identifies a thread — a sequential chunk of tasks within a lane.
/// Single-task threads have head == tail.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ThreadId {
    /// First task ID in the chain.
    pub head: String,
    /// Last task ID in the chain.
    pub tail: String,
}

impl ThreadId {
    /// Parse from wire format: `"head:tail"` or `"head"` (single-task shortcut).
    ///
    /// Expects full 32-char lowercase task IDs. No prefix resolution.
    /// Used by session files, env vars, history metadata.
    pub fn parse(input: &str) -> Result<Self> {
        if let Some((head, tail)) = input.split_once(':') {
            Self::validate_full_id(head, "head", input)?;
            Self::validate_full_id(tail, "tail", input)?;
            Ok(Self {
                head: head.to_string(),
                tail: tail.to_string(),
            })
        } else {
            Self::validate_full_id(input, "id", input)?;
            Ok(Self {
                head: input.to_string(),
                tail: input.to_string(),
            })
        }
    }

    /// Validate that a string is a canonical full 32-char lowercase task ID.
    fn validate_full_id(id: &str, label: &str, context: &str) -> Result<()> {
        if id.is_empty() {
            bail!("ThreadId: empty {label} in '{context}'");
        }
        if id.len() != 32 {
            bail!(
                "ThreadId: {label} must be 32 chars, got {} in '{context}'",
                id.len()
            );
        }
        if !id.bytes().all(|b| b.is_ascii_lowercase()) {
            bail!("ThreadId: {label} must be lowercase a-z in '{context}'");
        }
        Ok(())
    }

    /// Parse from CLI input, resolving both halves via prefix resolution.
    ///
    /// Used only at CLI entry points (`--thread` flag).
    pub fn resolve(input: &str, graph: &TaskGraph) -> Result<Self> {
        if let Some((head_prefix, tail_prefix)) = input.split_once(':') {
            let head = resolve_task_id_in_graph(graph, head_prefix)?;
            let tail = resolve_task_id_in_graph(graph, tail_prefix)?;
            Ok(Self { head, tail })
        } else {
            let id = resolve_task_id_in_graph(graph, input)?;
            Ok(Self {
                head: id.clone(),
                tail: id,
            })
        }
    }

    /// Single-task thread (head == tail).
    pub fn single(task_id: String) -> Self {
        Self {
            head: task_id.clone(),
            tail: task_id,
        }
    }

    /// Full-ID serialization for env vars, session files, history metadata.
    pub fn serialize(&self) -> String {
        if self.is_single() {
            self.head.clone()
        } else {
            format!("{}:{}", self.head, self.tail)
        }
    }

    pub fn is_single(&self) -> bool {
        self.head == self.tail
    }
}

impl fmt::Display for ThreadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_single() {
            write!(f, "{}", short_id(&self.head))
        } else {
            write!(f, "{}:{}", short_id(&self.head), short_id(&self.tail))
        }
    }
}

/// A thread within a lane: one or more task IDs that run in a single
/// agent session sharing context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    /// Ordered task IDs in this thread.
    pub task_ids: Vec<String>,
}

/// A derived lane: sequence of threads in execution order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lane {
    /// First task in the lane — also serves as the lane ID.
    pub head_task_id: String,
    /// Threads in execution order.
    pub threads: Vec<Thread>,
    /// Head IDs of predecessor lanes (cross-lane `depends-on`).
    pub depends_on_lanes: Vec<String>,
}

/// Full result of lane derivation for a parent task.
#[derive(Debug, Clone)]
pub struct LaneDecomposition {
    pub lanes: Vec<Lane>,
}

/// Status of a lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneStatus {
    /// All tasks `Closed(Done)`
    Complete,
    /// At least one task `Stopped` or `Closed(WontDo)`
    Failed,
    /// Lane's prerequisites are met and next thread can run
    Ready,
    /// Lane is waiting on predecessor lanes or blocked tasks
    Blocked,
}

// ── Public API ──────────────────────────────────────────────────────

/// Derive lanes from the subtask DAG of `parent_id`.
///
/// Algorithm:
/// 1. Collect subtasks.
/// 2. Build `needs-context` chains → multi-task sessions.
/// 3. Collapse each chain into a single "session node" for DAG analysis.
/// 4. Walk `depends-on` edges between session-nodes to form lanes:
///    - Independent roots become separate lanes.
///    - Linear `depends-on` paths stay in one lane.
///    - Fan-out creates separate lanes.
///    - Fan-in creates a lane that depends on predecessor lanes.
pub fn derive_lanes(graph: &TaskGraph, parent_id: &str) -> LaneDecomposition {
    // 1. Collect subtask IDs
    let subtasks = get_subtasks(graph, parent_id);
    let subtask_ids: HashSet<String> = subtasks.iter().map(|t| t.id.clone()).collect();

    if subtask_ids.is_empty() {
        return LaneDecomposition { lanes: Vec::new() };
    }

    // 2. Build needs-context sessions.
    //    Each session is identified by its head task ID.
    //    session_of: task_id → head_task_id of its session
    let mut session_of: HashMap<String, String> = HashMap::new();
    //    sessions: head_id → ordered list of task IDs in the session
    let mut sessions: HashMap<String, Vec<String>> = HashMap::new();

    for tid in &subtask_ids {
        if session_of.contains_key(tid) {
            continue;
        }
        // Walk the full chain containing this task
        let chain = graph.get_needs_context_chain(tid);

        // Filter chain to only include sibling subtasks
        let chain: Vec<String> = chain
            .into_iter()
            .filter(|id| subtask_ids.contains(id))
            .collect();

        if chain.is_empty() {
            continue;
        }

        let head = chain[0].clone();
        for id in &chain {
            session_of.insert(id.clone(), head.clone());
        }
        sessions.insert(head, chain);
    }

    // Any subtask not in a needs-context chain is its own single-task session
    for tid in &subtask_ids {
        if !session_of.contains_key(tid) {
            session_of.insert(tid.clone(), tid.clone());
            sessions.insert(tid.clone(), vec![tid.clone()]);
        }
    }

    // 3. Build a DAG of session-nodes using `depends-on` edges.
    //    An edge session_A → session_B means "session_A depends on session_B".
    //    We only look at cross-session depends-on edges between sibling subtasks.
    let session_heads: HashSet<String> = sessions.keys().cloned().collect();

    // session_deps: head_id → set of head_ids it depends on
    let mut session_deps: HashMap<String, HashSet<String>> = HashMap::new();
    // session_rdeps: head_id → set of head_ids that depend on it
    let mut session_rdeps: HashMap<String, HashSet<String>> = HashMap::new();

    for head in &session_heads {
        session_deps.entry(head.clone()).or_default();
        session_rdeps.entry(head.clone()).or_default();
    }

    for tid in &subtask_ids {
        let my_session = &session_of[tid];
        // Check depends-on targets (tid depends-on target)
        for target in graph.edges.targets(tid, "depends-on") {
            if !subtask_ids.contains(target) {
                continue;
            }
            let target_session = &session_of[target];
            if target_session != my_session {
                session_deps
                    .entry(my_session.clone())
                    .or_default()
                    .insert(target_session.clone());
                session_rdeps
                    .entry(target_session.clone())
                    .or_default()
                    .insert(my_session.clone());
            }
        }
        // Also check needs-context targets as implicit depends-on (cross-session)
        for target in graph.edges.targets(tid, "needs-context") {
            if !subtask_ids.contains(target) {
                continue;
            }
            let target_session = &session_of[target];
            if target_session != my_session {
                session_deps
                    .entry(my_session.clone())
                    .or_default()
                    .insert(target_session.clone());
                session_rdeps
                    .entry(target_session.clone())
                    .or_default()
                    .insert(my_session.clone());
            }
        }
    }

    // 4. Build lanes by walking the session DAG.
    //
    // Strategy: topological sort session-nodes, then group into lanes.
    // A session-node starts a new lane when:
    //   - It has zero dependencies (root)
    //   - It has multiple dependents (fan-out: each dependent starts its own lane)
    //   - It has multiple dependencies (fan-in: it starts a new lane)
    //
    // A session-node extends an existing lane when:
    //   - It has exactly one dependency AND that dependency has exactly one dependent.

    // lane_of: session_head → which lane head it belongs to
    let mut lane_of: HashMap<String, String> = HashMap::new();
    // lane_sessions: lane_head → ordered list of session heads in the lane
    let mut lane_sessions: HashMap<String, Vec<String>> = HashMap::new();
    // lane_deps: lane_head → set of lane heads it depends on
    let mut lane_deps: HashMap<String, HashSet<String>> = HashMap::new();

    // Topological sort (Kahn's algorithm)
    let mut in_degree: HashMap<String, usize> = HashMap::new();
    for head in &session_heads {
        in_degree.insert(head.clone(), session_deps[head].len());
    }
    let mut queue: VecDeque<String> = in_degree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(h, _)| h.clone())
        .collect();

    // Sort the initial queue for deterministic output
    let mut sorted_queue: Vec<String> = queue.drain(..).collect();
    sorted_queue.sort();
    queue.extend(sorted_queue);

    let mut topo_order: Vec<String> = Vec::new();

    while let Some(node) = queue.pop_front() {
        topo_order.push(node.clone());

        // Collect and sort dependents for deterministic processing
        let mut dependents: Vec<String> = session_rdeps
            .get(&node)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default();
        dependents.sort();

        for dep in dependents {
            let d = in_degree.get_mut(&dep).unwrap();
            *d -= 1;
            if *d == 0 {
                queue.push_back(dep);
            }
        }
    }

    // Process in topological order
    for session_head in &topo_order {
        let deps = &session_deps[session_head];

        if deps.is_empty() {
            // Root session → starts a new lane
            lane_of.insert(session_head.clone(), session_head.clone());
            lane_sessions.insert(session_head.clone(), vec![session_head.clone()]);
            lane_deps.insert(session_head.clone(), HashSet::new());
        } else if deps.len() == 1 {
            let single_dep = deps.iter().next().unwrap();
            let dep_rdeps = &session_rdeps[single_dep];

            if dep_rdeps.len() == 1 {
                // Linear chain: extend the predecessor's lane
                let pred_lane = lane_of[single_dep].clone();
                lane_of.insert(session_head.clone(), pred_lane.clone());
                lane_sessions
                    .get_mut(&pred_lane)
                    .unwrap()
                    .push(session_head.clone());
            } else {
                // Fan-out from predecessor: start a new lane
                let pred_lane = lane_of[single_dep].clone();
                lane_of.insert(session_head.clone(), session_head.clone());
                lane_sessions.insert(session_head.clone(), vec![session_head.clone()]);
                let mut deps_set = HashSet::new();
                deps_set.insert(pred_lane);
                lane_deps.insert(session_head.clone(), deps_set);
            }
        } else {
            // Fan-in (multiple deps): start a new lane that depends on all pred lanes
            lane_of.insert(session_head.clone(), session_head.clone());
            lane_sessions.insert(session_head.clone(), vec![session_head.clone()]);
            let pred_lanes: HashSet<String> = deps.iter().map(|d| lane_of[d].clone()).collect();
            lane_deps.insert(session_head.clone(), pred_lanes);
        }
    }

    // 5. Build Lane structs
    // Collect lane heads in order (preserve topo order for the first session in each lane)
    let mut lane_head_order: Vec<String> = Vec::new();
    let mut seen_lanes: HashSet<String> = HashSet::new();
    for session_head in &topo_order {
        let lane_head = &lane_of[session_head];
        if seen_lanes.insert(lane_head.clone()) {
            lane_head_order.push(lane_head.clone());
        }
    }

    let mut lanes = Vec::new();
    for lane_head in &lane_head_order {
        let session_heads_in_lane = &lane_sessions[lane_head];
        let lane_threads_list: Vec<Thread> = session_heads_in_lane
            .iter()
            .map(|sh| Thread {
                task_ids: sessions[sh].clone(),
            })
            .collect();

        let depends_on: Vec<String> = lane_deps
            .get(lane_head)
            .map(|s| {
                let mut v: Vec<String> = s.iter().cloned().collect();
                v.sort();
                v
            })
            .unwrap_or_default();

        lanes.push(Lane {
            head_task_id: lane_head.clone(),
            threads: lane_threads_list,
            depends_on_lanes: depends_on,
        });
    }

    LaneDecomposition { lanes }
}

/// Determine the status of a lane.
pub fn lane_status(lane: &Lane, graph: &TaskGraph, all_lanes: &[Lane]) -> LaneStatus {
    if is_lane_failed(lane, graph) {
        return LaneStatus::Failed;
    }
    if is_lane_complete(lane, graph) {
        return LaneStatus::Complete;
    }
    if is_lane_ready_with_decomposition(lane, graph, all_lanes) {
        return LaneStatus::Ready;
    }
    LaneStatus::Blocked
}

/// A lane is complete when all its tasks are `Closed(Done)`.
pub fn is_lane_complete(lane: &Lane, graph: &TaskGraph) -> bool {
    all_lane_tasks(lane).all(|tid| {
        graph.tasks.get(tid).map_or(false, |t| {
            t.status == TaskStatus::Closed && t.closed_outcome == Some(TaskOutcome::Done)
        })
    })
}

/// A lane is failed when any task is `Stopped` or `Closed(WontDo)`.
pub fn is_lane_failed(lane: &Lane, graph: &TaskGraph) -> bool {
    all_lane_tasks(lane).any(|tid| {
        graph.tasks.get(tid).map_or(false, |t| {
            t.status == TaskStatus::Stopped
                || (t.status == TaskStatus::Closed && t.closed_outcome == Some(TaskOutcome::WontDo))
        })
    })
}

/// A lane is ready when:
/// 1. All predecessor lanes are complete, AND
/// 2. No task in the lane's next uncompleted thread is blocked or in-progress.
///
/// `all_lanes` must be the full decomposition so we can check predecessors.
pub fn is_lane_ready_with_decomposition(
    lane: &Lane,
    graph: &TaskGraph,
    all_lanes: &[Lane],
) -> bool {
    // Check predecessor lanes are all complete
    for dep_head in &lane.depends_on_lanes {
        let dep_lane = all_lanes.iter().find(|l| l.head_task_id == *dep_head);
        match dep_lane {
            Some(dl) => {
                if !is_lane_complete(dl, graph) {
                    return false;
                }
            }
            None => {
                // Predecessor lane not found — treat as blocked
                // (unless all_lanes is empty, meaning no decomposition context)
                if !all_lanes.is_empty() {
                    return false;
                }
            }
        }
    }

    // Find the next uncompleted thread
    let next_thread = lane.threads.iter().find(|s| {
        !s.task_ids.iter().all(|tid| {
            graph.tasks.get(tid).map_or(false, |t| {
                t.status == TaskStatus::Closed && t.closed_outcome == Some(TaskOutcome::Done)
            })
        })
    });

    match next_thread {
        Some(thread) => {
            // A thread with InProgress tasks is already running, not "ready"
            let any_in_progress = thread.task_ids.iter().any(|tid| {
                graph
                    .tasks
                    .get(tid)
                    .map_or(false, |t| t.status == TaskStatus::InProgress)
            });
            if any_in_progress {
                return false;
            }
            // No task in the next thread should be blocked
            !thread.task_ids.iter().any(|tid| graph.is_blocked(tid))
        }
        None => {
            // All threads complete — lane is done, not "ready"
            false
        }
    }
}

/// Get the tasks in a lane that belong to the specified lane (by head_task_id).
///
/// Returns `None` if the lane is not found in the decomposition.
pub fn get_lane_task_ids(
    decomposition: &LaneDecomposition,
    lane_head: &str,
) -> Option<HashSet<String>> {
    decomposition
        .lanes
        .iter()
        .find(|l| l.head_task_id == lane_head)
        .map(|lane| {
            lane.threads
                .iter()
                .flat_map(|s| s.task_ids.iter().cloned())
                .collect()
        })
}

/// Resolve a lane ID prefix to a full lane head task ID.
///
/// Returns an error if:
/// - No lane matches the prefix
/// - Multiple lanes match the prefix (ambiguous)
pub fn resolve_lane_prefix(
    decomposition: &LaneDecomposition,
    prefix: &str,
    parent_short_id: &str,
) -> std::result::Result<String, String> {
    let matches: Vec<&Lane> = decomposition
        .lanes
        .iter()
        .filter(|l| l.head_task_id.starts_with(prefix))
        .collect();

    match matches.len() {
        0 => Err(format!(
            "No lane with head task matching '{}' for task {}",
            prefix, parent_short_id
        )),
        1 => Ok(matches[0].head_task_id.clone()),
        _ => Err(format!(
            "Multiple lanes match prefix '{}', be more specific",
            prefix
        )),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Iterate over all task IDs in a lane (across all threads).
fn all_lane_tasks(lane: &Lane) -> impl Iterator<Item = &str> {
    lane.threads
        .iter()
        .flat_map(|s| s.task_ids.iter().map(|id| id.as_str()))
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
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
            outcome: TaskOutcome::Done,
            confidence: None,
            summary: None,
            turn_id: None,
            timestamp: Utc::now(),
        }
    }

    fn make_stopped(id: &str) -> TaskEvent {
        TaskEvent::Stopped {
            session_id: None,
            task_ids: vec![id.to_string()],
            reason: Some("test".to_string()),
            turn_id: None,
            timestamp: Utc::now(),
        }
    }

    fn make_closed_wontdo(id: &str) -> TaskEvent {
        TaskEvent::Closed {
            session_id: None,
            task_ids: vec![id.to_string()],
            outcome: TaskOutcome::WontDo,
            confidence: None,
            summary: None,
            turn_id: None,
            timestamp: Utc::now(),
        }
    }

    fn make_started(id: &str) -> TaskEvent {
        TaskEvent::Started {
            task_ids: vec![id.to_string()],
            agent_type: "test".to_string(),
            session_id: None,
            turn_id: None,
            working_copy: None,
            instructions: None,
            timestamp: Utc::now(),
        }
    }

    // ── Derivation tests ────────────────────────────────────────────

    #[test]
    fn test_single_independent_task() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Task A"),
            make_link("A", "P", "subtask-of"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        assert_eq!(decomp.lanes.len(), 1);
        assert_eq!(decomp.lanes[0].head_task_id, "A");
        assert_eq!(decomp.lanes[0].threads.len(), 1);
        assert_eq!(decomp.lanes[0].threads[0].task_ids, vec!["A"]);
        assert!(decomp.lanes[0].depends_on_lanes.is_empty());
    }

    #[test]
    fn test_needs_context_chain_single_lane() {
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
        let decomp = derive_lanes(&graph, "P");

        assert_eq!(decomp.lanes.len(), 1);
        assert_eq!(decomp.lanes[0].head_task_id, "A");
        assert_eq!(decomp.lanes[0].threads.len(), 1);
        assert_eq!(decomp.lanes[0].threads[0].task_ids, vec!["A", "B", "C"]);
    }

    #[test]
    fn test_depends_on_chain_single_lane() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "First"),
            make_created("B", "Second"),
            make_created("C", "Third"),
            make_link("A", "P", "subtask-of"),
            make_link("B", "P", "subtask-of"),
            make_link("C", "P", "subtask-of"),
            make_link("B", "A", "depends-on"),
            make_link("C", "B", "depends-on"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        assert_eq!(decomp.lanes.len(), 1);
        assert_eq!(decomp.lanes[0].head_task_id, "A");
        assert_eq!(decomp.lanes[0].threads.len(), 3);
        assert_eq!(decomp.lanes[0].threads[0].task_ids, vec!["A"]);
        assert_eq!(decomp.lanes[0].threads[1].task_ids, vec!["B"]);
        assert_eq!(decomp.lanes[0].threads[2].task_ids, vec!["C"]);
    }

    #[test]
    fn test_fan_out_multiple_lanes() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Root"),
            make_created("B", "Branch 1"),
            make_created("C", "Branch 2"),
            make_link("A", "P", "subtask-of"),
            make_link("B", "P", "subtask-of"),
            make_link("C", "P", "subtask-of"),
            make_link("B", "A", "depends-on"),
            make_link("C", "A", "depends-on"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        assert_eq!(decomp.lanes.len(), 3);
        assert_eq!(decomp.lanes[0].head_task_id, "A");
        assert!(decomp.lanes[0].depends_on_lanes.is_empty());

        let lane_b = decomp.lanes.iter().find(|l| l.head_task_id == "B").unwrap();
        assert_eq!(lane_b.depends_on_lanes, vec!["A"]);

        let lane_c = decomp.lanes.iter().find(|l| l.head_task_id == "C").unwrap();
        assert_eq!(lane_c.depends_on_lanes, vec!["A"]);
    }

    #[test]
    fn test_fan_in_lane() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Root"),
            make_created("B", "Branch 1"),
            make_created("C", "Branch 2"),
            make_created("D", "Merge"),
            make_link("A", "P", "subtask-of"),
            make_link("B", "P", "subtask-of"),
            make_link("C", "P", "subtask-of"),
            make_link("D", "P", "subtask-of"),
            make_link("B", "A", "depends-on"),
            make_link("C", "A", "depends-on"),
            make_link("D", "B", "depends-on"),
            make_link("D", "C", "depends-on"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        assert_eq!(decomp.lanes.len(), 4);

        let lane_d = decomp.lanes.iter().find(|l| l.head_task_id == "D").unwrap();
        let mut d_deps = lane_d.depends_on_lanes.clone();
        d_deps.sort();
        assert_eq!(d_deps, vec!["B", "C"]);
    }

    #[test]
    fn test_mixed_needs_context_and_depends_on() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("E", "Explore"),
            make_created("PL", "Plan"),
            make_created("I", "Implement"),
            make_created("T", "Test"),
            make_created("V", "Verify"),
            make_link("E", "P", "subtask-of"),
            make_link("PL", "P", "subtask-of"),
            make_link("I", "P", "subtask-of"),
            make_link("T", "P", "subtask-of"),
            make_link("V", "P", "subtask-of"),
            make_link("PL", "E", "needs-context"),
            make_link("V", "T", "needs-context"),
            make_link("I", "PL", "depends-on"),
            make_link("T", "I", "depends-on"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        assert_eq!(decomp.lanes.len(), 1);
        let lane = &decomp.lanes[0];
        assert_eq!(lane.head_task_id, "E");
        assert_eq!(lane.threads.len(), 3);
        assert_eq!(lane.threads[0].task_ids, vec!["E", "PL"]);
        assert_eq!(lane.threads[1].task_ids, vec!["I"]);
        assert_eq!(lane.threads[2].task_ids, vec!["T", "V"]);
    }

    #[test]
    fn test_independent_tasks_separate_lanes() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Task A"),
            make_created("B", "Task B"),
            make_link("A", "P", "subtask-of"),
            make_link("B", "P", "subtask-of"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        assert_eq!(decomp.lanes.len(), 2);
        assert!(decomp.lanes[0].depends_on_lanes.is_empty());
        assert!(decomp.lanes[1].depends_on_lanes.is_empty());
    }

    #[test]
    fn test_no_subtasks() {
        let events = vec![make_created("P", "Parent")];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");
        assert!(decomp.lanes.is_empty());
    }

    // ── Readiness / completion / failure tests ──────────────────────

    #[test]
    fn test_lane_complete() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Task A"),
            make_link("A", "P", "subtask-of"),
            make_closed("A"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");
        assert!(is_lane_complete(&decomp.lanes[0], &graph));
        assert!(!is_lane_failed(&decomp.lanes[0], &graph));
        assert_eq!(
            lane_status(&decomp.lanes[0], &graph, &decomp.lanes),
            LaneStatus::Complete
        );
    }

    #[test]
    fn test_lane_failed_stopped() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Task A"),
            make_link("A", "P", "subtask-of"),
            make_stopped("A"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");
        assert!(is_lane_failed(&decomp.lanes[0], &graph));
        assert_eq!(
            lane_status(&decomp.lanes[0], &graph, &decomp.lanes),
            LaneStatus::Failed
        );
    }

    #[test]
    fn test_lane_failed_wontdo() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Task A"),
            make_link("A", "P", "subtask-of"),
            make_closed_wontdo("A"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");
        assert!(is_lane_failed(&decomp.lanes[0], &graph));
        assert_eq!(
            lane_status(&decomp.lanes[0], &graph, &decomp.lanes),
            LaneStatus::Failed
        );
    }

    #[test]
    fn test_lane_ready_no_deps() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Task A"),
            make_link("A", "P", "subtask-of"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");
        assert!(is_lane_ready_with_decomposition(
            &decomp.lanes[0],
            &graph,
            &decomp.lanes
        ));
        assert_eq!(
            lane_status(&decomp.lanes[0], &graph, &decomp.lanes),
            LaneStatus::Ready
        );
    }

    #[test]
    fn test_lane_blocked_by_predecessor() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Root"),
            make_created("B", "Dependent 1"),
            make_created("C", "Dependent 2"),
            make_link("A", "P", "subtask-of"),
            make_link("B", "P", "subtask-of"),
            make_link("C", "P", "subtask-of"),
            make_link("B", "A", "depends-on"),
            make_link("C", "A", "depends-on"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        let lane_b = decomp.lanes.iter().find(|l| l.head_task_id == "B").unwrap();
        assert!(!is_lane_ready_with_decomposition(
            lane_b,
            &graph,
            &decomp.lanes
        ));
    }

    #[test]
    fn test_lane_ready_after_predecessor_complete() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Root"),
            make_created("B", "Dependent 1"),
            make_created("C", "Dependent 2"),
            make_link("A", "P", "subtask-of"),
            make_link("B", "P", "subtask-of"),
            make_link("C", "P", "subtask-of"),
            make_link("B", "A", "depends-on"),
            make_link("C", "A", "depends-on"),
            make_closed("A"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        let lane_b = decomp.lanes.iter().find(|l| l.head_task_id == "B").unwrap();
        assert!(is_lane_ready_with_decomposition(
            lane_b,
            &graph,
            &decomp.lanes
        ));
    }

    #[test]
    fn test_get_lane_task_ids() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Explore"),
            make_created("B", "Plan"),
            make_link("A", "P", "subtask-of"),
            make_link("B", "P", "subtask-of"),
            make_link("B", "A", "needs-context"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        let ids = get_lane_task_ids(&decomp, "A").unwrap();
        assert!(ids.contains("A"));
        assert!(ids.contains("B"));
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn test_build_task_fan_out_example() {
        let events = vec![
            make_created("P", "Build Parent"),
            make_created("E", "explore"),
            make_created("PL", "plan"),
            make_created("FE", "implement-frontend"),
            make_created("BE", "implement-backend"),
            make_created("TS", "implement-tests"),
            make_link("E", "P", "subtask-of"),
            make_link("PL", "P", "subtask-of"),
            make_link("FE", "P", "subtask-of"),
            make_link("BE", "P", "subtask-of"),
            make_link("TS", "P", "subtask-of"),
            make_link("PL", "E", "needs-context"),
            make_link("FE", "PL", "depends-on"),
            make_link("BE", "PL", "depends-on"),
            make_link("TS", "FE", "depends-on"),
            make_link("TS", "BE", "depends-on"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        assert_eq!(decomp.lanes.len(), 4);

        let lane_e = decomp.lanes.iter().find(|l| l.head_task_id == "E").unwrap();
        assert_eq!(lane_e.threads.len(), 1);
        assert_eq!(lane_e.threads[0].task_ids, vec!["E", "PL"]);
        assert!(lane_e.depends_on_lanes.is_empty());

        let lane_fe = decomp
            .lanes
            .iter()
            .find(|l| l.head_task_id == "FE")
            .unwrap();
        assert_eq!(lane_fe.threads.len(), 1);
        assert_eq!(lane_fe.depends_on_lanes, vec!["E"]);

        let lane_be = decomp
            .lanes
            .iter()
            .find(|l| l.head_task_id == "BE")
            .unwrap();
        assert_eq!(lane_be.threads.len(), 1);
        assert_eq!(lane_be.depends_on_lanes, vec!["E"]);

        let lane_ts = decomp
            .lanes
            .iter()
            .find(|l| l.head_task_id == "TS")
            .unwrap();
        assert_eq!(lane_ts.threads.len(), 1);
        let mut ts_deps = lane_ts.depends_on_lanes.clone();
        ts_deps.sort();
        assert_eq!(ts_deps, vec!["BE", "FE"]);
    }

    #[test]
    fn test_fix_task_example() {
        let events = vec![
            make_created("P", "Fix Parent"),
            make_created("E", "explore"),
            make_created("PL", "plan"),
            make_created("I", "implement"),
            make_created("R", "review"),
            make_link("E", "P", "subtask-of"),
            make_link("PL", "P", "subtask-of"),
            make_link("I", "P", "subtask-of"),
            make_link("R", "P", "subtask-of"),
            make_link("PL", "E", "needs-context"),
            make_link("I", "PL", "needs-context"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        assert_eq!(decomp.lanes.len(), 2);

        let lane_e = decomp.lanes.iter().find(|l| l.head_task_id == "E").unwrap();
        assert_eq!(lane_e.threads.len(), 1);
        assert_eq!(lane_e.threads[0].task_ids, vec!["E", "PL", "I"]);

        let lane_r = decomp.lanes.iter().find(|l| l.head_task_id == "R").unwrap();
        assert_eq!(lane_r.threads.len(), 1);
        assert_eq!(lane_r.threads[0].task_ids, vec!["R"]);
    }

    // ── Prefix resolution tests ─────────────────────────────────────

    #[test]
    fn test_resolve_lane_prefix() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("abc123", "Task 1"),
            make_created("abd456", "Task 2"),
            make_created("xyz789", "Task 3"),
            make_link("abc123", "P", "subtask-of"),
            make_link("abd456", "P", "subtask-of"),
            make_link("xyz789", "P", "subtask-of"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        // Exact match
        assert_eq!(
            resolve_lane_prefix(&decomp, "xyz789", "P"),
            Ok("xyz789".to_string())
        );

        // Unique prefix
        assert_eq!(
            resolve_lane_prefix(&decomp, "xyz", "P"),
            Ok("xyz789".to_string())
        );

        // Ambiguous prefix
        assert!(resolve_lane_prefix(&decomp, "ab", "P").is_err());

        // No match
        assert!(resolve_lane_prefix(&decomp, "zzz", "P").is_err());
    }

    // ── Orchestrator lifecycle integration tests ────────────────────
    //
    // These tests simulate the orchestrator's execution loop by:
    // 1. Deriving lanes and checking lane statuses
    // 2. Advancing tasks (closing/stopping) to simulate agent completion
    // 3. Verifying the lane readiness transitions
    //
    // Note: `is_lane_ready_with_decomposition` checks sessions, and sessions
    // with needs-context chains may show as blocked when the head is ready but
    // later tasks are needs-context blocked. The real orchestrator uses
    // `resolve_next_thread_in_lane` which handles this correctly by resolving
    // individual ready tasks. These tests use `lane_status` for completeness
    // checks and standalone tasks for readiness checks.

    #[test]
    fn test_orchestrator_lifecycle_full_loop() {
        // Simulate orchestrator driving a 4-lane fan-out to completion:
        //   E (explore) → FE, BE (independent) → TS (merge)
        // Uses standalone tasks (no needs-context) to avoid readiness edge cases.
        let mut events = vec![
            make_created("P", "Build Parent"),
            make_created("E", "explore"),
            make_created("FE", "implement-frontend"),
            make_created("BE", "implement-backend"),
            make_created("TS", "implement-tests"),
            make_link("E", "P", "subtask-of"),
            make_link("FE", "P", "subtask-of"),
            make_link("BE", "P", "subtask-of"),
            make_link("TS", "P", "subtask-of"),
            make_link("FE", "E", "depends-on"),
            make_link("BE", "E", "depends-on"),
            make_link("TS", "FE", "depends-on"),
            make_link("TS", "BE", "depends-on"),
        ];

        // --- Iteration 1: Only root lane (E) is ready ---
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");
        assert_eq!(decomp.lanes.len(), 4);

        let ready_lanes: Vec<&Lane> = decomp
            .lanes
            .iter()
            .filter(|l| is_lane_ready_with_decomposition(l, &graph, &decomp.lanes))
            .collect();
        assert_eq!(ready_lanes.len(), 1);
        assert_eq!(ready_lanes[0].head_task_id, "E");

        // Orchestrator runs E lane — complete E
        events.push(make_closed("E"));

        // --- Iteration 2: FE and BE lanes are now ready ---
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        let mut ready_heads: Vec<_> = decomp
            .lanes
            .iter()
            .filter(|l| is_lane_ready_with_decomposition(l, &graph, &decomp.lanes))
            .map(|l| l.head_task_id.as_str())
            .collect();
        ready_heads.sort();
        assert_eq!(ready_heads, vec!["BE", "FE"]);

        // TS lane should be blocked
        let ts_lane = decomp
            .lanes
            .iter()
            .find(|l| l.head_task_id == "TS")
            .unwrap();
        assert_eq!(
            lane_status(ts_lane, &graph, &decomp.lanes),
            LaneStatus::Blocked
        );

        // Orchestrator runs both FE and BE — complete them
        events.push(make_closed("FE"));
        events.push(make_closed("BE"));

        // --- Iteration 3: TS lane is now ready ---
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        let ready_lanes: Vec<&Lane> = decomp
            .lanes
            .iter()
            .filter(|l| is_lane_ready_with_decomposition(l, &graph, &decomp.lanes))
            .collect();
        assert_eq!(ready_lanes.len(), 1);
        assert_eq!(ready_lanes[0].head_task_id, "TS");

        // Complete TS
        events.push(make_closed("TS"));

        // --- Iteration 4: All lanes complete, no ready lanes ---
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        let ready_lanes: Vec<&Lane> = decomp
            .lanes
            .iter()
            .filter(|l| is_lane_ready_with_decomposition(l, &graph, &decomp.lanes))
            .collect();
        assert_eq!(ready_lanes.len(), 0);

        // All lanes should be complete
        for lane in &decomp.lanes {
            assert_eq!(
                lane_status(lane, &graph, &decomp.lanes),
                LaneStatus::Complete
            );
        }
    }

    #[test]
    fn test_orchestrator_lifecycle_fan_out_fan_in() {
        // root → 2 branches → merge
        let mut events = vec![
            make_created("P", "Parent"),
            make_created("R", "Root"),
            make_created("B1", "Branch 1"),
            make_created("B2", "Branch 2"),
            make_created("M", "Merge"),
            make_link("R", "P", "subtask-of"),
            make_link("B1", "P", "subtask-of"),
            make_link("B2", "P", "subtask-of"),
            make_link("M", "P", "subtask-of"),
            make_link("B1", "R", "depends-on"),
            make_link("B2", "R", "depends-on"),
            make_link("M", "B1", "depends-on"),
            make_link("M", "B2", "depends-on"),
        ];

        // Phase 1: Only root ready
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        let ready: Vec<_> = decomp
            .lanes
            .iter()
            .filter(|l| is_lane_ready_with_decomposition(l, &graph, &decomp.lanes))
            .map(|l| l.head_task_id.as_str())
            .collect();
        assert_eq!(ready, vec!["R"]);

        events.push(make_closed("R"));

        // Phase 2: Both branches ready, merge blocked
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        let mut ready: Vec<_> = decomp
            .lanes
            .iter()
            .filter(|l| is_lane_ready_with_decomposition(l, &graph, &decomp.lanes))
            .map(|l| l.head_task_id.as_str())
            .collect();
        ready.sort();
        assert_eq!(ready, vec!["B1", "B2"]);

        let m_lane = decomp.lanes.iter().find(|l| l.head_task_id == "M").unwrap();
        assert!(!is_lane_ready_with_decomposition(
            m_lane,
            &graph,
            &decomp.lanes
        ));

        // Complete B1 only — merge still blocked
        events.push(make_closed("B1"));
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        let m_lane = decomp.lanes.iter().find(|l| l.head_task_id == "M").unwrap();
        assert!(!is_lane_ready_with_decomposition(
            m_lane,
            &graph,
            &decomp.lanes
        ));

        // Complete B2 — merge now ready
        events.push(make_closed("B2"));
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        let m_lane = decomp.lanes.iter().find(|l| l.head_task_id == "M").unwrap();
        assert!(is_lane_ready_with_decomposition(
            m_lane,
            &graph,
            &decomp.lanes
        ));
    }

    #[test]
    fn test_orchestrator_lifecycle_failure_isolation() {
        // Root R → fan-out to A and B (separate lanes).
        // C depends on both A and B (merge lane).
        // Fail A → C stays blocked; B continues independently.
        let mut events = vec![
            make_created("P", "Parent"),
            make_created("R", "Root"),
            make_created("A", "Branch A"),
            make_created("B", "Branch B"),
            make_created("C", "Merge"),
            make_link("R", "P", "subtask-of"),
            make_link("A", "P", "subtask-of"),
            make_link("B", "P", "subtask-of"),
            make_link("C", "P", "subtask-of"),
            make_link("A", "R", "depends-on"),
            make_link("B", "R", "depends-on"),
            make_link("C", "A", "depends-on"),
            make_link("C", "B", "depends-on"),
        ];

        // Only root lane is ready initially
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");
        assert_eq!(decomp.lanes.len(), 4); // R, A, B, C

        let ready: Vec<_> = decomp
            .lanes
            .iter()
            .filter(|l| is_lane_ready_with_decomposition(l, &graph, &decomp.lanes))
            .map(|l| l.head_task_id.as_str())
            .collect();
        assert_eq!(ready, vec!["R"]);

        // Complete root — A and B become ready
        events.push(make_closed("R"));
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        let mut ready: Vec<_> = decomp
            .lanes
            .iter()
            .filter(|l| is_lane_ready_with_decomposition(l, &graph, &decomp.lanes))
            .map(|l| l.head_task_id.as_str())
            .collect();
        ready.sort();
        assert_eq!(ready, vec!["A", "B"]);

        // Fail A (stop task A)
        events.push(make_stopped("A"));

        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        // Lane A should be Failed
        let a_lane = decomp.lanes.iter().find(|l| l.head_task_id == "A").unwrap();
        assert_eq!(
            lane_status(a_lane, &graph, &decomp.lanes),
            LaneStatus::Failed
        );

        // Lane B should still be Ready (independent)
        let b_lane = decomp.lanes.iter().find(|l| l.head_task_id == "B").unwrap();
        assert!(is_lane_ready_with_decomposition(
            b_lane,
            &graph,
            &decomp.lanes
        ));

        // Lane C should be blocked (depends on A which is failed, not complete)
        let c_lane = decomp.lanes.iter().find(|l| l.head_task_id == "C").unwrap();
        assert!(!is_lane_ready_with_decomposition(
            c_lane,
            &graph,
            &decomp.lanes
        ));

        // B can still complete independently
        events.push(make_closed("B"));
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        let b_lane = decomp.lanes.iter().find(|l| l.head_task_id == "B").unwrap();
        assert_eq!(
            lane_status(b_lane, &graph, &decomp.lanes),
            LaneStatus::Complete
        );

        // C remains blocked (A is failed)
        let c_lane = decomp.lanes.iter().find(|l| l.head_task_id == "C").unwrap();
        assert!(!is_lane_ready_with_decomposition(
            c_lane,
            &graph,
            &decomp.lanes
        ));
    }

    #[test]
    fn test_orchestrator_lifecycle_single_lane_sequential() {
        // Linear chain: A → B → C (all in one lane, separate sessions via depends-on)
        let mut events = vec![
            make_created("P", "Parent"),
            make_created("A", "First"),
            make_created("B", "Second"),
            make_created("C", "Third"),
            make_link("A", "P", "subtask-of"),
            make_link("B", "P", "subtask-of"),
            make_link("C", "P", "subtask-of"),
            make_link("B", "A", "depends-on"),
            make_link("C", "B", "depends-on"),
        ];

        // Verify: single lane with 3 sessions
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");
        assert_eq!(decomp.lanes.len(), 1);
        assert_eq!(decomp.lanes[0].threads.len(), 3);

        // Only 1 ready lane
        let ready: Vec<_> = decomp
            .lanes
            .iter()
            .filter(|l| is_lane_ready_with_decomposition(l, &graph, &decomp.lanes))
            .collect();
        assert_eq!(ready.len(), 1);

        // Sequential execution: A first
        events.push(make_closed("A"));

        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");
        let lane = &decomp.lanes[0];
        assert!(!is_lane_complete(lane, &graph));
        assert!(is_lane_ready_with_decomposition(
            lane,
            &graph,
            &decomp.lanes
        ));

        // Then B
        events.push(make_closed("B"));

        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");
        let lane = &decomp.lanes[0];
        assert!(!is_lane_complete(lane, &graph));
        assert!(is_lane_ready_with_decomposition(
            lane,
            &graph,
            &decomp.lanes
        ));

        // Then C — lane complete
        events.push(make_closed("C"));

        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");
        let lane = &decomp.lanes[0];
        assert!(is_lane_complete(lane, &graph));
        assert_eq!(
            lane_status(lane, &graph, &decomp.lanes),
            LaneStatus::Complete
        );

        // No more ready lanes
        let ready: Vec<_> = decomp
            .lanes
            .iter()
            .filter(|l| is_lane_ready_with_decomposition(l, &graph, &decomp.lanes))
            .collect();
        assert_eq!(ready.len(), 0);
    }

    #[test]
    fn test_lane_ready_in_progress_not_ready() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Task A"),
            make_link("A", "P", "subtask-of"),
            make_started("A"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");
        assert!(!is_lane_ready_with_decomposition(
            &decomp.lanes[0],
            &graph,
            &decomp.lanes
        ));
    }

    #[test]
    fn test_lane_ready_in_progress_chain() {
        // A needs-context chain where the head task is InProgress
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Head"),
            make_created("B", "Follower"),
            make_link("A", "P", "subtask-of"),
            make_link("B", "P", "subtask-of"),
            make_link("B", "A", "needs-context"),
            make_started("A"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");
        // The lane with A as head should not be ready since A is InProgress
        let lane_a = decomp.lanes.iter().find(|l| l.head_task_id == "A").unwrap();
        assert!(!is_lane_ready_with_decomposition(
            lane_a,
            &graph,
            &decomp.lanes
        ));
    }

    #[test]
    fn test_lane_status_blocked_by_predecessor() {
        // Fan-out: A is root, B and C depend on A.
        // A not complete → B and C should be Blocked.
        let events = vec![
            make_created("P", "Parent"),
            make_created("A", "Root"),
            make_created("B", "Branch 1"),
            make_created("C", "Branch 2"),
            make_link("A", "P", "subtask-of"),
            make_link("B", "P", "subtask-of"),
            make_link("C", "P", "subtask-of"),
            make_link("B", "A", "depends-on"),
            make_link("C", "A", "depends-on"),
        ];
        let graph = materialize_graph(&events);
        let decomp = derive_lanes(&graph, "P");

        // Lane A should be Ready (no predecessors)
        let a_lane = decomp.lanes.iter().find(|l| l.head_task_id == "A").unwrap();
        assert_eq!(
            lane_status(a_lane, &graph, &decomp.lanes),
            LaneStatus::Ready
        );

        // Lane B should be Blocked (A not complete)
        let b_lane = decomp.lanes.iter().find(|l| l.head_task_id == "B").unwrap();
        assert_eq!(
            lane_status(b_lane, &graph, &decomp.lanes),
            LaneStatus::Blocked
        );

        // Lane C should be Blocked (A not complete)
        let c_lane = decomp.lanes.iter().find(|l| l.head_task_id == "C").unwrap();
        assert_eq!(
            lane_status(c_lane, &graph, &decomp.lanes),
            LaneStatus::Blocked
        );
    }

    // ── ThreadId tests ──────────────────────────────────────────────

    #[test]
    fn test_thread_id_parse_single() {
        let tid = ThreadId::parse("abcdefghijklmnopqrstuvwxyzabcdef").unwrap();
        assert_eq!(tid.head, "abcdefghijklmnopqrstuvwxyzabcdef");
        assert_eq!(tid.tail, "abcdefghijklmnopqrstuvwxyzabcdef");
        assert!(tid.is_single());
    }

    #[test]
    fn test_thread_id_parse_head_tail() {
        let tid =
            ThreadId::parse("abcdefghijklmnopqrstuvwxyzabcdef:fedcbazyxwvutsrqponmlkjihgfedcba")
                .unwrap();
        assert_eq!(tid.head, "abcdefghijklmnopqrstuvwxyzabcdef");
        assert_eq!(tid.tail, "fedcbazyxwvutsrqponmlkjihgfedcba");
        assert!(!tid.is_single());
    }

    #[test]
    fn test_thread_id_parse_empty_errors() {
        assert!(ThreadId::parse("").is_err());
        assert!(ThreadId::parse(":tail").is_err());
        assert!(ThreadId::parse("head:").is_err());
    }

    #[test]
    fn test_thread_id_parse_rejects_short_ids() {
        // Prefix / short IDs must be rejected
        assert!(ThreadId::parse("abcdefg").is_err());
        assert!(ThreadId::parse("abcdefghijklmnop").is_err()); // 16 chars
    }

    #[test]
    fn test_thread_id_parse_rejects_too_long() {
        let too_long = "a".repeat(33);
        assert!(ThreadId::parse(&too_long).is_err());
    }

    #[test]
    fn test_thread_id_parse_rejects_non_lowercase() {
        // Uppercase
        assert!(ThreadId::parse("ABCDEFGHIJKLMNOPQRSTUVWXYZABCDEF").is_err());
        // Digits
        assert!(ThreadId::parse("abcdefghijklmnopqrstuvwxyz123456").is_err());
        // Mixed case
        assert!(ThreadId::parse("Abcdefghijklmnopqrstuvwxyzabcdef").is_err());
    }

    #[test]
    fn test_thread_id_parse_rejects_short_halves_in_pair() {
        let valid = "abcdefghijklmnopqrstuvwxyzabcdef";
        // Short head with valid tail
        assert!(ThreadId::parse(&format!("short:{valid}")).is_err());
        // Valid head with short tail
        assert!(ThreadId::parse(&format!("{valid}:short")).is_err());
    }

    #[test]
    fn test_thread_id_single_constructor() {
        let tid = ThreadId::single("mytaskid".to_string());
        assert_eq!(tid.head, "mytaskid");
        assert_eq!(tid.tail, "mytaskid");
        assert!(tid.is_single());
    }

    #[test]
    fn test_thread_id_serialize_roundtrip() {
        // Single
        let tid = ThreadId::parse("abcdefghijklmnopqrstuvwxyzabcdef").unwrap();
        let serialized = tid.serialize();
        assert_eq!(serialized, "abcdefghijklmnopqrstuvwxyzabcdef");
        let roundtripped = ThreadId::parse(&serialized).unwrap();
        assert_eq!(roundtripped, tid);

        // Head:tail
        let tid =
            ThreadId::parse("abcdefghijklmnopqrstuvwxyzabcdef:fedcbazyxwvutsrqponmlkjihgfedcba")
                .unwrap();
        let serialized = tid.serialize();
        assert_eq!(
            serialized,
            "abcdefghijklmnopqrstuvwxyzabcdef:fedcbazyxwvutsrqponmlkjihgfedcba"
        );
        let roundtripped = ThreadId::parse(&serialized).unwrap();
        assert_eq!(roundtripped, tid);
    }

    #[test]
    fn test_thread_id_display_uses_short_ids() {
        let tid = ThreadId::single("abcdefghijklmnopqrstuvwxyzabcdef".to_string());
        assert_eq!(format!("{tid}"), "abcdefg");

        let tid = ThreadId {
            head: "abcdefghijklmnopqrstuvwxyzabcdef".to_string(),
            tail: "zyxwvutsrqponmlkjihgfedcbazyxwvu".to_string(),
        };
        assert_eq!(format!("{tid}"), "abcdefg:zyxwvut");
    }

    #[test]
    fn test_thread_id_resolve() {
        let events = vec![
            make_created("P", "Parent"),
            make_created("abcdefghijklmnopqrstuvwxyzabcdef", "Task A"),
            make_created("zyxwvutsrqponmlkjihgfedcbazyxwvu", "Task B"),
            make_link("abcdefghijklmnopqrstuvwxyzabcdef", "P", "subtask-of"),
            make_link("zyxwvutsrqponmlkjihgfedcbazyxwvu", "P", "subtask-of"),
        ];
        let graph = materialize_graph(&events);

        // Single prefix
        let tid = ThreadId::resolve("abcdefg", &graph).unwrap();
        assert_eq!(tid.head, "abcdefghijklmnopqrstuvwxyzabcdef");
        assert_eq!(tid.tail, "abcdefghijklmnopqrstuvwxyzabcdef");
        assert!(tid.is_single());

        // Head:tail prefixes
        let tid = ThreadId::resolve("abcdefg:zyxwvut", &graph).unwrap();
        assert_eq!(tid.head, "abcdefghijklmnopqrstuvwxyzabcdef");
        assert_eq!(tid.tail, "zyxwvutsrqponmlkjihgfedcbazyxwvu");
    }
}
