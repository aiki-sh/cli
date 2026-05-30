//! Explore command for creating and running exploration tasks
//!
//! This module provides the `aiki explore` command which:
//! - Creates an explore task from scope-specific templates
//! - Supports different scopes: plan, code, task, session
//! - Supports run modes: blocking (default), --async, --start

use std::env;
use std::io::IsTerminal;
use std::path::Path;

use crate::agents::AgentType;
use crate::commands::review::{detect_target, ReviewScope, ReviewScopeKind};
use crate::error::{AikiError, Result};
use crate::session::find_active_session;
use crate::tasks::md::MdBuilder;
use crate::tasks::runner::{task_run, task_run_async, TaskRunOptions};
use crate::tasks::templates::create_review_task_from_template;
use crate::tasks::{
    get_current_scope_set, get_in_progress, get_ready_queue_for_scope_set, materialize_graph,
    read_events, reassign_task, start_task_core, Task, TaskStatus,
};

/// Arguments for the explore command
#[derive(clap::Args)]
pub struct ExploreArgs {
    /// Target to explore: file path (.md), task ID (32 lowercase letters), or session UUID
    pub target: String,

    /// Explore the codebase implementation described in a plan (only with file targets)
    #[arg(long)]
    pub code: bool,

    /// Run explore asynchronously (return immediately)
    #[arg(long = "async")]
    pub run_async: bool,

    /// Start explore and return control to calling agent
    #[arg(long)]
    pub start: bool,

    /// Task template to use (default: aiki/explore)
    #[arg(long)]
    pub template: Option<String>,

    /// Agent for explore assignment
    #[arg(long)]
    pub agent: Option<String>,
}

