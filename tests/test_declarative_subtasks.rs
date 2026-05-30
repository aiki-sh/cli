//! Integration tests for declarative subtasks in the template system
//!
//! These tests verify the full workflow from template parsing to task creation:
//! 1. Template parsing with `subtasks` frontmatter
//! 2. Variable substitution in parent task and subtasks
//! 3. Dynamic subtask creation from data sources
//! 4. Static subtask creation for templates without `subtasks` frontmatter
//! 5. Edge cases (empty data sources, missing sections, etc.)

use aiki::tasks::templates::{
    create_tasks_from_template, parse_data_source, parse_template, resolve_data_source,
    DataSource, TaskTemplate, VariableContext,
};
use aiki::tasks::types::{FastHashMap, Task, TaskComment, TaskPriority, TaskStatus};
use chrono::Utc;
use std::collections::HashMap;

// =============================================================================
// Helper functions
// =============================================================================

/// Create a test task with the given ID and comments
fn create_test_task(id: &str, name: &str, comments: Vec<TaskComment>) -> Task {
    Task {
        id: id.to_string(),
        name: name.to_string(),
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
        comments,
    }
}

/// Create a simple task comment
fn create_comment(text: &str) -> TaskComment {
    TaskComment {
        id: None,
        text: text.to_string(),
        timestamp: Utc::now(),
        data: HashMap::new(),
    }
}

/// Create a task comment with data metadata
fn create_comment_with_data(text: &str, data: HashMap<String, String>) -> TaskComment {
    TaskComment {
        id: None,
        text: text.to_string(),
        timestamp: Utc::now(),
        data,
    }
}

// =============================================================================
// Tests for parse_data_source
// =============================================================================

#[test]
fn test_parse_data_source_valid_source_comments() {
    let result = parse_data_source("source.comments");
    assert_eq!(result.unwrap(), DataSource::Comments);
}

#[test]
fn test_parse_data_source_with_whitespace() {
    let result = parse_data_source("  source.comments  ");
    assert_eq!(result.unwrap(), DataSource::Comments);
}

#[test]
fn test_parse_data_source_invalid_strings() {
    assert!(parse_data_source("invalid").is_err());
    assert!(parse_data_source("source").is_err());
    assert!(parse_data_source("source.").is_err());
    assert!(parse_data_source("comments").is_err());
    assert!(parse_data_source("source.files").is_err()); // Future data source, not yet implemented
    assert!(parse_data_source("").is_err());
}

// =============================================================================
// Tests for resolve_data_source
// =============================================================================

#[test]
fn test_resolve_data_source_with_empty_comments() {
    let mut tasks = FastHashMap::default();
    let task = create_test_task("task123", "Test task", vec![]);
    tasks.insert("task123".to_string(), task);

    let result = resolve_data_source(&DataSource::Comments, "task123", &tasks).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_resolve_data_source_with_single_comment() {
    let mut tasks = FastHashMap::default();
    let task = create_test_task(
        "task123",
        "Test task",
        vec![create_comment("Fix the authentication bug on line 42")],
    );
    tasks.insert("task123".to_string(), task);

    let result = resolve_data_source(&DataSource::Comments, "task123", &tasks).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].text, "Fix the authentication bug on line 42");
}

#[test]
fn test_resolve_data_source_with_multiple_comments() {
    let mut tasks = FastHashMap::default();
    let task = create_test_task(
        "task123",
        "Code review",
        vec![
            create_comment("Missing null check in auth handler"),
            create_comment("Consider extracting validation to helper"),
            create_comment("Add error handling for timeout case"),
        ],
    );
    tasks.insert("task123".to_string(), task);

    let result = resolve_data_source(&DataSource::Comments, "task123", &tasks).unwrap();
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].text, "Missing null check in auth handler");
    assert_eq!(result[1].text, "Consider extracting validation to helper");
    assert_eq!(result[2].text, "Add error handling for timeout case");
}

#[test]
fn test_resolve_data_source_with_comment_text() {
    let mut tasks = FastHashMap::default();
    let task = create_test_task(
        "review1",
        "Security review",
        vec![create_comment("Potential null pointer")],
    );
    tasks.insert("review1".to_string(), task);

    let result = resolve_data_source(&DataSource::Comments, "review1", &tasks).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].text, "Potential null pointer");
}

