//! Task DAG — edge store and graph materialization
//!
//! The `TaskGraph` is the primary data structure for task management.
//! Tasks are nodes, relationships are edges. Adding a new link kind
//! requires only a new entry in `LINK_KINDS` — zero changes to EdgeStore.

use super::types::{FastHashMap, Task, TaskComment, TaskEvent, TaskOutcome, TaskStatus};

/// Link kind metadata — defines cardinality rules and blocking behavior.
/// Checked at write time when adding links.
pub struct LinkKind {
    /// The kind string (e.g., "blocked-by")
    pub name: &'static str,
    /// Max active forward links per `from` node.
    /// None = unlimited, Some(1) = single-link kind (auto-replace on conflict).
    pub max_forward: Option<usize>,
    /// Max active reverse links per `to` node.
    /// None = unlimited, Some(1) = single reverse (e.g., orchestrates: one
    /// orchestrator per epic).
    pub max_reverse: Option<usize>,
    /// Whether unresolved links of this kind exclude the `from` task
    /// from the ready queue.
    pub blocks_ready: bool,
    /// Whether targets must resolve to task IDs (vs. external refs).
    pub task_only: bool,
}

/// Registry of all link kinds. Adding a new kind = one entry here + zero
/// changes to EdgeStore, TaskGraph, or materialization.
pub const LINK_KINDS: &[LinkKind] = &[
    // Legacy blocking link (deprecated in favor of semantic types)
    LinkKind {
        name: "blocked-by",
        max_forward: None,
        max_reverse: None,
        blocks_ready: true,
        task_only: true,
    },
    // Semantic blocking links (replace blocked-by with domain-specific relationships)
    LinkKind {
        name: "validates",
        max_forward: None,
        max_reverse: None,
        blocks_ready: true,
        task_only: true,
    },
    LinkKind {
        name: "remediates",
        max_forward: None,
        max_reverse: None,
        blocks_ready: true,
        task_only: true,
    },
    LinkKind {
        name: "depends-on",
        max_forward: None,
        max_reverse: None,
        blocks_ready: true,
        task_only: true,
    },
    // Non-blocking links
    LinkKind {
        name: "sourced-from",
        max_forward: None,
        max_reverse: None,
        blocks_ready: false,
        task_only: false,
    },
    LinkKind {
        name: "subtask-of",
        max_forward: Some(1),
        max_reverse: None,
        blocks_ready: false,
        task_only: true,
    },
    LinkKind {
        name: "implements-plan",
        max_forward: Some(1),
        max_reverse: Some(1),
        blocks_ready: false,
        task_only: false,
    },
    LinkKind {
        name: "orchestrates",
        max_forward: Some(1),
        max_reverse: Some(1),
        blocks_ready: false,
        task_only: true,
    },
    LinkKind {
        name: "decomposes-plan",
        max_forward: None,
        max_reverse: None,
        blocks_ready: false,
        task_only: false,
    },
    LinkKind {
        name: "adds-plan",
        max_forward: None,
        max_reverse: None,
        blocks_ready: false,
        task_only: false,
    },
    LinkKind {
        name: "fixes",
        max_forward: None,
        max_reverse: None,
        blocks_ready: false,
        task_only: false,
    },
    LinkKind {
        name: "supersedes",
        max_forward: Some(1),
        max_reverse: None,
        blocks_ready: false,
        task_only: true,
    },
    // Provenance: spawned task → spawner (tracks automatic task creation)
    LinkKind {
        name: "spawned-by",
        max_forward: Some(1),
        max_reverse: None,
        blocks_ready: false,
        task_only: true,
    },
];

/// Per-link metadata stored alongside edges.
#[derive(Debug, Clone, Default)]
pub struct LinkMeta {
    /// Whether to auto-start the `from` task when the `to` (blocker) completes.
    pub autorun: bool,
}

/// Generic edge store — indexes all links by kind.
///
/// Two parallel maps: forward (from → [to]) and reverse (to → [from]),
/// both keyed by link kind. Adding a new link kind requires zero changes
/// to this struct.
pub struct EdgeStore {
    /// kind → (from_id → [to_id])
    forward: FastHashMap<String, FastHashMap<String, Vec<String>>>,
    /// kind → (to_id → [from_id])
    reverse: FastHashMap<String, FastHashMap<String, Vec<String>>>,
    /// Per-link metadata keyed by (from, to, kind)
    link_meta: FastHashMap<(String, String, String), LinkMeta>,
}

impl EdgeStore {
    /// Create a new empty edge store
    pub fn new() -> Self {
        Self {
            forward: FastHashMap::default(),
            reverse: FastHashMap::default(),
            link_meta: FastHashMap::default(),
        }
    }

    /// Add a link to the store (idempotent — duplicate links are ignored)
    pub fn add(&mut self, from: &str, to: &str, kind: &str) {
        // Forward: from → to
        let targets = self
            .forward
            .entry(kind.to_string())
            .or_default()
            .entry(from.to_string())
            .or_default();
        if !targets.contains(&to.to_string()) {
            targets.push(to.to_string());
        }

        // Reverse: to → from
        let referrers = self
            .reverse
            .entry(kind.to_string())
            .or_default()
            .entry(to.to_string())
            .or_default();
        if !referrers.contains(&from.to_string()) {
            referrers.push(from.to_string());
        }
    }

    /// Remove a link from the store
    pub fn remove(&mut self, from: &str, to: &str, kind: &str) {
        // Forward: remove `to` from `from`'s list
        if let Some(kind_map) = self.forward.get_mut(kind) {
            if let Some(targets) = kind_map.get_mut(from) {
                targets.retain(|t| t != to);
            }
        }

        // Reverse: remove `from` from `to`'s list
        if let Some(kind_map) = self.reverse.get_mut(kind) {
            if let Some(referrers) = kind_map.get_mut(to) {
                referrers.retain(|r| r != from);
            }
        }

        // Clean up metadata
        self.link_meta.remove(&(from.to_string(), to.to_string(), kind.to_string()));
    }

