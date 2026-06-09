//! Template parser for markdown files with YAML frontmatter
//!
//! Parses template files in the format:
//! ```markdown
//! ---
//! version: "1.0.0"
//! description: Template description
//! type: review
//! ---
//!
//! # Task Name
//!
//! Task instructions...
//!
//! # Subtasks
//!
//! ## Subtask 1
//!
//! Subtask 1 instructions...
//!
//! ## Subtask 2
//!
//! Subtask 2 instructions...
//! ```

use crate::error::{AikiError, Result};

use super::types::{
    SubtaskFrontmatter, TaskDefaults, TaskDefinition, TaskTemplate, TemplateFrontmatter,
};

/// Errors that can occur when extracting YAML frontmatter
#[derive(Debug)]
pub enum FrontmatterError {
    /// YAML parsing failed
    Yaml(serde_yaml::Error),
    /// Frontmatter started with `---` but no closing `---` found
    Unterminated,
}

impl std::fmt::Display for FrontmatterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrontmatterError::Yaml(e) => write!(f, "{}", e),
            FrontmatterError::Unterminated => {
                write!(
                    f,
                    "Unterminated frontmatter: found opening '---' but no closing '---'"
                )
            }
        }
    }
}

impl std::error::Error for FrontmatterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FrontmatterError::Yaml(e) => Some(e),
            FrontmatterError::Unterminated => None,
        }
    }
}

impl From<serde_yaml::Error> for FrontmatterError {
    fn from(e: serde_yaml::Error) -> Self {
        FrontmatterError::Yaml(e)
    }
}

/// Parse a template from markdown content
///
/// # Arguments
/// * `content` - The raw markdown content
/// * `name` - The template name (from filename)
/// * `file_path` - The file path (for error messages)
pub fn parse_template(content: &str, name: &str, file_path: &str) -> Result<TaskTemplate> {
    // Extract frontmatter if present
    let (frontmatter, body) = extract_frontmatter(content, file_path)?;

    // Parse the markdown body (always parse H2 subtask sections)
    let (parent, subtasks) = parse_markdown_body(&body, file_path)?;

    // Build the template
    let mut template = TaskTemplate::new(name);
    template.version = frontmatter.version;
    template.description = frontmatter.description;
    template.defaults = TaskDefaults {
        task_type: frontmatter.task_type,
        assignee: frontmatter.assignee,
        priority: frontmatter.priority,
        data: frontmatter.data,
    };
    template.parent = parent;
    // Propagate slug from template-level frontmatter to the parent definition.
    // This allows composed templates (loaded via {% subtask %}) to declare their slug.
    template.parent.slug = frontmatter.slug;
    template.subtasks = subtasks;
    template.spawns = frontmatter.spawns;

    // Desugar loop: config into a self-spawn with autorun
    if let Some(loop_config) = frontmatter.loop_config {
        use super::spawn_config::{SpawnEntry, SpawnTaskConfig};
        let mut spawn_data = loop_config.data;
        // Add loop metadata expressions that increment from the spawner's current values
        // These are Rhai expressions evaluated against the spawner's post-close scope
        spawn_data.insert(
            "loop.index".to_string(),
            serde_yaml::Value::String("data.loop.index + 1".to_string()),
        );
        spawn_data.insert(
            "loop.index1".to_string(),
            serde_yaml::Value::String("data.loop.index1 + 1".to_string()),
        );
        spawn_data.insert("loop.first".to_string(), serde_yaml::Value::Bool(false));
        let spawn_entry = SpawnEntry {
            when: format!("not ({})", loop_config.until),
            max_iterations: Some(loop_config.max_iterations),
            task: Some(SpawnTaskConfig {
                template: "self".to_string(),
                priority: None,
                assignee: None,
                autorun: true,
                data: spawn_data,
            }),
            subtask: None,
        };
        template.spawns.push(spawn_entry);

        // Add initial loop metadata to template defaults
        template
            .defaults
            .data
            .insert("loop.index".to_string(), serde_json::json!(0));
        template
            .defaults
            .data
            .insert("loop.index1".to_string(), serde_json::json!(1));
        template
            .defaults
            .data
            .insert("loop.first".to_string(), serde_json::json!(true));
    }

    // Validate spawn entries: each must have exactly one of task/subtask
    for (i, entry) in template.spawns.iter().enumerate() {
        match (&entry.task, &entry.subtask) {
            (Some(_), Some(_)) => {
                return Err(AikiError::TemplateProcessingFailed {
                    details: format!(
                        "spawn entry {} in template '{}' has both 'task' and 'subtask' — exactly one is required",
                        i, name
                    ),
                });
            }
            (None, None) => {
                return Err(AikiError::TemplateProcessingFailed {
                    details: format!(
                        "spawn entry {} in template '{}' has neither 'task' nor 'subtask' — exactly one is required",
                        i, name
                    ),
                });
            }
            _ => {} // Exactly one is set — valid
        }
    }

    Ok(template)
}