#[test]
fn test_resolve_data_source_task_not_found() {
    let tasks = FastHashMap::default();

    let result = resolve_data_source(&DataSource::Comments, "nonexistent", &tasks);
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert!(err.to_string().contains("nonexistent"));
}

// =============================================================================
// Tests for template parsing with subtasks frontmatter
// =============================================================================

#[test]
fn test_parse_template_with_subtasks_source() {
    let template_content = r#"---
version: 1.0.0
subtasks: source.comments
---

# Followup: {data.scope}

Fix issues identified in the review.

# Subtasks

## Fix: {data.file}:{data.line}

**Severity**: {data.severity}

{text}
"#;

    let template = parse_template(template_content, "followup", "followup.md").unwrap();

    // Verify subtasks_source is set
    assert_eq!(
        template.subtasks_source,
        Some("source.comments".to_string())
    );

    // Verify static subtasks are NOT parsed when subtasks_source is present
    assert!(
        template.subtasks.is_empty(),
        "Static subtasks should be empty when subtasks_source is set"
    );

    // Verify subtask_template is extracted
    assert!(template.subtask_template.is_some());
    let subtask_template = template.subtask_template.as_ref().unwrap();
    assert!(subtask_template.contains("## Fix: {data.file}:{data.line}"));
    assert!(subtask_template.contains("{text}"));
    assert!(subtask_template.contains("**Severity**: {data.severity}"));

    // Verify parent task is parsed correctly
    assert_eq!(template.parent.name, "Followup: {data.scope}");
    assert!(template
        .parent
        .instructions
        .contains("Fix issues identified"));
}

#[test]
fn test_parse_template_without_subtasks_source() {
    let template_content = r#"---
version: 1.0.0
description: Standard code review
type: review
---

# Review: {data.scope}

Review the specified code.

# Subtasks

## Digest code

Examine and understand the code structure.

## Identify issues

Look for bugs and improvements.

## Write summary

Document your findings.
"#;

    let template = parse_template(template_content, "review", "review.md").unwrap();

    // Verify subtasks_source is NOT set
    assert!(template.subtasks_source.is_none());

    // Verify subtask_template is NOT set
    assert!(template.subtask_template.is_none());

    // Verify static subtasks ARE parsed
    assert_eq!(template.subtasks.len(), 3);
    assert_eq!(template.subtasks[0].name, "Digest code");
    assert_eq!(template.subtasks[1].name, "Identify issues");
    assert_eq!(template.subtasks[2].name, "Write summary");
}

#[test]
fn test_parse_template_with_subtasks_source_no_section() {
    // Template has subtasks: source.comments but no # Subtasks section
    let template_content = r#"---
version: 1.0.0
subtasks: source.comments
---

# Simple task

Just do the work based on comments.
"#;

    let template = parse_template(template_content, "simple", "simple.md").unwrap();

    // subtasks_source should still be set
    assert_eq!(
        template.subtasks_source,
        Some("source.comments".to_string())
    );

    // But subtask_template should be None since there's no section
    assert!(template.subtask_template.is_none());
}

// =============================================================================
// Tests for variable substitution in templates
// =============================================================================

#[test]
fn test_variable_substitution_in_parent_task() {
    let template_content = r#"---
version: 1.0.0
---

# Review: {data.scope}

You are reviewing **{data.scope}** for the {data.purpose} project.

Priority: {data.priority}

# Subtasks

## Check formatting

Verify code style.
"#;

    let template = parse_template(template_content, "test", "test.md").unwrap();

    let mut variables = VariableContext::new();
    variables.set_data("scope", "src/auth.rs");
    variables.set_data("purpose", "security audit");
    variables.set_data("priority", "high");

    let (parent, subtasks) = create_tasks_from_template(&template, &variables, None).unwrap();

    assert_eq!(parent.name, "Review: src/auth.rs");
    assert!(parent
        .instructions
        .contains("You are reviewing **src/auth.rs**"));
    assert!(parent.instructions.contains("security audit project"));
    assert!(parent.instructions.contains("Priority: high"));

    assert_eq!(subtasks.len(), 1);
    assert_eq!(subtasks[0].name, "Check formatting");
}