    /// Forward lookup: given a `from` node and kind, return all targets.
    pub fn targets(&self, from: &str, kind: &str) -> &[String] {
        self.forward
            .get(kind)
            .and_then(|m| m.get(from))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Reverse lookup: given a `to` node and kind, return all referrers.
    pub fn referrers(&self, to: &str, kind: &str) -> &[String] {
        self.reverse
            .get(kind)
            .and_then(|m| m.get(to))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Forward lookup for single-link kinds: return the one target (if any).
    pub fn target(&self, from: &str, kind: &str) -> Option<&str> {
        debug_assert!(
            LINK_KINDS
                .iter()
                .any(|k| k.name == kind && k.max_forward == Some(1)),
            "target() called on many-link kind '{kind}'"
        );
        self.targets(from, kind).first().map(|s| s.as_str())
    }

    /// Reverse lookup for single-link kinds: return the one referrer (if any).
    #[allow(dead_code)]
    pub fn referrer(&self, to: &str, kind: &str) -> Option<&str> {
        self.referrers(to, kind).first().map(|s| s.as_str())
    }

    /// Check if a specific forward link exists.
    pub fn has_link(&self, from: &str, to: &str, kind: &str) -> bool {
        self.targets(from, kind).contains(&to.to_string())
    }

    /// Set metadata for a specific link.
    pub fn set_meta(&mut self, from: &str, to: &str, kind: &str, meta: LinkMeta) {
        self.link_meta.insert(
            (from.to_string(), to.to_string(), kind.to_string()),
            meta,
        );
    }

    /// Get metadata for a specific link.
    pub fn get_meta(&self, from: &str, to: &str, kind: &str) -> Option<&LinkMeta> {
        self.link_meta.get(&(from.to_string(), to.to_string(), kind.to_string()))
    }
}

impl Default for EdgeStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Materialized task graph (computed from events)
///
/// Events (on the aiki/tasks branch) are the source of truth; the
/// EdgeStore indexes are derived during replay.
pub struct TaskGraph {
    /// Node data (tasks)
    pub tasks: FastHashMap<String, Task>,
    /// Generic edge indexes (forward + reverse for every link kind)
    pub edges: EdgeStore,
    /// Slug index: (parent_id, slug) → child task ID.
    /// Enables O(1) lookup of subtasks by slug within a parent scope.
    pub slug_index: FastHashMap<(String, String), String>,
}

impl TaskGraph {
    /// A task is blocked if any of its blocking links are unsatisfied.
    ///
    /// Unblocking rules differ by link type:
    /// - `validates` / `remediates`: Unblock on **any** terminal state
    ///   (Closed regardless of outcome, or Stopped).
    /// - `depends-on` / `blocked-by`: Unblock **only** when blocker is
    ///   Closed with Done outcome. Stopped or won't-do keeps the task blocked.
    ///
    /// Parent links (subtask-of) do not block.
    pub fn is_blocked(&self, task_id: &str) -> bool {
        // Link types that unblock on any terminal state (Closed or Stopped)
        // "follows-up" kept for backward compat with existing links (renamed to "remediates")
        const TERMINAL_UNBLOCK: &[&str] = &["validates", "remediates", "follows-up"];
        // Link types that only unblock on Closed(Done)
        const DONE_ONLY_UNBLOCK: &[&str] = &["blocked-by", "depends-on"];

        let terminal_blocked = TERMINAL_UNBLOCK.iter().any(|link_type| {
            self.edges
                .targets(task_id, link_type)
                .iter()
                .any(|blocker_id| {
                    self.tasks.get(blocker_id).map_or(true, |t| {
                        // Unblocks when blocker reaches any terminal state
                        !matches!(t.status, TaskStatus::Closed | TaskStatus::Stopped)
                    })
                })
        });

        let done_blocked = DONE_ONLY_UNBLOCK.iter().any(|link_type| {
            self.edges
                .targets(task_id, link_type)
                .iter()
                .any(|blocker_id| {
                    self.tasks.get(blocker_id).map_or(true, |t| {
                        // Only unblocks when blocker is Closed with Done outcome
                        !(t.status == TaskStatus::Closed
                            && t.closed_outcome == Some(TaskOutcome::Done))
                    })
                })
        });

        terminal_blocked || done_blocked
    }

    /// A parent task cannot be closed while it has open children.
    /// Returns the list of open children, or empty if closeable.
    #[allow(dead_code)]
    pub fn open_children(&self, task_id: &str) -> Vec<&Task> {
        self.edges
            .referrers(task_id, "subtask-of")
            .iter()
            .filter_map(|c| self.tasks.get(c))
            .filter(|t| t.status != TaskStatus::Closed)
            .collect()
    }

    /// Children of a parent: `edges.referrers(parent_id, "subtask-of")`.
    pub fn children_of(&self, parent_id: &str) -> Vec<&Task> {
        self.edges
            .referrers(parent_id, "subtask-of")
            .iter()
            .filter_map(|c| self.tasks.get(c))
            .collect()
    }

    /// Walk `subtask-of` links upward to get the full ancestor chain.
    /// Returns parent IDs from immediate parent to root.
    pub fn ancestor_chain(&self, task_id: &str) -> Vec<String> {
        let mut ancestors = Vec::new();
        let mut visited = std::collections::HashSet::new();
        visited.insert(task_id.to_string());
        let mut current = task_id;
        while let Some(parent) = self.edges.target(current, "subtask-of") {
            if !visited.insert(parent.to_string()) {
                break; // cycle detected — defense-in-depth
            }
            ancestors.push(parent.to_string());
            current = parent;
        }
        ancestors
    }

    /// Cycle detection for a proposed new link.
    /// Walks `edges.targets(id, kind)` via DFS to verify acyclicity.
    #[allow(dead_code)]
    pub fn would_create_cycle(&self, from: &str, to: &str, kind: &str) -> bool {
        // Adding from→to would create a cycle if `from` is reachable from `to`
        // by following existing links of the same kind
        let mut visited = std::collections::HashSet::new();
        let mut stack = vec![to];
        while let Some(node) = stack.pop() {
            if node == from {
                return true;
            }
            if visited.insert(node.to_string()) {
                for target in self.edges.targets(node, kind) {
                    stack.push(target);
                }
            }
        }
        false
    }

    /// Find a subtask by its slug within a parent scope.
    ///
    /// Returns `None` if the parent has no subtask with the given slug.
    pub fn find_by_slug(&self, parent_id: &str, slug: &str) -> Option<&Task> {
        let key = (parent_id.to_string(), slug.to_string());
        self.slug_index.get(&key).and_then(|id| self.tasks.get(id))
    }

    /// Find tasks that should be auto-started after a task closes.
    ///
    /// Performs a reverse lookup across all blocking link kinds to find tasks
    /// that have a blocking link pointing to `closed_task_id`. For each candidate:
    /// 1. Check if ANY of its blocking links has `autorun: true` in metadata
    /// 2. Check if ALL blockers are now closed (via `is_blocked`)
    ///
    /// Returns task IDs that should be auto-started.
    pub fn find_autorun_candidates(&self, closed_task_id: &str) -> Vec<String> {
        const BLOCKING_KINDS: &[&str] = &["validates", "remediates", "follows-up", "depends-on", "blocked-by"];

        let mut candidates = std::collections::HashSet::new();

        for kind in BLOCKING_KINDS {
            // Reverse lookup: find tasks that have a link of this kind pointing to closed_task_id
            for candidate_id in self.edges.referrers(closed_task_id, kind) {
                // Skip if already checked or task doesn't exist
                if candidates.contains(candidate_id) {
                    continue;
                }
                if let Some(task) = self.tasks.get(candidate_id) {
                    // Only auto-start tasks that are Open or Stopped
                    if task.status != TaskStatus::Open && task.status != TaskStatus::Stopped {
                        continue;
                    }
                }

                // Check if this candidate has autorun on ANY of its blocking links
                let has_autorun = BLOCKING_KINDS.iter().any(|k| {
                    self.edges.targets(candidate_id, k).iter().any(|target| {
                        self.edges
                            .get_meta(candidate_id, target, k)
                            .map_or(false, |m| m.autorun)
                    })
                });

                if has_autorun && !self.is_blocked(candidate_id) {
                    candidates.insert(candidate_id.clone());
                }
            }
        }

        candidates.into_iter().collect()
    }

    /// Full provenance chain: walk `sourced-from` links.
    /// Uses a visited set to handle cycles.
    #[allow(dead_code)]
    pub fn provenance_chain(&self, task_id: &str) -> Vec<String> {
        let mut chain = Vec::new();
        let mut visited = std::collections::HashSet::new();
        visited.insert(task_id.to_string());
        let mut stack = vec![task_id.to_string()];
        while let Some(node) = stack.pop() {
            for target in self.edges.targets(&node, "sourced-from") {
                if visited.insert(target.clone()) {
                    chain.push(target.clone());
                    stack.push(target.clone());
                }
            }
        }
        chain
    }

    /// Reverse provenance: what tasks came from this origin?
    #[allow(dead_code)]
    pub fn spawned_from(&self, origin: &str) -> Vec<&Task> {
        self.edges
            .referrers(origin, "sourced-from")
            .iter()
            .filter_map(|id| self.tasks.get(id))
            .collect()
    }
}

/// Materialize a task graph from an event stream.
///
/// Processes events in order and builds up the current state of each task
/// plus the edge indexes for all link kinds.
#[must_use]
pub fn materialize_graph(events: &[TaskEvent]) -> TaskGraph {
    let mut tasks: FastHashMap<String, Task> = FastHashMap::default();
    let mut edges = EdgeStore::new();
    let mut slug_index: FastHashMap<(String, String), String> = FastHashMap::default();

    for event in events {
        process_event(event, &mut tasks, &mut edges, &mut slug_index);
    }

    TaskGraph { tasks, edges, slug_index }
}

/// Materialize a task graph from an event stream with change IDs.
///
/// Like `materialize_graph`, but accepts `EventWithId` to populate comment IDs.
/// This is needed when generating followup tasks that need to reference specific
/// comments via `source: comment:<change_id>`.
#[must_use]
pub fn materialize_graph_with_ids(events: &[super::storage::EventWithId]) -> TaskGraph {
    let plain_events: Vec<&TaskEvent> = events.iter().map(|e| &e.event).collect();
    let mut graph = materialize_graph_refs(&plain_events);

    // Second pass: populate comment IDs from change_ids
    for event_with_id in events {
        if let TaskEvent::CommentAdded {
            task_ids,
            text,
            timestamp,
            ..
        } = &event_with_id.event
        {
            for task_id in task_ids {
                if let Some(task) = graph.tasks.get_mut(task_id) {
                    // Find the matching comment and set its ID
                    for comment in &mut task.comments {
                        if comment.id.is_none()
                            && comment.text == *text
                            && comment.timestamp == *timestamp
                        {
                            comment.id = Some(event_with_id.change_id.clone());
                            break;
                        }
                    }
                }
            }
        }
    }

    graph
}

/// Internal: materialize from a slice of event references.
fn materialize_graph_refs(events: &[&TaskEvent]) -> TaskGraph {
    let mut tasks: FastHashMap<String, Task> = FastHashMap::default();
    let mut edges = EdgeStore::new();
    let mut slug_index: FastHashMap<(String, String), String> = FastHashMap::default();

    for event in events {
        process_event(event, &mut tasks, &mut edges, &mut slug_index);
    }

    TaskGraph { tasks, edges, slug_index }
}

/// Process a single event into the tasks map, edge store, and slug index.
fn process_event(
    event: &TaskEvent,
    tasks: &mut FastHashMap<String, Task>,
    edges: &mut EdgeStore,
    slug_index: &mut FastHashMap<(String, String), String>,
) {
    match event {
        TaskEvent::Created {
            task_id,
            name,
            slug,
            task_type,
            priority,
            assignee,
            sources,
            template,
            working_copy,
            instructions,
            data,
            timestamp,
        } => {
            // Index old-style dot-notation parent-child as subtask-of (backward compat).
            if let Some(parent_id) = super::id::get_parent_id(task_id) {
                edges.add(task_id, parent_id, "subtask-of");
                // Index slug under the dot-notation parent
                if let Some(s) = slug {
                    slug_index.insert((parent_id.to_string(), s.clone()), task_id.clone());
                }
            }

            // Index old-style sources as sourced-from edges (backward compat).
            for source in sources {
                edges.add(task_id, source, "sourced-from");
            }

            // Index old-style data attributes as edges.
            // Synthesize implements-plan only for epic tasks:
            // - Must have data.plan (the plan file path)
            // - Must NOT have data.epic (decompose/build tasks have this)
            // - data.plan must be a file path, not a task ID (old system used
            //   data.plan for epic task IDs)
            if let Some(plan) = data.get("plan") {
                let plan_raw = plan.strip_prefix("file:").unwrap_or(plan);
                if !data.contains_key("epic") && plan_raw.contains('/') {
                    let target = if plan.starts_with("file:") {
                        plan.clone()
                    } else {
                        format!("file:{}", plan)
                    };
                    edges.add(task_id, &target, "implements-plan");
                }
            }

            if let Some(epic_id) = data.get("epic") {
                edges.add(task_id, epic_id, "orchestrates");
            }

            if let Some(scope_id) = data.get("scope.id") {
                let scope_kind = data.get("scope.kind").map(|s| s.as_str());
                let target = match scope_kind {
                    Some("plan") | Some("implementation") => {
                        if scope_id.starts_with("file:") {
                            scope_id.clone()
                        } else {
                            format!("file:{}", scope_id)
                        }
                    }
                    _ => scope_id.clone(),
                };
                edges.add(task_id, &target, "scoped-to");
            }

            if let Some(task_ids_str) = data.get("scope.task_ids") {
                for tid in task_ids_str.split(',') {
                    let tid = tid.trim();
                    if !tid.is_empty() {
                        edges.add(task_id, tid, "scoped-to");
                    }
                }
            }

            tasks.insert(
                task_id.clone(),
                Task {
                    id: task_id.clone(),
                    name: name.clone(),
                    slug: slug.clone(),
                    task_type: task_type.clone(),
                    status: TaskStatus::Open,
                    priority: *priority,
                    assignee: assignee.clone(),
                    sources: sources.clone(),
                    template: template.clone(),
                    working_copy: working_copy.clone(),
                    instructions: instructions.clone(),
                    data: data.clone(),
                    created_at: *timestamp,
                    started_at: None,
                    claimed_by_session: None,
                    last_session_id: None,
                    stopped_reason: None,
                    closed_outcome: None,
                    summary: None,
                    turn_started: None,
                    turn_closed: None,
                    turn_stopped: None,
                    comments: Vec::new(),
                },
            );
        }
        TaskEvent::Started {
            task_ids,
            session_id,
            turn_id,
            timestamp,
            ..
        } => {
            for task_id in task_ids {
                if let Some(task) = tasks.get_mut(task_id) {
                    task.status = TaskStatus::InProgress;
                    task.stopped_reason = None;
                    task.claimed_by_session = session_id.clone();
                    task.last_session_id = session_id.clone();
                    task.started_at = Some(*timestamp);
                    task.turn_started = turn_id.clone();
                    task.turn_stopped = None;
                }
            }
        }
        TaskEvent::Stopped {
            task_ids,
            reason,
            turn_id,
            ..
        } => {
            for task_id in task_ids {
                if let Some(task) = tasks.get_mut(task_id) {
                    task.status = TaskStatus::Stopped;
                    task.stopped_reason = reason.clone();
                    task.claimed_by_session = None;
                    task.turn_stopped = turn_id.clone();
                }
            }
        }
        TaskEvent::Closed {
            task_ids,
            outcome,
            summary,
            turn_id,
            ..
        } => {
            for task_id in task_ids {
                if let Some(task) = tasks.get_mut(task_id) {
                    task.status = TaskStatus::Closed;
                    task.closed_outcome = Some(*outcome);
                    task.summary = summary.clone();
                    task.claimed_by_session = None;
                    task.turn_closed = turn_id.clone();
                }
            }
        }
        TaskEvent::Reopened { task_id, .. } => {
            if let Some(task) = tasks.get_mut(task_id) {
                task.status = TaskStatus::Open;
                task.closed_outcome = None;
                task.claimed_by_session = None;
            }
        }
        TaskEvent::CommentAdded {
            task_ids,
            text,
            data,
            timestamp,
        } => {
            for task_id in task_ids {
                if let Some(task) = tasks.get_mut(task_id) {
                    task.comments.push(TaskComment {
                        id: None,
                        text: text.clone(),
                        timestamp: *timestamp,
                        data: data.clone(),
                    });
                }
            }
        }
        TaskEvent::Updated {
            task_id,
            name,
            priority,
            assignee,
            data,
            instructions,
            ..
        } => {
            if let Some(task) = tasks.get_mut(task_id) {
                if let Some(new_name) = name {
                    task.name = new_name.clone();
                }
                if let Some(new_priority) = priority {
                    task.priority = *new_priority;
                }
                if let Some(new_assignee) = assignee {
                    task.assignee = Some(new_assignee.clone());
                }
                if let Some(new_data) = data {
                    for (key, value) in new_data {
                        if value.is_empty() {
                            task.data.remove(key);
                        } else {
                            task.data.insert(key.clone(), value.clone());
                        }
                    }
                }
                if let Some(new_instructions) = instructions {
                    task.instructions = Some(new_instructions.clone());
                }
            }
        }
        TaskEvent::FieldsCleared {
            task_id, fields, ..
        } => {
            if let Some(task) = tasks.get_mut(task_id) {
                for field in fields {
                    if field == "assignee" {
                        task.assignee = None;
                    } else if field == "instructions" {
                        task.instructions = None;
                    } else if let Some(key) = field.strip_prefix("data.") {
                        task.data.remove(key);
                    }
                }
            }
        }
        TaskEvent::LinkAdded { from, to, kind, autorun, .. } => {
            // Map renamed link kinds for backward compatibility
            let effective_kind = match kind.as_str() {
                "implements" => "implements-plan",
                _ => kind.as_str(),
            };
            edges.add(from, to, effective_kind);
            // Store link metadata if autorun is explicitly set
            if let Some(ar) = autorun {
                if *ar {
                    edges.set_meta(from, to, effective_kind, LinkMeta { autorun: true });
                }
            }
            // When a subtask-of link is added, index the child's slug under the parent
            if kind == "subtask-of" {
                if let Some(task) = tasks.get(from) {
                    if let Some(s) = &task.slug {
                        slug_index.insert((to.clone(), s.clone()), from.clone());
                    }
                }
            }
        }
        TaskEvent::LinkRemoved { from, to, kind, .. } => {
            // Map renamed link kinds for backward compatibility
            let effective_kind = match kind.as_str() {
                "implements" => "implements-plan",
                _ => kind.as_str(),
            };
            edges.remove(from, to, effective_kind);
            // When a subtask-of link is removed, clean up the slug index
            // to prevent stale slug mappings after reparenting
            if kind == "subtask-of" {
                if let Some(task) = tasks.get(from) {
                    if let Some(s) = &task.slug {
                        slug_index.remove(&(to.clone(), s.clone()));
                    }
                }
            }
        }
    }
}

/// Validate that a slug is unique within the parent's children.
/// Returns an error if a sibling already has this slug.
pub fn validate_slug_unique(
    graph: &TaskGraph,
    parent_id: &str,
    slug: &str,
) -> crate::error::Result<()> {
    if let Some(existing) = graph.find_by_slug(parent_id, slug) {
        return Err(crate::error::AikiError::DuplicateSlug {
            slug: slug.to_string(),
            parent_id: parent_id.to_string(),
            existing_task: existing.name.clone(),
        });
    }
    Ok(())
}

/// Check if a link kind requires task-only targets.
pub fn is_task_only_kind(kind: &str) -> bool {
    LINK_KINDS
        .iter()
        .find(|k| k.name == kind)
        .map_or(false, |k| k.task_only)
}

/// Look up a link kind by name.
#[allow(dead_code)]
pub fn find_link_kind(name: &str) -> Option<&'static LinkKind> {
    LINK_KINDS.iter().find(|k| k.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::types::TaskPriority;
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
            working_copy: None,
            instructions: None,
            data: HashMap::new(),
            timestamp: Utc::now(),
        }
    }

