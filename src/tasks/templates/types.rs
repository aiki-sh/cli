//! Template types for the task template system
//!
//! Defines the structure of task templates including:
//! - TaskTemplate: The full template definition
//! - TaskDefaults: Default values for tasks created from templates
//! - TaskDefinition: Individual task/subtask definitions

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::spawn_config::SpawnEntry;

/// A task template defining a workflow with parent task and subtasks
#[derive(Debug, Clone)]
pub struct TaskTemplate {
    /// Template name (inferred from filename, e.g., "review" from "review.md")
    pub name: String,
    /// Semantic version (e.g., "1.2.0")
    pub version: Option<String>,
    /// Human-readable description
    pub description: Option<String>,
    /// Default values for tasks created from this template
    pub defaults: TaskDefaults,
    /// Parent task definition (name and instructions)
    pub parent: TaskDefinition,
    /// Subtask definitions (parsed from H2 sections in the markdown body)
    pub subtasks: Vec<TaskDefinition>,
    /// Source file path (for display purposes)
    pub source_path: Option<String>,
    /// Raw template content (for display purposes)
    pub raw_content: Option<String>,
    /// Spawn configurations: conditional task creation on close
    pub spawns: Vec<SpawnEntry>,
}

impl TaskTemplate {
    /// Create a new template with the given name
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: None,
            description: None,
            defaults: TaskDefaults::default(),
            parent: TaskDefinition::default(),
            subtasks: Vec::new(),
            source_path: None,
            raw_content: None,
            spawns: Vec::new(),
        }
    }

    /// Get the template identifier for storage (name@version or just name)
    #[must_use]
    pub fn template_id(&self) -> String {
        match &self.version {
            Some(v) => format!("{}@{}", self.name, v),
            None => self.name.clone(),
        }
    }
}

/// Default values for tasks created from a template
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TaskDefaults {
    /// Task type (e.g., "review", "refactor", "test")
    #[serde(rename = "type")]
    pub task_type: Option<String>,
    /// Default assignee
    pub assignee: Option<String>,
    /// Default priority
    pub priority: Option<String>,
    /// Default data values
    #[serde(default)]
    pub data: HashMap<String, serde_json::Value>,
}

/// Definition of a single task or subtask
#[derive(Debug, Clone, Default)]
pub struct TaskDefinition {
    /// Task name (may contain variables like {data.scope})
    pub name: String,
    /// Stable slug for automation references (e.g., "build", "run-tests")
    pub slug: Option<String>,
    /// Task type (e.g., "review", "fix") - enables sugar triggers
    pub task_type: Option<String>,
    /// Task instructions (markdown content)
    pub instructions: String,
    /// Override priority for this subtask
    pub priority: Option<String>,
    /// Override assignee for this subtask
    pub assignee: Option<String>,
    /// Sources for this subtask (e.g., "task:abc123")
    pub sources: Vec<String>,
    /// Additional data specific to this subtask
    pub data: HashMap<String, serde_json::Value>,
}

/// Loop configuration for repeating templates
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoopConfig {
    /// Rhai expression — loop terminates when this evaluates to true
    pub until: String,
    /// Additional data to pass to each iteration
    #[serde(default)]
    pub data: HashMap<String, serde_yaml::Value>,
}

/// YAML frontmatter structure for template files
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TemplateFrontmatter {
    /// Slug for this template when composed into a parent via {% subtask %}
    pub slug: Option<String>,
    /// Semantic version
    pub version: Option<String>,
    /// Human-readable description
    pub description: Option<String>,
    /// Task type
    #[serde(rename = "type")]
    pub task_type: Option<String>,
    /// Default assignee
    pub assignee: Option<String>,
    /// Default priority
    pub priority: Option<String>,
    /// Default data values
    #[serde(default)]
    pub data: HashMap<String, serde_json::Value>,
    /// Spawn configurations: conditional task creation on close
    #[serde(default)]
    pub spawns: Vec<SpawnEntry>,
    /// Loop configuration: sugar for self-spawn with autorun
    #[serde(default, rename = "loop")]
    pub loop_config: Option<LoopConfig>,
}