#[test]
fn test_variable_substitution_in_static_subtasks() {
    let template_content = r#"---
version: 1.0.0
---

# Task for {data.module}

Review the module.

# Subtasks

## Review {data.module}

Check {data.module} for issues.

## Test {data.module}

Run tests for {data.module}.
"#;

    let template = parse_template(template_content, "test", "test.md").unwrap();

    let mut variables = VariableContext::new();
    variables.set_data("module", "authentication");

    let (parent, subtasks) = create_tasks_from_template(&template, &variables, None).unwrap();

    assert_eq!(parent.name, "Task for authentication");
    assert_eq!(subtasks.len(), 2);
    assert_eq!(subtasks[0].name, "Review authentication");
    assert!(subtasks[0].instructions.contains("Check authentication"));
    assert_eq!(subtasks[1].name, "Test authentication");
    assert!(subtasks[1].instructions.contains("Run tests for authentication"));
}

// =============================================================================
// Tests for dynamic subtask creation from data source
// =============================================================================

#[test]
fn test_full_declarative_subtask_workflow() {
    // 1. Create a template with subtasks: source.comments
    let template_content = r#"---
version: 1.0.0
subtasks: source.comments
---

# Followup: {data.scope}

Fix all issues from the review.

# Subtasks

## Fix: {data.file}:{data.line}

**Severity**: {data.severity}
**Category**: {data.category}

{text}
"#;

    let template = parse_template(template_content, "followup", "followup.md").unwrap();

    // 2. Create a mock task with comments (simulating a review task)
    let mut comment1_data = HashMap::new();
    comment1_data.insert("file".to_string(), "src/auth.rs".to_string());
    comment1_data.insert("line".to_string(), "42".to_string());
    comment1_data.insert("severity".to_string(), "error".to_string());
    comment1_data.insert("category".to_string(), "null-safety".to_string());

    let mut comment2_data = HashMap::new();
    comment2_data.insert("file".to_string(), "src/utils.rs".to_string());
    comment2_data.insert("line".to_string(), "15".to_string());
    comment2_data.insert("severity".to_string(), "warning".to_string());
    comment2_data.insert("category".to_string(), "unused-import".to_string());

    let mut tasks = FastHashMap::default();
    let task = create_test_task(
        "review123",
        "Code review",
        vec![
            create_comment_with_data("Missing null check", comment1_data),
            create_comment_with_data("Remove unused import", comment2_data),
        ],
    );
    tasks.insert("review123".to_string(), task);

    // 3. Resolve the data source
    let data_source = parse_data_source(template.subtasks_source.as_ref().unwrap()).unwrap();
    let data_items = resolve_data_source(&data_source, "review123", &tasks).unwrap();

    // 4. Create tasks from template with the data source
    let mut variables = VariableContext::new();
    variables.set_data("scope", "authentication module");

    let (parent, subtasks) =
        create_tasks_from_template(&template, &variables, Some(data_items)).unwrap();

    // 5. Verify the results
    assert_eq!(parent.name, "Followup: authentication module");
    assert!(parent.instructions.contains("Fix all issues from the review"));

    assert_eq!(subtasks.len(), 2);

    // First subtask
    assert_eq!(subtasks[0].name, "Fix: src/auth.rs:42");
    assert!(subtasks[0].instructions.contains("**Severity**: error"));
    assert!(subtasks[0].instructions.contains("**Category**: null-safety"));
    assert!(subtasks[0].instructions.contains("Missing null check"));

    // Second subtask
    assert_eq!(subtasks[1].name, "Fix: src/utils.rs:15");
    assert!(subtasks[1].instructions.contains("**Severity**: warning"));
    assert!(subtasks[1]
        .instructions
        .contains("**Category**: unused-import"));
    assert!(subtasks[1].instructions.contains("Remove unused import"));
}

#[test]
fn test_dynamic_subtasks_with_text_only() {
    // Template that uses {text} without structured data fields
    let template_content = r#"---
version: 1.0.0
subtasks: source.comments
---

# Followup task

Address all comments.

# Subtasks

## Address issue

{text}
"#;

    let template = parse_template(template_content, "simple-followup", "simple-followup.md").unwrap();

    let mut tasks = FastHashMap::default();
    let task = create_test_task(
        "review1",
        "Simple review",
        vec![
            create_comment("Fix the typo in the error message"),
            create_comment("Add documentation for public API"),
            create_comment("Consider refactoring the loop"),
        ],
    );
    tasks.insert("review1".to_string(), task);

    let data_source = parse_data_source("source.comments").unwrap();
    let data_items = resolve_data_source(&data_source, "review1", &tasks).unwrap();

    let variables = VariableContext::new();
    let (_, subtasks) = create_tasks_from_template(&template, &variables, Some(data_items)).unwrap();

    assert_eq!(subtasks.len(), 3);
    assert!(subtasks[0]
        .instructions
        .contains("Fix the typo in the error message"));
    assert!(subtasks[1]
        .instructions
        .contains("Add documentation for public API"));
    assert!(subtasks[2]
        .instructions
        .contains("Consider refactoring the loop"));
}