/// Extract YAML frontmatter from content (generic over frontmatter type)
///
/// Returns `Ok((Some(frontmatter), body))` if frontmatter is present and valid.
/// Returns `Ok((None, content))` if no frontmatter delimiters are found.
/// Returns `Err(FrontmatterError::Yaml)` if YAML is malformed.
/// Returns `Err(FrontmatterError::Unterminated)` if opening `---` found but no closing `---`.
///
/// This ensures users get clear error messages when their frontmatter is invalid,
/// rather than silently treating it as no frontmatter.
pub fn extract_yaml_frontmatter<T>(
    content: &str,
) -> std::result::Result<(Option<T>, String), FrontmatterError>
where
    T: serde::de::DeserializeOwned,
{
    let content = content.trim_start();

    // Check for frontmatter delimiter
    if !content.starts_with("---") {
        return Ok((None, content.to_string()));
    }

    // Find the closing delimiter
    let after_first = &content[3..];
    let after_first = after_first.trim_start_matches(['\n', '\r']);
    let end_idx = after_first
        .find("\n---")
        .or_else(|| after_first.find("\r\n---"));

    match end_idx {
        Some(idx) => {
            let yaml_content = &after_first[..idx];
            let body_start = idx + 4; // Skip "\n---"
            let body = after_first[body_start..].trim_start_matches(['\n', '\r']);

            // Parse YAML - return error if malformed
            let fm = serde_yaml::from_str::<T>(yaml_content)?;
            Ok((Some(fm), body.to_string()))
        }
        None => {
            // No closing delimiter found - error, not silent ignore
            Err(FrontmatterError::Unterminated)
        }
    }
}

/// Extract YAML frontmatter from markdown content (with error reporting)
fn extract_frontmatter(content: &str, file_path: &str) -> Result<(TemplateFrontmatter, String)> {
    let content = content.trim_start();

    // Check for frontmatter delimiter
    if !content.starts_with("---") {
        return Ok((TemplateFrontmatter::default(), content.to_string()));
    }

    // Find the closing delimiter
    let after_first = &content[3..];
    let end_idx = after_first
        .find("\n---")
        .or_else(|| after_first.find("\r\n---"));

    match end_idx {
        Some(idx) => {
            let yaml_content = &after_first[..idx];
            let body_start = idx + 4; // Skip "\n---"
            let body = after_first[body_start..].trim_start_matches(['\n', '\r']);

            // Parse YAML with error reporting
            let frontmatter: TemplateFrontmatter =
                serde_yaml::from_str(yaml_content).map_err(|e| {
                    AikiError::TemplateFrontmatterInvalid {
                        file: file_path.to_string(),
                        details: e.to_string(),
                    }
                })?;

            Ok((frontmatter, body.to_string()))
        }
        None => {
            // No closing delimiter found - error
            Err(AikiError::TemplateFrontmatterInvalid {
                file: file_path.to_string(),
                details: "Unterminated frontmatter: found opening '---' but no closing '---'"
                    .to_string(),
            })
        }
    }
}