/// YAML frontmatter for subtasks
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SubtaskFrontmatter {
    /// Stable slug for automation references (e.g., "build", "run-tests")
    pub slug: Option<String>,
    /// Override priority
    pub priority: Option<String>,
    /// Override assignee
    pub assignee: Option<String>,
    /// Sources for this subtask (e.g., "task:{source.id}")
    #[serde(default)]
    pub sources: Vec<String>,
    /// Additional data
    #[serde(default)]
    pub data: HashMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_template_new() {
        let template = TaskTemplate::new("review");
        assert_eq!(template.name, "review");
        assert!(template.version.is_none());
        assert!(template.description.is_none());
        assert!(template.subtasks.is_empty());
    }

    #[test]
    fn test_template_id_without_version() {
        let template = TaskTemplate::new("aiki/review");
        assert_eq!(template.template_id(), "aiki/review");
    }

    #[test]
    fn test_template_id_with_version() {
        let mut template = TaskTemplate::new("aiki/review");
        template.version = Some("1.2.0".to_string());
        assert_eq!(template.template_id(), "aiki/review@1.2.0");
    }

    #[test]
    fn test_task_defaults_default() {
        let defaults = TaskDefaults::default();
        assert!(defaults.task_type.is_none());
        assert!(defaults.assignee.is_none());
        assert!(defaults.priority.is_none());
        assert!(defaults.data.is_empty());
    }

    #[test]
    fn test_frontmatter_deserialize() {
        let yaml = r#"
version: "1.0.0"
description: Test template
type: review
assignee: claude-code
priority: p1
data:
  scope: "@"
"#;
        let fm: TemplateFrontmatter = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fm.version, Some("1.0.0".to_string()));
        assert_eq!(fm.description, Some("Test template".to_string()));
        assert_eq!(fm.task_type, Some("review".to_string()));
        assert_eq!(fm.assignee, Some("claude-code".to_string()));
        assert_eq!(fm.priority, Some("p1".to_string()));
        assert_eq!(fm.data.get("scope"), Some(&serde_json::json!("@")));
    }

    #[test]
    fn test_frontmatter_minimal() {
        let yaml = "";
        let fm: TemplateFrontmatter = serde_yaml::from_str(yaml).unwrap_or_default();
        assert!(fm.version.is_none());
        assert!(fm.data.is_empty());
    }

    #[test]
    fn test_loop_config_deserialize() {
        let yaml = r#"
loop:
  until: "subtasks.review.approved or data.loop.index1 >= 10"
  data:
    custom_field: value
"#;
        let fm: TemplateFrontmatter = serde_yaml::from_str(yaml).unwrap();
        assert!(fm.loop_config.is_some());
        let lc = fm.loop_config.unwrap();
        assert_eq!(
            lc.until,
            "subtasks.review.approved or data.loop.index1 >= 10"
        );
        assert_eq!(lc.data.len(), 1);
    }

    #[test]
    fn test_loop_config_minimal() {
        let yaml = r#"
loop:
  until: approved
"#;
        let fm: TemplateFrontmatter = serde_yaml::from_str(yaml).unwrap();
        let lc = fm.loop_config.unwrap();
        assert_eq!(lc.until, "approved");
        assert!(lc.data.is_empty());
    }

    #[test]
    fn test_frontmatter_without_loop() {
        let yaml = r#"
version: "1.0.0"
"#;
        let fm: TemplateFrontmatter = serde_yaml::from_str(yaml).unwrap();
        assert!(fm.loop_config.is_none());
    }

    #[test]
    fn test_frontmatter_with_both_loop_and_spawns() {
        let yaml = r#"
loop:
  until: approved
spawns:
  - when: "not approved"
    task:
      template: aiki/fix
"#;
        let fm: TemplateFrontmatter = serde_yaml::from_str(yaml).unwrap();
        assert!(fm.loop_config.is_some());
        assert_eq!(fm.spawns.len(), 1);
    }
}
