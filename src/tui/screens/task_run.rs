// cli/src/tui/screens/task_run.rs

use super::helpers::{format_progress, get_subtasks, loading_lines};
use crate::tasks::types::TaskStatus;
use crate::tasks::TaskGraph;
use crate::tui::app::{Line, LineStyle, WindowState};
use crate::tui::components::{self, ChildLine, SubtaskData};

pub fn view(graph: &TaskGraph, task_id: &str, _window: &WindowState) -> Vec<Line> {
    let task = match graph.tasks.get(task_id) {
        Some(t) => t,
        None => return loading_lines(),
    };
    let subtasks = get_subtasks(graph, task_id);
    let mut lines = vec![];

    match task.status {
        // Active states: spinner header + status child
        TaskStatus::Open | TaskStatus::Reserved | TaskStatus::InProgress => {
            let children = match task.status {
                TaskStatus::Open | TaskStatus::Reserved => {
                    vec![ChildLine::active("starting session...")]
                }
                TaskStatus::InProgress if subtasks.is_empty() => {
                    vec![ChildLine::active_with_elapsed(
                        task.latest_heartbeat(),
                        task.elapsed_str(),
                    )]
                }
                TaskStatus::InProgress => vec![ChildLine::normal(
                    &format_progress(&subtasks),
                    task.elapsed_str(),
                )],
                _ => unreachable!(),
            };
            lines.extend(components::phase(
                0,
                "task",
                task.agent_label(),
                true,
                children,
            ));
        }

        // Done: 合 task completed — <summary>           <elapsed>
        //       ⎿ <summary detail>
        TaskStatus::Closed => {
            let header_text = format!("task completed — {}", task.display_summary());
            lines.push(Line {
                indent: 0,
                text: header_text,
                meta: task.elapsed_str(),
                style: LineStyle::PhaseHeader { active: false },
                group: 0,
                dimmed: false,
            });
            lines.push(Line {
                indent: 1,
                text: task.display_summary().to_string(),
                meta: None,
                style: LineStyle::Child,
                group: 0,
                dimmed: false,
            });
        }

        // Failed: 合 task failed — <name>               <elapsed>
        //         ⎿ ✘ <stopped reason>
        TaskStatus::Stopped => {
            let header_text = format!("task failed — {}", task.name);
            lines.push(Line {
                indent: 0,
                text: header_text,
                meta: task.elapsed_str(),
                style: LineStyle::PhaseHeaderFailed,
                group: 0,
                dimmed: false,
            });
            lines.push(Line {
                indent: 1,
                text: task.display_stopped_reason().to_string(),
                meta: None,
                style: LineStyle::ChildError,
                group: 0,
                dimmed: false,
            });
        }
    }

    // Subtask table (if parent task)
    if !subtasks.is_empty() {
        let data: Vec<SubtaskData> = subtasks.iter().map(|s| s.into()).collect();
        lines.extend(components::subtask_table(
            0,
            task.short_id(),
            &task.name,
            &data,
            false,
        ));
    }

    // Hint text when done
    if task.is_terminal() {
        lines.extend(components::blank());
        lines.push(Line {
            indent: 0,
            text: format!("Run `aiki task show {}` for details.", task.short_id()),
            meta: None,
            style: LineStyle::Dim,
            group: 0,
            dimmed: false,
        });
    }

    lines
}
