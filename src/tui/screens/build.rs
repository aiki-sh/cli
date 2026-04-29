//! Build screen view — the most complex view function.
//!
//! Phases: plan → section header → decompose → subtask table → loop →
//! (review → fix iterations) → summary.

use chrono::{DateTime, Utc};
use std::collections::HashMap;

use crate::tasks::lanes::{derive_lanes as derive_lane_decomposition, lane_status, LaneStatus};
use crate::tasks::types::TaskStatus;
use crate::tasks::TaskGraph;
use crate::tui::app::{Line, LineStyle, WindowState};
use crate::tui::components::{self, ChildLine, LaneData, SubtaskData};
use crate::tui::screens::fix;
use crate::tui::screens::helpers::{
    current_iteration, decompose_in_progress, extract_issues, find_build_review,
    find_decompose_task, find_fix_parent, get_work_subtasks, is_build_complete, review_id_for_epic,
    review_phase_lines,
};
use crate::tui::theme;

/// Max quality iterations — mirrors the constant in commands/fix.rs.
const MAX_QUALITY_ITERATIONS: u16 = 10;

// ── Main view ────────────────────────────────────────────────────────

pub fn view(graph: &TaskGraph, epic_id: &str, plan_path: &str, _window: &WindowState) -> Vec<Line> {
    let mut lines = vec![];
    let mut group: u16 = 0;

    // 1. Plan phase (always "done" — plan file exists before build starts)
    lines.extend(components::phase(
        group,
        "plan",
        Some("claude"),
        false,
        vec![ChildLine::done(
            &format!("{} {}", theme::SYM_CHECK, plan_path),
            None,
        )],
    ));
    group += 1;

    // 2. Section header
    lines.extend(components::section_header(group, "Initial Build"));

    // 3. Decompose phase
    if let Some(decompose) = find_decompose_task(graph, epic_id) {
        let active = !decompose.is_terminal();
        let children = match decompose.status {
            TaskStatus::Open => vec![ChildLine::active("Reading task graph...")],
            TaskStatus::InProgress => vec![ChildLine::active_with_elapsed(
                decompose.latest_heartbeat(),
                decompose.elapsed_str(),
            )],
            TaskStatus::Closed => {
                let subtask_count = get_work_subtasks(graph, epic_id).len();
                vec![ChildLine::normal(
                    &format!("{} subtasks created", subtask_count),
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

    // 4. Subtask table (show when subtasks exist or decompose is still running)
    let subtasks = get_work_subtasks(graph, epic_id);
    if !subtasks.is_empty() || decompose_in_progress(graph, epic_id) {
        let data: Vec<SubtaskData> = subtasks.iter().map(|s| s.into()).collect();
        let loading = subtasks.is_empty(); // shows "..." placeholder
        let epic = &graph.tasks[epic_id];
        lines.extend(components::subtask_table(
            group,
            epic.short_id(),
            &epic.name,
            &data,
            loading,
        ));
    }

    // 5. Loop phase (when lanes have been assigned)
    if let Some(lane_data) = derive_lanes(graph, epic_id) {
        lines.extend(components::loop_block(group + 1, &lane_data));
        group += 2;
    }

    // 6. Review phase (graph-driven — appears when review subtask exists)
    if let Some(review) = find_build_review(graph, epic_id) {
        lines.extend(review_phase_lines(group, review, graph));
        group += 1;
    }

    // 7. Fix iterations (graph-driven — appears when fix subtasks exist)
    let review_id = review_id_for_epic(graph, epic_id);
    for iteration in 2..=current_iteration(graph, epic_id) {
        lines.extend(components::section_header(
            group,
            &format!("Iteration {}", iteration),
        ));
        if let Some(fix_parent) = find_fix_parent(graph, epic_id, iteration) {
            lines.extend(fix::view(graph, &fix_parent, &review_id, _window));
        }
    }

    // 8. Summary (when build is complete)
    if is_build_complete(graph, epic_id) {
        lines.extend(build_summary_lines(graph, epic_id, plan_path, group));
    }

    lines
}

// ── Lane derivation ──────────────────────────────────────────────────

/// Derive lanes from the task graph and convert to `LaneData` for rendering.
/// Returns `None` if no subtasks have sessions assigned.
fn derive_lanes(graph: &TaskGraph, parent_id: &str) -> Option<Vec<LaneData>> {
    let decomposition = derive_lane_decomposition(graph, parent_id);
    if decomposition.lanes.is_empty() {
        return None;
    }

    // Check if any lane has tasks with sessions assigned
    let has_sessions = decomposition.lanes.iter().any(|lane| {
        lane.threads.iter().any(|session| {
            session.task_ids.iter().any(|tid| {
                graph
                    .tasks
                    .get(tid)
                    .and_then(|t| t.claimed_by_session.as_ref())
                    .is_some()
            })
        })
    });
    if !has_sessions {
        return None;
    }

    let lanes: Vec<LaneData> = decomposition
        .lanes
        .iter()
        .enumerate()
        .map(|(i, lane)| {
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

            let status = lane_status(lane, graph, &decomposition.lanes);
            let shutdown = matches!(status, LaneStatus::Complete | LaneStatus::Failed);

            if agent.is_empty() {
                agent = "agent".to_string();
            }

            LaneData {
                number: i + 1,
                agent,
                completed,
                total,
                failed,
                heartbeat,
                elapsed,
                shutdown,
            }
        })
        .collect();

    Some(lanes)
}

// ── Build summary ────────────────────────────────────────────────────

fn build_summary_lines(graph: &TaskGraph, epic_id: &str, plan_path: &str, group: u16) -> Vec<Line> {
    let mut lines = vec![];
    let mut children = vec![];

    // Max iterations warning (if applicable)
    if let Some(warning) = max_iterations_warning(graph, epic_id) {
        children.push(ChildLine::warning(&warning));
    }

    // Per-agent breakdown: aggregate sessions, time, and tokens by agent type.
    // Only show per-agent lines when multiple agent types were used.
    let epic_closed_at = graph.tasks.get(epic_id).and_then(|t| t.closed_at);
    let agent_stats = aggregate_agent_stats(graph, epic_id, epic_closed_at);
    if agent_stats.len() > 1 {
        for stat in &agent_stats {
            children.push(ChildLine::normal(
                &format!(
                    "{}: {} session{} \u{2014} {} \u{2014} {} tokens",
                    stat.agent,
                    stat.sessions,
                    if stat.sessions == 1 { "" } else { "s" },
                    stat.elapsed,
                    stat.tokens,
                ),
                None,
            ));
        }
    }

    // Total line (always present, bold)
    let totals = sum_agent_stats(&agent_stats);
    children.push(ChildLine::bold(&format!(
        "Total: {} session{} \u{2014} {} \u{2014} {} tokens",
        totals.sessions,
        if totals.sessions == 1 { "" } else { "s" },
        totals.elapsed,
        totals.tokens,
    )));

    lines.extend(components::separator(group));
    lines.extend(components::blank());
    lines.extend(components::phase(
        group,
        &format!("build completed \u{2014} {}", plan_path),
        None,
        false,
        children,
    ));
    lines.extend(components::blank());

    // Hint — context-dependent next action.
    // #1: No review → suggest `aiki review`
    // #2: Review has issues, no fix attempted → suggest `aiki fix`
    // #3: Review has no issues → suggest `aiki tldr`
    // #4: Fix resolved all issues → suggest `aiki tldr`
    // #5: Fix attempted, issues remain → suggest `aiki fix --pair`
    let epic = &graph.tasks[epic_id];
    let hint_text = match find_build_review(graph, epic_id) {
        None => {
            // #1: No review step was run
            format!("Run `aiki review {}` to review.", epic.short_id())
        }
        Some(review) => {
            let issues = extract_issues(review);
            if issues.is_empty() {
                // #3 or #4: No issues
                format!("Run `aiki tldr {}` for a walkthrough.", epic.short_id())
            } else {
                let fix_attempted = !graph.edges.referrers(&review.id, "remediates").is_empty();
                if fix_attempted {
                    // #5: Fix was attempted, issues remain
                    format!(
                        "Run `aiki fix {} --pair` to interactively remediate {} remaining issue{}.",
                        review.short_id(),
                        issues.len(),
                        if issues.len() == 1 { "" } else { "s" },
                    )
                } else {
                    // #2: Review found issues, no fix yet
                    format!(
                        "Run `aiki fix {}` to remediate {} issue{}.",
                        review.short_id(),
                        issues.len(),
                        if issues.len() == 1 { "" } else { "s" },
                    )
                }
            }
        }
    };
    lines.push(Line {
        indent: 0,
        text: hint_text,
        meta: None,
        style: LineStyle::Dim,
        group,
        dimmed: false,
    });
    lines.extend(components::blank());

    lines
}

// ── Stats helpers ────────────────────────────────────────────────────

struct AgentStat {
    agent: String,
    sessions: usize,
    total_secs: i64,
    total_tokens: u64,
    elapsed: String,
    tokens: String,
}

/// Aggregate sessions, time, and tokens per agent type from epic children.
/// `epic_closed_at` caps elapsed time for non-terminal tasks (abandoned in-progress).
fn aggregate_agent_stats(
    graph: &TaskGraph,
    epic_id: &str,
    epic_closed_at: Option<DateTime<Utc>>,
) -> Vec<AgentStat> {
    let children = graph.children_of(epic_id);
    let mut by_agent: HashMap<String, (usize, i64, u64)> = HashMap::new(); // (sessions, seconds, tokens)

    for task in &children {
        let agent = task.agent_label().unwrap_or("agent").to_string();

        let entry = by_agent.entry(agent).or_insert((0, 0, 0));
        entry.0 += 1; // sessions

        // Elapsed seconds: use closed_at if available, otherwise cap at epic's
        // close time (for abandoned in-progress tasks), falling back to now.
        if let Some(started) = task.started_at {
            let end = task
                .closed_at
                .or(epic_closed_at)
                .unwrap_or_else(chrono::Utc::now);
            let secs = (end - started).num_seconds().max(0);
            entry.1 += secs;
        }

        // Tokens from data field
        if let Some(tok_str) = task.data.get("tokens") {
            if let Ok(tok) = tok_str.parse::<u64>() {
                entry.2 += tok;
            }
        }
    }

    let mut stats: Vec<AgentStat> = by_agent
        .into_iter()
        .map(|(agent, (sessions, secs, tokens))| AgentStat {
            agent,
            sessions,
            total_secs: secs,
            total_tokens: tokens,
            elapsed: format_duration(secs),
            tokens: format_tokens(tokens),
        })
        .collect();
    stats.sort_by(|a, b| a.agent.cmp(&b.agent));
    stats
}

/// Sum all agent stats into a single total.
fn sum_agent_stats(stats: &[AgentStat]) -> AgentStat {
    let mut total_sessions = 0usize;
    let mut total_secs = 0i64;
    let mut total_tokens = 0u64;

    for stat in stats {
        total_sessions += stat.sessions;
        total_secs += stat.total_secs;
        total_tokens += stat.total_tokens;
    }

    AgentStat {
        agent: "Total".to_string(),
        sessions: total_sessions,
        total_secs,
        total_tokens,
        elapsed: format_duration(total_secs),
        tokens: format_tokens(total_tokens),
    }
}

/// Warning if max iterations reached with remaining issues.
fn max_iterations_warning(graph: &TaskGraph, epic_id: &str) -> Option<String> {
    let iteration = current_iteration(graph, epic_id);
    if iteration < MAX_QUALITY_ITERATIONS {
        return None;
    }

    // Check if the latest review still has issues
    let review_id = review_id_for_epic(graph, epic_id);
    if review_id.is_empty() {
        return None;
    }
    if let Some(review) = graph.tasks.get(&review_id) {
        let issues = extract_issues(review);
        if !issues.is_empty() {
            return Some(format!(
                "Max iterations reached \u{2014} {} issues remain",
                issues.len()
            ));
        }
    }
    None
}

// ── Formatting utilities ─────────────────────────────────────────────

fn format_duration(secs: i64) -> String {
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

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{}", tokens)
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::graph::EdgeStore;
    use crate::tasks::types::{FastHashMap, TaskOutcome, TaskPriority, TaskStatus};
    use crate::tasks::Task;
    use chrono::Utc;
    use std::collections::HashMap;

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
    fn view_plan_loaded_only() {
        let mut graph = make_graph();
        let epic = make_task("epic1x", "Epic: Build app", TaskStatus::InProgress);
        graph.tasks.insert("epic1x".to_string(), epic);

        let window = WindowState::new(80);
        let lines = view(&graph, "epic1x", "ops/now/plan.md", &window);

        // Should have plan phase header + plan done child
        assert!(lines.len() >= 2);
        assert!(lines[0].text.contains("plan"));
        assert!(lines[1].text.contains("plan.md"));

        // Should have section header
        assert!(lines.iter().any(|l| l.text.contains("Initial Build")));
    }

    #[test]
    fn view_with_decompose_active() {
        let mut graph = make_graph();
        let epic = make_task("epic1x", "Epic: Build app", TaskStatus::InProgress);
        let mut decompose = make_task("decomp1", "Decompose plan", TaskStatus::InProgress);
        decompose.task_type = Some("decompose".to_string());
        decompose.started_at = Some(Utc::now());

        graph.tasks.insert("epic1x".to_string(), epic);
        graph.tasks.insert("decomp1".to_string(), decompose);
        graph.edges.add("decomp1", "epic1x", "subtask-of");

        let window = WindowState::new(80);
        let lines = view(&graph, "epic1x", "ops/now/plan.md", &window);

        assert!(lines.iter().any(|l| l.text.contains("decompose")));
    }

    #[test]
    fn view_build_complete_shows_summary() {
        let mut graph = make_graph();
        let mut epic = make_task("epic1x", "Epic: Build app", TaskStatus::Closed);
        epic.started_at = Some(Utc::now() - chrono::Duration::minutes(5));
        epic.closed_at = Some(Utc::now());

        graph.tasks.insert("epic1x".to_string(), epic);

        let window = WindowState::new(80);
        let lines = view(&graph, "epic1x", "ops/now/plan.md", &window);

        // Should contain build completed summary
        assert!(lines.iter().any(|l| l.text.contains("build completed")));
        // Should contain hint (no review → suggest review)
        assert!(lines.iter().any(|l| l.text.contains("aiki review")));
    }

    #[test]
    fn format_duration_various() {
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(90), "1m 30s");
        assert_eq!(format_duration(3661), "1h01m");
    }

    #[test]
    fn format_tokens_various() {
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1500), "1.5K");
        assert_eq!(format_tokens(2_500_000), "2.5M");
    }

    #[test]
    fn sum_agent_stats_exact_totals() {
        let stats = vec![
            AgentStat {
                agent: "opus".to_string(),
                sessions: 3,
                total_secs: 3661,
                total_tokens: 1_500_123,
                elapsed: format_duration(3661),
                tokens: format_tokens(1_500_123),
            },
            AgentStat {
                agent: "sonnet".to_string(),
                sessions: 2,
                total_secs: 90,
                total_tokens: 500_456,
                elapsed: format_duration(90),
                tokens: format_tokens(500_456),
            },
        ];

        let totals = sum_agent_stats(&stats);
        assert_eq!(totals.sessions, 5);
        assert_eq!(totals.total_secs, 3751);
        assert_eq!(totals.total_tokens, 2_000_579);
        assert_eq!(totals.agent, "Total");
        assert_eq!(totals.elapsed, format_duration(3751));
        assert_eq!(totals.tokens, format_tokens(2_000_579));
    }

    #[test]
    fn subtask_table_shown_during_decompose() {
        let mut graph = make_graph();
        let epic = make_task("epic1x", "Epic: Build app", TaskStatus::InProgress);
        let mut decompose = make_task("decomp1", "Decompose", TaskStatus::InProgress);
        decompose.task_type = Some("decompose".to_string());

        graph.tasks.insert("epic1x".to_string(), epic);
        graph.tasks.insert("decomp1".to_string(), decompose);
        graph.edges.add("decomp1", "epic1x", "subtask-of");

        let window = WindowState::new(80);
        let lines = view(&graph, "epic1x", "ops/now/plan.md", &window);

        // Subtask table should be present (decompose task shows as a subtask)
        assert!(lines.iter().any(|l| l.text.contains("epic1x")));
        // Decompose phase should be rendered
        assert!(lines.iter().any(|l| l.text.contains("decompose")));
    }

    #[test]
    fn max_iterations_warning_below_max() {
        let graph = make_graph();
        assert!(max_iterations_warning(&graph, "nonexistent").is_none());
    }

    #[test]
    fn derive_lanes_returns_none_for_empty() {
        let graph = make_graph();
        assert!(derive_lanes(&graph, "nonexistent").is_none());
    }
}
