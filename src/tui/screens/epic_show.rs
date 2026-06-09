use super::helpers::{format_progress, get_subtasks};
use crate::tasks::types::TaskStatus;
use crate::tasks::TaskGraph;
use crate::tui::app::{Line, WindowState};
use crate::tui::components::{self, ChildLine, SubtaskData};

pub fn view(graph: &TaskGraph, epic_id: &str, _window: &WindowState) -> Vec<Line> {
    let epic = match graph.tasks.get(epic_id) {
        Some(t) => t,
        None => return vec![],
    };
    let subtasks = get_subtasks(graph, epic_id);
    let mut lines = vec![];

    // Epic header
    let status_child = match epic.status {
        TaskStatus::Closed => ChildLine::done(epic.display_summary(), epic.elapsed_str()),
        TaskStatus::InProgress => {
            ChildLine::normal(&format_progress(&subtasks), epic.elapsed_str())
        }
        TaskStatus::Stopped => ChildLine::error(epic.display_stopped_reason(), epic.elapsed_str()),
        _ => ChildLine::normal("pending", None),
    };
    lines.extend(components::phase(
        0,
        &epic.name,
        epic.agent_label(),
        false,
        vec![status_child],
    ));

    // Subtask table
    let data: Vec<SubtaskData> = subtasks.iter().map(|s| s.into()).collect();
    lines.extend(components::subtask_table(
        1,
        epic.short_id(),
        &epic.name,
        &data,
        false,
    ));

    lines
}
