//! Markdown output generation for task commands
//!
//! Action commands (add, start, comment) return slim single-line confirmations.
//! State-transition commands (stop, close) return confirmation + context footer.
//! Read commands (list, show) return full context.

use super::types::Task;

/// Sentinel printed when no tasks remain (checked by agent prompts and hooks)
pub const DONE_MESSAGE: &str = "Done. No remaining tasks.";

/// Navigation hint shown in the context footer
const NAV_HINT: &str = "Run `aiki task` to view - OR - `aiki task start` to begin work.";

/// Return first 7 characters of a task ID as a short reference.
#[must_use]
pub fn short_id(id: &str) -> &str {
    &id[..7.min(id.len())]
}

/// Markdown builder for task command responses (used by read commands and errors)
pub struct MdBuilder;

impl MdBuilder {
    /// Create a new builder
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Build the full markdown response (content only, no context banner).
    #[must_use]
    pub fn build(&self, content: &str) -> String {
        let mut md = String::new();

        if !content.is_empty() {
            md.push_str(content);
            if !content.ends_with('\n') {
                md.push('\n');
            }
            md.push('\n');
        }

        md
    }

    /// Build an error response
    #[must_use]
    pub fn build_error(&self, message: &str) -> String {
        format!("Error: {}\n", message)
    }
}

/// Build the context section showing current state with short IDs.
///
/// Shows separate "In Progress" and "Ready" sections.
/// Omits "In Progress" when empty.
#[must_use]
pub fn build_context(in_progress: &[&Task], ready_queue: &[&Task]) -> String {
    let mut ctx = String::new();

    // In Progress section (omitted when empty)
    if !in_progress.is_empty() {
        ctx.push_str("In Progress:\n");
        for task in in_progress {
            let type_suffix = task
                .task_type
                .as_ref()
                .map(|t| format!(" [{}]", t))
                .unwrap_or_default();
            ctx.push_str(&format!(
                "[{}] {} — {}{}\n",
                task.priority,
                short_id(&task.id),
                task.name,
                type_suffix
            ));
        }
        ctx.push('\n');
    }

    // Ready section (always shown)
    let ready_count = ready_queue.len();
    ctx.push_str(&format!("Ready ({}):\n", ready_count));
    for task in ready_queue.iter().take(5) {
        let type_suffix = task
            .task_type
            .as_ref()
            .map(|t| format!(" [{}]", t))
            .unwrap_or_default();
        ctx.push_str(&format!(
            "[{}] {}  {}{}\n",
            task.priority,
            short_id(&task.id),
            task.name,
            type_suffix
        ));
    }

    ctx
}

/// Build the footer shown below context when tasks exist.
///
/// Returns: `---\nTasks (N ready)\nRun `aiki task start <id>` to begin work.\n`
#[must_use]
fn build_footer(
    in_progress: &[&Task],
    ready_queue: &[&Task],
    graph: Option<&super::graph::TaskGraph>,
) -> String {
    let hint = if let Some(first) = ready_queue.first() {
        // Ready tasks → start hint
        format!("Run `aiki task start {}` to begin.", short_id(&first.id))
    } else if let Some(parent) = in_progress.first() {
        // Nothing ready, something in-progress
        let all_subtasks_done = graph
            .map(|g| super::manager::all_subtasks_closed(g, &parent.id))
            .unwrap_or(false);
        if all_subtasks_done {
            // Parent with all subtasks done → close hint
            format!(
                "All subtasks done. Run `aiki task close {} --confidence <1-4> --summary \"...\"` to finish.",
                short_id(&parent.id)
            )
        } else if graph.map_or(false, |g| super::manager::has_subtasks(g, &parent.id)) {
            // Parent with subtasks still in progress → nav hint
            NAV_HINT.to_string()
        } else {
            // Leaf task in progress → close hint
            format!(
                "If done, run `aiki task close {} --confidence <1-4> --summary \"...\"` to finish.",
                short_id(&parent.id)
            )
        }
    } else {
        NAV_HINT.to_string()
    };
    let summary = match (in_progress.len(), ready_queue.len()) {
        (0, r) => format!("Tasks ({} ready)", r),
        (p, 0) => format!("Tasks ({} in progress)", p),
        (p, r) => format!("Tasks ({} in progress, {} ready)", p, r),
    };
    format!("---\n{}\n{}\n", summary, hint)
}

/// Build the task list output for read commands (`task` / `task list`).
///
/// Context sections + footer with nav hint when tasks exist.
/// Pass `graph` to enable subtask-aware hints in the footer.
#[must_use]
pub fn build_list_output(
    in_progress: &[&Task],
    ready_queue: &[&Task],
    graph: Option<&super::graph::TaskGraph>,
) -> String {
    let mut out = build_context(in_progress, ready_queue);
    if !in_progress.is_empty() || !ready_queue.is_empty() {
        out.push_str(&build_footer(in_progress, ready_queue, graph));
    } else {
        out.push_str(DONE_MESSAGE);
        out.push('\n');
    }
    out
}