// =============================================================================
// Edge case tests
// =============================================================================

#[test]
fn test_empty_data_source_produces_no_subtasks() {
    let template_content = r#"---
version: 1.0.0
subtasks: source.comments
---

# Followup

Fix issues.

# Subtasks

## Fix: {data.file}

{text}
"#;

    let template = parse_template(template_content, "followup", "followup.md").unwrap();

    let mut tasks = FastHashMap::default();
    let task = create_test_task("review1", "Empty review", vec![]); // No comments
    tasks.insert("review1".to_string(), task);

    let data_source = parse_data_source("source.comments").unwrap();
    let data_items = resolve_data_source(&data_source, "review1", &tasks).unwrap();

    let variables = VariableContext::new();
    let (parent, subtasks) =
        create_tasks_from_template(&template, &variables, Some(data_items)).unwrap();

    // Parent should still be created
    assert_eq!(parent.name, "Followup");

    // But no subtasks when data source is empty
    assert!(
        subtasks.is_empty(),
        "Subtasks should be empty when data source has no items"
    );
}

#[test]
fn test_none_data_source_produces_no_subtasks() {
    let mut template = TaskTemplate::new("test/dynamic");
    template.parent.name = "Review".to_string();
    template.parent.instructions = "Review all issues.".to_string();
    template.subtasks_source = Some("source.comments".to_string());
    template.subtask_template = Some("## Fix: {data.file}\n\n{text}".to_string());

    let variables = VariableContext::new();

    // Pass None as data source
    let (_, subtasks) = create_tasks_from_template(&template, &variables, None).unwrap();
    assert!(subtasks.is_empty());
}

#[test]
fn test_missing_subtask_template_section() {
    // Template has subtasks_source but subtask_template is None
    let mut template = TaskTemplate::new("test/no-template");
    template.parent.name = "Task".to_string();
    template.parent.instructions = "Do work.".to_string();
    template.subtasks_source = Some("source.comments".to_string());
    template.subtask_template = None; // No template section

    let comments = vec![create_comment("Some issue")];

    let variables = VariableContext::new();
    let (_, subtasks) =
        create_tasks_from_template(&template, &variables, Some(comments)).unwrap();

    // Should produce no subtasks when template section is missing
    assert!(subtasks.is_empty());
}

#[test]
fn test_missing_variable_in_template() {
    let template_content = r#"---
version: 1.0.0
---

# Review: {data.scope}

Do the review.
"#;

    let template = parse_template(template_content, "test", "test.md").unwrap();

    // Don't set data.scope
    let variables = VariableContext::new();

    let result = create_tasks_from_template(&template, &variables, None);
    assert!(result.is_err());

    let err = result.unwrap_err();
    assert!(err.to_string().contains("data.scope"));
}

#[test]
fn test_static_subtasks_not_affected_by_data_source() {
    // Static template (no subtasks frontmatter)
    let template_content = r#"---
version: 1.0.0
---

# Static task

Do work.

# Subtasks

## First

First subtask.

## Second

Second subtask.
"#;

    let template = parse_template(template_content, "static", "static.md").unwrap();

    // Even if we pass a data source, it should be ignored for static templates
    let comments = vec![create_comment("This should be ignored")];

    let variables = VariableContext::new();
    let (_, subtasks) =
        create_tasks_from_template(&template, &variables, Some(comments)).unwrap();

    // Should have the static subtasks, not dynamic ones
    assert_eq!(subtasks.len(), 2);
    assert_eq!(subtasks[0].name, "First");
    assert_eq!(subtasks[1].name, "Second");
}

