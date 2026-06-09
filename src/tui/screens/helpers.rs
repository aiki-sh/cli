//! Shared graph query and rendering helpers used by all view functions.

use std::time::Instant;

use crate::tasks::lanes::{derive_lanes, lane_status, Lane, LaneStatus};
use crate::tasks::types::TaskStatus;
use crate::tasks::{Task, TaskGraph};
use crate::tui::app::{
    Entry, Line, LineStyle, Model, PhaseLifecycle, PhaseState, Screen, SubtaskStatus, WindowState,
};
use crate::tui::components::{self, ChildLine, LaneData, SubtaskData};
use crate::tui::theme;

// ── Graph query helpers ────────────────────────────────────────────

/// Get ordered subtasks for a parent (by creation time).
/// Uses EdgeStore reverse lookup on "subtask-of" link kind.
pub fn get_subtasks<'a>(graph: &'a TaskGraph, parent_id: &str) -> Vec<&'a Task> {
    let mut children = graph.children_of(parent_id);
    children.sort_by_key(|t| t.created_at);
    children
}

/// Get subtasks that represent actual work items (excludes decompose, review, fix, orchestrator).
pub fn get_work_subtasks<'a>(graph: &'a TaskGraph, parent_id: &str) -> Vec<&'a Task> {
    let mut children = graph.children_of(parent_id);
    children.retain(|t| {
        !matches!(
            t.task_type.as_deref(),
            Some("decompose") | Some("review") | Some("fix") | Some("orchestrator")
        )
    });
    children.sort_by_key(|t| t.created_at);
    children
}

/// Find the decompose subtask of an epic/fix parent.
/// Looks for a child task with task_type == "decompose".
pub fn find_decompose_task<'a>(graph: &'a TaskGraph, parent_id: &str) -> Option<&'a Task> {
    graph
        .children_of(parent_id)
        .into_iter()
        .find(|t| t.task_type.as_deref() == Some("decompose"))
}

/// Whether decompose task exists and is not terminal.
pub fn decompose_in_progress(graph: &TaskGraph, parent_id: &str) -> bool {
    find_decompose_task(graph, parent_id)
        .map(|t| !t.is_terminal())
        .unwrap_or(false)
}

/// Find the review subtask in a build epic.
/// Looks for a child task with task_type == "review".
pub fn find_build_review<'a>(graph: &'a TaskGraph, epic_id: &str) -> Option<&'a Task> {
    graph
        .children_of(epic_id)
        .into_iter()
        .find(|t| t.task_type.as_deref() == Some("review"))
}

/// Find the review subtask in a fix parent.
pub fn find_fix_review<'a>(graph: &'a TaskGraph, fix_parent_id: &str) -> Option<&'a Task> {
    graph
        .children_of(fix_parent_id)
        .into_iter()
        .find(|t| t.task_type.as_deref() == Some("review"))
}

/// Find the regression review task (linked via "validates" to the original review).
pub fn find_regression_review<'a>(graph: &'a TaskGraph, review_id: &str) -> Option<&'a Task> {
    let referrers = graph.edges.referrers(review_id, "validates");
    referrers
        .iter()
        .filter_map(|id| graph.tasks.get(id.as_str()))
        .find(|t| t.task_type.as_deref() == Some("review"))
}

/// Find the fix parent task for a specific iteration.
/// Fix parents are children of the epic with task_type == "fix" and data["iteration"] matching.
pub fn find_fix_parent(graph: &TaskGraph, epic_id: &str, iteration: u16) -> Option<String> {
    graph
        .children_of(epic_id)
        .into_iter()
        .find(|t| {
            t.task_type.as_deref() == Some("fix")
                && t.data.get("iteration").and_then(|v| v.parse::<u16>().ok()) == Some(iteration)
        })
        .map(|t| t.id.clone())
}