/// Format a list of tasks for filtered list views
#[must_use]
pub fn format_task_list(tasks: &[&Task]) -> String {
    use super::types::TaskStatus;

    let mut md = format!("Tasks ({}):\n", tasks.len());

    for task in tasks {
        let mut line = format!("[{}] {}  {}", task.priority, short_id(&task.id), task.name);
        if let Some(ref task_type) = task.task_type {
            line.push_str(&format!(" [{}]", task_type));
        }
        if let Some(ref assignee) = task.assignee {
            line.push_str(&format!(" (assignee: {})", assignee));
        }
        if task.status == TaskStatus::Closed {
            if let Some(confidence) = task.confidence {
                line.push_str(&format!(" [c{}]", confidence.as_u8()));
            }
        }
        md.push_str(&line);
        md.push('\n');
        // Show summary for closed tasks
        if task.status == TaskStatus::Closed {
            if let Some(summary) = task.effective_summary() {
                md.push_str(&format!("  ↳ {}\n", summary));
            }
        }
    }

    md
}

/// Format action confirmation for `task add`
#[must_use]
pub fn format_action_added(task: &Task) -> String {
    format!(
        "Added {}\n---\nRun `aiki task start` to begin work\n",
        short_id(&task.id)
    )
}

/// Format action confirmation for `task start`
///
/// When `show_name` is true, appends ` — <name>` (useful when starting by ID).
/// When false, omits the name to avoid duplicating what the user just typed (quick-start).
/// Includes instructions section if present.
#[must_use]
pub fn format_action_started(
    task: &Task,
    show_name: bool,
    graph: Option<&super::graph::TaskGraph>,
) -> String {
    let header = if show_name {
        format!("Started {} — {}", short_id(&task.id), task.name)
    } else {
        format!("Started {}", short_id(&task.id))
    };
    let mut md = format!("{}\n", header);

    if let Some(ref instructions) = task.instructions {
        md.push('\n');
        md.push_str(&format_instructions(instructions));
    }

    if let Some(graph) = graph {
        let subtasks = super::manager::get_subtasks(graph, &task.id);
        if !subtasks.is_empty() {
            let total = subtasks.len();
            let completed = subtasks
                .iter()
                .filter(|t| t.status == super::types::TaskStatus::Closed)
                .count();
            let percentage = if total > 0 {
                (completed * 100) / total
            } else {
                0
            };
            md.push_str(&format!(
                "\nSubtasks ({}/{} — {}%):\n",
                completed, total, percentage
            ));
            for subtask in &subtasks {
                let check = match subtask.status {
                    super::types::TaskStatus::Closed => "[x]",
                    super::types::TaskStatus::InProgress => "[>]",
                    super::types::TaskStatus::Reserved => "[~]",
                    _ => "[ ]",
                };
                let label = if let Some(ref slug) = subtask.slug {
                    slug.clone()
                } else {
                    short_id(&subtask.id).to_string()
                };
                md.push_str(&format!("{} {} {}\n", check, label, subtask.name));
            }
        }
    }

    md
}

/// Format action confirmation when a parent task is auto-started after all subtasks complete.
///
/// Guides the agent to review the original instructions for completeness before closing.
/// Instructions (if present) are appended after the review guidance.
#[must_use]
pub fn format_action_parent_autostarted(task: &Task) -> String {
    let mut md = format!(
        "Started {} — {}\n\
         ---\n\
         All subtasks have been completed. Review the original instructions for completeness, \
         then close with a summary:\n\
         `aiki task close {} --summary \"...\"`\n",
        short_id(&task.id),
        task.name,
        short_id(&task.id),
    );

    if let Some(ref instructions) = task.instructions {
        md.push('\n');
        md.push_str(&format_instructions(instructions));
    }

    md
}

/// Format action confirmation for `task stop`.
///
/// Returns: `Stopped <short-id> — <name>\n`
#[must_use]
pub fn format_action_stopped(task: &Task, _reason: Option<&str>) -> String {
    format!("Stopped {} — {}\n", short_id(&task.id), task.name)
}

/// Format action confirmation for `task close`.
///
/// Returns: `Closed <short-id> — <name>\n`
#[must_use]
pub fn format_action_closed(task: &Task) -> String {
    format!("Closed {} — {}\n", short_id(&task.id), task.name)
}

/// Format the close summary line when multiple tasks were closed.
///
/// `total` is the number of tasks closed (explicit + cascade), `explicit` is
/// how many the user explicitly requested.
#[must_use]
pub fn format_close_summary(total: usize, explicit: usize) -> String {
    let subtask_count = total.saturating_sub(explicit);
    if subtask_count > 0 {
        format!(
            "Closed (including {} subtask{})\n",
            subtask_count,
            if subtask_count == 1 { "" } else { "s" }
        )
    } else {
        format!("Closed {} tasks\n", total)
    }
}

