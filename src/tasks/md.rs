//! Markdown output generation for task commands
//!
//! Action commands (add, start, comment) return slim single-line confirmations.
//! State-transition commands (stop, close) return confirmation + full context.
//! Read commands (list, show) return full context.

use super::types::Task;

/// Navigation hint shown in the context footer
const NAV_HINT: &str = "Run `aiki task` to view - OR - `aiki task start` to begin work.";

/// Return first 7 characters of a task ID as a short reference.
#[must_use]
pub fn short_id(id: &str) -> &str {
    &id[..7.min(id.len())]
}

/// Markdown builder for task command responses (used by read commands and errors)
pub struct MdBuilder {
    cmd: String,
    is_error: bool,
    scopes: Vec<String>,
}

impl MdBuilder {
    /// Create a new builder for a command
    #[must_use]
    pub fn new(cmd: &str) -> Self {
        Self {
            cmd: cmd.to_string(),
            is_error: false,
            scopes: Vec::new(),
        }
    }

    /// Mark this response as an error
    #[must_use]
    pub fn error(mut self) -> Self {
        self.is_error = true;
        self
    }

    /// Set a single scope (parent task ID when working within a parent's subtasks)
    #[must_use]
    #[allow(dead_code)]
    pub fn with_scope(mut self, scope: &str) -> Self {
        self.scopes = vec![scope.to_string()];
        self
    }

    /// Set multiple scopes (when working on subtasks from multiple parents)
    #[must_use]
    pub fn with_scopes(mut self, scopes: &[String]) -> Self {
        self.scopes = scopes.to_vec();
        self
    }

    /// Build the full markdown response with context (no header/footer hint).
    ///
    /// Used by `task show` which doesn't need the navigation hint.
    #[must_use]
    pub fn build(&self, content: &str, in_progress: &[&Task], ready_queue: &[&Task]) -> String {
        let mut md = String::new();

        if !content.is_empty() {
            md.push_str(content);
            if !content.ends_with('\n') {
                md.push('\n');
            }
            md.push('\n');
        }

        md.push_str(&build_context(in_progress, ready_queue));

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
/// Returns: `---\nTasks (N ready)\n{NAV_HINT}\n`
#[must_use]
fn build_footer(ready_count: usize) -> String {
    format!("---\nTasks ({} ready)\n{}\n", ready_count, NAV_HINT)
}

/// Build the task list output for read commands (`task` / `task list`).
///
/// Context sections + footer with nav hint when tasks exist.
#[must_use]
pub fn build_list_output(in_progress: &[&Task], ready_queue: &[&Task]) -> String {
    let mut out = build_context(in_progress, ready_queue);
    if !in_progress.is_empty() || !ready_queue.is_empty() {
        out.push_str(&build_footer(ready_queue.len()));
    }
    out
}

/// Build the context block for state-transition commands (stop, close).
///
/// Returns `---\n` separator + context sections + footer, to be appended after the action line.
#[must_use]
pub fn build_transition_context(in_progress: &[&Task], ready_queue: &[&Task]) -> String {
    let mut out = String::from("---\n");
    out.push_str(&build_context(in_progress, ready_queue));
    out.push_str(&build_footer(ready_queue.len()));
    out
}

/// Format a task element for output
#[must_use]
#[allow(dead_code)]
pub fn format_task(task: &Task, _include_body: bool) -> String {
    let mut line = format!(
        "[{}] {}  {}",
        task.priority,
        short_id(&task.id),
        task.name
    );

    if let Some(ref task_type) = task.task_type {
        line.push_str(&format!(" [{}]", task_type));
    }

    if let Some(ref assignee) = task.assignee {
        line.push_str(&format!(" (assignee: {})", assignee));
    }

    line
}

/// Format a list of tasks for filtered list views
#[must_use]
pub fn format_task_list(tasks: &[&Task]) -> String {
    use super::types::TaskStatus;

    let mut md = format!("Tasks ({}):\n", tasks.len());

    for task in tasks {
        let mut line = format!(
            "[{}] {}  {}",
            task.priority,
            short_id(&task.id),
            task.name
        );
        if let Some(ref task_type) = task.task_type {
            line.push_str(&format!(" [{}]", task_type));
        }
        if let Some(ref assignee) = task.assignee {
            line.push_str(&format!(" (assignee: {})", assignee));
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
pub fn format_action_started(task: &Task, show_name: bool) -> String {
    let header = if show_name {
        format!("Started {} — {}", short_id(&task.id), task.name)
    } else {
        format!("Started {}", short_id(&task.id))
    };
    let mut md = format!(
        "{}\n---\nRun `aiki task comment` to leave updates as you go\n",
        header,
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
            working_copy: None,
            instructions: None,
            data: std::collections::HashMap::new(),
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
        let md = format_action_started(&task, true);
        assert!(md.starts_with("Started abcdefg"));
        assert!(md.contains("Test task"));
        assert!(md.contains("Run `aiki task comment`"));

        let md_no_name = format_action_started(&task, false);
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
        let md = format_action_started(&task, true);
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
        let md = build_list_output(&[], &[&task]);
        assert!(md.contains("Ready (1):"));
        assert!(md.contains("---\nTasks (1 ready)"));
        assert!(md.contains(NAV_HINT));
    }

    #[test]
    fn test_list_output_empty() {
        let md = build_list_output(&[], &[]);
        assert_eq!(md, "Ready (0):\n");
        assert!(!md.contains("---"));
    }

    #[test]
    fn test_transition_context() {
        let task = make_task(
            "abcdefghijklmnopqrstuvwxyzabcdef",
            "Test",
            TaskPriority::P2,
            TaskStatus::Open,
        );
        let md = build_transition_context(&[], &[&task]);
        assert!(md.contains("Ready (1):"));
        assert!(md.contains("---\nTasks (1 ready)"));
        assert!(md.contains(NAV_HINT));
    }

    #[test]
    fn test_close_plus_transition() {
        let task = make_task(
            "abcdefghijklmnopqrstuvwxyzabcdef",
            "Test",
            TaskPriority::P2,
            TaskStatus::Open,
        );
        let md = format!(
            "{}{}",
            format_action_closed(&task),
            build_transition_context(&[], &[&task])
        );
        assert!(md.contains("Closed abcdefg"));
        assert!(md.contains("Test"));
        assert!(md.contains("---\n"));
        assert!(md.contains("Ready (1):"));
    }

    #[test]
    fn test_builder_basic() {
        let task = make_task(
            "abcdefghijklmnopqrstuvwxyzabcdef",
            "Test task",
            TaskPriority::P2,
            TaskStatus::Open,
        );
        let md = MdBuilder::new("show").build("", &[], &[&task]);
        assert!(md.contains("Ready (1):"));
        assert!(md.contains("[p2] abcdefg  Test task"));
        // MdBuilder::build does NOT include nav hint
        assert!(!md.contains(NAV_HINT));
    }

    #[test]
    fn test_builder_error() {
        let md = MdBuilder::new("start").error().build_error("Task not found");
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
        assert!(md.contains("[p2] abcdefg  Fixed auth bug"));
        assert!(md.contains("↳ Added null check before token access"));
    }

    #[test]
    fn test_format_task_list_closed_with_comment_fallback() {
        use std::collections::HashMap;
        use crate::tasks::types::TaskComment;
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

}