/// Get the review task ID associated with an epic.
pub fn review_id_for_epic(graph: &TaskGraph, epic_id: &str) -> String {
    find_build_review(graph, epic_id)
        .map(|t| t.id.clone())
        .unwrap_or_default()
}

/// Count fix iterations in the graph (number of fix children of the epic).
pub fn current_iteration(graph: &TaskGraph, epic_id: &str) -> u16 {
    let fix_count = graph
        .children_of(epic_id)
        .into_iter()
        .filter(|t| t.task_type.as_deref() == Some("fix"))
        .count();
    // First build is iteration 1, fix cycles start at 2
    (fix_count as u16) + 1
}

/// Check if all phases (including fix iterations) are done.
pub fn is_build_complete(graph: &TaskGraph, epic_id: &str) -> bool {
    let epic = match graph.tasks.get(epic_id) {
        Some(t) => t,
        None => return false,
    };
    epic.is_terminal()
}

/// Check if review found zero actionable issues.
pub fn no_actionable_issues(graph: &TaskGraph, review_id: &str) -> bool {
    graph
        .tasks
        .get(review_id)
        .map(|t| t.status == TaskStatus::Closed && extract_issues(t).is_empty())
        .unwrap_or(false)
}

/// Extract issues from a review task's comments.
/// Issues are comments with data["type"] == "issue".
pub fn extract_issues(task: &Task) -> Vec<Issue> {
    task.comments
        .iter()
        .filter(|c| c.data.get("type").map(|t| t == "issue").unwrap_or(false))
        .map(|c| Issue {
            title: c.text.clone(),
            severity: c
                .data
                .get("severity")
                .cloned()
                .unwrap_or_else(|| "medium".to_string()),
        })
        .collect()
}

pub struct Issue {
    pub title: String,
    pub severity: String,
}

// ── Rendering helpers ──────────────────────────────────────────────

/// Standard loading placeholder.
pub fn loading_lines() -> Vec<Line> {
    vec![Line {
        indent: 0,
        text: "Reading task graph...".to_string(),
        meta: None,
        style: LineStyle::PhaseHeader { active: true },
        group: 0,
        dimmed: false,
    }]
}

/// Format progress string: "2/5 subtasks completed" or "2/5 subtasks completed, 1 failed"
pub fn format_progress(subtasks: &[&Task]) -> String {
    let total = subtasks.len();
    let done = subtasks
        .iter()
        .filter(|t| t.status == TaskStatus::Closed)
        .count();
    let failed = subtasks
        .iter()
        .filter(|t| t.status == TaskStatus::Stopped)
        .count();
    if failed > 0 {
        format!("{}/{} subtasks completed, {} failed", done, total, failed)
    } else {
        format!("{}/{} subtasks completed", done, total)
    }
}

/// Render a review phase + inline issue list (shared by build and fix).
pub fn review_phase_lines(group: u16, review: &Task, graph: &TaskGraph) -> Vec<Line> {
    let _ = graph; // available for future use
    let mut lines = vec![];
    let issues = extract_issues(review);
    let active = !review.is_terminal();

    let children = match review.status {
        TaskStatus::Open => vec![ChildLine::active("starting session...")],
        TaskStatus::InProgress => {
            vec![ChildLine::active_with_elapsed(
                review.latest_heartbeat(),
                review.elapsed_str(),
            )]
        }
        TaskStatus::Closed if !issues.is_empty() => vec![ChildLine::normal(
            &format!("Found {} issues", issues.len()),
            review.elapsed_str(),
        )],
        TaskStatus::Closed => {
            vec![ChildLine::done(
                &format!("{} approved", crate::tui::theme::SYM_CHECK),
                review.elapsed_str(),
            )]
        }
        _ => vec![],
    };

    lines.extend(components::phase(
        group,
        "review",
        review.agent_label(),
        active,
        children,
    ));

    if !issues.is_empty() {
        lines.extend(components::blank());
        let issue_texts: Vec<String> = issues.iter().map(|i| i.title.clone()).collect();
        lines.extend(components::issues(group, &issue_texts));
    }

    lines
}

