//! Spawn configuration types for template frontmatter
//!
//! Defines the `spawns:` field that templates use to declare conditional
//! task creation when the parent task closes.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration for a single spawn entry in template frontmatter.
///
/// Each entry declares a condition (`when`) and either a standalone `task`
/// or a `subtask` to create when the condition is true at close time.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SpawnEntry {
    /// Rhai expression evaluated against task state on close.
    /// The spawn triggers if this evaluates to true.
    pub when: String,
    /// Maximum loop iterations. If set, the spawn evaluator checks
    /// `loop.index1 >= max_iterations` before evaluating `when`.
    /// `None` means no iteration cap. `Some(0)` means no limit (skip check).
    #[serde(default)]
    pub max_iterations: Option<usize>,
    /// Standalone task configuration (no parent relationship).
    /// Mutually exclusive with `subtask`.
    #[serde(default)]
    pub task: Option<SpawnTaskConfig>,
    /// Subtask configuration (spawner becomes parent).
    /// Mutually exclusive with `task`.
    #[serde(default)]
    pub subtask: Option<SpawnTaskConfig>,
}

/// Configuration for a spawned task or subtask.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SpawnTaskConfig {
    /// Template to instantiate (e.g., "fix").
    pub template: String,
    /// Priority override. Inherits from spawner if not set.
    #[serde(default)]
    pub priority: Option<String>,
    /// Assignee override. Uses template default if not set.
    #[serde(default)]
    pub assignee: Option<String>,
    /// Whether to auto-start the spawned task immediately after creation.
    /// Defaults to false (manual start required).
    #[serde(default)]
    pub autorun: bool,
    /// Data fields to pass to spawned task.
    /// Values are Rhai expressions evaluated against the spawner's state.
    #[serde(default)]
    pub data: HashMap<String, serde_yaml::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_spawn_entry_with_task() {
        let yaml = r#"
when: not data.approved
task:
  template: fix
  priority: p0
  data:
    max_iterations: 3
"#;
        let entry: SpawnEntry = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(entry.when, "not data.approved");
        assert!(entry.task.is_some());
        assert!(entry.subtask.is_none());

        let task = entry.task.unwrap();
        assert_eq!(task.template, "fix");
        assert_eq!(task.priority, Some("p0".to_string()));
        assert_eq!(task.data.len(), 1);
    }

    #[test]
    fn test_parse_spawn_entry_with_subtask() {
        let yaml = r#"
when: data.needs_analysis
subtask:
  template: analysis
  assignee: claude-code
"#;
        let entry: SpawnEntry = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(entry.when, "data.needs_analysis");
        assert!(entry.task.is_none());
        assert!(entry.subtask.is_some());

        let subtask = entry.subtask.unwrap();
        assert_eq!(subtask.template, "analysis");
        assert_eq!(subtask.assignee, Some("claude-code".to_string()));
    }

    #[test]
    fn test_parse_spawn_entry_data_values() {
        let yaml = r#"
when: not data.approved
task:
  template: fix
  data:
    max_iterations: 3
    issue_count: data.issue_count
    label: "urgent"
    is_critical: true
"#;
        let entry: SpawnEntry = serde_yaml::from_str(yaml).unwrap();
        let task = entry.task.unwrap();
        assert_eq!(task.data.len(), 4);

        // Integer literal
        assert_eq!(task.data["max_iterations"], serde_yaml::Value::from(3));
        // Variable reference (stored as string)
        assert_eq!(
            task.data["issue_count"],
            serde_yaml::Value::from("data.issue_count")
        );
        // String literal
        assert_eq!(task.data["label"], serde_yaml::Value::from("urgent"));
        // Boolean literal
        assert_eq!(task.data["is_critical"], serde_yaml::Value::from(true));
    }

    #[test]
    fn test_parse_spawns_array() {
        let yaml = r#"
- when: not data.approved
  task:
    template: fix
- when: data.issue_count > 3
  task:
    template: follow-up
    data:
      issue_count: data.issue_count
"#;
        let entries: Vec<SpawnEntry> = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].when, "not data.approved");
        assert_eq!(entries[1].when, "data.issue_count > 3");
    }

    #[test]
    fn test_parse_spawn_entry_minimal() {
        let yaml = r#"
when: "true"
task:
  template: fix
"#;
        let entry: SpawnEntry = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(entry.when, "true");
        let task = entry.task.unwrap();
        assert_eq!(task.template, "fix");
        assert!(task.priority.is_none());
        assert!(task.assignee.is_none());
        assert!(!task.autorun, "autorun should default to false");
        assert!(task.data.is_empty());
    }

    #[test]
    fn test_parse_spawn_entry_with_autorun() {
        let yaml = r#"
when: not data.approved
task:
  template: fix
  autorun: true
  data:
    max_iterations: 3
"#;
        let entry: SpawnEntry = serde_yaml::from_str(yaml).unwrap();
        let task = entry.task.unwrap();
        assert_eq!(task.template, "fix");
        assert!(task.autorun, "autorun should be true when explicitly set");
    }

    #[test]
    fn test_parse_spawn_entry_autorun_false() {
        let yaml = r#"
when: "true"
task:
  template: fix
  autorun: false
"#;
        let entry: SpawnEntry = serde_yaml::from_str(yaml).unwrap();
        let task = entry.task.unwrap();
        assert!(!task.autorun, "autorun should be false when explicitly set");
    }

    #[test]
    fn test_parse_spawn_entry_autorun_missing_defaults_false() {
        // Backward compat: missing autorun field should default to false
        let yaml = r#"
when: "true"
task:
  template: fix
  priority: p1
"#;
        let entry: SpawnEntry = serde_yaml::from_str(yaml).unwrap();
        let task = entry.task.unwrap();
        assert!(!task.autorun, "Missing autorun should default to false");
    }

    #[test]
    fn test_parse_spawn_entry_with_max_iterations() {
        let yaml = r#"
when: "not data.approved"
max_iterations: 3
task:
  template: fix
"#;
        let entry: SpawnEntry = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(entry.max_iterations, Some(3));
    }

    #[test]
    fn test_parse_spawn_entry_without_max_iterations_defaults_none() {
        let yaml = r#"
when: "true"
task:
  template: fix
"#;
        let entry: SpawnEntry = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(entry.max_iterations, None);
    }
}
