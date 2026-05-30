//! PlanGraph — first-class plan management with O(1) reverse index
//!
//! The PlanGraph builds a reverse index from plan file paths to the tasks
//! that implement them. It unifies the duplicate `find_epic_for_plan()`
//! functions that existed in `decompose.rs` and `build.rs`.

use std::path::Path;

use crate::tasks::graph::TaskGraph;
use crate::tasks::types::{FastHashMap, Task, TaskOutcome, TaskStatus};

use super::parser::{parse_plan_metadata, PlanMetadata};

/// Status of a plan in the lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    /// No epic exists (or epic closed as wont_do)
    Draft,
    /// Epic exists and is open (not started)
    Planned,
    /// Epic is in_progress
    Implementing,
    /// Epic is closed (successfully)
    Implemented,
}

impl std::fmt::Display for PlanStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanStatus::Draft => write!(f, "draft"),
            PlanStatus::Planned => write!(f, "planned"),
            PlanStatus::Implementing => write!(f, "implementing"),
            PlanStatus::Implemented => write!(f, "implemented"),
        }
    }
}

/// A plan entry with parsed metadata and derived status.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Canonical path key (e.g., "file:ops/now/foo.md")
    pub path: String,
    /// Parsed metadata from the markdown file
    pub metadata: PlanMetadata,
    /// Derived status based on implementing tasks
    pub status: PlanStatus,
}

/// PlanGraph: indexes plans and their implementing tasks.
///
/// Built from a `TaskGraph` and optionally from filesystem plan files.
/// Provides O(1) lookups for common plan queries.
pub struct PlanGraph {
    /// Reverse index: plan_path (normalized "file:..." URI) → implementing task IDs
    plan_to_tasks: FastHashMap<String, Vec<String>>,
    /// Plan metadata: plan_path → Plan
    plans: FastHashMap<String, Plan>,
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

        PlanGraph {
            plan_to_tasks,
            plans: FastHashMap::default(),
        }
    }

    /// Scan the filesystem for plan files and populate metadata.
    ///
    /// Recursively scans the given directories (relative to `cwd`) for `.md`
    /// files and parses their metadata. Merges with existing plan entries from
    /// the task graph.
    pub fn with_filesystem_plans(mut self, cwd: &Path, dirs: &[&str]) -> Self {
        for dir in dirs {
            let dir_path = cwd.join(dir);
            if !dir_path.is_dir() {
                continue;
            }
            self.scan_dir_recursive(cwd, &dir_path);
        }
        self
    }

    /// Recursively scan a directory for .md files and add them as plans.
    fn scan_dir_recursive(&mut self, cwd: &Path, dir: &Path) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                self.scan_dir_recursive(cwd, &path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Ok(rel) = path.strip_prefix(cwd) {
                    let plan_path = format!("file:{}", rel.display());
                    let metadata = parse_plan_metadata(&path);
                    self.plans.entry(plan_path.clone()).or_insert(Plan {
                        path: plan_path,
                        metadata,
                        status: PlanStatus::Draft,
                    });
                }
            }
        }
    }

    /// Infer and update status for all known plans.
    pub fn infer_statuses(mut self, task_graph: &TaskGraph) -> Self {
        let paths: Vec<String> = self
            .plans
            .keys()
            .chain(self.plan_to_tasks.keys())
            .cloned()
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        for path in paths {
            let status = self.infer_status(&path, task_graph);
            if let Some(plan) = self.plans.get_mut(&path) {
                plan.status = status;
            }
        }
        self
    }

    /// Get all task IDs that implement a given plan.
    ///
    /// O(1) lookup via the reverse index.
    pub fn implementing_task_ids(&self, plan_path: &str) -> &[String] {
        let normalized = normalize_plan_path(plan_path);
        self.plan_to_tasks
            .get(&normalized)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Get all tasks that implement a given plan.
    pub fn implementing_tasks<'a>(
        &self,
        plan_path: &str,
        task_graph: &'a TaskGraph,
    ) -> Vec<&'a Task> {
        self.implementing_task_ids(plan_path)
            .iter()
            .filter_map(|id| task_graph.tasks.get(id))
            .collect()
    }

    /// Find the most recent epic for a plan.
    ///
    /// Returns the most recently created task that has an `implements-plan`
    /// edge to the given plan file. Only epic tasks should have this edge;
    /// decompose and build tasks use `decomposes-plan` and `orchestrates`
    /// respectively.
    pub fn find_epic_for_plan<'a>(
        &self,
        plan_path: &str,
        task_graph: &'a TaskGraph,
    ) -> Option<&'a Task> {
        let normalized = normalize_plan_path(plan_path);
        self.plan_to_tasks
            .get(&normalized)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| task_graph.tasks.get(id))
            .max_by_key(|t| t.created_at)
    }

    /// Find plans that have no implementing tasks.
    pub fn unimplemented(&self) -> Vec<&Plan> {
        self.plans
            .values()
            .filter(|plan| {
                self.plan_to_tasks
                    .get(&plan.path)
                    .map_or(true, |ids| ids.is_empty())
            })
            .collect()
    }

    /// Get a plan by its path.
    #[allow(dead_code)]
    pub fn get_plan(&self, plan_path: &str) -> Option<&Plan> {
        let normalized = normalize_plan_path(plan_path);
        self.plans.get(&normalized)
    }

    /// Get all known plans.
    #[allow(dead_code)]
    pub fn all_plans(&self) -> Vec<&Plan> {
        self.plans.values().collect()
    }

    /// Infer the status of a plan based on its draft flag and implementing tasks.
    fn infer_status(&self, plan_path: &str, task_graph: &TaskGraph) -> PlanStatus {
        // Check draft flag first — always Draft if explicitly marked
        if let Some(plan) = self.plans.get(plan_path) {
            if plan.metadata.draft {
                return PlanStatus::Draft;
            }
        }

        let epic = self.find_epic_for_plan(plan_path, task_graph);

        match epic {
            None => PlanStatus::Draft,
            Some(epic) => match epic.status {
                TaskStatus::Closed => {
                    if epic.closed_outcome == Some(TaskOutcome::WontDo) {
                        PlanStatus::Draft
                    } else {
                        PlanStatus::Implemented
                    }
                }
                TaskStatus::InProgress => PlanStatus::Implementing,
                _ => PlanStatus::Planned,
            },
        }
    }

    /// Check if a plan is a draft.
    pub fn is_draft(&self, plan_path: &str) -> bool {
        let normalized = normalize_plan_path(plan_path);
        self.plans
            .get(&normalized)
            .map_or(false, |plan| plan.metadata.draft)
    }
}

