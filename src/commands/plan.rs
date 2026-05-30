//! Plan command for interactive plan authoring
//!
//! This module provides the `aiki plan` command which:
//! - Creates and refines plan documents collaboratively with an AI agent
//! - Supports three modes: edit existing, create at path, autogenerate filename
//! - Always runs interactively (no --async or --start flags)

use std::env;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

use crate::agents::AgentType;
use crate::error::{AikiError, Result};
use crate::session::find_active_session;
use crate::tasks::templates::{
    create_tasks_from_template, find_templates_dir, load_template, substitute_parent_id,
    VariableContext, PARENT_ID_PLACEHOLDER,
};
use crate::tasks::md::MdBuilder;
use crate::tasks::{
    generate_task_id, get_current_scope_set, get_in_progress,
    get_ready_queue_for_scope_set, materialize_graph, read_events, reassign_task, start_task_core,
    write_event, Task, TaskEvent, TaskPriority, TaskStatus,
};

/// Plan mode determined from input arguments
#[derive(Debug, Clone)]
pub enum PlanMode {
    /// Edit an existing plan file
    Edit { path: PathBuf, text: String },
    /// Create a new plan at the specified path
    CreateAtPath { path: PathBuf, initial_idea: String, text: String },
    /// Create a new plan with an auto-generated filename
    Autogen { description: String, slug: String },
}

/// Run the plan command
pub fn run(
    args: Vec<String>,
    template: Option<String>,
    agent: Option<String>,
) -> Result<()> {
    let cwd = env::current_dir().map_err(|_| {
        AikiError::InvalidArgument("Failed to get current directory".to_string())
    })?;

    // Determine plan mode from arguments
    let mode = determine_mode(&cwd, &args)?;

    // Run the plan session
    run_plan(&cwd, mode, template, agent)
}

/// Determine plan mode from command arguments
fn determine_mode(cwd: &Path, args: &[String]) -> Result<PlanMode> {
    if args.is_empty() {
        // Interactive mode - would prompt for input
        // For now, return an error since we need input
        return Err(AikiError::InvalidArgument(
            "No plan path or description provided. Usage: aiki plan <path-or-text...>".to_string(),
        ));
    }

    let first_arg = &args[0];

    // Check if first arg ends with .md
    if first_arg.ends_with(".md") {
        let path = if first_arg.starts_with('/') {
            PathBuf::from(first_arg)
        } else {
            cwd.join(first_arg)
        };

        // Validate path is inside repo
        validate_path_in_repo(cwd, &path)?;

        // Remaining args after the .md path become free-form guidance text
        let text = if args.len() > 1 {
            args[1..].join(" ")
        } else {
            String::new()
        };

        if path.exists() {
            // Check it's a file, not a directory
            if !path.is_file() {
                return Err(AikiError::InvalidArgument(format!(
                    "Not a markdown file: {}",
                    path.display()
                )));
            }
            // Edit mode
            Ok(PlanMode::Edit { path, text })
        } else {
            // Create at path mode - parse initial idea from filename
            let initial_idea = parse_idea_from_filename(&path);
            Ok(PlanMode::CreateAtPath { path, initial_idea, text })
        }
    } else {
        // Autogen mode - join all args as description
        let description = args.join(" ");
        let slug = generate_slug(&description);

        // Find a unique filename
        let base_path = cwd.join("ops/now");
        let path = find_unique_path(&base_path, &slug)?;

        Ok(PlanMode::Autogen {
            description,
            slug: path.file_stem().unwrap().to_string_lossy().to_string(),
        })
    }
}

/// Validate that a path is inside the repository
fn validate_path_in_repo(cwd: &Path, path: &Path) -> Result<()> {
    // Canonicalize both paths for comparison
    let cwd_canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());

    // For paths that don't exist yet, canonicalize the parent
    let path_for_check = if path.exists() {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    } else if let Some(parent) = path.parent() {
        if parent.exists() {
            parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf())
        } else {
            // Parent doesn't exist, use the path as-is
            path.to_path_buf()
        }
    } else {
        path.to_path_buf()
    };

    if !path_for_check.starts_with(&cwd_canonical) {
        return Err(AikiError::InvalidArgument(
            "Path must be inside repository".to_string(),
        ));
    }

    Ok(())
}