// ── SubtaskData conversion ─────────────────────────────────────────

/// Convert a task to SubtaskData for rendering.
/// `in_active_lane` determines whether pending tasks show ○ (true) or ◌ (false).
pub fn subtask_data_from_task(task: &Task, in_active_lane: bool) -> SubtaskData {
    let status = match task.status {
        TaskStatus::Open => {
            if task.claimed_by_session.is_some() {
                SubtaskStatus::Assigned
            } else if in_active_lane {
                SubtaskStatus::Pending
            } else {
                SubtaskStatus::PendingUnassigned
            }
        }
        TaskStatus::Reserved => SubtaskStatus::Assigned,
        TaskStatus::InProgress => SubtaskStatus::Active,
        TaskStatus::Closed => SubtaskStatus::Done,
        TaskStatus::Stopped => SubtaskStatus::Failed,
    };
    SubtaskData {
        name: task.name.clone(),
        status,
        elapsed: task.elapsed_str(),
    }
}

impl From<&&Task> for SubtaskData {
    fn from(task: &&Task) -> Self {
        subtask_data_from_task(task, false)
    }
}

// ── Entry-based rendering helpers ─────────────────────────────────

/// Render a single phase entry as header + status child lines.
///
/// **Heartbeat handoff**: when `phase.task_id` is set and the graph has a
/// heartbeat for that task, the view switches from `worker_status` to
/// the heartbeat text.
pub fn render_phase_line(
    phase: &PhaseState,
    graph: &TaskGraph,
    _window: &WindowState,
) -> Vec<Line> {
    let elapsed = Some(format_instant_elapsed(phase.started_at));

    let (active, children) = match &phase.state {
        PhaseLifecycle::Active => {
            let child = if let Some(ref task_id) = phase.task_id {
                // Has task_id — prefer graph heartbeat over worker_status
                let heartbeat = graph
                    .tasks
                    .get(task_id.as_str())
                    .map(|t| t.latest_heartbeat())
                    .unwrap_or("");
                if !heartbeat.is_empty() {
                    ChildLine::active_with_elapsed(heartbeat, elapsed)
                } else {
                    ChildLine::normal(phase.worker_status.as_deref().unwrap_or(""), elapsed)
                }
            } else {
                // No task_id — use worker_status
                ChildLine::normal(phase.worker_status.as_deref().unwrap_or(""), elapsed)
            };
            (true, vec![child])
        }
        PhaseLifecycle::Done { result } => {
            let child = ChildLine::done(&format!("{} {}", theme::SYM_CHECK, result), elapsed);
            (false, vec![child])
        }
        PhaseLifecycle::Failed { error } => {
            let child = ChildLine::error(&format!("{} {}", theme::SYM_FAILED, error), elapsed);
            (false, vec![child])
        }
    };

    components::phase(0, phase.name, phase.agent.as_deref(), active, children)
}

/// Render a bordered subtask table for the orchestrated parent task.
pub fn render_subtask_table(
    graph: &TaskGraph,
    orchestrates_id: &str,
    _window: &WindowState,
) -> Vec<Line> {
    let parent = match graph.tasks.get(orchestrates_id) {
        Some(t) => t,
        None => return vec![],
    };
    let subtasks = get_subtasks(graph, orchestrates_id);
    if subtasks.is_empty() {
        return vec![];
    }
    let data: Vec<SubtaskData> = subtasks.iter().map(|s| s.into()).collect();
    components::subtask_table(0, parent.short_id(), &parent.name, &data, false)
}

/// Render per-lane progress blocks.
///
/// Groups children of `orchestrates_id` by lane, then renders each lane
/// with heartbeat, completion counts, and agent type.
pub fn render_lane_blocks(
    graph: &TaskGraph,
    orchestrates_id: &str,
    _window: &WindowState,
) -> Vec<Line> {
    let decomposition = derive_lanes(graph, orchestrates_id);
    if decomposition.lanes.is_empty() {
        return vec![];
    }
    let lane_data: Vec<LaneData> = decomposition
        .lanes
        .iter()
        .enumerate()
        .map(|(i, lane)| lane_to_render_data(i + 1, lane, graph, &decomposition.lanes))
        .collect();
    components::loop_block(0, &lane_data)
}