/// Parse the markdown body into parent task and subtasks
fn parse_markdown_body(
    body: &str,
    file_path: &str,
) -> Result<(TaskDefinition, Vec<TaskDefinition>)> {
    let lines: Vec<&str> = body.lines().collect();

    // Find the first h1 heading (# Task Name)
    let (task_name, task_start_idx) = find_first_h1(&lines, file_path)?;

    // Find the "# Subtasks" marker
    let subtasks_marker_idx = find_subtasks_marker(&lines, task_start_idx);

    // Extract parent task instructions (between first h1 and # Subtasks or end)
    let parent_end = subtasks_marker_idx.unwrap_or(lines.len());
    let parent_instructions = extract_instructions(&lines, task_start_idx + 1, parent_end);

    let parent = TaskDefinition {
        name: task_name,
        slug: None,      // Parent tasks don't have slugs from templates
        task_type: None, // Set from frontmatter by resolver
        instructions: parent_instructions,
        priority: None,
        assignee: None,
        sources: Vec::new(),
        data: Default::default(),
        needs_context: None,
    };

    // Parse subtasks from H2 sections after the # Subtasks marker
    if let Some(marker_idx) = subtasks_marker_idx {
        let subtasks = parse_subtasks(&lines, marker_idx + 1, file_path)?;
        Ok((parent, subtasks))
    } else {
        Ok((parent, Vec::new()))
    }
}

/// Find the first h1 heading in the markdown
fn find_first_h1(lines: &[&str], file_path: &str) -> Result<(String, usize)> {
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("# ") && !trimmed.starts_with("## ") {
            let name = trimmed[2..].trim().to_string();
            return Ok((name, idx));
        }
    }

    Err(AikiError::TemplateStructureInvalid {
        file: file_path.to_string(),
        details: "Missing required '# ' heading for task name".to_string(),
    })
}

/// Find the "# Subtasks" marker line
fn find_subtasks_marker(lines: &[&str], start_idx: usize) -> Option<usize> {
    for (idx, line) in lines.iter().enumerate().skip(start_idx) {
        let trimmed = line.trim().to_lowercase();
        if trimmed == "# subtasks" {
            return Some(idx);
        }
    }
    None
}

/// Extract instructions from a range of lines
fn extract_instructions(lines: &[&str], start: usize, end: usize) -> String {
    if start >= end || start >= lines.len() {
        return String::new();
    }

    let end = end.min(lines.len());
    lines[start..end].join("\n").trim().to_string()
}

/// Parse subtasks from lines after the # Subtasks marker
fn parse_subtasks(
    lines: &[&str],
    start_idx: usize,
    file_path: &str,
) -> Result<Vec<TaskDefinition>> {
    let mut subtasks = Vec::new();
    let mut current_subtask: Option<(String, usize)> = None; // (name, start_line)

    for (idx, line) in lines.iter().enumerate().skip(start_idx) {
        let trimmed = line.trim();

        // Check for h2 heading (## Subtask Name)
        if trimmed.starts_with("## ") {
            // Finish previous subtask if any
            if let Some((name, start)) = current_subtask.take() {
                let subtask = parse_single_subtask(&name, &lines[start..idx], file_path)?;
                subtasks.push(subtask);
            }

            // Start new subtask
            let name = trimmed[3..].trim().to_string();
            current_subtask = Some((name, idx + 1));
        }
    }

    // Finish last subtask
    if let Some((name, start)) = current_subtask {
        let subtask = parse_single_subtask(&name, &lines[start..], file_path)?;
        subtasks.push(subtask);
    }

    Ok(subtasks)
}

/// Parse a single subtask from its content lines
fn parse_single_subtask(name: &str, lines: &[&str], file_path: &str) -> Result<TaskDefinition> {
    // Check for subtask frontmatter
    let (frontmatter, instructions) = extract_subtask_frontmatter(lines, file_path)?;

    Ok(TaskDefinition {
        name: name.to_string(),
        slug: frontmatter.slug,
        task_type: None, // Subtasks inherit type from parent
        instructions,
        priority: frontmatter.priority,
        assignee: frontmatter.assignee,
        sources: frontmatter.sources,
        data: frontmatter.data,
        needs_context: frontmatter.needs_context,
    })
}

