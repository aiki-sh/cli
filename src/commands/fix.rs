//! Fix command for creating followup tasks from review comments
//!
//! This module provides the `aiki fix` command which:
//! - Reads a task ID (from argument or stdin for piping)
//! - Checks the task for comments
//! - If no comments: succeeds with "approved" message (review passed)
//! - If comments found: creates followup task (agent creates subtasks from comments)
//! - Runs the followup task (default: completion, --async: async, --start: hand off)

use std::collections::HashMap;
use std::env;
use std::io::{self, BufRead, IsTerminal};
use std::path::Path;

use crate::agents::AgentType;
use crate::error::{AikiError, Result};
use crate::session::find_active_session;
use crate::tasks::runner::{task_run, task_run_async, TaskRunOptions};
use crate::tasks::md::MdBuilder;
use crate::tasks::{
    find_task, get_current_scope_set, get_in_progress,
    get_ready_queue_for_scope_set, materialize_graph, materialize_graph_with_ids,
    read_events, read_events_with_ids, reassign_task,
    reopen_if_closed, start_task_core, write_link_event, write_link_event_with_autorun,
    Task, TaskComment,
};

/// Run the fix command
///
/// Creates followup tasks from review comments and runs them.
pub fn run(
    task_id: Option<String>,
    run_async: bool,
    start: bool,
    template_name: Option<String>,
    agent: Option<String>,
    autorun: bool,
    once: bool,
) -> Result<()> {
    let cwd = env::current_dir().map_err(|_| {
        AikiError::InvalidArgument("Failed to get current directory".to_string())
    })?;

    // Get task ID from argument or stdin
    let task_id = match task_id {
        Some(id) => extract_task_id(&id),
        None => read_task_id_from_stdin()?,
    };

    run_fix(&cwd, &task_id, run_async, start, template_name, agent, autorun, once)
}

/// Extract task ID from input, handling XML output format
fn extract_task_id(input: &str) -> String {
    let trimmed = input.trim();

    // Try to extract from XML task_id attribute
    if let Some(start) = trimmed.find("task_id=\"") {
        let after_quote = &trimmed[start + 9..];
        if let Some(end) = after_quote.find('"') {
            return after_quote[..end].to_string();
        }
    }

    trimmed.to_string()
}

/// Read task ID from stdin
fn read_task_id_from_stdin() -> Result<String> {
    let stdin = io::stdin();
    let mut input = String::new();

    for line in stdin.lock().lines() {
        let line = line.map_err(|e| {
            AikiError::InvalidArgument(format!("Failed to read from stdin: {}", e))
        })?;
        input.push_str(&line);
        input.push('\n');
    }

    if input.trim().is_empty() {
        return Err(AikiError::InvalidArgument(
            "No task ID provided. Pass as argument or pipe from another command.".to_string(),
        ));
    }

    Ok(extract_task_id(&input))
}

use super::review::{ReviewScope, ReviewScopeKind};

/// Check if a JJ change ID has unresolved conflicts.
fn has_jj_conflicts(cwd: &Path, change_id: &str) -> bool {
    let output = crate::jj::jj_cmd()
        .current_dir(cwd)
        .args(["resolve", "--list", "-r", change_id, "--ignore-working-copy"])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            !String::from_utf8_lossy(&out.stdout).trim().is_empty()
        }
        _ => false, // Not a valid change ID or no conflicts
    }
}

/// Handle conflict fix: create a merge-conflict task from a JJ change ID.
fn handle_conflict_fix(
    cwd: &Path,
    conflict_id: &str,
    run_async: bool,
    start: bool,
    agent: Option<String>,
) -> Result<()> {
    use super::task::{create_from_template, TemplateTaskParams};

    let mut data = HashMap::new();
    data.insert("conflict_id".to_string(), conflict_id.to_string());

    let params = TemplateTaskParams {
        template_name: "aiki/fix/merge-conflict".to_string(),
        data,
        sources: vec![format!("conflict:{}", conflict_id)],
        assignee: agent,
        ..Default::default()
    };

    let task_id = create_from_template(cwd, params)?;

    // Re-read tasks to include newly created task
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let tasks = &graph.tasks;
    let scope_set = get_current_scope_set(&graph);
    let in_progress: Vec<&Task> = get_in_progress(tasks).into_iter().collect();
    let ready = get_ready_queue_for_scope_set(&graph, &scope_set);

    if start {
        if let Some(session) = find_active_session(cwd) {
            reassign_task(cwd, &task_id, session.agent_type.as_str())?;
        }
        start_task_core(cwd, &[task_id.clone()])?;
        output_conflict_fix_started(&task_id, conflict_id, &in_progress, &ready)?;
    } else if run_async {
        let options = TaskRunOptions::new();
        task_run_async(cwd, &task_id, options)?;
        output_conflict_fix_async(&task_id, conflict_id)?;
        if !std::io::stdout().is_terminal() {
            println!("{}", task_id);
        }
    } else {
        let options = TaskRunOptions::new();
        task_run(cwd, &task_id, options)?;
        output_conflict_fix_completed(&task_id, conflict_id)?;
        if !std::io::stdout().is_terminal() {
            println!("{}", task_id);
        }
    }

    Ok(())
}

