//! PlanGraph — first-class plan management with O(1) reverse index
//!
//! The PlanGraph builds a reverse index from plan file paths to the tasks
//! that implement them. It unifies the duplicate plan-resolution logic
//! functions that existed in `decompose.rs` and `build.rs`.

use anyhow::{anyhow, Result};

use crate::tasks::graph::TaskGraph;
use crate::tasks::types::{FastHashMap, Task};

/// PlanGraph: indexes plans and their implementing tasks.
///
/// Built from a `TaskGraph` and optionally from filesystem plan files.
/// Provides O(1) lookups for common plan queries.
pub struct PlanGraph {
    /// Reverse index: plan_path (normalized "file:..." URI) → implementing task IDs
    plan_to_tasks: FastHashMap<String, Vec<String>>,
}

pub enum PlanEpicMatch<'a> {
    None,
    One(&'a Task),
    Many(Vec<&'a Task>),
}

impl PlanGraph {
    /// Build a PlanGraph from a TaskGraph.
    ///
    /// Scans the `implements` edges in the TaskGraph to build the reverse index.
    /// Does not scan the filesystem — call `with_filesystem_plans()` for that.
    pub fn build(task_graph: &TaskGraph) -> Self {
        let mut plan_to_tasks: FastHashMap<String, Vec<String>> = FastHashMap::default();

        // Build reverse index from implements edges.
        // The TaskGraph already materializes implements edges from both:
        // - Explicit LinkAdded events
        // - Data attribute synthesis in graph.rs
        for (task_id, _task) in &task_graph.tasks {
            // Check if this task has an implements edge
            let targets = task_graph.edges.targets(task_id, "implements-plan");
            for target in targets {
                if target.starts_with("file:") {
                    let entry = plan_to_tasks.entry(target.clone()).or_default();
                    if !entry.contains(task_id) {
                        entry.push(task_id.clone());
                    }
                }
            }
        }

        PlanGraph { plan_to_tasks }
    }

    /// Find all epics for a plan and classify the match cardinality.
    pub fn match_epics_for_plan<'a>(
        &self,
        plan_path: &str,
        task_graph: &'a TaskGraph,
    ) -> PlanEpicMatch<'a> {
        let normalized = normalize_plan_path(plan_path);
        let mut matches: Vec<&Task> = self
            .plan_to_tasks
            .get(&normalized)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| task_graph.tasks.get(id))
            .collect();
        matches.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });

        match matches.len() {
            0 => PlanEpicMatch::None,
            1 => PlanEpicMatch::One(matches[0]),
            _ => PlanEpicMatch::Many(matches),
        }
    }

    /// Resolve a single epic for a plan or return an explicit ambiguity error.
    pub fn resolve_epic_for_plan<'a>(
        &self,
        plan_path: &str,
        task_graph: &'a TaskGraph,
    ) -> Result<Option<&'a Task>> {
        match self.match_epics_for_plan(plan_path, task_graph) {
            PlanEpicMatch::None => Ok(None),
            PlanEpicMatch::One(task) => Ok(Some(task)),
            PlanEpicMatch::Many(matches) => Err(ambiguous_plan_error(plan_path, &matches)),
        }
    }
}

fn ambiguous_plan_error(plan_path: &str, matches: &[&Task]) -> anyhow::Error {
    let details = matches
        .iter()
        .map(|task| format!("{} ({})", task.id, task.name))
        .collect::<Vec<_>>()
        .join(", ");
    anyhow!(
        "Multiple epics implement {}: {}. Use an epic ID to disambiguate.",
        normalize_plan_path(plan_path),
        details
    )
}