/// Normalize a plan path to the canonical "file:..." URI format.
///
/// Handles variations like `./ops/now/foo.md`, `file:./ops/now/foo.md`,
/// and `ops/now/foo.md` — all normalize to `file:ops/now/foo.md`.
pub fn normalize_plan_path(plan_path: &str) -> String {
    let path = plan_path
        .strip_prefix("file:")
        .unwrap_or(plan_path);

    // Strip leading "./" to normalize relative paths
    let path = path.strip_prefix("./").unwrap_or(path);

    format!("file:{}", path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::graph::{materialize_graph, EdgeStore};
    use crate::tasks::types::{TaskEvent, TaskPriority};
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
            working_copy: None,
            instructions: None,
            data: HashMap::new(),
            created_at: Utc::now(),
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
            working_copy: None,
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

    // --- PlanGraph::build tests ---

    #[test]
    fn test_build_empty_graph() {
        let tg = make_task_graph(FastHashMap::default(), EdgeStore::new());
        let pg = PlanGraph::build(&tg);
        assert!(pg.implementing_task_ids("ops/now/feature.md").is_empty());
    }

    #[test]
    fn test_build_indexes_implements_edges() {
        let mut tasks = FastHashMap::default();
        tasks.insert("epic1".to_string(), make_task("epic1", "Epic", TaskStatus::Open));

        let mut edges = EdgeStore::new();
        edges.add("epic1", "file:ops/now/feature.md", "implements-plan");

        let tg = make_task_graph(tasks, edges);
        let pg = PlanGraph::build(&tg);

        assert_eq!(
            pg.implementing_task_ids("ops/now/feature.md"),
            &["epic1"]
        );
        assert_eq!(
            pg.implementing_task_ids("file:ops/now/feature.md"),
            &["epic1"]
        );
    }

    #[test]
    fn test_build_multiple_implementors() {
        let mut tasks = FastHashMap::default();
        tasks.insert("p1".to_string(), make_task("p1", "Epic 1", TaskStatus::Closed));
        tasks.insert("p2".to_string(), make_task("p2", "Epic 2", TaskStatus::Open));

        let mut edges = EdgeStore::new();
        edges.add("p1", "file:ops/now/feature.md", "implements-plan");
        edges.add("p2", "file:ops/now/feature.md", "implements-plan");

        let tg = make_task_graph(tasks, edges);
        let pg = PlanGraph::build(&tg);

        let ids = pg.implementing_task_ids("ops/now/feature.md");
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"p1".to_string()));
        assert!(ids.contains(&"p2".to_string()));
    }

    // --- find_epic_for_plan tests ---

    #[test]
    fn test_find_epic_none() {
        let tg = make_task_graph(FastHashMap::default(), EdgeStore::new());
        let pg = PlanGraph::build(&tg);
        assert!(pg.find_epic_for_plan("ops/now/feature.md", &tg).is_none());
    }

    #[test]
    fn test_find_epic_basic() {
        let mut tasks = FastHashMap::default();
        tasks.insert("epic1".to_string(), make_task("epic1", "Epic", TaskStatus::Open));

        let mut edges = EdgeStore::new();
        edges.add("epic1", "file:ops/now/feature.md", "implements-plan");

        let tg = make_task_graph(tasks, edges);
        let pg = PlanGraph::build(&tg);

        let result = pg.find_epic_for_plan("ops/now/feature.md", &tg);
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "epic1");
    }

    #[test]
    fn test_find_epic_returns_most_recent() {
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

        let result = pg.find_epic_for_plan("ops/now/feature.md", &tg);
        assert_eq!(result.unwrap().id, "new_epic");
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
            working_copy: None,
            instructions: None,
            data,
            timestamp: Utc::now(),
        }];

        let tg = materialize_graph(&events);
        let pg = PlanGraph::build(&tg);

        let result = pg.find_epic_for_plan("ops/now/feature.md", &tg);
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

        let result = pg.find_epic_for_plan("ops/now/feature.md", &tg);
        assert!(result.is_some());
        assert_eq!(result.unwrap().id, "epic1");
    }

    // --- infer_status tests ---

    #[test]
    fn test_infer_status_draft_no_epic() {
        let tg = make_task_graph(FastHashMap::default(), EdgeStore::new());
        let pg = PlanGraph::build(&tg);
        assert_eq!(pg.infer_status("file:ops/now/feature.md", &tg), PlanStatus::Draft);
    }

    #[test]
    fn test_infer_status_planned() {
        let mut tasks = FastHashMap::default();
        tasks.insert("p1".to_string(), make_task("p1", "Epic", TaskStatus::Open));

        let mut edges = EdgeStore::new();
        edges.add("p1", "file:ops/now/feature.md", "implements-plan");

        let tg = make_task_graph(tasks, edges);
        let pg = PlanGraph::build(&tg);
        assert_eq!(
            pg.infer_status("file:ops/now/feature.md", &tg),
            PlanStatus::Planned
        );
    }

    #[test]
    fn test_infer_status_implementing() {
        let mut tasks = FastHashMap::default();
        tasks.insert(
            "p1".to_string(),
            make_task("p1", "Epic", TaskStatus::InProgress),
        );

        let mut edges = EdgeStore::new();
        edges.add("p1", "file:ops/now/feature.md", "implements-plan");

        let tg = make_task_graph(tasks, edges);
        let pg = PlanGraph::build(&tg);
        assert_eq!(
            pg.infer_status("file:ops/now/feature.md", &tg),
            PlanStatus::Implementing
        );
    }

    #[test]
    fn test_infer_status_implemented() {
        let mut tasks = FastHashMap::default();
        let mut t = make_task("p1", "Epic", TaskStatus::Closed);
        t.closed_outcome = Some(TaskOutcome::Done);
        tasks.insert("p1".to_string(), t);

        let mut edges = EdgeStore::new();
        edges.add("p1", "file:ops/now/feature.md", "implements-plan");

        let tg = make_task_graph(tasks, edges);
        let pg = PlanGraph::build(&tg);
        assert_eq!(
            pg.infer_status("file:ops/now/feature.md", &tg),
            PlanStatus::Implemented
        );
    }

    #[test]
    fn test_infer_status_draft_wont_do() {
        let mut tasks = FastHashMap::default();
        let mut t = make_task("p1", "Epic", TaskStatus::Closed);
        t.closed_outcome = Some(TaskOutcome::WontDo);
        tasks.insert("p1".to_string(), t);

        let mut edges = EdgeStore::new();
        edges.add("p1", "file:ops/now/feature.md", "implements-plan");

        let tg = make_task_graph(tasks, edges);
        let pg = PlanGraph::build(&tg);
        assert_eq!(
            pg.infer_status("file:ops/now/feature.md", &tg),
            PlanStatus::Draft
        );
    }

    #[test]
    fn test_infer_status_draft_overrides_epic() {
        // Even with an implementing task, a draft plan should stay Draft
        let dir = tempfile::TempDir::new().unwrap();
        let ops_dir = dir.path().join("ops/now");
        std::fs::create_dir_all(&ops_dir).unwrap();
        std::fs::write(
            ops_dir.join("feature.md"),
            "---\ndraft: true\n---\n\n# Feature\n\nDesc.\n",
        )
        .unwrap();

        let mut tasks = FastHashMap::default();
        tasks.insert(
            "p1".to_string(),
            make_task("p1", "Epic", TaskStatus::InProgress),
        );

        let mut edges = EdgeStore::new();
        edges.add("p1", "file:ops/now/feature.md", "implements-plan");

        let tg = make_task_graph(tasks, edges);
        let pg = PlanGraph::build(&tg)
            .with_filesystem_plans(dir.path(), &["ops/now"])
            .infer_statuses(&tg);

        let plan = pg.get_plan("file:ops/now/feature.md").unwrap();
        assert_eq!(plan.status, PlanStatus::Draft);
        assert!(pg.is_draft("ops/now/feature.md"));
    }

    // --- PlanStatus display ---

    #[test]
    fn test_plan_status_display() {
        assert_eq!(PlanStatus::Draft.to_string(), "draft");
        assert_eq!(PlanStatus::Planned.to_string(), "planned");
        assert_eq!(PlanStatus::Implementing.to_string(), "implementing");
        assert_eq!(PlanStatus::Implemented.to_string(), "implemented");
    }

    // --- unimplemented ---

    #[test]
    fn test_unimplemented_with_filesystem_plans() {
        let dir = tempfile::TempDir::new().unwrap();
        let ops_dir = dir.path().join("ops/now");
        std::fs::create_dir_all(&ops_dir).unwrap();
        std::fs::write(ops_dir.join("feature.md"), "# Feature\n\nDesc.\n").unwrap();
        std::fs::write(ops_dir.join("other.md"), "# Other\n\nOther desc.\n").unwrap();

        // Only feature.md has an implementing task
        let mut tasks = FastHashMap::default();
        tasks.insert("p1".to_string(), make_task("p1", "Epic", TaskStatus::Open));

        let mut edges = EdgeStore::new();
        edges.add("p1", "file:ops/now/feature.md", "implements-plan");

        let tg = make_task_graph(tasks, edges);
        let pg = PlanGraph::build(&tg)
            .with_filesystem_plans(dir.path(), &["ops/now"]);

        let unimplemented = pg.unimplemented();
        assert_eq!(unimplemented.len(), 1);
        assert_eq!(unimplemented[0].path, "file:ops/now/other.md");
    }

    #[test]
    fn test_filesystem_plans_recursive() {
        let dir = tempfile::TempDir::new().unwrap();
        let ops_dir = dir.path().join("ops/now");
        let sub_dir = dir.path().join("ops/now/subdir");
        std::fs::create_dir_all(&sub_dir).unwrap();
        std::fs::write(ops_dir.join("top.md"), "# Top\n\nTop level.\n").unwrap();
        std::fs::write(sub_dir.join("nested.md"), "# Nested\n\nNested plan.\n").unwrap();

        let tg = make_task_graph(FastHashMap::default(), EdgeStore::new());
        let pg = PlanGraph::build(&tg)
            .with_filesystem_plans(dir.path(), &["ops/now"]);

        assert!(pg.get_plan("file:ops/now/top.md").is_some());
        assert!(pg.get_plan("file:ops/now/subdir/nested.md").is_some());
        assert_eq!(pg.all_plans().len(), 2);
    }
}