/// Format action confirmation for `task comment`
#[must_use]
pub fn format_action_commented() -> String {
    "Comment added.\n".to_string()
}

/// Format instructions as a markdown section.
#[must_use]
pub fn format_instructions(instructions: &str) -> String {
    format!("### Instructions\n{}\n", instructions)
}

/// Print aiki output to stdout.
pub fn aiki_print(s: &str) {
    print!("{}", s);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::types::{TaskPriority, TaskStatus};
    use chrono::Utc;

    fn make_task(id: &str, name: &str, priority: TaskPriority, status: TaskStatus) -> Task {
        Task {
            id: id.to_string(),
            name: name.to_string(),
            slug: None,
            task_type: None,
            status,
            priority,
            assignee: None,
            sources: Vec::new(),
            template: None,
            instructions: None,
            data: std::collections::HashMap::new(),
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

    #[test]
    fn test_short_id() {
        assert_eq!(short_id("abcdefghijklmnop"), "abcdefg");
        assert_eq!(short_id("abc"), "abc");
    }

    #[test]
    fn test_format_action_added() {
        let task = make_task(
            "abcdefghijklmnopqrstuvwxyzabcdef",
            "New task",
            TaskPriority::P2,
            TaskStatus::Open,
        );
        let md = format_action_added(&task);
        assert!(md.starts_with("Added abcdefg"));
        assert!(md.contains("Run `aiki task start`"));
    }

    #[test]
    fn test_format_action_started() {
        let task = make_task(
            "abcdefghijklmnopqrstuvwxyzabcdef",
            "Test task",
            TaskPriority::P2,
            TaskStatus::InProgress,
        );
        let md = format_action_started(&task, true, None);
        assert!(md.starts_with("Started abcdefg"));
        assert!(md.contains("Test task"));

        let md_no_name = format_action_started(&task, false, None);
        assert!(md_no_name.starts_with("Started abcdefg"));
        assert!(!md_no_name.contains("Test task"));
    }

    #[test]
    fn test_format_action_started_with_instructions() {
        let mut task = make_task(
            "abcdefghijklmnopqrstuvwxyzabcdef",
            "Test task",
            TaskPriority::P2,
            TaskStatus::InProgress,
        );
        task.instructions = Some("Do the thing".to_string());
        let md = format_action_started(&task, true, None);
        assert!(md.contains("Started abcdefg"));
        assert!(md.contains("### Instructions\nDo the thing\n"));
    }

    #[test]
    fn test_format_action_stopped() {
        let task = make_task(
            "abcdefghijklmnopqrstuvwxyzabcdef",
            "Test task",
            TaskPriority::P2,
            TaskStatus::Stopped,
        );
        let md = format_action_stopped(&task, None);
        assert!(md.starts_with("Stopped abcdefg"));
        assert!(md.contains("Test task"));
    }

    #[test]
    fn test_format_action_closed() {
        let task = make_task(
            "abcdefghijklmnopqrstuvwxyzabcdef",
            "Test task",
            TaskPriority::P2,
            TaskStatus::Closed,
        );
        let md = format_action_closed(&task);
        assert!(md.starts_with("Closed abcdefg"));
        assert!(md.contains("Test task"));
    }

    #[test]
    fn test_format_close_summary_one_parent_two_subtasks() {
        let result = format_close_summary(3, 1);
        assert!(result.contains("2 subtasks"), "got: {result}");
        assert!(!result.contains("3"), "should not show total count, got: {result}");
    }

    #[test]
    fn test_format_close_summary_one_parent_one_subtask() {
        let result = format_close_summary(2, 1);
        assert!(result.contains("1 subtask"), "got: {result}");
        assert!(!result.contains("subtasks"), "should be singular, got: {result}");
    }

    #[test]
    fn test_format_close_summary_multiple_explicit_no_subtasks() {
        let result = format_close_summary(3, 3);
        assert!(result.contains("3 tasks"), "got: {result}");
        assert!(!result.contains("subtask"), "no subtasks, got: {result}");
    }

    #[test]
    fn test_format_close_summary_multiple_explicit_with_subtasks() {
        let result = format_close_summary(5, 2);
        assert!(result.contains("3 subtasks"), "got: {result}");
    }

    #[test]
    fn test_format_action_commented() {
        assert_eq!(format_action_commented(), "Comment added.\n");
    }

    #[test]
    fn test_context_with_in_progress_and_ready() {
        let ip = make_task(
            "abcdefghijklmnopqrstuvwxyzabcdef",
            "Working",
            TaskPriority::P0,
            TaskStatus::InProgress,
        );
        let ready = make_task(
            "zyxwvutsrqponmlkjihgfedcbazyxwvu",
            "Ready task",
            TaskPriority::P2,
            TaskStatus::Open,
        );
        let md = build_context(&[&ip], &[&ready]);

        assert!(md.contains("In Progress:\n"));
        assert!(md.contains("[p0] abcdefg — Working"));
        assert!(md.contains("Ready (1):\n"));
        assert!(md.contains("[p2] zyxwvut  Ready task"));
    }

    #[test]
    fn test_context_ready_only() {
        let task = make_task(
            "abcdefghijklmnopqrstuvwxyzabcdef",
            "Test",
            TaskPriority::P2,
            TaskStatus::Open,
        );
        let md = build_context(&[], &[&task]);
        assert!(!md.contains("In Progress:"));
        assert!(md.contains("Ready (1):"));
        assert!(md.contains("[p2] abcdefg  Test"));
    }

    #[test]
    fn test_context_empty() {
        assert_eq!(build_context(&[], &[]), "Ready (0):\n");
    }

    #[test]
    fn test_list_output_with_tasks() {
        let task = make_task(
            "abcdefghijklmnopqrstuvwxyzabcdef",
            "Test",
            TaskPriority::P2,
            TaskStatus::Open,
        );
        let md = build_list_output(&[], &[&task], None);
        assert!(md.contains("Ready (1):"));
        assert!(md.contains("---\nTasks (1 ready)"));
        assert!(md.contains("aiki task start abcdefg"));
    }

    #[test]
    fn test_list_output_empty() {
        let md = build_list_output(&[], &[], None);
        assert!(md.contains(DONE_MESSAGE));
        assert!(!md.contains("---"));
    }

    #[test]
    fn test_builder_basic() {
        let md = MdBuilder::new().build("");
        // MdBuilder::build no longer includes context
        assert!(!md.contains("Ready"));
        assert!(!md.contains(NAV_HINT));
    }

    #[test]
    fn test_builder_error() {
        let md = MdBuilder::new().build_error("Task not found");
        assert!(md.contains("Error: Task not found"));
    }

    #[test]
    fn test_format_task_list() {
        let t1 = make_task(
            "abcdefghijklmnopqrstuvwxyzabcdef",
            "Task 1",
            TaskPriority::P0,
            TaskStatus::Open,
        );
        let t2 = make_task(
            "zyxwvutsrqponmlkjihgfedcbazyxwvu",
            "Task 2",
            TaskPriority::P2,
            TaskStatus::Open,
        );
        let md = format_task_list(&[&t1, &t2]);
        assert!(md.contains("[p0] abcdefg  Task 1"));
        assert!(md.contains("[p2] zyxwvut  Task 2"));
    }

    #[test]
    fn test_format_task_list_closed_with_summary() {
        let mut task = make_task(
            "abcdefghijklmnopqrstuvwxyzabcdef",
            "Fixed auth bug",
            TaskPriority::P2,
            TaskStatus::Closed,
        );
        task.summary = Some("Added null check before token access".to_string());
        let md = format_task_list(&[&task]);
        assert_eq!(
            md,
            "Tasks (1):\n[p2] abcdefg  Fixed auth bug\n  ↳ Added null check before token access\n"
        );
    }

    #[test]
    fn test_format_task_list_closed_with_comment_fallback() {
        use crate::tasks::types::TaskComment;
        use std::collections::HashMap;
        let mut task = make_task(
            "abcdefghijklmnopqrstuvwxyzabcdef",
            "Old task",
            TaskPriority::P2,
            TaskStatus::Closed,
        );
        task.comments.push(TaskComment {
            id: None,
            text: "Fallback comment summary".to_string(),
            timestamp: Utc::now(),
            data: HashMap::new(),
        });
        let md = format_task_list(&[&task]);
        assert!(md.contains("↳ Fallback comment summary"));
    }

    #[test]
    fn test_format_task_list_closed_confidence_badge_only_when_present() {
        let mut task = make_task(
            "abcdefghijklmnopqrstuvwxyzabcdef",
            "Done task",
            TaskPriority::P2,
            TaskStatus::Closed,
        );
        task.confidence = Some(crate::tasks::types::ConfidenceLevel::High);
        task.summary = Some("Verified formatting contract".to_string());
        let with_badge = format_task_list(&[&task]);
        assert_eq!(
            with_badge,
            "Tasks (1):\n[p2] abcdefg  Done task [c3]\n  ↳ Verified formatting contract\n"
        );

        task.confidence = None;
        let without_badge = format_task_list(&[&task]);
        assert_eq!(
            without_badge,
            "Tasks (1):\n[p2] abcdefg  Done task\n  ↳ Verified formatting contract\n"
        );
    }
}