/// Render numbered issue list for a review task.
pub fn render_issue_list(graph: &TaskGraph, task_id: &str, _window: &WindowState) -> Vec<Line> {
    let task = match graph.tasks.get(task_id) {
        Some(t) => t,
        None => return vec![],
    };
    let issues = extract_issues(task);
    if issues.is_empty() {
        return vec![];
    }
    let issue_texts: Vec<String> = issues.iter().map(|i| i.title.clone()).collect();
    components::issues(0, &issue_texts)
}

/// Render final stats line (sessions, tokens, elapsed) when finished.
pub fn render_summary_line(model: &Model) -> Vec<Line> {
    if !model.finished {
        return vec![];
    }

    // Extract root task ID from screen
    let root_id = match &model.screen {
        Screen::TaskRun { task_id } => task_id.as_str(),
        Screen::Build { epic_id, .. } => epic_id.as_str(),
        Screen::Review { review_id, .. } => review_id.as_str(),
        Screen::Fix { fix_parent_id, .. } => fix_parent_id.as_str(),
        Screen::EpicShow { epic_id } => epic_id.as_str(),
        Screen::ReviewShow { review_id } => review_id.as_str(),
    };

    let children = model.graph.children_of(root_id);
    let sessions = if children.is_empty() {
        1
    } else {
        children.len()
    };

    // Sum tokens from graph children
    let mut total_tokens: u64 = 0;
    for child in &children {
        if let Some(tok_str) = child.data.get("tokens") {
            if let Ok(tok) = tok_str.parse::<u64>() {
                total_tokens += tok;
            }
        }
    }

    // Elapsed from earliest entry
    let elapsed = model
        .entries
        .iter()
        .filter_map(|e| match e {
            Entry::Phase(p) => Some(p.started_at),
            _ => None,
        })
        .min()
        .map(format_instant_elapsed)
        .unwrap_or_else(|| "0s".to_string());

    let tokens_str = format_tokens_compact(total_tokens);

    vec![Line {
        indent: 0,
        text: format!(
            "{} session{} \u{2014} {} \u{2014} {} tokens",
            sessions,
            if sessions == 1 { "" } else { "s" },
            elapsed,
            tokens_str,
        ),
        meta: None,
        style: LineStyle::Dim,
        group: 0,
        dimmed: false,
    }]
}

// ── Shared lane conversion ────────────────────────────────────────

/// Convert a `Lane` into rendering `LaneData`.
pub fn lane_to_render_data(
    number: usize,
    lane: &Lane,
    graph: &TaskGraph,
    all_lanes: &[Lane],
) -> LaneData {
    let all_task_ids: Vec<&str> = lane
        .threads
        .iter()
        .flat_map(|s| s.task_ids.iter())
        .map(|s| s.as_str())
        .collect();

    let total = all_task_ids.len();
    let mut completed = 0;
    let mut failed = 0;
    let mut heartbeat = None;
    let mut elapsed = None;
    let mut agent = String::new();

    for tid in &all_task_ids {
        if let Some(task) = graph.tasks.get(*tid) {
            match task.status {
                TaskStatus::Closed => completed += 1,
                TaskStatus::Stopped => failed += 1,
                TaskStatus::InProgress => {
                    let hb = task.latest_heartbeat();
                    if !hb.is_empty() {
                        heartbeat = Some(hb.to_string());
                    }
                    elapsed = task.elapsed_str();
                }
                _ => {}
            }
            if agent.is_empty() {
                if let Some(label) = task.agent_label() {
                    agent = label.to_string();
                }
            }
        }
    }

    let status = lane_status(lane, graph, all_lanes);
    let shutdown = matches!(status, LaneStatus::Complete | LaneStatus::Failed);

    if agent.is_empty() {
        agent = "agent".to_string();
    }

    LaneData {
        number,
        agent,
        completed,
        total,
        failed,
        heartbeat,
        elapsed,
        shutdown,
    }
}