/// Extract optional frontmatter from subtask content
fn extract_subtask_frontmatter(
    lines: &[&str],
    file_path: &str,
) -> Result<(SubtaskFrontmatter, String)> {
    let content = lines.join("\n");
    let (frontmatter, body) =
        extract_yaml_frontmatter::<SubtaskFrontmatter>(&content).map_err(|e| {
            AikiError::TemplateFrontmatterInvalid {
                file: file_path.to_string(),
                details: e.to_string(),
            }
        })?;
    Ok((frontmatter.unwrap_or_default(), body.trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_template() {
        let content = r#"
# Minimal task

Do something.

# Subtasks

## Do the work

Instructions here.
"#;

        let template = parse_template(content, "minimal", "minimal.md").unwrap();
        assert_eq!(template.name, "minimal");
        assert!(template.version.is_none());
        assert_eq!(template.parent.name, "Minimal task");
        assert!(template.parent.instructions.contains("Do something"));
        assert_eq!(template.subtasks.len(), 1);
        assert_eq!(template.subtasks[0].name, "Do the work");
        assert!(template.subtasks[0]
            .instructions
            .contains("Instructions here"));
    }

    #[test]
    fn test_parse_with_frontmatter() {
        let content = r#"---
version: "1.2.0"
description: Test template
type: review
assignee: claude-code
priority: p1
data:
  scope: "@"
---

# Review: {{data.scope}}

Review the changes.

# Subtasks

## Digest code

Examine the code.

## Review code

Review for issues.
"#;

        let template = parse_template(content, "review", "review.md").unwrap();
        assert_eq!(template.name, "review");
        assert_eq!(template.version, Some("1.2.0".to_string()));
        assert_eq!(template.description, Some("Test template".to_string()));
        assert_eq!(template.defaults.task_type, Some("review".to_string()));
        assert_eq!(template.defaults.assignee, Some("claude-code".to_string()));
        assert_eq!(template.defaults.priority, Some("p1".to_string()));
        assert_eq!(template.parent.name, "Review: {{data.scope}}");
        assert_eq!(template.subtasks.len(), 2);
        assert_eq!(template.subtasks[0].name, "Digest code");
        assert_eq!(template.subtasks[1].name, "Review code");
    }

    #[test]
    fn test_parse_no_subtasks() {
        let content = r#"
# Simple task

Just do it.
"#;

        let template = parse_template(content, "simple", "simple.md").unwrap();
        assert_eq!(template.parent.name, "Simple task");
        assert!(template.subtasks.is_empty());
    }

    #[test]
    fn test_parse_subtask_frontmatter() {
        let content = r#"
# Parent task

Instructions.

# Subtasks

## Urgent subtask
---
priority: p0
assignee: security-specialist
---

Do this urgently.

## Normal subtask

Do this normally.
"#;

        let template = parse_template(content, "test", "test.md").unwrap();
        assert_eq!(template.subtasks.len(), 2);

        let urgent = &template.subtasks[0];
        assert_eq!(urgent.name, "Urgent subtask");
        assert_eq!(urgent.priority, Some("p0".to_string()));
        assert_eq!(urgent.assignee, Some("security-specialist".to_string()));
        assert!(urgent.instructions.contains("Do this urgently"));

        let normal = &template.subtasks[1];
        assert_eq!(normal.name, "Normal subtask");
        assert!(normal.priority.is_none());
        assert!(normal.instructions.contains("Do this normally"));
    }

    #[test]
    fn test_parse_missing_h1() {
        let content = r#"
## Only h2 heading

Some content.
"#;

        let result = parse_template(content, "test", "test.md");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Missing required '# ' heading"));
    }

    #[test]
    fn test_parse_invalid_frontmatter() {
        let content = r#"---
invalid: yaml: :
---

# Task

Content.
"#;

        let result = parse_template(content, "test", "test.md");
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_frontmatter_no_closing() {
        let content = "---\nversion: 1.0\n\n# Task\n\nContent.";
        let result = extract_frontmatter(content, "test.md");
        // When no closing delimiter, error instead of silently ignoring
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Unterminated frontmatter"));
    }

    #[test]
    fn test_extract_yaml_frontmatter_no_closing() {
        let content = "---\nkey: value\n\n# Body content";
        let result = extract_yaml_frontmatter::<SubtaskFrontmatter>(content);
        // When no closing delimiter, error instead of silently ignoring
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, FrontmatterError::Unterminated));
        assert!(err.to_string().contains("Unterminated frontmatter"));
    }

    #[test]
    fn test_template_id() {
        let mut template = TaskTemplate::new("review");
        assert_eq!(template.template_id(), "review");

        template.version = Some("1.0.0".to_string());
        assert_eq!(template.template_id(), "review@1.0.0");
    }

    #[test]
    fn test_h2_before_subtasks_marker() {
        let content = r#"
# Parent task

## Section in parent

This h2 is part of the parent instructions.

More parent content.

# Subtasks

## Real subtask

Subtask instructions.
"#;

        let template = parse_template(content, "test", "test.md").unwrap();
        // The h2 before # Subtasks should be part of parent instructions
        assert!(template.parent.instructions.contains("Section in parent"));
        assert!(template
            .parent
            .instructions
            .contains("This h2 is part of the parent"));

        // Only one subtask should exist
        assert_eq!(template.subtasks.len(), 1);
        assert_eq!(template.subtasks[0].name, "Real subtask");
    }

    #[test]
    fn test_subtask_with_slug_frontmatter() {
        let content = r#"
# Build task

Parent instructions.

# Subtasks

## Build binary

---
slug: build
priority: p0
---

Build the binary.

## Run tests

---
slug: run-tests
---

Run all tests.
"#;

        let template = parse_template(content, "test", "test.md").unwrap();
        assert_eq!(template.subtasks.len(), 2);
        assert_eq!(template.subtasks[0].slug, Some("build".to_string()));
        assert_eq!(template.subtasks[0].priority, Some("p0".to_string()));
        assert_eq!(template.subtasks[1].slug, Some("run-tests".to_string()));
    }

    #[test]
    fn test_subtask_without_slug_frontmatter() {
        let content = r#"
# Build task

Parent instructions.

# Subtasks

## Step one

Do the first step.

## Step two

Do the second step.
"#;

        let template = parse_template(content, "test", "test.md").unwrap();
        assert_eq!(template.subtasks.len(), 2);
        assert!(template.subtasks[0].slug.is_none());
        assert!(template.subtasks[1].slug.is_none());
    }

    #[test]
    fn test_parse_template_with_spawns() {
        let content = r#"---
version: "1.0.0"
type: review
spawns:
  - when: not data.approved
    task:
      template: fix
      priority: p0
      data:
        max_iterations: 3
  - when: data.needs_analysis
    subtask:
      template: analysis
      assignee: claude-code
---

# Review task

Review the changes.
"#;

        let template = parse_template(content, "review", "review.md").unwrap();
        assert_eq!(template.spawns.len(), 2);

        // First spawn: standalone task
        assert_eq!(template.spawns[0].when, "not data.approved");
        assert!(template.spawns[0].task.is_some());
        assert!(template.spawns[0].subtask.is_none());
        let task_cfg = template.spawns[0].task.as_ref().unwrap();
        assert_eq!(task_cfg.template, "fix");
        assert_eq!(task_cfg.priority, Some("p0".to_string()));

        // Second spawn: subtask
        assert_eq!(template.spawns[1].when, "data.needs_analysis");
        assert!(template.spawns[1].task.is_none());
        assert!(template.spawns[1].subtask.is_some());
        let subtask_cfg = template.spawns[1].subtask.as_ref().unwrap();
        assert_eq!(subtask_cfg.template, "analysis");
        assert_eq!(subtask_cfg.assignee, Some("claude-code".to_string()));
    }

    #[test]
    fn test_parse_template_without_spawns() {
        let content = r#"---
version: "1.0.0"
---

# Simple task

Do something.
"#;

        let template = parse_template(content, "test", "test.md").unwrap();
        assert!(template.spawns.is_empty());
    }

    #[test]
    fn test_parse_template_spawn_both_task_and_subtask_rejected() {
        let content = r#"---
version: "1.0.0"
spawns:
  - when: not data.approved
    task:
      template: fix
    subtask:
      template: analysis
---

# Review task

Review the changes.
"#;

        let result = parse_template(content, "review", "review.md");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("both 'task' and 'subtask'"), "Error: {}", err);
    }

    #[test]
    fn test_parse_template_spawn_neither_task_nor_subtask_rejected() {
        let content = r#"---
version: "1.0.0"
spawns:
  - when: not data.approved
---

# Review task

Review the changes.
"#;

        let result = parse_template(content, "review", "review.md");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("neither 'task' nor 'subtask'"),
            "Error: {}",
            err
        );
    }

    #[test]
    fn test_template_level_slug_propagates_to_parent() {
        let content = r#"---
slug: criteria
---

# Understand Criteria: Code

Evaluate the implementation.
"#;

        let template = parse_template(content, "review/criteria/code", "code.md").unwrap();
        assert_eq!(template.parent.slug, Some("criteria".to_string()));
        assert_eq!(template.parent.name, "Understand Criteria: Code");
    }

    #[test]
    fn test_template_without_slug_has_none() {
        let content = r#"---
version: "1.0.0"
---

# Simple task

Do something.
"#;

        let template = parse_template(content, "test", "test.md").unwrap();
        assert!(template.parent.slug.is_none());
    }

    #[test]
    fn test_frontmatter_slug_deserialize() {
        let yaml = r#"
slug: criteria
version: "1.0.0"
"#;
        let fm: TemplateFrontmatter = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(fm.slug, Some("criteria".to_string()));
        assert_eq!(fm.version, Some("1.0.0".to_string()));
    }

    #[test]
    fn test_parse_template_with_loop_desugars_to_spawn() {
        let content = r#"---
version: "1.0.0"
loop:
  until: data.approved
---

# Looping task

Do something repeatedly.
"#;
        let template = parse_template(content, "test", "test.md").unwrap();
        assert_eq!(template.spawns.len(), 1);
        let entry = &template.spawns[0];
        assert_eq!(entry.when, "not (data.approved)");
        // Default max_iterations (100) should be passed through
        assert_eq!(entry.max_iterations, Some(100));
        let task_cfg = entry.task.as_ref().unwrap();
        assert_eq!(task_cfg.template, "self");
        assert!(task_cfg.autorun);
    }

    #[test]
    fn test_parse_template_with_loop_explicit_max_iterations() {
        let content = r#"---
loop:
  until: subtasks.review.approved
  max_iterations: 5
---

# Fix loop

Fix and review.
"#;
        let template = parse_template(content, "test", "test.md").unwrap();
        assert_eq!(template.spawns.len(), 1);
        let entry = &template.spawns[0];
        // when condition should only contain the user's until expression
        assert_eq!(entry.when, "not (subtasks.review.approved)");
        // Explicit max_iterations should be passed through
        assert_eq!(entry.max_iterations, Some(5));
        // Loop metadata should still be present
        let task_cfg = entry.task.as_ref().unwrap();
        assert!(task_cfg.data.contains_key("loop.index"));
        assert!(task_cfg.data.contains_key("loop.index1"));
        assert!(task_cfg.data.contains_key("loop.first"));
    }

    #[test]
    fn test_parse_template_with_loop_and_spawns() {
        let content = r#"---
version: "1.0.0"
loop:
  until: data.approved
spawns:
  - when: "not data.approved"
    task:
      template: fix
---

# Looping task with spawns

Do something.
"#;
        let template = parse_template(content, "test", "test.md").unwrap();
        // Explicit spawn + desugared loop spawn
        assert_eq!(template.spawns.len(), 2);
        assert_eq!(template.spawns[0].when, "not data.approved");
        assert_eq!(template.spawns[0].task.as_ref().unwrap().template, "fix");
        assert_eq!(template.spawns[1].when, "not (data.approved)");
        assert_eq!(template.spawns[1].task.as_ref().unwrap().template, "self");
        assert!(template.spawns[1].task.as_ref().unwrap().autorun);
    }

    #[test]
    fn test_parse_template_with_loop_data() {
        let content = r#"---
version: "1.0.0"
loop:
  until: data.iterations >= 5
  data:
    max_retries: 3
---

# Looping task with data

Do something.
"#;
        let template = parse_template(content, "test", "test.md").unwrap();
        assert_eq!(template.spawns.len(), 1);
        let task_cfg = template.spawns[0].task.as_ref().unwrap();
        assert_eq!(task_cfg.template, "self");
        assert!(task_cfg.autorun);
        // 1 user data + 3 loop metadata (loop.index, loop.index1, loop.first)
        assert_eq!(task_cfg.data.len(), 4);
        assert!(task_cfg.data.contains_key("max_retries"));
        assert!(task_cfg.data.contains_key("loop.index"));
        assert!(task_cfg.data.contains_key("loop.index1"));
        assert!(task_cfg.data.contains_key("loop.first"));

        // Verify initial loop defaults are set on template
        assert_eq!(
            template.defaults.data.get("loop.index"),
            Some(&serde_json::json!(0))
        );
        assert_eq!(
            template.defaults.data.get("loop.index1"),
            Some(&serde_json::json!(1))
        );
        assert_eq!(
            template.defaults.data.get("loop.first"),
            Some(&serde_json::json!(true))
        );
    }
}