    fn make_closed(id: &str) -> TaskEvent {
        TaskEvent::Closed {
            task_ids: vec![id.to_string()],
            outcome: crate::tasks::types::TaskOutcome::Done,
            summary: None,
            turn_id: None,
            timestamp: Utc::now(),
        }
    }

    fn make_closed_wont_do(id: &str) -> TaskEvent {
        TaskEvent::Closed {
            task_ids: vec![id.to_string()],
            outcome: crate::tasks::types::TaskOutcome::WontDo,
            summary: None,
            turn_id: None,
            timestamp: Utc::now(),
        }
    }

    fn make_stopped(id: &str) -> TaskEvent {
        TaskEvent::Stopped {
            task_ids: vec![id.to_string()],
            reason: Some("test stop".to_string()),
            turn_id: None,
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

    fn make_unlink(from: &str, to: &str, kind: &str) -> TaskEvent {
        TaskEvent::LinkRemoved {
            from: from.to_string(),
            to: to.to_string(),
            kind: kind.to_string(),
            reason: None,
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_edge_store_add_and_lookup() {
        let mut store = EdgeStore::new();
        store.add("A", "B", "blocked-by");
        store.add("A", "C", "blocked-by");

        assert_eq!(store.targets("A", "blocked-by"), &["B", "C"]);
        assert_eq!(store.referrers("B", "blocked-by"), &["A"]);
        assert_eq!(store.referrers("C", "blocked-by"), &["A"]);
        assert!(store.targets("B", "blocked-by").is_empty());
    }

    #[test]
    fn test_edge_store_remove() {
        let mut store = EdgeStore::new();
        store.add("A", "B", "blocked-by");
        store.add("A", "C", "blocked-by");
        store.remove("A", "B", "blocked-by");

        assert_eq!(store.targets("A", "blocked-by"), &["C"]);
        assert!(store.referrers("B", "blocked-by").is_empty());
    }

    #[test]
    fn test_edge_store_has_link() {
        let mut store = EdgeStore::new();
        store.add("A", "B", "blocked-by");

        assert!(store.has_link("A", "B", "blocked-by"));
        assert!(!store.has_link("B", "A", "blocked-by"));
        assert!(!store.has_link("A", "B", "sourced-from"));
    }

    #[test]
    fn test_edge_store_target_single() {
        let mut store = EdgeStore::new();
        store.add("child", "parent", "subtask-of");

        assert_eq!(store.target("child", "subtask-of"), Some("parent"));
        assert_eq!(store.target("other", "subtask-of"), None);
    }

    #[test]
    fn test_materialize_graph_basic() {
        let events = vec![
            make_created("A", "Task A"),
            make_created("B", "Task B"),
            make_link("B", "A", "blocked-by"),
        ];

        let graph = materialize_graph(&events);
        assert_eq!(graph.tasks.len(), 2);
        assert_eq!(graph.edges.targets("B", "blocked-by"), &["A"]);
        assert_eq!(graph.edges.referrers("A", "blocked-by"), &["B"]);
    }

    #[test]
    fn test_is_blocked_open_blocker() {
        let events = vec![
            make_created("A", "Blocker"),
            make_created("B", "Blocked task"),
            make_link("B", "A", "blocked-by"),
        ];

        let graph = materialize_graph(&events);
        assert!(graph.is_blocked("B"));
        assert!(!graph.is_blocked("A"));
    }

    #[test]
    fn test_is_blocked_closed_blocker() {
        let events = vec![
            make_created("A", "Blocker"),
            make_created("B", "Blocked task"),
            make_link("B", "A", "blocked-by"),
            make_closed("A"),
        ];

        let graph = materialize_graph(&events);
        assert!(!graph.is_blocked("B"));
    }

    #[test]
    fn test_is_blocked_semantic_validates() {
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Review"),
            make_link("B", "A", "validates"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            graph.is_blocked("B"),
            "Review should be blocked until implementation closes"
        );
        assert!(
            !graph.is_blocked("A"),
            "Implementation should not be blocked"
        );
    }

    #[test]
    fn test_is_blocked_semantic_remediates() {
        let events = vec![
            make_created("A", "Review"),
            make_created("B", "Fix"),
            make_link("B", "A", "remediates"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            graph.is_blocked("B"),
            "Fix should be blocked until review closes"
        );
    }

    #[test]
    fn test_is_blocked_semantic_depends_on() {
        let events = vec![
            make_created("A", "Design"),
            make_created("B", "Implementation"),
            make_link("B", "A", "depends-on"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            graph.is_blocked("B"),
            "Implementation should be blocked until design closes"
        );
    }

    #[test]
    fn test_is_blocked_semantic_unblocks_when_closed() {
        let events = vec![
            make_created("A", "Design"),
            make_created("B", "Implementation"),
            make_link("B", "A", "depends-on"),
            make_closed("A"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            !graph.is_blocked("B"),
            "Implementation should unblock when design closes"
        );
    }

    // --- Differential unblocking: depends-on vs validates/remediates ---

    #[test]
    fn test_depends_on_stays_blocked_when_blocker_stopped() {
        let events = vec![
            make_created("A", "Prerequisite"),
            make_created("B", "Dependent"),
            make_link("B", "A", "depends-on"),
            make_stopped("A"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            graph.is_blocked("B"),
            "depends-on should stay blocked when prerequisite is stopped"
        );
    }

    #[test]
    fn test_depends_on_stays_blocked_when_blocker_wont_do() {
        let events = vec![
            make_created("A", "Prerequisite"),
            make_created("B", "Dependent"),
            make_link("B", "A", "depends-on"),
            make_closed_wont_do("A"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            graph.is_blocked("B"),
            "depends-on should stay blocked when prerequisite is closed as won't-do"
        );
    }

    #[test]
    fn test_validates_unblocks_when_blocker_stopped() {
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Review"),
            make_link("B", "A", "validates"),
            make_stopped("A"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            !graph.is_blocked("B"),
            "validates should unblock when target is stopped"
        );
    }

    #[test]
    fn test_validates_unblocks_when_blocker_wont_do() {
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Review"),
            make_link("B", "A", "validates"),
            make_closed_wont_do("A"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            !graph.is_blocked("B"),
            "validates should unblock when target is closed as won't-do"
        );
    }

    #[test]
    fn test_remediates_unblocks_when_blocker_stopped() {
        let events = vec![
            make_created("A", "Review"),
            make_created("B", "Fix"),
            make_link("B", "A", "remediates"),
            make_stopped("A"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            !graph.is_blocked("B"),
            "remediates should unblock when target is stopped"
        );
    }

    #[test]
    fn test_remediates_unblocks_when_blocker_wont_do() {
        let events = vec![
            make_created("A", "Review"),
            make_created("B", "Fix"),
            make_link("B", "A", "remediates"),
            make_closed_wont_do("A"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            !graph.is_blocked("B"),
            "remediates should unblock when target is closed as won't-do"
        );
    }

    #[test]
    fn test_mixed_links_validates_stopped_depends_on_closed() {
        // validates link: A is stopped → unblocked
        // depends-on link: B is closed (done) → unblocked
        // Result: C should be READY
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Design"),
            make_created("C", "Task C"),
            make_link("C", "A", "validates"),
            make_link("C", "B", "depends-on"),
            make_stopped("A"),
            make_closed("B"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            !graph.is_blocked("C"),
            "Task C should be ready: validates unblocked (A stopped), depends-on unblocked (B done)"
        );
    }

    #[test]
    fn test_mixed_links_validates_stopped_depends_on_stopped() {
        // validates link: A is stopped → unblocked
        // depends-on link: B is stopped → BLOCKED
        // Result: C should be BLOCKED
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Design"),
            make_created("C", "Task C"),
            make_link("C", "A", "validates"),
            make_link("C", "B", "depends-on"),
            make_stopped("A"),
            make_stopped("B"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            graph.is_blocked("C"),
            "Task C should be blocked: depends-on B is stopped (not closed as done)"
        );
    }

    #[test]
    fn test_is_blocked_multiple_semantic_types() {
        // Task blocked by multiple different semantic link types
        let events = vec![
            make_created("A", "Design"),
            make_created("B", "Review"),
            make_created("C", "Implementation"),
            make_link("C", "A", "depends-on"),
            make_link("C", "B", "validates"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            graph.is_blocked("C"),
            "Task should be blocked by multiple semantic links"
        );

        // Close one blocker - still blocked
        let events = vec![
            make_created("A", "Design"),
            make_created("B", "Review"),
            make_created("C", "Implementation"),
            make_link("C", "A", "depends-on"),
            make_link("C", "B", "validates"),
            make_closed("A"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            graph.is_blocked("C"),
            "Task should remain blocked while one blocker is open"
        );

        // Close both blockers - unblocked
        let events = vec![
            make_created("A", "Design"),
            make_created("B", "Review"),
            make_created("C", "Implementation"),
            make_link("C", "A", "depends-on"),
            make_link("C", "B", "validates"),
            make_closed("A"),
            make_closed("B"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            !graph.is_blocked("C"),
            "Task should be unblocked when all blockers close"
        );
    }

    #[test]
    fn test_unlink_removes_blocking() {
        let events = vec![
            make_created("A", "Blocker"),
            make_created("B", "Blocked task"),
            make_link("B", "A", "blocked-by"),
            make_unlink("B", "A", "blocked-by"),
        ];

        let graph = materialize_graph(&events);
        assert!(!graph.is_blocked("B"));
        assert!(graph.edges.targets("B", "blocked-by").is_empty());
    }

    #[test]
    fn test_would_create_cycle() {
        let events = vec![
            make_created("A", "Task A"),
            make_created("B", "Task B"),
            make_created("C", "Task C"),
            make_link("B", "A", "blocked-by"),
            make_link("C", "B", "blocked-by"),
        ];

        let graph = materialize_graph(&events);
        // A→B→C chain exists. Adding C→A would create cycle
        assert!(graph.would_create_cycle("A", "C", "blocked-by"));
        // Adding A→C would not (direction matters)
        assert!(!graph.would_create_cycle("C", "A", "blocked-by"));
    }

    #[test]
    fn test_ancestor_chain() {
        let events = vec![
            make_created("root", "Root"),
            make_created("child", "Child"),
            make_created("grandchild", "Grandchild"),
            make_link("child", "root", "subtask-of"),
            make_link("grandchild", "child", "subtask-of"),
        ];

        let graph = materialize_graph(&events);
        let ancestors = graph.ancestor_chain("grandchild");
        assert_eq!(ancestors, vec!["child", "root"]);
    }

    #[test]
    fn test_children_of() {
        let events = vec![
            make_created("parent", "Parent"),
            make_created("child1", "Child 1"),
            make_created("child2", "Child 2"),
            make_link("child1", "parent", "subtask-of"),
            make_link("child2", "parent", "subtask-of"),
        ];

        let graph = materialize_graph(&events);
        let children = graph.children_of("parent");
        assert_eq!(children.len(), 2);
    }

    #[test]
    fn test_provenance_chain() {
        let events = vec![
            make_created("A", "Task A"),
            make_created("B", "Task B"),
            make_created("C", "Task C"),
            make_link("B", "A", "sourced-from"),
            make_link("C", "B", "sourced-from"),
            make_link("B", "file:design.md", "sourced-from"),
        ];

        let graph = materialize_graph(&events);
        let chain = graph.provenance_chain("C");
        assert!(chain.contains(&"B".to_string()));
        assert!(chain.contains(&"A".to_string()));
        assert!(chain.contains(&"file:design.md".to_string()));
    }

    #[test]
    fn test_spawned_from() {
        let events = vec![
            make_created("A", "Task A"),
            make_created("B", "From A"),
            make_created("C", "Also from A"),
            make_link("B", "A", "sourced-from"),
            make_link("C", "A", "sourced-from"),
        ];

        let graph = materialize_graph(&events);
        let spawned = graph.spawned_from("A");
        assert_eq!(spawned.len(), 2);
    }

    #[test]
    fn test_is_task_only_kind() {
        // Task-only blocking links (legacy + semantic)
        assert!(is_task_only_kind("blocked-by"));
        assert!(is_task_only_kind("validates"));
        assert!(is_task_only_kind("remediates"));
        assert!(is_task_only_kind("depends-on"));

        // Other task-only links
        assert!(is_task_only_kind("subtask-of"));
        assert!(is_task_only_kind("orchestrates"));
        assert!(is_task_only_kind("supersedes"));

        // Non-task-only links
        assert!(!is_task_only_kind("sourced-from"));
        assert!(!is_task_only_kind("implements-plan"));
        assert!(!is_task_only_kind("decomposes-plan"));
        assert!(!is_task_only_kind("adds-plan"));
        assert!(!is_task_only_kind("fixes"));
        assert!(!is_task_only_kind("unknown-kind"));
    }

    #[test]
    fn test_link_kinds_registry() {
        // Verify all 13 kinds are registered
        assert_eq!(LINK_KINDS.len(), 13);

        let blocked = find_link_kind("blocked-by").unwrap();
        assert!(blocked.blocks_ready);
        assert!(blocked.task_only);
        assert!(blocked.max_forward.is_none());

        // Semantic blocking link types
        let validates = find_link_kind("validates").unwrap();
        assert!(validates.blocks_ready);
        assert!(validates.task_only);

        let remediates = find_link_kind("remediates").unwrap();
        assert!(remediates.blocks_ready);
        assert!(remediates.task_only);

        let depends_on = find_link_kind("depends-on").unwrap();
        assert!(depends_on.blocks_ready);
        assert!(depends_on.task_only);

        let subtask = find_link_kind("subtask-of").unwrap();
        assert_eq!(subtask.max_forward, Some(1));
        assert!(subtask.max_reverse.is_none());

        let orchestrates = find_link_kind("orchestrates").unwrap();
        assert_eq!(orchestrates.max_forward, Some(1));
        assert_eq!(orchestrates.max_reverse, Some(1));

        let implements_plan = find_link_kind("implements-plan").unwrap();
        assert_eq!(implements_plan.max_forward, Some(1));
        assert_eq!(implements_plan.max_reverse, Some(1));
        assert!(!implements_plan.blocks_ready);
        assert!(!implements_plan.task_only);

        let decomposes_plan = find_link_kind("decomposes-plan").unwrap();
        assert!(decomposes_plan.max_forward.is_none());
        assert!(!decomposes_plan.blocks_ready);
        assert!(!decomposes_plan.task_only);

        let adds_plan = find_link_kind("adds-plan").unwrap();
        assert!(adds_plan.max_forward.is_none());
        assert!(!adds_plan.blocks_ready);
        assert!(!adds_plan.task_only);

        let fixes = find_link_kind("fixes").unwrap();
        assert!(fixes.max_forward.is_none());
        assert!(!fixes.blocks_ready);
        assert!(!fixes.task_only);

        // scoped-to should no longer exist
        assert!(find_link_kind("scoped-to").is_none());

        let spawned_by = find_link_kind("spawned-by").unwrap();
        assert_eq!(spawned_by.max_forward, Some(1));
        assert!(spawned_by.max_reverse.is_none());
        assert!(!spawned_by.blocks_ready);
        assert!(spawned_by.task_only);
    }

    #[test]
    fn test_backward_compat_sources_indexed_as_sourced_from() {
        // Old-style tasks have sources in the Created event but no LinkAdded events.
        // materialize_graph should index them as sourced-from edges.
        let events = vec![TaskEvent::Created {
            task_id: "task1".to_string(),
            name: "Task with sources".to_string(),
            slug: None,
            task_type: None,
            priority: TaskPriority::P2,
            assignee: None,
            sources: vec!["file:design.md".to_string(), "task:task0".to_string()],
            template: None,
            working_copy: None,
            instructions: None,
            data: HashMap::new(),
            timestamp: Utc::now(),
        }];

        let graph = materialize_graph(&events);
        assert_eq!(
            graph.edges.targets("task1", "sourced-from"),
            &["file:design.md", "task:task0"]
        );
        assert_eq!(
            graph.edges.referrers("file:design.md", "sourced-from"),
            &["task1"]
        );
    }

    #[test]
    fn test_backward_compat_no_duplicate_when_link_also_exists() {
        // New-style tasks emit both sources in Created AND LinkAdded events.
        // materialize_graph should not double-count them.
        let events = vec![
            TaskEvent::Created {
                task_id: "task1".to_string(),
                name: "New task".to_string(),
                slug: None,
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: vec!["file:design.md".to_string()],
                template: None,
                working_copy: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: Utc::now(),
            },
            // Explicit LinkAdded emitted alongside the Created event
            TaskEvent::LinkAdded {
                from: "task1".to_string(),
                to: "file:design.md".to_string(),
                kind: "sourced-from".to_string(),
                autorun: None,
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        // Should have exactly one edge, not two
        let targets = graph.edges.targets("task1", "sourced-from");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0], "file:design.md");
    }

    #[test]
    fn test_data_plan_synthesizes_implements_plan_for_epics() {
        // A task with data.plan but NO data.epic is an epic — should get synthesis.
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/feature.md".to_string());

        let events = vec![TaskEvent::Created {
            task_id: "epic1".to_string(),
            name: "Epic: Feature".to_string(),
            slug: None,
            task_type: None,
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data,
            timestamp: Utc::now(),
        }];

        let graph = materialize_graph(&events);
        assert_eq!(
            graph.edges.target("epic1", "implements-plan"),
            Some("file:ops/now/feature.md"),
            "data.plan without data.epic should synthesize implements-plan"
        );
    }

    #[test]
    fn test_data_plan_with_epic_does_not_synthesize_implements_plan() {
        // A task with BOTH data.plan and data.epic is a helper task
        // (decompose/build) — should NOT get implements-plan synthesis.
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "ops/now/feature.md".to_string());
        data.insert("epic".to_string(), "some_epic_id".to_string());

        let events = vec![TaskEvent::Created {
            task_id: "decompose1".to_string(),
            name: "Decompose: ops/now/feature.md".to_string(),
            slug: None,
            task_type: Some("decompose".to_string()),
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data,
            timestamp: Utc::now(),
        }];

        let graph = materialize_graph(&events);
        assert_eq!(
            graph.edges.target("decompose1", "implements-plan"),
            None,
            "data.plan with data.epic should not synthesize implements-plan"
        );
    }

    #[test]
    fn test_data_epic_as_orchestrates() {
        let mut data = HashMap::new();
        data.insert("plan".to_string(), "feature.md".to_string());
        data.insert("epic".to_string(), "epic_task_id".to_string());

        let events = vec![TaskEvent::Created {
            task_id: "build1".to_string(),
            name: "Build task".to_string(),
            slug: None,
            task_type: Some("orchestrator".to_string()),
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data,
            timestamp: Utc::now(),
        }];

        let graph = materialize_graph(&events);
        assert_eq!(
            graph.edges.target("build1", "orchestrates"),
            Some("epic_task_id")
        );
    }

    #[test]
    fn test_data_scope_as_scoped_to() {
        let mut data = HashMap::new();
        data.insert("scope.kind".to_string(), "plan".to_string());
        data.insert("scope.id".to_string(), "ops/now/auth.md".to_string());
        data.insert("scope.name".to_string(), "Auth plan".to_string());

        let events = vec![TaskEvent::Created {
            task_id: "review1".to_string(),
            name: "Review auth".to_string(),
            slug: None,
            task_type: Some("review".to_string()),
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data,
            timestamp: Utc::now(),
        }];

        let graph = materialize_graph(&events);
        assert_eq!(
            graph.edges.targets("review1", "scoped-to"),
            &["file:ops/now/auth.md"]
        );
    }

    #[test]
    fn test_backward_compat_scope_task_ids() {
        let mut data = HashMap::new();
        data.insert("scope.kind".to_string(), "task".to_string());
        data.insert("scope.id".to_string(), "taskid1".to_string());
        data.insert(
            "scope.task_ids".to_string(),
            "taskid1,taskid2,taskid3".to_string(),
        );

        let events = vec![TaskEvent::Created {
            task_id: "review2".to_string(),
            name: "Review tasks".to_string(),
            slug: None,
            task_type: Some("review".to_string()),
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data,
            timestamp: Utc::now(),
        }];

        let graph = materialize_graph(&events);
        let targets = graph.edges.targets("review2", "scoped-to");
        // scope.id produces one link, scope.task_ids produces 3
        // But taskid1 appears in both, so EdgeStore dedup makes it 3 total
        assert!(targets.contains(&"taskid1".to_string()));
        assert!(targets.contains(&"taskid2".to_string()));
        assert!(targets.contains(&"taskid3".to_string()));
    }

    #[test]
    fn test_backward_compat_dot_notation_as_subtask_of() {
        let events = vec![
            make_created("parent", "Parent task"),
            TaskEvent::Created {
                task_id: "parent.1".to_string(),
                name: "First subtask".to_string(),
                slug: None,
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: Utc::now(),
            },
            TaskEvent::Created {
                task_id: "parent.2".to_string(),
                name: "Second subtask".to_string(),
                slug: None,
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        // Children should have subtask-of links to parent
        assert_eq!(graph.edges.target("parent.1", "subtask-of"), Some("parent"));
        assert_eq!(graph.edges.target("parent.2", "subtask-of"), Some("parent"));
        // Parent should see its children via reverse lookup
        let children = graph.children_of("parent");
        assert_eq!(children.len(), 2);
    }

    /// Regression: source filtering depends on `graph.edges.targets(id, "sourced-from")`.
    /// After a LinkRemoved event for a sourced-from edge, the edge must be gone
    /// so that `matches_source` (in task list) no longer matches the removed source.
    #[test]
    fn test_link_removed_sourced_from_excludes_from_filter() {
        let events = vec![
            make_created("task1", "Task with source"),
            make_link("task1", "file:design.md", "sourced-from"),
            make_link("task1", "task:origin", "sourced-from"),
            // Remove the file source link
            make_unlink("task1", "file:design.md", "sourced-from"),
        ];

        let graph = materialize_graph(&events);

        // The removed source must no longer appear in forward lookup
        let sources = graph.edges.targets("task1", "sourced-from");
        assert!(
            !sources.contains(&"file:design.md".to_string()),
            "LinkRemoved should remove the sourced-from edge"
        );
        // The remaining source should still be present
        assert!(sources.contains(&"task:origin".to_string()));

        // Reverse lookup should also reflect the removal
        assert!(
            graph
                .edges
                .referrers("file:design.md", "sourced-from")
                .is_empty(),
            "Reverse lookup should be empty after LinkRemoved"
        );
        assert_eq!(
            graph.edges.referrers("task:origin", "sourced-from"),
            &["task1"]
        );
    }

    /// Regression: old-style sources (from Created event) indexed as sourced-from
    /// should also be removable via LinkRemoved, and source filtering should
    /// stop matching after removal.
    #[test]
    fn test_backward_compat_source_removable_via_link_removed() {
        let events = vec![
            // Old-style: sources in Created event
            TaskEvent::Created {
                task_id: "task1".to_string(),
                name: "Old-style task".to_string(),
                slug: None,
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: vec!["file:plan.md".to_string()],
                template: None,
                working_copy: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: Utc::now(),
            },
            // Later the source link is explicitly removed
            make_unlink("task1", "file:plan.md", "sourced-from"),
        ];

        let graph = materialize_graph(&events);

        // The edge should be removed despite being indexed from Created event
        assert!(
            graph.edges.targets("task1", "sourced-from").is_empty(),
            "LinkRemoved should remove old-style sourced-from edges too"
        );
        assert!(graph
            .edges
            .referrers("file:plan.md", "sourced-from")
            .is_empty());
    }

    /// Regression: run_stop/run_close compute a ready-queue footer using
    /// `graph.is_blocked()`. After closing a blocker in-memory, the graph's
    /// tasks map must be updated so `is_blocked()` sees the new Closed status.
    /// Without `graph.tasks = tasks.clone()`, the stale graph still shows the
    /// blocker as Open, hiding newly-unblocked tasks from the footer.
    #[test]
    fn test_is_blocked_reflects_updated_tasks_map() {
        // Set up: B is blocked by A (both open)
        let events = vec![
            make_created("A", "Blocker"),
            make_created("B", "Blocked task"),
            make_link("B", "A", "blocked-by"),
        ];

        let mut graph = materialize_graph(&events);

        // Before mutation: B is blocked
        assert!(graph.is_blocked("B"), "B should be blocked by open A");

        // Simulate what run_close does: close A in the tasks map, then
        // update graph.tasks with the mutated map
        let a = graph.tasks.get_mut("A").unwrap();
        a.status = TaskStatus::Closed;
        a.closed_outcome = Some(TaskOutcome::Done);

        // After updating graph.tasks: B should no longer be blocked
        assert!(
            !graph.is_blocked("B"),
            "B should be unblocked after A is closed in graph.tasks"
        );
    }

    /// Regression: same pattern with multiple blockers — closing one should
    /// not unblock a task that has other open blockers.
    #[test]
    fn test_is_blocked_partial_close_still_blocked() {
        let events = vec![
            make_created("A", "Blocker 1"),
            make_created("B", "Blocker 2"),
            make_created("C", "Blocked by both"),
            make_link("C", "A", "blocked-by"),
            make_link("C", "B", "blocked-by"),
        ];

        let mut graph = materialize_graph(&events);
        assert!(graph.is_blocked("C"));

        // Close only A
        let a = graph.tasks.get_mut("A").unwrap();
        a.status = TaskStatus::Closed;
        a.closed_outcome = Some(TaskOutcome::Done);

        // C should still be blocked (B is still open)
        assert!(
            graph.is_blocked("C"),
            "C should remain blocked while B is still open"
        );

        // Close B too
        let b = graph.tasks.get_mut("B").unwrap();
        b.status = TaskStatus::Closed;
        b.closed_outcome = Some(TaskOutcome::Done);

        // Now C should be unblocked
        assert!(
            !graph.is_blocked("C"),
            "C should be unblocked after both blockers are closed"
        );
    }

    #[test]
    fn test_materialize_turn_started() {
        let events = vec![
            make_created("t1", "Task 1"),
            TaskEvent::Started {
                task_ids: vec!["t1".to_string()],
                agent_type: "claude-code".to_string(),
                session_id: None,
                turn_id: Some("turn-aaa-1".to_string()),
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        let task = graph.tasks.get("t1").unwrap();
        assert_eq!(task.turn_started, Some("turn-aaa-1".to_string()));
        assert_eq!(task.turn_stopped, None);
        assert_eq!(task.turn_closed, None);
    }

    #[test]
    fn test_materialize_turn_stopped() {
        let events = vec![
            make_created("t1", "Task 1"),
            TaskEvent::Started {
                task_ids: vec!["t1".to_string()],
                agent_type: "claude-code".to_string(),
                session_id: None,
                turn_id: Some("turn-aaa-1".to_string()),
                timestamp: Utc::now(),
            },
            TaskEvent::Stopped {
                task_ids: vec!["t1".to_string()],
                reason: None,
                turn_id: Some("turn-aaa-2".to_string()),
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        let task = graph.tasks.get("t1").unwrap();
        assert_eq!(task.turn_started, Some("turn-aaa-1".to_string()));
        assert_eq!(task.turn_stopped, Some("turn-aaa-2".to_string()));
    }

    #[test]
    fn test_materialize_turn_closed() {
        let events = vec![
            make_created("t1", "Task 1"),
            TaskEvent::Started {
                task_ids: vec!["t1".to_string()],
                agent_type: "claude-code".to_string(),
                session_id: None,
                turn_id: Some("turn-aaa-1".to_string()),
                timestamp: Utc::now(),
            },
            TaskEvent::Closed {
                task_ids: vec!["t1".to_string()],
                outcome: crate::tasks::types::TaskOutcome::Done,
                summary: None,
                turn_id: Some("turn-aaa-3".to_string()),
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        let task = graph.tasks.get("t1").unwrap();
        assert_eq!(task.turn_started, Some("turn-aaa-1".to_string()));
        assert_eq!(task.turn_closed, Some("turn-aaa-3".to_string()));
    }

    #[test]
    fn test_materialize_restart_clears_turn_stopped() {
        let events = vec![
            make_created("t1", "Task 1"),
            TaskEvent::Started {
                task_ids: vec!["t1".to_string()],
                agent_type: "claude-code".to_string(),
                session_id: None,
                turn_id: Some("turn-aaa-1".to_string()),
                timestamp: Utc::now(),
            },
            TaskEvent::Stopped {
                task_ids: vec!["t1".to_string()],
                reason: None,
                turn_id: Some("turn-aaa-2".to_string()),
                timestamp: Utc::now(),
            },
            TaskEvent::Started {
                task_ids: vec!["t1".to_string()],
                agent_type: "claude-code".to_string(),
                session_id: None,
                turn_id: Some("turn-bbb-1".to_string()),
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        let task = graph.tasks.get("t1").unwrap();
        // After restart, turn_started should be updated and turn_stopped cleared
        assert_eq!(task.turn_started, Some("turn-bbb-1".to_string()));
        assert_eq!(task.turn_stopped, None);
    }

    #[test]
    fn test_materialize_turn_id_none() {
        let events = vec![
            make_created("t1", "Task 1"),
            TaskEvent::Started {
                task_ids: vec!["t1".to_string()],
                agent_type: "claude-code".to_string(),
                session_id: None,
                turn_id: None,
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        let task = graph.tasks.get("t1").unwrap();
        assert_eq!(task.turn_started, None);
    }

    #[test]
    fn test_slug_index_dot_notation() {
        // Dot-notation subtask with slug should be indexed
        let events = vec![
            make_created("parent", "Parent"),
            TaskEvent::Created {
                task_id: "parent.1".to_string(),
                name: "Build step".to_string(),
                slug: Some("build".to_string()),
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        let found = graph.find_by_slug("parent", "build");
        assert!(found.is_some(), "Should find subtask by slug");
        assert_eq!(found.unwrap().id, "parent.1");
    }

    #[test]
    fn test_slug_index_link_added() {
        // Subtask linked via LinkAdded (not dot-notation) should also be indexed
        let events = vec![
            make_created("parent", "Parent"),
            TaskEvent::Created {
                task_id: "child-id".to_string(),
                name: "Test step".to_string(),
                slug: Some("test".to_string()),
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: Utc::now(),
            },
            make_link("child-id", "parent", "subtask-of"),
        ];

        let graph = materialize_graph(&events);
        let found = graph.find_by_slug("parent", "test");
        assert!(found.is_some(), "Should find subtask by slug via LinkAdded");
        assert_eq!(found.unwrap().id, "child-id");
    }

    #[test]
    fn test_slug_index_no_slug() {
        // Subtask without slug should not appear in slug_index
        let events = vec![
            make_created("parent", "Parent"),
            make_created("parent.1", "No slug subtask"),
        ];

        let graph = materialize_graph(&events);
        assert!(graph.slug_index.is_empty(), "No slugs, no index entries");
        assert!(graph.find_by_slug("parent", "anything").is_none());
    }

    #[test]
    fn test_slug_index_multiple_slugs_same_parent() {
        let events = vec![
            make_created("parent", "Parent"),
            TaskEvent::Created {
                task_id: "parent.1".to_string(),
                name: "Build".to_string(),
                slug: Some("build".to_string()),
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: Utc::now(),
            },
            TaskEvent::Created {
                task_id: "parent.2".to_string(),
                name: "Test".to_string(),
                slug: Some("test".to_string()),
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        assert_eq!(graph.slug_index.len(), 2);
        assert_eq!(graph.find_by_slug("parent", "build").unwrap().id, "parent.1");
        assert_eq!(graph.find_by_slug("parent", "test").unwrap().id, "parent.2");
    }

    #[test]
    fn test_slug_index_different_parents() {
        // Same slug under different parents should work independently
        let events = vec![
            make_created("p1", "Parent 1"),
            make_created("p2", "Parent 2"),
            TaskEvent::Created {
                task_id: "p1.1".to_string(),
                name: "Build for P1".to_string(),
                slug: Some("build".to_string()),
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: Utc::now(),
            },
            TaskEvent::Created {
                task_id: "p2.1".to_string(),
                name: "Build for P2".to_string(),
                slug: Some("build".to_string()),
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        assert_eq!(graph.find_by_slug("p1", "build").unwrap().id, "p1.1");
        assert_eq!(graph.find_by_slug("p2", "build").unwrap().id, "p2.1");
    }

    #[test]
    fn test_validate_slug_unique_duplicate() {
        let events = vec![
            make_created("parent", "Parent"),
            TaskEvent::Created {
                task_id: "parent.1".to_string(),
                name: "Build step".to_string(),
                slug: Some("build".to_string()),
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        // Same slug under same parent should fail
        let err = validate_slug_unique(&graph, "parent", "build").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("build"), "Error should mention the slug: {msg}");
        assert!(msg.contains("parent"), "Error should mention the parent: {msg}");
    }

    #[test]
    fn test_validate_slug_unique_different_parents() {
        let events = vec![
            make_created("p1", "Parent 1"),
            make_created("p2", "Parent 2"),
            TaskEvent::Created {
                task_id: "p1.1".to_string(),
                name: "Build for P1".to_string(),
                slug: Some("build".to_string()),
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        // Same slug under different parent should succeed
        assert!(validate_slug_unique(&graph, "p2", "build").is_ok());
    }

    #[test]
    fn test_validate_slug_unique_no_conflict() {
        let events = vec![
            make_created("parent", "Parent"),
            TaskEvent::Created {
                task_id: "parent.1".to_string(),
                name: "Build step".to_string(),
                slug: Some("build".to_string()),
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        // Different slug under same parent should succeed
        assert!(validate_slug_unique(&graph, "parent", "test").is_ok());
    }

    #[test]
    fn test_slug_index_cleaned_on_link_removed() {
        // When a subtask-of link is removed, the slug index entry should be cleaned up
        let events = vec![
            make_created("parent", "Parent"),
            TaskEvent::Created {
                task_id: "child-id".to_string(),
                name: "Build step".to_string(),
                slug: Some("build".to_string()),
                task_type: None,
                priority: TaskPriority::P2,
                assignee: None,
                sources: Vec::new(),
                template: None,
                working_copy: None,
                instructions: None,
                data: HashMap::new(),
                timestamp: Utc::now(),
            },
            make_link("child-id", "parent", "subtask-of"),
        ];

        // Before removal: slug should be indexed
        let graph = materialize_graph(&events);
        assert!(
            graph.find_by_slug("parent", "build").is_some(),
            "Slug should be indexed before link removal"
        );

        // After removal: slug should be cleaned up
        let mut events_with_removal = events.clone();
        events_with_removal.push(make_unlink("child-id", "parent", "subtask-of"));
        let graph = materialize_graph(&events_with_removal);
        assert!(
            graph.find_by_slug("parent", "build").is_none(),
            "Slug index should be cleaned up after LinkRemoved"
        );
    }

    #[test]
    fn test_backward_compat_old_implements_link_mapped_to_implements_plan() {
        // Old events have kind: "implements" — should be materialized as "implements-plan"
        let events = vec![
            make_created("epic1", "Epic: Feature"),
            make_link("epic1", "file:ops/now/feature.md", "implements"),
        ];

        let graph = materialize_graph(&events);
        // Old "implements" should be indexed under "implements-plan"
        assert_eq!(
            graph.edges.target("epic1", "implements-plan"),
            Some("file:ops/now/feature.md"),
            "Old implements link should be mapped to implements-plan"
        );
        // No edge under the old kind name
        assert!(
            graph.edges.targets("epic1", "implements").is_empty(),
            "Old implements kind should not be indexed directly"
        );
    }

    #[test]
    fn test_backward_compat_old_implements_link_removed() {
        // Old LinkRemoved with kind: "implements" should remove the implements-plan edge
        let events = vec![
            make_created("epic1", "Epic: Feature"),
            make_link("epic1", "file:ops/now/feature.md", "implements"),
            make_unlink("epic1", "file:ops/now/feature.md", "implements"),
        ];

        let graph = materialize_graph(&events);
        assert!(
            graph.edges.targets("epic1", "implements-plan").is_empty(),
            "Old implements LinkRemoved should remove the implements-plan edge"
        );
    }

    // --- LinkMeta / autorun tests ---

    #[test]
    fn test_link_meta_set_and_get() {
        let mut store = EdgeStore::new();
        store.add("A", "B", "blocked-by");
        store.set_meta("A", "B", "blocked-by", LinkMeta { autorun: true });

        let meta = store.get_meta("A", "B", "blocked-by");
        assert!(meta.is_some());
        assert!(meta.unwrap().autorun);

        // Non-existent link should return None
        assert!(store.get_meta("A", "C", "blocked-by").is_none());
        assert!(store.get_meta("A", "B", "validates").is_none());
    }

    #[test]
    fn test_link_meta_removed_with_link() {
        let mut store = EdgeStore::new();
        store.add("A", "B", "blocked-by");
        store.set_meta("A", "B", "blocked-by", LinkMeta { autorun: true });

        // Remove the link — metadata should also be removed
        store.remove("A", "B", "blocked-by");
        assert!(store.get_meta("A", "B", "blocked-by").is_none());
    }

    #[test]
    fn test_link_meta_default() {
        let meta = LinkMeta::default();
        assert!(!meta.autorun, "Default LinkMeta should have autorun=false");
    }

    #[test]
    fn test_materialize_autorun_link() {
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Review"),
            TaskEvent::LinkAdded {
                from: "B".to_string(),
                to: "A".to_string(),
                kind: "validates".to_string(),
                autorun: Some(true),
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        // Link should exist
        assert!(graph.edges.has_link("B", "A", "validates"));
        // Autorun metadata should be set
        let meta = graph.edges.get_meta("B", "A", "validates");
        assert!(meta.is_some(), "Autorun metadata should be stored");
        assert!(meta.unwrap().autorun);
    }

    #[test]
    fn test_materialize_no_autorun_link() {
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Review"),
            TaskEvent::LinkAdded {
                from: "B".to_string(),
                to: "A".to_string(),
                kind: "validates".to_string(),
                autorun: None,
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        // Link should exist
        assert!(graph.edges.has_link("B", "A", "validates"));
        // No autorun metadata
        assert!(
            graph.edges.get_meta("B", "A", "validates").is_none(),
            "No metadata should be stored when autorun is None"
        );
    }

    #[test]
    fn test_materialize_autorun_false_no_meta() {
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Review"),
            TaskEvent::LinkAdded {
                from: "B".to_string(),
                to: "A".to_string(),
                kind: "validates".to_string(),
                autorun: Some(false),
                timestamp: Utc::now(),
            },
        ];

        let graph = materialize_graph(&events);
        // Link should exist
        assert!(graph.edges.has_link("B", "A", "validates"));
        // autorun: false should not store metadata (saves space)
        assert!(
            graph.edges.get_meta("B", "A", "validates").is_none(),
            "autorun=false should not store metadata"
        );
    }

    fn make_autorun_link(from: &str, to: &str, kind: &str) -> TaskEvent {
        TaskEvent::LinkAdded {
            from: from.to_string(),
            to: to.to_string(),
            kind: kind.to_string(),
            autorun: Some(true),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn test_find_autorun_candidates_basic() {
        // B validates A with autorun. When A closes, B should be a candidate.
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Review"),
            make_autorun_link("B", "A", "validates"),
            make_closed("A"),
        ];
        let graph = materialize_graph(&events);
        let candidates = graph.find_autorun_candidates("A");
        assert_eq!(candidates, vec!["B".to_string()]);
    }

    #[test]
    fn test_find_autorun_candidates_no_autorun_flag() {
        // B validates A without autorun. B should NOT be a candidate.
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Review"),
            make_link("B", "A", "validates"),
            make_closed("A"),
        ];
        let graph = materialize_graph(&events);
        let candidates = graph.find_autorun_candidates("A");
        assert!(candidates.is_empty());
    }

    #[test]
    fn test_find_autorun_candidates_still_blocked() {
        // C validates both A and B with autorun. When only A closes, C is still blocked by B.
        let events = vec![
            make_created("A", "Impl A"),
            make_created("B", "Impl B"),
            make_created("C", "Review both"),
            make_autorun_link("C", "A", "validates"),
            make_autorun_link("C", "B", "validates"),
            make_closed("A"),
        ];
        let graph = materialize_graph(&events);
        let candidates = graph.find_autorun_candidates("A");
        assert!(candidates.is_empty(), "C still blocked by B");
    }

    #[test]
    fn test_find_autorun_candidates_all_blockers_closed() {
        // C validates both A and B with autorun. When both close, C is a candidate.
        let events = vec![
            make_created("A", "Impl A"),
            make_created("B", "Impl B"),
            make_created("C", "Review both"),
            make_autorun_link("C", "A", "validates"),
            make_autorun_link("C", "B", "validates"),
            make_closed("A"),
            make_closed("B"),
        ];
        let graph = materialize_graph(&events);
        let candidates = graph.find_autorun_candidates("B");
        assert_eq!(candidates, vec!["C".to_string()]);
    }

    #[test]
    fn test_find_autorun_candidates_skips_closed_tasks() {
        // B validates A with autorun. But B is already closed. Should not be a candidate.
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Review"),
            make_autorun_link("B", "A", "validates"),
            make_closed("B"),
            make_closed("A"),
        ];
        let graph = materialize_graph(&events);
        let candidates = graph.find_autorun_candidates("A");
        assert!(candidates.is_empty(), "Already-closed tasks should not auto-start");
    }

    #[test]
    fn test_find_autorun_candidates_skips_in_progress() {
        // B validates A with autorun. But B is already in progress. Should not be a candidate.
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Review"),
            make_autorun_link("B", "A", "validates"),
            TaskEvent::Started {
                task_ids: vec!["B".to_string()],
                agent_type: "test".to_string(),
                session_id: None,
                turn_id: None,
                timestamp: Utc::now(),
            },
            make_closed("A"),
        ];
        let graph = materialize_graph(&events);
        let candidates = graph.find_autorun_candidates("A");
        assert!(candidates.is_empty(), "In-progress tasks should not auto-start");
    }

    #[test]
    fn test_find_autorun_candidates_stopped_task_eligible() {
        // B validates A with autorun. B was stopped. Should be a candidate (restart).
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Review"),
            make_autorun_link("B", "A", "validates"),
            TaskEvent::Started {
                task_ids: vec!["B".to_string()],
                agent_type: "test".to_string(),
                session_id: None,
                turn_id: None,
                timestamp: Utc::now(),
            },
            make_stopped("B"),
            make_closed("A"),
        ];
        let graph = materialize_graph(&events);
        let candidates = graph.find_autorun_candidates("A");
        assert_eq!(candidates, vec!["B".to_string()]);
    }

    #[test]
    fn test_find_autorun_candidates_multiple_link_kinds() {
        // Test with depends-on link kind
        let events = vec![
            make_created("A", "Dependency"),
            make_created("B", "Dependent"),
            make_autorun_link("B", "A", "depends-on"),
            make_closed("A"),
        ];
        let graph = materialize_graph(&events);
        let candidates = graph.find_autorun_candidates("A");
        assert_eq!(candidates, vec!["B".to_string()]);
    }

    #[test]
    fn test_find_autorun_candidates_non_blocking_kind_ignored() {
        // A non-blocking link kind (like "related-to") should not trigger autorun
        let events = vec![
            make_created("A", "Task A"),
            make_created("B", "Task B"),
            TaskEvent::LinkAdded {
                from: "B".to_string(),
                to: "A".to_string(),
                kind: "related-to".to_string(),
                autorun: Some(true),
                timestamp: Utc::now(),
            },
            make_closed("A"),
        ];
        let graph = materialize_graph(&events);
        let candidates = graph.find_autorun_candidates("A");
        assert!(candidates.is_empty(), "Non-blocking kinds should not trigger autorun");
    }

    #[test]
    fn test_find_autorun_candidates_wont_do_still_triggers() {
        // B validates A with autorun. When A closes as wont_do, B should still auto-start.
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Review"),
            make_autorun_link("B", "A", "validates"),
            make_closed_wont_do("A"),
        ];
        let graph = materialize_graph(&events);
        let candidates = graph.find_autorun_candidates("A");
        assert_eq!(candidates, vec!["B".to_string()]);
    }

    #[test]
    fn test_find_autorun_candidates_mixed_autorun_flags() {
        // C depends on A (autorun) and B (no autorun). When both close,
        // C should auto-start because at least one link has autorun.
        let events = vec![
            make_created("A", "Task A"),
            make_created("B", "Task B"),
            make_created("C", "Dependent"),
            make_autorun_link("C", "A", "depends-on"),
            make_link("C", "B", "depends-on"),
            make_closed("A"),
            make_closed("B"),
        ];
        let graph = materialize_graph(&events);
        let candidates = graph.find_autorun_candidates("B");
        assert_eq!(candidates, vec!["C".to_string()]);
    }

    #[test]
    fn test_find_autorun_candidates_no_duplicates() {
        // B has two autorun links to A (different kinds). Should only appear once.
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Review & Fix"),
            make_autorun_link("B", "A", "validates"),
            make_autorun_link("B", "A", "remediates"),
            make_closed("A"),
        ];
        let graph = materialize_graph(&events);
        let candidates = graph.find_autorun_candidates("A");
        assert_eq!(candidates.len(), 1, "Should not have duplicate candidates");
        assert_eq!(candidates[0], "B");
    }

    #[test]
    fn test_find_autorun_candidates_link_removed() {
        // B validates A with autorun, then link is removed. No candidate.
        let events = vec![
            make_created("A", "Implementation"),
            make_created("B", "Review"),
            make_autorun_link("B", "A", "validates"),
            make_unlink("B", "A", "validates"),
            make_closed("A"),
        ];
        let graph = materialize_graph(&events);
        let candidates = graph.find_autorun_candidates("A");
        assert!(candidates.is_empty(), "Removed link should not trigger autorun");
    }
}
