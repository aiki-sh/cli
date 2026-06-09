// cli/src/tui/screens/review_show.rs

use crate::tasks::TaskGraph;
use crate::tui::app::{Line, WindowState};
use crate::tui::components::{self, ChildLine};
use crate::tui::theme;

use super::helpers::extract_issues;

pub fn view(graph: &TaskGraph, review_id: &str, _window: &WindowState) -> Vec<Line> {
    let review = match graph.tasks.get(review_id) {
        Some(t) => t,
        None => return vec![],
    };
    let issues = extract_issues(review);
    let mut lines = vec![];

    let status_child = if !issues.is_empty() {
        ChildLine::normal(
            &format!("Found {} issues", issues.len()),
            review.elapsed_str(),
        )
    } else {
        ChildLine::done(
            &format!("{} approved", theme::SYM_CHECK),
            review.elapsed_str(),
        )
    };
    lines.extend(components::phase(
        0,
        "review",
        review.agent_label(),
        false,
        vec![status_child],
    ));

    if !issues.is_empty() {
        lines.extend(components::blank());
        let texts: Vec<String> = issues.iter().map(|i| i.title.clone()).collect();
        lines.extend(components::issues(1, &texts));
    }

    lines
}
