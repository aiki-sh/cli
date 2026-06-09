//! Fix pipeline view: fix-plan → decompose → subtask table → loop → review of fixes → regression review.

use crate::tasks::lanes::{derive_lanes, lane_status, LaneStatus};
use crate::tasks::types::TaskStatus;
use crate::tasks::TaskGraph;
use crate::tui::app::{Line, WindowState};
use crate::tui::components::{self, ChildLine, LaneData, SubtaskData};

use crate::tui::theme;

use super::helpers::*;

pub fn view(
    graph: &TaskGraph,
    fix_parent_id: &str,
    review_id: &str,
    _window: &WindowState,
) -> Vec<Line> {
    let mut lines = vec![];
    let mut group: u16 = 0;

    // Short-circuit: no actionable issues
    if no_actionable_issues(graph, review_id) {
        lines.extend(components::phase(
            group,
            "fix",
            Some("claude"),
            false,
            vec![ChildLine::done(
                &format!("{} approved — no actionable issues", theme::SYM_CHECK),
                None,
            )],
        ));
        return lines;
    }

    // Fix plan phase
    let fix_task = match graph.tasks.get(fix_parent_id) {
        Some(t) => t,
        None => return loading_lines(),
    };
    let fix_active = !fix_task.is_terminal();
    let fix_children = match fix_task.status {
        TaskStatus::Open => vec![ChildLine::active("starting session...")],
        TaskStatus::InProgress => vec![ChildLine::active_with_elapsed(
            fix_task.latest_heartbeat(),
            fix_task.elapsed_str(),
        )],
        TaskStatus::Closed => vec![ChildLine::done(
            fix_task.display_summary(),
            fix_task.elapsed_str(),
        )],
        _ => vec![],
    };
    lines.extend(components::phase(
        group,
        "fix",
        fix_task.agent_label(),
        fix_active,
        fix_children,
    ));
    group += 1;

    // Decompose phase (reuses same pattern as build)
    if let Some(decompose) = find_decompose_task(graph, fix_parent_id) {
        let active = !decompose.is_terminal();
        let children = match decompose.status {
            TaskStatus::Open => vec![ChildLine::active("Reading task graph...")],
            TaskStatus::InProgress => vec![ChildLine::active_with_elapsed(
                decompose.latest_heartbeat(),
                decompose.elapsed_str(),
            )],
            TaskStatus::Closed => {
                let count = get_work_subtasks(graph, fix_parent_id).len();
                vec![ChildLine::normal(
                    &format!("{} subtasks created", count),
                    decompose.elapsed_str(),
                )]
            }
            _ => vec![],
        };
        lines.extend(components::phase(
            group,
            "decompose",
            decompose.agent_label(),
            active,
            children,
        ));
        group += 1;
    }

    // Subtask table
    let subtasks = get_work_subtasks(graph, fix_parent_id);
    if !subtasks.is_empty() || decompose_in_progress(graph, fix_parent_id) {
        let data: Vec<SubtaskData> = subtasks.iter().map(|s| s.into()).collect();
        let loading = subtasks.is_empty();
        lines.extend(components::subtask_table(
            group,
            fix_task.short_id(),
            "Followup",
            &data,
            loading,
        ));
    }

    // Loop phase (when lanes have been assigned)
    let decomposition = derive_lanes(graph, fix_parent_id);
    if !decomposition.lanes.is_empty() {
        let lane_data: Vec<LaneData> = decomposition
            .lanes
            .iter()
            .enumerate()
            .map(|(i, lane)| lane_to_data(i + 1, lane, graph, &decomposition.lanes))
            .collect();
        lines.extend(components::loop_block(group + 1, &lane_data));
        group += 2;
    }

    // Review of fixes
    if let Some(review) = find_fix_review(graph, fix_parent_id) {
        lines.extend(review_phase_lines(group, review, graph));
        group += 1;
    }

    // Regression review (checks original review target for regressions)
    if let Some(regression) = find_regression_review(graph, review_id) {
        let active = !regression.is_terminal();
        let children = match regression.status {
            TaskStatus::Open => vec![ChildLine::active("starting session...")],
            TaskStatus::InProgress => vec![ChildLine::active_with_elapsed(
                regression.latest_heartbeat(),
                regression.elapsed_str(),
            )],
            TaskStatus::Closed => {
                let issues = extract_issues(regression);
                if issues.is_empty() {
                    vec![ChildLine::done(
                        &format!("{} no regressions", theme::SYM_CHECK),
                        regression.elapsed_str(),
                    )]
                } else {
                    vec![ChildLine::error(
                        &format!("Found {} regressions", issues.len()),
                        regression.elapsed_str(),
                    )]
                }
            }
            _ => vec![],
        };
        lines.extend(components::phase(
            group,
            "review for regressions",
            regression.agent_label(),
            active,
            children,
        ));
    }

    lines
}

/// Convert a `Lane` into a `LaneData` for rendering.
fn lane_to_data(
    number: usize,
    lane: &crate::tasks::lanes::Lane,
    graph: &TaskGraph,
    all_lanes: &[crate::tasks::lanes::Lane],
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
    let mut agent = String::from("claude");

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
                    if let Some(label) = task.agent_label() {
                        agent = label.to_string();
                    }
                }
                _ => {}
            }
        }
    }

    let status = lane_status(lane, graph, all_lanes);
    let shutdown = matches!(status, LaneStatus::Complete | LaneStatus::Failed);

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
