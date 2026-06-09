use std::path::{Path, PathBuf};

use crate::error::{AikiError, Result};
use crate::tasks::{
    materialize_graph, read_events, write_event, TaskEvent, TaskOutcome, TaskStatus,
};

/// Resolve a plan path that may be relative or absolute.
pub(crate) fn resolve_plan_path(cwd: &Path, plan_path: &str) -> PathBuf {
    if plan_path.starts_with('/') {
        PathBuf::from(plan_path)
    } else {
        cwd.join(plan_path)
    }
}

/// Validate that the plan path is a .md file and exists
pub(crate) fn validate_plan_path(cwd: &Path, plan_path: &str) -> Result<()> {
    if !plan_path.ends_with(".md") {
        return Err(AikiError::InvalidArgument(
            "Plan file must be markdown (.md)".to_string(),
        ));
    }

    let full_path = resolve_plan_path(cwd, plan_path);

    if !full_path.exists() {
        return Err(AikiError::InvalidArgument(format!(
            "Plan file not found: {}",
            plan_path
        )));
    }

    if !full_path.is_file() {
        return Err(AikiError::InvalidArgument(format!(
            "Not a file: {}",
            plan_path
        )));
    }

    Ok(())
}

/// Clean up stale build tasks for this plan.
///
/// Finds any in_progress or open build tasks with `data.plan` matching the plan path
/// and closes them as wont_do with a comment.
pub(crate) fn cleanup_stale_builds(cwd: &Path, plan_path: &str) -> Result<()> {
    let events = read_events(cwd)?;
    let tasks = materialize_graph(&events).tasks;

    let stale_builds: Vec<String> = tasks
        .values()
        .filter(|t| {
            t.task_type.as_deref() == Some("orchestrator")
                && t.data.get("plan").map(|s| s.as_str()) == Some(plan_path)
                && (t.status == TaskStatus::InProgress || t.status == TaskStatus::Open)
        })
        .map(|t| t.id.clone())
        .collect();

    for build_id in &stale_builds {
        let comment_event = TaskEvent::CommentAdded {
            task_ids: vec![build_id.clone()],
            text: "Stale build cleaned up".to_string(),
            data: std::collections::HashMap::new(),
            timestamp: chrono::Utc::now(),
        };
        write_event(cwd, &comment_event)?;

        let close_event = TaskEvent::Closed {
            task_ids: vec![build_id.clone()],
            outcome: TaskOutcome::WontDo,
            confidence: None,
            summary: Some("Stale build cleaned up".to_string()),
            session_id: None,
            turn_id: None,
            timestamp: chrono::Utc::now(),
        };
        write_event(cwd, &close_event)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_plan_path_not_md() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let result = validate_plan_path(temp_dir.path(), "not-markdown.txt");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be markdown"));
    }

    #[test]
    fn test_validate_plan_path_not_found() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let result = validate_plan_path(temp_dir.path(), "nonexistent.md");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Plan file not found"));
    }

    #[test]
    fn test_validate_plan_path_exists() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let plan_file = temp_dir.path().join("my-plan.md");
        std::fs::write(&plan_file, "# My Plan").unwrap();
        let result = validate_plan_path(temp_dir.path(), "my-plan.md");
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_plan_path_absolute() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let plan_file = temp_dir.path().join("absolute-plan.md");
        std::fs::write(&plan_file, "# Plan").unwrap();
        let result = validate_plan_path(temp_dir.path(), &plan_file.to_string_lossy());
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_plan_path_directory() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let dir_path = temp_dir.path().join("subdir.md");
        std::fs::create_dir_all(&dir_path).unwrap();
        let result = validate_plan_path(temp_dir.path(), "subdir.md");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Not a file"));
    }
}
