// cli/src/tui/screens/review.rs

use crate::tasks::types::TaskStatus;
use crate::tasks::TaskGraph;
use crate::tui::app::{Line, WindowState};
use crate::tui::components::{self, ChildLine};
use crate::tui::theme;

use super::helpers::{extract_issues, loading_lines};

pub fn view(graph: &TaskGraph, review_id: &str, target: &str, _window: &WindowState) -> Vec<Line> {
    let review_task = match graph.tasks.get(review_id) {
        Some(t) => t,
        None => return loading_lines(),
    };
    let issues = extract_issues(review_task);
    let active = !review_task.is_terminal();
    let mut lines = vec![];

    // Review phase — target as first child, heartbeat/result as second
    let mut children = vec![ChildLine::normal(target, None)];

    match review_task.status {
        TaskStatus::Open => children.push(ChildLine::active("creating isolated workspace...")),
        TaskStatus::InProgress => children.push(ChildLine::active_with_elapsed(
            review_task.latest_heartbeat(),
            review_task.elapsed_str(),
        )),
        TaskStatus::Closed if !issues.is_empty() => {
            children.push(ChildLine::normal(
                &format!("Found {} issues", issues.len()),
                None,
            ));
        }
        TaskStatus::Closed => {
            children.push(ChildLine::done(
                &format!("{} approved", theme::SYM_CHECK),
                None,
            ));
        }
        _ => {}
    }

    lines.extend(components::phase(
        0,
        "review",
        review_task.agent_label(),
        active,
        children,
    ));

    // Issue list
    if !issues.is_empty() {
        lines.extend(components::blank());
        let issue_texts: Vec<String> = issues.iter().map(|i| i.title.clone()).collect();
        lines.extend(components::issues(0, &issue_texts));
    }

    lines
}