// ── Private format helpers ────────────────────────────────────────

/// Format elapsed time from an `Instant`.
fn format_instant_elapsed(started_at: Instant) -> String {
    let secs = started_at.elapsed().as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        format!("{}h{:02}m", h, m)
    }
}

/// Format token count compactly.
fn format_tokens_compact(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{}", tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::graph::EdgeStore;
    use crate::tasks::types::{FastHashMap, TaskOutcome, TaskPriority, TaskStatus};
    use chrono::Utc;
    use std::collections::HashMap;

    fn make_task(id: &str, name: &str, status: TaskStatus, task_type: Option<&str>) -> Task {
        Task {
            id: id.to_string(),
            name: name.to_string(),
            slug: None,
            task_type: task_type.map(|s| s.to_string()),
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
            closed_outcome: if status == TaskStatus::Closed {
                Some(TaskOutcome::Done)
            } else {
                None
            },
            confidence: None,
            summary: None,
            turn_started: None,
            closed_at: None,
            turn_closed: None,
            turn_stopped: None,
            comments: Vec::new(),
        }
    }

    fn make_graph() -> TaskGraph {
        TaskGraph {
            tasks: FastHashMap::default(),
            edges: EdgeStore::default(),
            slug_index: FastHashMap::default(),
        }
    }

    #[test]
    fn get_work_subtasks_filters_non_work_tasks() {
        let mut graph = make_graph();

        let parent = make_task("parent1", "Epic", TaskStatus::InProgress, None);
        let work1 = make_task("work1", "Implement feature A", TaskStatus::Open, None);
        let work2 = make_task("work2", "Implement feature B", TaskStatus::InProgress, None);
        let decompose = make_task(
            "decomp1",
            "Decompose",
            TaskStatus::Closed,
            Some("decompose"),
        );
        let review = make_task("review1", "Review", TaskStatus::Open, Some("review"));
        let fix = make_task("fix1", "Fix issues", TaskStatus::Open, Some("fix"));
        let orchestrator = make_task(
            "orch1",
            "Orchestrator",
            TaskStatus::Open,
            Some("orchestrator"),
        );

        graph.tasks.insert("parent1".to_string(), parent);
        graph.tasks.insert("work1".to_string(), work1);
        graph.tasks.insert("work2".to_string(), work2);
        graph.tasks.insert("decomp1".to_string(), decompose);
        graph.tasks.insert("review1".to_string(), review);
        graph.tasks.insert("fix1".to_string(), fix);
        graph.tasks.insert("orch1".to_string(), orchestrator);

        graph.edges.add("work1", "parent1", "subtask-of");
        graph.edges.add("work2", "parent1", "subtask-of");
        graph.edges.add("decomp1", "parent1", "subtask-of");
        graph.edges.add("review1", "parent1", "subtask-of");
        graph.edges.add("fix1", "parent1", "subtask-of");
        graph.edges.add("orch1", "parent1", "subtask-of");

        // get_subtasks returns all 6 children
        let all = get_subtasks(&graph, "parent1");
        assert_eq!(all.len(), 6);

        // get_work_subtasks excludes decompose, review, fix, orchestrator
        let work = get_work_subtasks(&graph, "parent1");
        assert_eq!(work.len(), 2);
        let ids: Vec<&str> = work.iter().map(|t| t.id.as_str()).collect();
        assert!(ids.contains(&"work1"));
        assert!(ids.contains(&"work2"));
        assert!(!ids
            .iter()
            .any(|id| ["decomp1", "review1", "fix1", "orch1"].contains(id)));
    }
}