/// Output conflict fix started message
fn output_conflict_fix_started(
    task_id: &str,
    conflict_id: &str,
    in_progress: &[&Task],
    ready: &[&Task],
) -> Result<()> {
    use super::output::{CommandOutput, format_command_output};
    let status = format!("Created merge-conflict resolution task for conflict {}.", conflict_id);
    let output = CommandOutput {
        heading: "Conflict Fix",
        task_id,
        scope: None,
        status: &status,
        issues: None,
        hint: None,
    };
    let content = format_command_output(&output);
    let md = MdBuilder::new("fix").build(&content, in_progress, ready);
    eprintln!("{}", md);

    if !std::io::stdout().is_terminal() {
        println!("{}", task_id);
    }

    Ok(())
}

/// Output conflict fix async message
fn output_conflict_fix_async(task_id: &str, conflict_id: &str) -> Result<()> {
    use super::output::{CommandOutput, format_command_output};
    let status = format!("Merge-conflict resolution for {} started in background.", conflict_id);
    let output = CommandOutput {
        heading: "Conflict Fix Started",
        task_id,
        scope: None,
        status: &status,
        issues: None,
        hint: None,
    };
    let content = format_command_output(&output);
    let md = MdBuilder::new("fix").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

/// Output conflict fix completed message
fn output_conflict_fix_completed(task_id: &str, conflict_id: &str) -> Result<()> {
    use super::output::{CommandOutput, format_command_output};
    let status = format!("Merge-conflict resolution for {} completed.", conflict_id);
    let output = CommandOutput {
        heading: "Conflict Fix Completed",
        task_id,
        scope: None,
        status: &status,
        issues: None,
        hint: None,
    };
    let content = format_command_output(&output);
    let md = MdBuilder::new("fix").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

/// Core fix implementation
fn run_fix(
    cwd: &Path,
    task_id: &str,
    run_async: bool,
    start: bool,
    template_name: Option<String>,
    agent: Option<String>,
    autorun: bool,
    once: bool,
) -> Result<()> {
    // 1. Check if ID has JJ conflicts: jj resolve --list -r {id}
    if has_jj_conflicts(cwd, task_id) {
        return handle_conflict_fix(cwd, task_id, run_async, start, agent);
    }

    // 2. Fall back to existing review task logic
    // Parse agent if provided
    let agent_type = if let Some(ref agent_str) = agent {
        Some(
            AgentType::from_str(agent_str)
                .ok_or_else(|| AikiError::UnknownAgentType(agent_str.clone()))?,
        )
    } else {
        None
    };

    // Load tasks with change IDs (needed for comment IDs)
    let events_with_ids = read_events_with_ids(cwd)?;
    let tasks = materialize_graph_with_ids(&events_with_ids).tasks;

    // Find the review task (the task we're creating followups for)
    let review_task = find_task(&tasks, task_id)?;

    // Validate that the input task is actually a review task
    // Check task_type first, then fall back to template name for backward compatibility
    // (older review tasks may not have task_type set)
    if !is_review_task(review_task) {
        return Err(AikiError::InvalidArgument(format!(
            "No conflict or review task found for ID: {}",
            task_id
        )));
    }

    // Check for structured issues first, fall back to comment count for older reviews
    let has_issues = if let Some(issues_found) = review_task.data.get("issues_found") {
        // Structured review: use data.issues_found
        // On parse failure (malformed value), fall back to counting issue comments
        // rather than assuming 0 — avoids incorrectly approving when issues exist
        match issues_found.parse::<usize>() {
            Ok(n) => n > 0,
            Err(_) => !super::review::get_issue_comments(review_task).is_empty(),
        }
    } else {
        // Backward compatibility: older reviews without data.issues_found
        // Treat all comments as issues (existing behavior)
        !review_task.comments.is_empty()
    };

    if !has_issues {
        output_approved(task_id)?;
        return Ok(());
    }

    // Get issue comments to create fix subtasks
    let comments: Vec<TaskComment> = if review_task.data.contains_key("issues_found") {
        // Structured: use get_issue_comments()
        super::review::get_issue_comments(review_task)
            .into_iter()
            .cloned()
            .collect()
    } else {
        // Backward compat: all comments are issues
        review_task.comments.clone()
    };

    // Determine what was reviewed from typed scope data
    let scope = ReviewScope::from_data(&review_task.data)?;

    let followup_id = match scope.kind {
        ReviewScopeKind::Task => {
            // Fix targets a task — add fix subtask to the original task
            let original_task = find_task(&tasks, &scope.id)?;
            let assignee = determine_followup_assignee(agent_type, Some(original_task));
            let template = template_name.as_deref().unwrap_or("aiki/fix");
            create_fix_task(cwd, review_task, &scope, Some(original_task), &assignee, template, autorun, once)?
        }
        ReviewScopeKind::Plan | ReviewScopeKind::Code => {
            // Fix targets a file — create standalone fix task (no parent)
            let assignee = determine_followup_assignee(agent_type, None);
            let template = template_name.as_deref().unwrap_or("aiki/fix");
            create_fix_task(cwd, review_task, &scope, None, &assignee, template, autorun, once)?
        }
        ReviewScopeKind::Session => {
            return Err(AikiError::InvalidArgument(
                "Fixing session reviews is not yet supported. Only task-targeted reviews can be fixed.".to_string(),
            ));
        }
    };

    // Re-read tasks to include newly created followup task
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let tasks = &graph.tasks;
    let scope_set = get_current_scope_set(&graph);
    let in_progress: Vec<&Task> = get_in_progress(tasks).into_iter().collect();
    let ready = get_ready_queue_for_scope_set(&graph, &scope_set);

    // Handle execution mode
    if start {
        // Reassign task to current agent (caller takes over)
        if let Some(session) = find_active_session(cwd) {
            reassign_task(cwd, &followup_id, session.agent_type.as_str())?;
        }
        // Start task using core logic (validates, auto-stops, emits events)
        start_task_core(cwd, &[followup_id.clone()])?;
        output_followup_started(&followup_id, &scope, &comments, &in_progress, &ready)?;
    } else if run_async {
        // Run async and return immediately
        let options = TaskRunOptions::new();
        task_run_async(cwd, &followup_id, options)?;
        output_followup_async(&followup_id, &scope, &comments)?;
        // Output task ID to stdout if piped
        if !std::io::stdout().is_terminal() {
            println!("{}", followup_id);
        }
    } else {
        // Run to completion (default)
        let options = TaskRunOptions::new();
        task_run(cwd, &followup_id, options)?;
        output_followup_completed(&followup_id, &scope, &comments)?;
        // Output task ID to stdout if piped
        if !std::io::stdout().is_terminal() {
            println!("{}", followup_id);
        }
    }

    Ok(())
}

/// Output approved message when no issues found
fn output_approved(task_id: &str) -> Result<()> {
    use super::output::{CommandOutput, format_command_output};
    let output = CommandOutput {
        heading: "Approved",
        task_id,
        scope: None,
        status: "Review approved - no issues found.",
        issues: None,
        hint: None,
    };
    let content = format_command_output(&output);
    let md = MdBuilder::new("fix").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

/// Describe the fix action based on scope
fn fix_description(scope: &ReviewScope) -> String {
    match scope.kind {
        ReviewScopeKind::Task => "Created fix followup subtask under original task".to_string(),
        _ => format!("Created standalone fix task for {}", scope.name()),
    }
}

/// Output followup started message
fn output_followup_started(
    followup_id: &str,
    scope: &ReviewScope,
    comments: &[TaskComment],
    in_progress: &[&Task],
    ready: &[&Task],
) -> Result<()> {
    use super::output::{CommandOutput, format_command_output};
    let status = format!("{} ({} issue(s)).", fix_description(scope), comments.len());
    let output = CommandOutput {
        heading: "Fix Followup",
        task_id: followup_id,
        scope: Some(scope),
        status: &status,
        issues: Some(comments),
        hint: None,
    };
    let content = format_command_output(&output);
    let md = MdBuilder::new("fix").build(&content, in_progress, ready);
    eprintln!("{}", md);

    // Output task ID to stdout if piped
    if !std::io::stdout().is_terminal() {
        println!("{}", followup_id);
    }

    Ok(())
}

/// Output followup async message (for --async mode)
fn output_followup_async(followup_id: &str, scope: &ReviewScope, comments: &[TaskComment]) -> Result<()> {
    use super::output::{CommandOutput, format_command_output};
    let status = format!("{} in background.", fix_description(scope));
    let output = CommandOutput {
        heading: "Fix Started",
        task_id: followup_id,
        scope: Some(scope),
        status: &status,
        issues: Some(comments),
        hint: None,
    };
    let content = format_command_output(&output);
    let md = MdBuilder::new("fix").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

/// Output followup completed message (for blocking mode)
fn output_followup_completed(followup_id: &str, scope: &ReviewScope, comments: &[TaskComment]) -> Result<()> {
    use super::output::{CommandOutput, format_command_output};
    let status = format!("{} completed.", fix_description(scope));
    let output = CommandOutput {
        heading: "Fix Completed",
        task_id: followup_id,
        scope: Some(scope),
        status: &status,
        issues: Some(comments),
        hint: None,
    };
    let content = format_command_output(&output);
    let md = MdBuilder::new("fix").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

/// Check if a task is a review task.
///
/// A task is considered a review task if:
/// 1. Its task_type is explicitly "review", OR
/// 2. It was created from a review template (template starts with "aiki/review")
///
/// The fallback to template name provides backward compatibility with review tasks
/// created before the template set the `type` field in frontmatter.
pub fn is_review_task(task: &Task) -> bool {
    if task.task_type.as_deref() == Some("review") {
        return true;
    }
    if let Some(ref template) = task.template {
        if template.starts_with("aiki/review") {
            return true;
        }
    }
    false
}

/// Determine assignee for followup task.
///
/// The followup should be assigned to whoever did the original work (the reviewed task's assignee),
/// not the opposite of the reviewer. The person who wrote the code should fix issues in their code.
fn determine_followup_assignee(agent_override: Option<AgentType>, reviewed_task: Option<&Task>) -> Option<String> {
    if let Some(agent) = agent_override {
        return Some(agent.as_str().to_string());
    }

    // Assign to whoever did the original work
    if let Some(task) = reviewed_task {
        if let Some(ref assignee) = task.assignee {
            return Some(assignee.clone());
        }
    }

    // Fallback to claude-code if we can't determine the original worker
    Some("claude-code".to_string())
}

/// Create a fix task, either as a subtask on the original task or standalone.
///
/// When `parent` is `Some`, the fix is added as a child of the original task
/// (e.g., X.1) and the original task is reopened if closed.
/// When `parent` is `None` (file-targeted reviews), a standalone task is created.
///
/// Scope data is always passed to the template so `{{data.scope.name}}` works for all scope kinds.
fn create_fix_task(
    cwd: &Path,
    review_task: &Task,
    scope: &ReviewScope,
    parent: Option<&Task>,
    assignee: &Option<String>,
    template_name: &str,
    autorun: bool,
    once: bool,
) -> Result<String> {
    use super::task::{create_from_template, TemplateTaskParams};

    if let Some(p) = parent {
        let events = read_events(cwd)?;
        let current_tasks = materialize_graph(&events).tasks;
        reopen_if_closed(cwd, &p.id, &current_tasks, "Subtasks added")?;
    }

    // Pass scope data to both `data` (persisted on task, {{data.scope.*}}) and
    // `source_data` (review task metadata, {{source.*}})
    let scope_data = scope.to_data();

    // Add options.once if flag is set
    let mut scope_data = scope_data;
    if once {
        scope_data.insert("options.once".to_string(), "true".to_string());
    }
    let mut source_data = HashMap::new();
    source_data.insert("name".to_string(), review_task.name.clone());
    source_data.insert("id".to_string(), review_task.id.clone());

    let params = TemplateTaskParams {
        template_name: template_name.to_string(),
        data: scope_data,
        sources: vec![format!("task:{}", review_task.id)],
        assignee: assignee.clone(),
        priority: parent.map(|p| p.priority),
        parent_id: parent.map(|p| p.id.clone()),
        parent_name: parent.map(|p| p.name.clone()),
        source_data,
        ..Default::default()
    };

    let task_id = create_from_template(cwd, params)?;

    // Emit remediates link: fix task remediates the review task
    // Autorun is opt-in only (--autorun flag); default is no autorun
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let autorun_opt = if autorun { Some(true) } else { None };
    write_link_event_with_autorun(cwd, &graph, "remediates", &task_id, &review_task.id, autorun_opt)?;

    // Emit fixes link to the target(s) that were reviewed (traverse validates from review task)
    let reviewed_targets = graph.edges.targets(&review_task.id, "validates");
    for target in reviewed_targets {
        write_link_event(cwd, &graph, "fixes", &task_id, target)?;
    }

    Ok(task_id)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{TaskPriority, TaskStatus};
    use std::collections::HashMap;

    #[test]
    fn test_extract_task_id_plain() {
        assert_eq!(extract_task_id("xqrmnpst"), "xqrmnpst");
        assert_eq!(extract_task_id("  xqrmnpst  "), "xqrmnpst");
    }

    #[test]
    fn test_extract_task_id_xml() {
        let xml = r#"<aiki_review cmd="review" status="ok">
  <completed task_id="xqrmnpst" comments="2"/>
</aiki_review>"#;
        assert_eq!(extract_task_id(xml), "xqrmnpst");
    }

    #[test]
    fn test_determine_followup_assignee_override() {
        let task = Task {
            id: "test".to_string(),
            name: "Test".to_string(),
            slug: None,
            task_type: None,
            status: TaskStatus::Open,
            priority: TaskPriority::P2,
            assignee: Some("codex".to_string()),
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data: HashMap::new(),
            created_at: chrono::Utc::now(),
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
        };

        // Override should take precedence
        let result = determine_followup_assignee(Some(AgentType::Codex), Some(&task));
        assert_eq!(result, Some("codex".to_string()));
    }

    #[test]
    fn test_determine_followup_assignee_from_reviewed_task() {
        // The reviewed task's assignee is who should fix the issues
        let reviewed_task = Task {
            id: "reviewed".to_string(),
            name: "Original Work".to_string(),
            slug: None,
            task_type: None,
            status: TaskStatus::Closed,
            priority: TaskPriority::P2,
            assignee: Some("claude-code".to_string()), // claude-code did the work
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data: HashMap::new(),
            created_at: chrono::Utc::now(),
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
        };

        // Should return the reviewed task's assignee (claude-code fixes their own work)
        let result = determine_followup_assignee(None, Some(&reviewed_task));
        assert_eq!(result, Some("claude-code".to_string()));
    }

    #[test]
    fn test_determine_followup_assignee_no_reviewed_task() {
        // If we can't find the reviewed task, fall back to claude-code
        let result = determine_followup_assignee(None, None);
        assert_eq!(result, Some("claude-code".to_string()));
    }

    fn make_test_task(id: &str) -> Task {
        Task {
            id: id.to_string(),
            name: format!("Task {}", id),
            slug: None,
            task_type: None,
            status: TaskStatus::Open,
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data: HashMap::new(),
            created_at: chrono::Utc::now(),
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
    fn test_review_scope_from_data_task() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Task,
            id: "original123".to_string(),
            task_ids: vec![],
        };
        let data = scope.to_data();
        let restored = ReviewScope::from_data(&data).unwrap();
        assert_eq!(restored.kind, ReviewScopeKind::Task);
        assert_eq!(restored.id, "original123");
    }

    #[test]
    fn test_review_scope_from_data_missing() {
        let data = HashMap::new();
        assert!(ReviewScope::from_data(&data).is_err());
    }

    #[test]
    fn test_is_review_task_by_type() {
        let mut task = make_test_task("t1");
        task.task_type = Some("review".to_string());
        assert!(is_review_task(&task));
    }

    #[test]
    fn test_is_review_task_by_template() {
        let mut task = make_test_task("t2");
        task.template = Some("aiki/review@1.0.0".to_string());
        assert!(is_review_task(&task));
    }

    #[test]
    fn test_is_review_task_neither() {
        let task = make_test_task("t3");
        assert!(!is_review_task(&task));
    }

    // fix_description tests

    #[test]
    fn test_fix_description_task_scope() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Task,
            id: "abc123".to_string(),
            task_ids: vec![],
        };
        assert_eq!(
            fix_description(&scope),
            "Created fix followup subtask under original task"
        );
    }

    #[test]
    fn test_fix_description_spec_scope() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Plan,
            id: "ops/now/feature.md".to_string(),
            task_ids: vec![],
        };
        assert_eq!(
            fix_description(&scope),
            "Created standalone fix task for Plan (feature.md)"
        );
    }

    #[test]
    fn test_fix_description_code_scope() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Code,
            id: "ops/now/feature.md".to_string(),
            task_ids: vec![],
        };
        assert_eq!(
            fix_description(&scope),
            "Created standalone fix task for Code (feature.md)"
        );
    }
}