/// Check if a string looks like a UUID (8-4-4-4-12 hex pattern)
fn looks_like_uuid(s: &str) -> bool {
    // Match new 8-char hex session IDs
    if s.len() == 8 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    // Match legacy full UUID format (8-4-4-4-12)
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return false;
    }
    let expected_lengths = [8, 4, 4, 4, 12];
    parts
        .iter()
        .zip(expected_lengths.iter())
        .all(|(part, &len)| part.len() == len && part.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Detect the explore target from the CLI argument and flags.
///
/// Extends the review `detect_target` logic with UUID detection for session scope.
fn detect_explore_target(
    cwd: &Path,
    target: &str,
    code: bool,
) -> Result<(ReviewScope, Option<String>)> {
    // Check for UUID format first (session scope)
    if looks_like_uuid(target) {
        if code {
            return Err(AikiError::InvalidArgument(
                "--code flag only applies to file targets".to_string(),
            ));
        }
        let session_agent = find_active_session(cwd)
            .map(|s| s.agent_type.as_str().to_string());

        // Collect closed task IDs for this session
        let session_id = target.to_string();
        let task_ids: Vec<String> = read_events(cwd)
            .map(|events| {
                let tasks = materialize_graph(&events).tasks;
                tasks
                    .values()
                    .filter(|t| {
                        t.status == TaskStatus::Closed
                            && t.last_session_id.as_deref() == Some(session_id.as_str())
                    })
                    .map(|t| t.id.clone())
                    .collect()
            })
            .unwrap_or_default();

        let scope = ReviewScope {
            kind: ReviewScopeKind::Session,
            id: session_id,
            task_ids,
        };
        return Ok((scope, session_agent));
    }

    // Fall back to review's detect_target for .md files and task IDs
    detect_target(cwd, Some(target), code)
}

/// Run the explore command
pub fn run(args: ExploreArgs) -> Result<()> {
    let cwd = env::current_dir()
        .map_err(|_| AikiError::InvalidArgument("Failed to get current directory".to_string()))?;

    // Parse agent if provided
    let agent_override = if let Some(ref agent_str) = args.agent {
        let agent_type = AgentType::from_str(agent_str)
            .ok_or_else(|| AikiError::UnknownAgentType(agent_str.clone()))?;
        Some(agent_type.as_str().to_string())
    } else {
        None
    };

    // Detect target and resolve scope
    let (scope, _worker) = detect_explore_target(&cwd, &args.target, args.code)?;

    // Determine assignee
    let assignee = agent_override
        .or_else(|| {
            find_active_session(&cwd).map(|s| s.agent_type.as_str().to_string())
        });

    // Create explore task from template
    // Route to scope-specific template: aiki/explore/{kind}
    let default_template = format!("aiki/explore/{}", scope.kind.as_str());
    let template = args.template.as_deref().unwrap_or(&default_template);
    let scope_data = scope.to_data();

    // Build sources for lineage
    let sources = match scope.kind {
        ReviewScopeKind::Task => vec![format!("task:{}", scope.id)],
        ReviewScopeKind::Plan | ReviewScopeKind::Code => {
            vec![format!("file:{}", scope.id)]
        }
        _ => vec![],
    };

    let explore_id = create_review_task_from_template(
        &cwd,
        &scope_data,
        &sources,
        &assignee,
        template,
    )?;

    // Re-read tasks to include newly created explore task
    let events = read_events(&cwd)?;
    let graph = materialize_graph(&events);
    let tasks = &graph.tasks;
    let scope_set = get_current_scope_set(&graph);
    let in_progress: Vec<&Task> = get_in_progress(tasks).into_iter().collect();
    let ready = get_ready_queue_for_scope_set(&graph, &scope_set);

    // Handle execution mode
    if args.start {
        // Reassign task to current agent (caller takes over)
        if let Some(session) = find_active_session(&cwd) {
            reassign_task(&cwd, &explore_id, session.agent_type.as_str())?;
        }
        // Start task
        start_task_core(&cwd, &[explore_id.clone()])?;
        output_explore_started(&explore_id, &scope, &in_progress, &ready)?;
    } else if args.run_async {
        let options = TaskRunOptions::new();
        task_run_async(&cwd, &explore_id, options)?;
        output_explore_async(&explore_id, &scope)?;
        if !std::io::stdout().is_terminal() {
            println!("{}", explore_id);
        }
    } else {
        // Run to completion (default)
        let options = TaskRunOptions::new();
        task_run(&cwd, &explore_id, options)?;
        output_explore_completed(&explore_id, &scope)?;
        if !std::io::stdout().is_terminal() {
            println!("{}", explore_id);
        }
    }

    Ok(())
}

/// Output explore started message (for --start mode)
fn output_explore_started(
    explore_id: &str,
    scope: &ReviewScope,
    in_progress: &[&Task],
    ready: &[&Task],
) -> Result<()> {
    use super::output::{format_command_output, CommandOutput};
    let output = CommandOutput {
        heading: "Explore Started",
        task_id: explore_id,
        scope: Some(scope),
        status: "Explore task started. You are now exploring.",
        issues: None,
        hint: None,
    };
    let content = format_command_output(&output);
    let md = MdBuilder::new("explore").build(&content, in_progress, ready);
    eprintln!("{}", md);

    if !std::io::stdout().is_terminal() {
        println!("{}", explore_id);
    }

    Ok(())
}

/// Output explore async message (for --async mode)
fn output_explore_async(explore_id: &str, scope: &ReviewScope) -> Result<()> {
    use super::output::{format_command_output, CommandOutput};
    let output = CommandOutput {
        heading: "Explore Started",
        task_id: explore_id,
        scope: Some(scope),
        status: "Explore started in background.",
        issues: None,
        hint: None,
    };
    let content = format_command_output(&output);
    let md = MdBuilder::new("explore").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

/// Output explore completed message (for blocking mode)
fn output_explore_completed(explore_id: &str, scope: &ReviewScope) -> Result<()> {
    use super::output::{format_command_output, CommandOutput};
    let output = CommandOutput {
        heading: "Explore Completed",
        task_id: explore_id,
        scope: Some(scope),
        status: "Explore completed.",
        issues: None,
        hint: None,
    };
    let content = format_command_output(&output);
    let md = MdBuilder::new("explore").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_looks_like_uuid_valid() {
        assert!(looks_like_uuid("6ba7b810-9dad-11d1-80b4-00c04fd430c8"));
        assert!(looks_like_uuid("550e8400-e29b-41d4-a716-446655440000"));
        assert!(looks_like_uuid("00000000-0000-0000-0000-000000000000"));
    }

    #[test]
    fn test_looks_like_uuid_invalid() {
        assert!(!looks_like_uuid(""));
        assert!(!looks_like_uuid("not-a-uuid"));
        assert!(!looks_like_uuid("6ba7b810-9dad-11d1-80b4"));
        assert!(!looks_like_uuid("6ba7b810-9dad-11d1-80b4-00c04fd430c8x"));
        assert!(!looks_like_uuid("abcdefghijklmnopqrstuvwxyzabcdef")); // task ID
        assert!(!looks_like_uuid("ops/now/feature.md")); // file path
    }

    #[test]
    fn test_detect_explore_target_uuid_session() {
        let dir = tempfile::tempdir().unwrap();
        let (scope, _worker) = detect_explore_target(
            dir.path(),
            "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
            false,
        )
        .unwrap();
        assert_eq!(scope.kind, ReviewScopeKind::Session);
        assert_eq!(scope.id, "6ba7b810-9dad-11d1-80b4-00c04fd430c8");
    }

    #[test]
    fn test_detect_explore_target_uuid_with_code_flag_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result = detect_explore_target(
            dir.path(),
            "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
            true,
        );
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("--code flag only applies to file targets"));
    }

    #[test]
    fn test_detect_explore_target_md_file_spec() {
        let dir = tempfile::tempdir().unwrap();
        let md_path = dir.path().join("feature.md");
        std::fs::write(&md_path, "# Feature\n").unwrap();
        let path_str = md_path.to_str().unwrap();

        let (scope, _worker) =
            detect_explore_target(dir.path(), path_str, false).unwrap();
        assert_eq!(scope.kind, ReviewScopeKind::Plan);
    }

    #[test]
    fn test_detect_explore_target_md_file_code() {
        let dir = tempfile::tempdir().unwrap();
        let md_path = dir.path().join("feature.md");
        std::fs::write(&md_path, "# Feature\n").unwrap();
        let path_str = md_path.to_str().unwrap();

        let (scope, _worker) =
            detect_explore_target(dir.path(), path_str, true).unwrap();
        assert_eq!(scope.kind, ReviewScopeKind::Code);
    }
}