/// Normalize a plan path to the canonical "file:..." URI format.
///
/// Handles variations like `./ops/now/foo.md`, `file:./ops/now/foo.md`,
/// and `ops/now/foo.md` — all normalize to `file:ops/now/foo.md`.
pub fn normalize_plan_path(plan_path: &str) -> String {
    let path = plan_path.strip_prefix("file:").unwrap_or(plan_path);

    // Strip leading "./" to normalize relative paths
    let path = path.strip_prefix("./").unwrap_or(path);

    format!("file:{}", path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::graph::{materialize_graph, EdgeStore};
    use crate::tasks::types::{TaskEvent, TaskPriority, TaskStatus};
    use chrono::Utc;
    use std::collections::HashMap;

    fn make_task_graph(tasks: FastHashMap<String, Task>, edges: EdgeStore) -> TaskGraph {
        TaskGraph {
            tasks,
            edges,
            slug_index: FastHashMap::default(),
        }
    }

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

    // --- normalize_plan_path tests ---

    #[test]
    fn test_normalize_bare_path() {
        assert_eq!(
            normalize_plan_path("ops/now/feature.md"),
            "file:ops/now/feature.md"
        );
    }

    #[test]
    fn test_normalize_dot_slash_prefix() {
        assert_eq!(
            normalize_plan_path("./ops/now/feature.md"),
            "file:ops/now/feature.md"
        );
    }

    #[test]
    fn test_normalize_file_dot_slash_prefix() {
        assert_eq!(
            normalize_plan_path("file:./ops/now/feature.md"),
            "file:ops/now/feature.md"
        );
    }

    #[test]
    fn test_normalize_already_prefixed() {
        assert_eq!(
            normalize_plan_path("file:ops/now/feature.md"),
            "file:ops/now/feature.md"
        );
    }

    // --- PlanGraph tests ---

    #[test]
    fn test_find_epic_none() {
        let tg = make_task_graph(FastHashMap::default(), EdgeStore::new());
        let pg = PlanGraph::build(&tg);
        assert!(matches!(
            pg.match_epics_for_plan("ops/now/feature.md", &tg),
            PlanEpicMatch::None
        ));
    }

    #[test]
    fn test_find_epic_basic() {
        let mut tasks = FastHashMap::default();
        tasks.insert(
            "epic1".to_string(),
            make_task("epic1", "Epic", TaskStatus::Open),
        );

        let mut edges = EdgeStore::new();
        edges.add("epic1", "file:ops/now/feature.md", "implements-plan");

        let tg = make_task_graph(tasks, edges);
        let pg = PlanGraph::build(&tg);

        let result = pg.resolve_epic_for_plan("ops/now/feature.md", &tg).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "epic1");
    }

    #[test]
    fn test_find_epic_returns_ambiguity_error_for_multiple_matches() {
        let mut tasks = FastHashMap::default();

        let mut old = make_task("old_epic", "Old Epic", TaskStatus::Closed);
        old.created_at = Utc::now() - chrono::Duration::hours(1);
        tasks.insert("old_epic".to_string(), old);

        let new = make_task("new_epic", "New Epic", TaskStatus::Open);
        tasks.insert("new_epic".to_string(), new);

        let mut edges = EdgeStore::new();
        edges.add("old_epic", "file:ops/now/feature.md", "implements-plan");
        edges.add("new_epic", "file:ops/now/feature.md", "implements-plan");

        let tg = make_task_graph(tasks, edges);
        let pg = PlanGraph::build(&tg);

        let err = pg
            .resolve_epic_for_plan("ops/now/feature.md", &tg)
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Multiple epics implement file:ops/now/feature.md"));
        assert!(msg.contains("old_epic (Old Epic)"));
        assert!(msg.contains("new_epic (New Epic)"));
    }

    // --- build from events (integration) ---

    #[test]
    fn test_data_plan_without_epic_creates_implements_edge() {
        // data.plan WITHOUT data.epic is an epic task — synthesis should work.
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
            instructions: None,
            data,
            timestamp: Utc::now(),
        }];

        let tg = materialize_graph(&events);
        let pg = PlanGraph::build(&tg);

        let result = pg.resolve_epic_for_plan("ops/now/feature.md", &tg).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "epic1");
    }

    #[test]
    fn test_build_from_events_with_link() {
        let events = vec![
            make_created("epic1", "Epic: Feature"),
            make_link("epic1", "file:ops/now/feature.md", "implements-plan"),
        ];

        let tg = materialize_graph(&events);
        let pg = PlanGraph::build(&tg);

        let result = pg.resolve_epic_for_plan("ops/now/feature.md", &tg).unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "epic1");
    }
}