#[test]
fn test_complex_subtask_template_multiline() {
    let template_content = r#"---
version: 1.0.0
subtasks: source.comments
---

# Review followup

Fix all issues.

# Subtasks

## Fix issue in {data.file}

**Location**: Line {data.line}
**Severity**: {data.severity}

### Description

{text}

### Steps to fix

1. Open {data.file}
2. Go to line {data.line}
3. Apply the fix
"#;

    let template = parse_template(template_content, "complex", "complex.md").unwrap();

    let mut comment_data = HashMap::new();
    comment_data.insert("file".to_string(), "src/main.rs".to_string());
    comment_data.insert("line".to_string(), "100".to_string());
    comment_data.insert("severity".to_string(), "critical".to_string());

    let mut tasks = FastHashMap::default();
    let task = create_test_task(
        "review1",
        "Complex review",
        vec![create_comment_with_data(
            "Memory leak in resource handler",
            comment_data,
        )],
    );
    tasks.insert("review1".to_string(), task);

    let data_items = resolve_data_source(&DataSource::Comments, "review1", &tasks).unwrap();

    let variables = VariableContext::new();
    let (_, subtasks) =
        create_tasks_from_template(&template, &variables, Some(data_items)).unwrap();

    assert_eq!(subtasks.len(), 1);
    assert_eq!(subtasks[0].name, "Fix issue in src/main.rs");

    let instructions = &subtasks[0].instructions;
    assert!(instructions.contains("**Location**: Line 100"));
    assert!(instructions.contains("**Severity**: critical"));
    assert!(instructions.contains("Memory leak in resource handler"));
    assert!(instructions.contains("1. Open src/main.rs"));
    assert!(instructions.contains("2. Go to line 100"));
}

// =============================================================================
// Tests for VariableContext behavior with dynamic subtasks
// =============================================================================

#[test]
fn test_variable_context_merges_parent_and_item_data() {
    let mut template = TaskTemplate::new("test/merge");
    template.parent.name = "Review: {data.project}".to_string();
    template.parent.instructions = "Review {data.project}.".to_string();
    template.subtasks_source = Some("source.comments".to_string());
    template.subtask_template =
        Some("## {data.project}: {data.file}\n\nProject: {data.project}\n\n{text}".to_string());

    // Parent variable
    let mut variables = VariableContext::new();
    variables.set_data("project", "MyApp");

    // Comment with file data
    let mut comment_data = HashMap::new();
    comment_data.insert("file".to_string(), "app.rs".to_string());
    let comments = vec![create_comment_with_data("Fix the bug", comment_data)];

    let (parent, subtasks) =
        create_tasks_from_template(&template, &variables, Some(comments)).unwrap();

    // Parent uses project variable
    assert_eq!(parent.name, "Review: MyApp");

    // Subtask uses both project (from parent) and file/text (from item)
    assert_eq!(subtasks.len(), 1);
    assert_eq!(subtasks[0].name, "MyApp: app.rs");
    assert!(subtasks[0].instructions.contains("Project: MyApp"));
    assert!(subtasks[0].instructions.contains("Fix the bug"));
}

#[test]
fn test_item_data_overrides_parent_data() {
    let mut template = TaskTemplate::new("test/override");
    template.parent.name = "Task".to_string();
    template.parent.instructions = "Instructions.".to_string();
    template.subtasks_source = Some("source.comments".to_string());
    template.subtask_template = Some("## Check {data.scope}\n\n{text}".to_string());

    // Parent sets scope to "global"
    let mut variables = VariableContext::new();
    variables.set_data("scope", "global");

    // Comment also has scope="local" - should override parent
    let mut comment_data = HashMap::new();
    comment_data.insert("scope".to_string(), "local".to_string());
    let comments = vec![create_comment_with_data("Check this", comment_data)];

    let (_, subtasks) =
        create_tasks_from_template(&template, &variables, Some(comments)).unwrap();

    assert_eq!(subtasks.len(), 1);
    // Comment's scope ("local") should override parent's scope ("global")
    assert_eq!(subtasks[0].name, "Check local");
}

#[test]
fn test_parent_prefixed_variables_in_dynamic_subtasks() {
    // Test that parent.* prefixed variables are available in dynamic subtasks
    let mut template = TaskTemplate::new("test/parent-prefix");
    template.parent.name = "Review: {data.scope}".to_string();
    template.parent.instructions = "Review instructions.".to_string();
    template.subtasks_source = Some("source.comments".to_string());
    // Use {parent.data.scope} to access parent's scope, and {data.file} for item's file
    template.subtask_template =
        Some("## [{parent.data.scope}] Fix: {data.file}\n\nScope: {parent.data.scope}\nFile: {data.file}\n\n{text}".to_string());

    // Parent sets scope
    let mut variables = VariableContext::new();
    variables.set_data("scope", "auth-module");
    variables.set_builtin("id", "parent123");
    variables.set_builtin("priority", "p1");

    // Comment has file data
    let mut comment_data = HashMap::new();
    comment_data.insert("file".to_string(), "src/login.rs".to_string());
    let comments = vec![create_comment_with_data("Fix null check", comment_data)];

    let (_, subtasks) =
        create_tasks_from_template(&template, &variables, Some(comments)).unwrap();

    assert_eq!(subtasks.len(), 1);
    // Name should use parent.data.scope
    assert_eq!(subtasks[0].name, "[auth-module] Fix: src/login.rs");
    // Instructions should contain both parent and item data
    assert!(subtasks[0].instructions.contains("Scope: auth-module"));
    assert!(subtasks[0].instructions.contains("File: src/login.rs"));
    assert!(subtasks[0].instructions.contains("Fix null check"));
}