/// Parse initial idea from filename
/// e.g., "dark-mode.md" -> "Dark Mode"
fn parse_idea_from_filename(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    // Convert hyphens to spaces and capitalize words
    stem.split('-')
        .map(|word| {
            let mut chars: Vec<char> = word.chars().collect();
            if let Some(first) = chars.first_mut() {
                *first = first.to_uppercase().next().unwrap_or(*first);
            }
            chars.into_iter().collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Generate a slug from a description
/// e.g., "Add User Auth" -> "add-user-auth"
fn generate_slug(description: &str) -> String {
    description
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c
            } else if c.is_whitespace() {
                '-'
            } else {
                // Skip special characters
                '\0'
            }
        })
        .filter(|&c| c != '\0')
        .collect::<String>()
        // Collapse multiple hyphens
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Build a formatted user context block from initial idea and free-form text
///
/// Returns a markdown section if there's any user-provided context, or empty string if not.
fn build_user_context(initial_idea: &str, user_text: &str) -> String {
    let has_idea = !initial_idea.is_empty();
    let has_text = !user_text.is_empty();

    if !has_idea && !has_text {
        return String::new();
    }

    let mut parts = Vec::new();

    if has_idea && has_text {
        // Both: idea is the title/topic, text is the guidance
        parts.push(format!("**Topic:** {}", initial_idea));
        parts.push(format!("\n**User guidance:**\n> {}", user_text.replace('\n', "\n> ")));
    } else if has_idea {
        // Only idea (from filename or description)
        parts.push(format!("**Topic:** {}", initial_idea));
    } else {
        // Only text (edit mode with guidance)
        parts.push(format!("**User guidance:**\n> {}", user_text.replace('\n', "\n> ")));
    }

    format!(
        "\n## User Context\n\n{}\n",
        parts.join("\n")
    )
}

/// Prompt for multi-line text input using crossterm raw mode.
///
/// - Enter: submit text
/// - Shift+Enter: insert newline
/// - Esc: skip (return None)
/// - Ctrl+C: skip (return None)
/// - Backspace: delete character
///
/// Returns None if skipped, Some(text) if submitted.
fn prompt_multiline_input(header: &str) -> Result<Option<String>> {
    let mut stderr = io::stderr();

    // Print the prompt header
    eprintln!("\x1b[1m{}\x1b[0m", header);
    eprintln!("\x1b[2m(Enter to submit, Shift+Enter for newline, Esc to skip)\x1b[0m");
    eprint!("> ");
    stderr.flush().ok();

    enable_raw_mode().map_err(|e| {
        AikiError::InvalidArgument(format!("Failed to enable raw mode: {}", e))
    })?;

    let mut lines: Vec<String> = vec![String::new()];

    loop {
        let ev = event::read().map_err(|e| {
            disable_raw_mode().ok();
            AikiError::InvalidArgument(format!("Failed to read input: {}", e))
        })?;

        if let Event::Key(key_event) = ev {
            // Only react to Press events (not Release/Repeat)
            if key_event.kind != crossterm::event::KeyEventKind::Press {
                continue;
            }

            match (key_event.code, key_event.modifiers) {
                // Esc: skip
                (KeyCode::Esc, _) => {
                    disable_raw_mode().ok();
                    eprintln!();
                    return Ok(None);
                }
                // Ctrl+C: skip
                (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                    disable_raw_mode().ok();
                    eprintln!();
                    return Ok(None);
                }
                // Shift+Enter: new line
                (KeyCode::Enter, m) if m.contains(KeyModifiers::SHIFT) => {
                    lines.push(String::new());
                    eprint!("\r\n> ");
                    stderr.flush().ok();
                }
                // Enter (without shift): submit
                (KeyCode::Enter, _) => {
                    disable_raw_mode().ok();
                    eprintln!();
                    let text = lines.join("\n").trim().to_string();
                    return Ok(if text.is_empty() { None } else { Some(text) });
                }
                // Backspace
                (KeyCode::Backspace, _) => {
                    if let Some(current_line) = lines.last_mut() {
                        if !current_line.is_empty() {
                            current_line.pop();
                            eprint!("\x08 \x08");
                            stderr.flush().ok();
                        } else if lines.len() > 1 {
                            lines.pop();
                            let prev = lines.last().unwrap();
                            eprint!("\x1b[A\r> {}\x1b[K", prev);
                            stderr.flush().ok();
                        }
                    }
                }
                // Regular character
                (KeyCode::Char(c), _) => {
                    if let Some(current_line) = lines.last_mut() {
                        current_line.push(c);
                        eprint!("{}", c);
                        stderr.flush().ok();
                    }
                }
                _ => {}
            }
        }
    }
}

/// Find a unique path for a plan file, incrementing suffix if needed
fn find_unique_path(base_dir: &Path, slug: &str) -> Result<PathBuf> {
    // Ensure base directory exists
    fs::create_dir_all(base_dir).map_err(|e| {
        AikiError::InvalidArgument(format!("Cannot create directory {}: {}", base_dir.display(), e))
    })?;

    let base_path = base_dir.join(format!("{}.md", slug));

    if !base_path.exists() {
        return Ok(base_path);
    }

    // Try incrementing suffixes
    for i in 2..=100 {
        let path = base_dir.join(format!("{}-{}.md", slug, i));
        if !path.exists() {
            return Ok(path);
        }
    }

    Err(AikiError::InvalidArgument(format!(
        "Could not find unique filename for slug: {}",
        slug
    )))
}

/// Core plan implementation
fn run_plan(
    cwd: &Path,
    mode: PlanMode,
    template_name: Option<String>,
    agent: Option<String>,
) -> Result<()> {
    let timestamp = chrono::Utc::now();

    // Determine plan file path, initial idea, and args-provided text
    let (plan_path, is_new, args_idea, args_text) = match &mode {
        PlanMode::Edit { path, text } => {
            (path.clone(), false, String::new(), text.clone())
        }
        PlanMode::CreateAtPath { path, initial_idea, text } => {
            (path.clone(), true, initial_idea.clone(), text.clone())
        }
        PlanMode::Autogen { description, slug } => {
            let path = cwd.join("ops/now").join(format!("{}.md", slug));
            (path, true, description.clone(), String::new())
        }
    };

    // Prompt for guidance text interactively if:
    // 1. No text was provided as trailing args
    // 2. We're running from a terminal (interactive)
    // 3. Not in autogen mode (where the description already serves as guidance)
    let user_text = if args_text.is_empty()
        && !matches!(mode, PlanMode::Autogen { .. })
        && io::stdin().is_terminal()
    {
        let header = if args_idea.is_empty() {
            format!("Plan: {}", plan_path.display())
        } else {
            format!("Plan: {} ({})", plan_path.display(), args_idea)
        };
        prompt_multiline_input(&header)?.unwrap_or_default()
    } else {
        args_text
    };

    // Build initial_idea from filename idea + user text
    let initial_idea = if user_text.is_empty() {
        args_idea
    } else if args_idea.is_empty() {
        user_text.clone()
    } else {
        format!("{}: {}", args_idea, user_text)
    };

    // Parse agent if provided
    let agent_type = if let Some(ref agent_str) = agent {
        Some(
            AgentType::from_str(agent_str)
                .ok_or_else(|| AikiError::UnknownAgentType(agent_str.clone()))?,
        )
    } else {
        None
    };

    // Check for existing plan task with source: file:<path>
    let events = read_events(cwd)?;
    let tasks = materialize_graph(&events).tasks;

    let source_key = format!("file:{}", plan_path.display());
    let existing_task = tasks.values().find(|t| {
        t.task_type.as_deref() == Some("plan")
            && t.status != TaskStatus::Closed
            && t.sources.iter().any(|s| s == &source_key)
    });

    let plan_task_id = if let Some(task) = existing_task {
        // Resume existing task
        eprintln!("Resuming existing plan task: {}", task.id);

        // If agent differs, update assignee
        if let Some(agent) = agent_type {
            if task.assignee.as_deref() != Some(agent.as_str()) {
                reassign_task(cwd, &task.id, agent.as_str())?;
            }
        }

        task.id.clone()
    } else {
        // Create new plan task
        create_plan_task(
            cwd,
            &plan_path,
            &initial_idea,
            is_new,
            &user_text,
            template_name.as_deref().unwrap_or("aiki/plan"),
            agent_type.as_ref().map(|a| a.as_str().to_string()),
            timestamp,
        )?
    };

    // If this is a new file, create it
    if is_new && !plan_path.exists() {
        // Ensure parent directory exists
        if let Some(parent) = plan_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                AikiError::InvalidArgument(format!(
                    "Cannot create directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        // Create file with draft frontmatter
        fs::write(&plan_path, "---\ndraft: true\n---\n\n").map_err(|e| {
            AikiError::InvalidArgument(format!(
                "Cannot write to {}: {}",
                plan_path.display(),
                e
            ))
        })?;
    }

    // Re-read tasks to include newly created plan task
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let tasks = &graph.tasks;
    let scope_set = get_current_scope_set(&graph);
    let in_progress: Vec<&Task> = get_in_progress(tasks).into_iter().collect();
    let ready = get_ready_queue_for_scope_set(&graph, &scope_set);

    // Reassign task to current agent if we're in an agent session
    if let Some(session) = find_active_session(cwd) {
        if agent.is_none() {
            // Use the current session's agent
            reassign_task(cwd, &plan_task_id, session.agent_type.as_str())?;
        }
    }

    // Start the task
    start_task_core(cwd, &[plan_task_id.clone()])?;

    // Output started message
    output_plan_started(&plan_task_id, &plan_path, is_new, &in_progress, &ready)?;

    // Spawn Claude interactively (not using task_run which is for autonomous execution)
    // The prompt includes the user's context so Claude sees it immediately,
    // rather than requiring it to discover the instructions via `aiki task show`
    let prompt = if initial_idea.is_empty() {
        format!(
            "Run `aiki task start {}` to begin working on this plan task.",
            plan_task_id
        )
    } else {
        format!(
            "Run `aiki task start {}` to begin working on this plan task.\n\nUser's request: {}",
            plan_task_id, initial_idea
        )
    };

    eprintln!(
        "Spawning Claude agent session for task {}...",
        plan_task_id
    );

    // Spawn claude interactively - inherits stdin/stdout/stderr for user interaction
    // Note: We don't use --print or --dangerously-skip-permissions here because
    // plan sessions are interactive and the user can approve actions themselves
    // AIKI_TASK is set so the session tracks which task is driving it, enabling
    // auto-end when the plan task closes
    let status = Command::new("claude")
        .current_dir(cwd)
        .env("AIKI_TASK", &plan_task_id)
        .arg(&prompt)
        .status();

    match status {
        Ok(exit_status) => {
            if exit_status.success() {
                output_plan_completed(&plan_task_id, &plan_path)?;
            } else {
                // Claude exited with non-zero - could be user cancelled, graceful termination, or error
                let code = exit_status.code().unwrap_or(-1);
                if code == 130 {
                    // SIGINT (Ctrl+C) - user cancelled, not an error
                    eprintln!("Plan session cancelled by user.");
                } else if code == 143 {
                    // SIGTERM - graceful termination (e.g., via `claude --exit` when task closes)
                    // This is expected behavior, not an error
                    output_plan_completed(&plan_task_id, &plan_path)?;
                } else {
                    output_plan_error(&plan_task_id, &format!("Claude exited with code {}", code))?;
                }
            }
        }
        Err(e) => {
            output_plan_error(&plan_task_id, &format!("Failed to spawn claude: {}", e))?;
            return Err(AikiError::AgentSpawnFailed(e.to_string()));
        }
    }

    // Output task ID to stdout if piped
    if !std::io::stdout().is_terminal() {
        println!("{}", plan_task_id);
    }

    Ok(())
}

/// Create a plan task from template
fn create_plan_task(
    cwd: &Path,
    plan_path: &Path,
    initial_idea: &str,
    is_new: bool,
    user_text: &str,
    template_name: &str,
    assignee: Option<String>,
    timestamp: chrono::DateTime<chrono::Utc>,
) -> Result<String> {
    use crate::tasks::templates::get_working_copy_change_id;

    let working_copy = get_working_copy_change_id(cwd);

    // Load the template
    let templates_dir = find_templates_dir(cwd)?;
    let template = load_template(template_name, &templates_dir)?;

    // Set up variable context
    let mut variables = VariableContext::new();
    variables.set_data("plan_path", &plan_path.display().to_string());
    variables.set_data("is_new", if is_new { "true" } else { "false" });
    variables.set_data("initial_idea", initial_idea);
    variables.set_data("user_text", user_text);

    // Compose a formatted user_context block from initial_idea and user_text
    let user_context = build_user_context(initial_idea, user_text);
    variables.set_data("user_context", &user_context);

    // Set parent.id placeholder - it will be replaced after we generate the actual parent ID
    variables.set_parent("id", PARENT_ID_PLACEHOLDER);

    // Create tasks from template
    let (parent_def, mut subtask_defs) = create_tasks_from_template(&template, &variables, None)?;

    // Generate parent task ID
    let parent_id = generate_task_id(&parent_def.name);

    // Substitute {{parent.id}} in subtask instructions now that we have the parent ID
    substitute_parent_id(&mut subtask_defs, &parent_id);

    // Determine task type
    let task_type = parent_def
        .task_type
        .or_else(|| template.defaults.task_type.clone())
        .or_else(|| Some("plan".to_string()));

    // Determine priority
    let priority = parent_def
        .priority
        .as_ref()
        .and_then(|p| crate::tasks::templates::parse_priority(p))
        .or_else(|| {
            template
                .defaults
                .priority
                .as_ref()
                .and_then(|p| crate::tasks::templates::parse_priority(p))
        })
        .unwrap_or(TaskPriority::P2);

    // Build sources
    let mut sources = parent_def.sources.clone();
    sources.push(format!("file:{}", plan_path.display()));

    // Create parent task event
    let parent_event = TaskEvent::Created {
        task_id: parent_id.clone(),
        name: parent_def.name.clone(),
        slug: None,
        task_type,
        priority,
        assignee: assignee
            .or_else(|| template.defaults.assignee.clone())
            .or_else(|| Some("claude-code".to_string())),
        sources,
        template: Some(template.template_id()),
        working_copy: working_copy.clone(),
        instructions: Some(parent_def.instructions.clone()),
        data: crate::tasks::templates::convert_data(&parent_def.data),
        timestamp,
    };
    write_event(cwd, &parent_event)?;

    // Create subtasks
    for (i, subtask_def) in subtask_defs.iter().enumerate() {
        let subtask_id = format!("{}.{}", parent_id, i + 1);

        let subtask_priority = subtask_def
            .priority
            .as_ref()
            .and_then(|p| crate::tasks::templates::parse_priority(p))
            .unwrap_or(priority);

        let mut subtask_sources = subtask_def.sources.clone();
        if !subtask_sources.iter().any(|s| s.starts_with("task:")) {
            subtask_sources.push(format!("task:{}", parent_id));
        }

        let subtask_event = TaskEvent::Created {
            task_id: subtask_id,
            name: subtask_def.name.clone(),
            slug: None,
            task_type: Some("plan".to_string()),
            priority: subtask_priority,
            assignee: subtask_def.assignee.clone(),
            sources: subtask_sources,
            template: None,
            working_copy: working_copy.clone(),
            instructions: Some(subtask_def.instructions.clone()),
            data: crate::tasks::templates::convert_data(&subtask_def.data),
            timestamp,
        };
        write_event(cwd, &subtask_event)?;
    }

    Ok(parent_id)
}

/// Output plan started message
fn output_plan_started(
    plan_id: &str,
    plan_path: &Path,
    is_new: bool,
    in_progress: &[&Task],
    ready: &[&Task],
) -> Result<()> {
    let action = if is_new { "Creating" } else { "Editing" };
    let content = format!(
        "## Plan Started\n- **Task:** {}\n- **File:** {}\n- {} plan at {}.\n",
        plan_id,
        plan_path.display(),
        action,
        plan_path.display()
    );
    let md = MdBuilder::new("plan").build(&content, in_progress, ready);
    eprintln!("{}", md);
    Ok(())
}

/// Output plan completed message
fn output_plan_completed(plan_id: &str, plan_path: &Path) -> Result<()> {
    let content = format!(
        "## Plan Completed\n- **Task:** {}\n- **File:** {}\n- Created: {}\n",
        plan_id,
        plan_path.display(),
        plan_path.display()
    );
    let md = MdBuilder::new("plan").build(&content, &[], &[]);
    eprintln!("{}", md);
    Ok(())
}

/// Output plan error message
fn output_plan_error(plan_id: &str, error: &str) -> Result<()> {
    let content = format!(
        "Plan task {}: {}",
        plan_id, error
    );
    let md = MdBuilder::new("plan").error().build_error(&content);
    eprintln!("{}", md);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_idea_from_filename() {
        assert_eq!(
            parse_idea_from_filename(Path::new("dark-mode.md")),
            "Dark Mode"
        );
        assert_eq!(
            parse_idea_from_filename(Path::new("user-auth-v2.md")),
            "User Auth V2"
        );
        assert_eq!(
            parse_idea_from_filename(Path::new("simple.md")),
            "Simple"
        );
        assert_eq!(
            parse_idea_from_filename(Path::new("add-user-authentication.md")),
            "Add User Authentication"
        );
    }

    #[test]
    fn test_generate_slug() {
        assert_eq!(generate_slug("Add user authentication"), "add-user-authentication");
        assert_eq!(generate_slug("Fix the login bug"), "fix-the-login-bug");
        assert_eq!(generate_slug("Add User Auth"), "add-user-auth");
        assert_eq!(generate_slug("Simple"), "simple");
        assert_eq!(generate_slug("  Multiple   Spaces  "), "multiple-spaces");
        assert_eq!(generate_slug("Special! @#$ Characters"), "special-characters");
    }

    #[test]
    fn test_determine_mode_edit() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let plan_path = temp_dir.path().join("existing.md");
        fs::write(&plan_path, "# Existing Plan").unwrap();

        let mode = determine_mode(temp_dir.path(), &["existing.md".to_string()]).unwrap();

        match mode {
            PlanMode::Edit { path, text } => {
                assert_eq!(path, plan_path);
                assert_eq!(text, "");
            }
            _ => panic!("Expected Edit mode"),
        }
    }

    #[test]
    fn test_determine_mode_edit_with_text() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let plan_path = temp_dir.path().join("existing.md");
        fs::write(&plan_path, "# Existing Plan").unwrap();

        let mode = determine_mode(
            temp_dir.path(),
            &[
                "existing.md".to_string(),
                "add".to_string(),
                "rate".to_string(),
                "limiting".to_string(),
            ],
        )
        .unwrap();

        match mode {
            PlanMode::Edit { path, text } => {
                assert_eq!(path, plan_path);
                assert_eq!(text, "add rate limiting");
            }
            _ => panic!("Expected Edit mode"),
        }
    }

    #[test]
    fn test_determine_mode_create_at_path() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();

        let mode = determine_mode(temp_dir.path(), &["new-feature.md".to_string()]).unwrap();

        match mode {
            PlanMode::CreateAtPath { path, initial_idea, text } => {
                assert_eq!(path, temp_dir.path().join("new-feature.md"));
                assert_eq!(initial_idea, "New Feature");
                assert_eq!(text, "");
            }
            _ => panic!("Expected CreateAtPath mode"),
        }
    }

    #[test]
    fn test_determine_mode_create_at_path_with_text() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();

        let mode = determine_mode(
            temp_dir.path(),
            &[
                "jwt-auth.md".to_string(),
                "JWT".to_string(),
                "auth".to_string(),
                "with".to_string(),
                "refresh".to_string(),
                "tokens".to_string(),
            ],
        )
        .unwrap();

        match mode {
            PlanMode::CreateAtPath { path, initial_idea, text } => {
                assert_eq!(path, temp_dir.path().join("jwt-auth.md"));
                assert_eq!(initial_idea, "Jwt Auth");
                assert_eq!(text, "JWT auth with refresh tokens");
            }
            _ => panic!("Expected CreateAtPath mode"),
        }
    }

    #[test]
    fn test_determine_mode_autogen() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        // Create ops/now directory
        fs::create_dir_all(temp_dir.path().join("ops/now")).unwrap();

        let mode = determine_mode(
            temp_dir.path(),
            &["Add".to_string(), "user".to_string(), "authentication".to_string()],
        ).unwrap();

        match mode {
            PlanMode::Autogen { description, slug } => {
                assert_eq!(description, "Add user authentication");
                assert_eq!(slug, "add-user-authentication");
            }
            _ => panic!("Expected Autogen mode"),
        }
    }

    #[test]
    fn test_determine_mode_empty_args() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();

        let result = determine_mode(temp_dir.path(), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_build_user_context_empty() {
        assert_eq!(build_user_context("", ""), "");
    }

    #[test]
    fn test_build_user_context_idea_only() {
        let result = build_user_context("Dark Mode", "");
        assert!(result.contains("**Topic:** Dark Mode"));
        assert!(!result.contains("User guidance"));
    }

    #[test]
    fn test_build_user_context_text_only() {
        let result = build_user_context("", "Add rate limiting to the API");
        assert!(result.contains("**User guidance:**"));
        assert!(result.contains("> Add rate limiting to the API"));
        assert!(!result.contains("**Topic:**"));
    }

    #[test]
    fn test_build_user_context_both() {
        let result = build_user_context("JWT Auth", "with refresh tokens and rate limiting");
        assert!(result.contains("**Topic:** JWT Auth"));
        assert!(result.contains("**User guidance:**"));
        assert!(result.contains("> with refresh tokens and rate limiting"));
    }
}