#[test]
fn test_parent_builtin_variables_in_dynamic_subtasks() {
    // Test that parent builtins (id, priority, etc.) are available via parent.* prefix
    let mut template = TaskTemplate::new("test/parent-builtins");
    template.parent.name = "Task".to_string();
    template.parent.instructions = "Instructions.".to_string();
    template.subtasks_source = Some("source.comments".to_string());
    template.subtask_template =
        Some("## Fix for {parent.id}\n\nParent: {parent.id}, Priority: {parent.priority}\n\n{text}".to_string());

    let mut variables = VariableContext::new();
    variables.set_builtin("id", "task_abc123");
    variables.set_builtin("priority", "p0");

    let comments = vec![create_comment("Check this issue")];

    let (_, subtasks) =
        create_tasks_from_template(&template, &variables, Some(comments)).unwrap();

    assert_eq!(subtasks.len(), 1);
    assert_eq!(subtasks[0].name, "Fix for task_abc123");
    assert!(subtasks[0].instructions.contains("Parent: task_abc123"));
    assert!(subtasks[0].instructions.contains("Priority: p0"));
}

// =============================================================================
// Tests for subtask frontmatter with sources
// =============================================================================

#[test]
fn test_dynamic_subtask_with_frontmatter() {
    let template_content = r#"---
version: 1.0.0
subtasks: source.comments
---

# Followup: {data.scope}

Fix all issues.

# Subtasks

## Fix: {data.file}
---
sources:
  - task:{data.source_task}
priority: p1
---

{text}
"#;

    let template = parse_template(template_content, "followup", "followup.md").unwrap();

    let mut comment_data = HashMap::new();
    comment_data.insert("file".to_string(), "src/auth.rs".to_string());
    comment_data.insert("source_task".to_string(), "review123".to_string());
    let comments = vec![create_comment_with_data("Fix null check", comment_data)];

    let mut variables = VariableContext::new();
    variables.set_data("scope", "auth module");

    let (parent, subtasks) =
        create_tasks_from_template(&template, &variables, Some(comments)).unwrap();

    assert_eq!(parent.name, "Followup: auth module");
    assert_eq!(subtasks.len(), 1);

    let subtask = &subtasks[0];
    assert_eq!(subtask.name, "Fix: src/auth.rs");
    assert_eq!(subtask.priority, Some("p1".to_string()));
    assert_eq!(subtask.sources.len(), 1);
    assert_eq!(subtask.sources[0], "task:review123");
    assert!(subtask.instructions.contains("Fix null check"));
}

#[test]
fn test_dynamic_subtask_frontmatter_with_data_field() {
    let template_content = r#"---
version: 1.0.0
subtasks: source.comments
---

# Task

Do work.

# Subtasks

## Fix issue
---
data:
  original_file: "{data.file}"
  severity: "{data.severity}"
---

{text}
"#;

    let template = parse_template(template_content, "test", "test.md").unwrap();

    let mut comment_data = HashMap::new();
    comment_data.insert("file".to_string(), "src/main.rs".to_string());
    comment_data.insert("severity".to_string(), "error".to_string());
    let comments = vec![create_comment_with_data("Memory leak", comment_data)];

    let variables = VariableContext::new();

    let (_, subtasks) =
        create_tasks_from_template(&template, &variables, Some(comments)).unwrap();

    assert_eq!(subtasks.len(), 1);
    let subtask = &subtasks[0];

    // Data field values should have variables substituted
    assert_eq!(
        subtask.data.get("original_file"),
        Some(&serde_json::json!("src/main.rs"))
    );
    assert_eq!(
        subtask.data.get("severity"),
        Some(&serde_json::json!("error"))
    );
}
