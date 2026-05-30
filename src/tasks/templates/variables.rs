//! Variable substitution for task templates
//!
//! Provides safe, single-pass variable substitution with these guarantees:
//! - Single-pass evaluation: Variables are substituted once, values are never re-evaluated
//! - No recursion: Values containing {{braces}} are inserted as literal text
//! - Safe by design: Substitution happens after frontmatter and conditional parsing
//! - Deterministic: Same inputs always produce same output
//!
//! # Syntax
//!
//! Uses Tera-style double-brace syntax for variables:
//! - `{{var}}` - Substitute built-in variable
//! - `{{data.key}}` - Substitute data variable
//! - `{{source}}` - Substitute source variable
//! - `{{parent.key}}` - Substitute parent task variable (in subtasks)
//! - `{{item.key}}` - Substitute iteration item variable (in dynamic subtasks)

use crate::error::{AikiError, Result};
use std::collections::HashMap;

/// Coerce a string value to a typed serde_json::Value
///
/// Type coercion rules:
/// - "true" / "false" (case-insensitive) → boolean
/// - Numeric strings (integers and floats) → number
/// - Everything else → string
///
/// # Examples
/// ```
/// use aiki::tasks::templates::variables::coerce_value;
///
/// assert_eq!(coerce_value("true"), serde_json::json!(true));
/// assert_eq!(coerce_value("42"), serde_json::json!(42));
/// assert_eq!(coerce_value("3.14"), serde_json::json!(3.14));
/// assert_eq!(coerce_value("hello"), serde_json::json!("hello"));
/// ```
#[must_use]
pub fn coerce_value(value: &str) -> serde_json::Value {
    // Try boolean
    match value.to_lowercase().as_str() {
        "true" => return serde_json::Value::Bool(true),
        "false" => return serde_json::Value::Bool(false),
        _ => {}
    }

    // Try integer
    if let Ok(n) = value.parse::<i64>() {
        return serde_json::json!(n);
    }

    // Try float
    if let Ok(n) = value.parse::<f64>() {
        return serde_json::json!(n);
    }

    // Default to string
    serde_json::Value::String(value.to_string())
}

/// Convert a string to a typed value and back to string representation
///
/// This is useful when you want to normalize values (e.g., "TRUE" → "true")
/// while keeping the storage as strings.
#[must_use]
pub fn coerce_to_string(value: &str) -> String {
    let typed = coerce_value(value);
    match typed {
        serde_json::Value::String(s) => s,
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => value.to_string(),
    }
}

/// Context for variable substitution
#[derive(Debug, Clone, Default)]
pub struct VariableContext {
    /// Built-in variables (id, assignee, priority, type, created)
    pub builtins: HashMap<String, String>,
    /// Data variables (accessible as {data.key})
    pub data: HashMap<String, String>,
    /// Source variable (accessible as {source})
    pub source: Option<String>,
    /// Source data variables (accessible as {source.key})
    /// Populated by parsing the source string (e.g., "task:abc123" → source.id = "abc123")
    pub source_data: HashMap<String, String>,
    /// Parent task variables (accessible as {parent.key})
    /// Used when creating subtasks to reference parent task properties
    pub parent: HashMap<String, String>,
    /// Item variables (accessible as {item.key})
    /// Used when iterating over a data source to create subtasks
    /// Contains the current item's data (e.g., {item.text}, {item.file}, {item.line})
    pub item: HashMap<String, String>,
}

impl VariableContext {
    /// Create a new empty context
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a built-in variable
    pub fn set_builtin(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.builtins.insert(key.into(), value.into());
    }

    /// Set a data variable
    pub fn set_data(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.data.insert(key.into(), value.into());
    }

    /// Set the source variable
    pub fn set_source(&mut self, value: impl Into<String>) {
        let source_str = value.into();
        self.source = Some(source_str.clone());

        // Clear previous source_data to avoid stale values from prior set_source calls
        self.source_data.clear();

        // Parse source string to populate source_data
        // Format: "prefix:value" (e.g., "task:abc123", "file:ops/now/plan.md")
        if let Some((prefix, value)) = source_str.split_once(':') {
            match prefix {
                "task" | "comment" | "issue" | "prompt" => {
                    self.source_data.insert("id".to_string(), value.to_string());
                    self.source_data.insert("type".to_string(), prefix.to_string());
                }
                "file" => {
                    self.source_data.insert("id".to_string(), value.to_string());
                    self.source_data.insert("path".to_string(), value.to_string());
                    self.source_data.insert("type".to_string(), "file".to_string());
                }
                _ => {
                    // Unknown prefix, just store the raw value
                    self.source_data.insert("value".to_string(), value.to_string());
                    self.source_data.insert("type".to_string(), prefix.to_string());
                }
            }
        }
    }

    /// Set a source data variable directly
    pub fn set_source_data(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.source_data.insert(key.into(), value.into());
    }

    /// Set a parent variable (accessible as {parent.key})
    pub fn set_parent(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.parent.insert(key.into(), value.into());
    }

    /// Set an item variable (accessible as {item.key})
    /// Used when iterating over a data source to create subtasks
    pub fn set_item(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.item.insert(key.into(), value.into());
    }

    /// Resolve a variable reference to its value
    fn resolve(&self, var_ref: &str) -> Option<String> {
        // Check for data.* variables first
        if let Some(key) = var_ref.strip_prefix("data.") {
            return self.data.get(key).cloned();
        }

        // Check for source.* variables
        if let Some(key) = var_ref.strip_prefix("source.") {
            return self.source_data.get(key).cloned();
        }

        // Check for parent.* variables
        if let Some(key) = var_ref.strip_prefix("parent.") {
            return self.parent.get(key).cloned();
        }

        // Check for item.* variables (used in subtask iteration)
        if let Some(key) = var_ref.strip_prefix("item.") {
            return self.item.get(key).cloned();
        }

        // Check for spawner.* variables (populated from spawn data for spawned tasks)
        // These are stored as "spawner.id", "spawner.priority", etc. in the data map
        if var_ref.starts_with("spawner.") {
            return self.data.get(var_ref).cloned();
        }

        // Check for source variable (raw source string)
        if var_ref == "source" {
            return self.source.clone();
        }

        // Check built-in variables
        self.builtins.get(var_ref).cloned()
    }
}

/// Substitute variables in text using the given context
///
/// # Substitution Rules (Tera-style syntax)
///
/// - `{{var}}` - Substitute built-in variable
/// - `{{data.key}}` - Substitute data variable
/// - `{{source}}` - Substitute source variable
///
/// # Errors
///
/// Returns an error if a referenced variable is not found in the context.
pub fn substitute(text: &str, ctx: &VariableContext) -> Result<String> {
    substitute_with_template_name(text, ctx, None)
}

/// Substitute variables with template name for better error messages
///
/// Uses Tera-style `{{var}}` syntax for variable substitution.
/// Delegates parsing to the shared `interpolation::parse_template` core.
pub fn substitute_with_template_name(
    text: &str,
    ctx: &VariableContext,
    template_name: Option<&str>,
) -> Result<String> {
    let refs = crate::parsing::interpolation::parse_template(text);

    if refs.is_empty() {
        return Ok(text.to_string());
    }

    let mut result = String::with_capacity(text.len());
    let mut last_pos = 0;

    for var_ref in &refs {
        // Add text before this variable
        result.push_str(&text[last_pos..var_ref.start]);

        // Resolve variable
        match ctx.resolve(&var_ref.name) {
            Some(value) => result.push_str(&value),
            None => {
                // Compute line number from byte offset
                let line_number = text[..var_ref.start].matches('\n').count() + 1;

                let hint = if var_ref.name.starts_with("data.") {
                    format!(
                        "Use: --data {}=<value>",
                        var_ref.name.strip_prefix("data.").unwrap_or(&var_ref.name)
                    )
                } else if var_ref.name.starts_with("source.") {
                    format!(
                        "Variable 'source.{}' requires --source with a valid source string (e.g., --source task:<id>)",
                        var_ref.name.strip_prefix("source.").unwrap_or(&var_ref.name)
                    )
                } else if var_ref.name.starts_with("parent.") {
                    format!(
                        "Variable 'parent.{}' is only available in subtask templates",
                        var_ref.name.strip_prefix("parent.").unwrap_or(&var_ref.name)
                    )
                } else if var_ref.name.starts_with("item.") {
                    format!(
                        "Variable 'item.{}' is only available in dynamic subtask templates (when iterating over a data source)",
                        var_ref.name.strip_prefix("item.").unwrap_or(&var_ref.name)
                    )
                } else if var_ref.name.starts_with("spawner.") {
                    format!(
                        "Variable 'spawner.{}' is only available in spawned task templates (tasks created via spawns: config)",
                        var_ref.name.strip_prefix("spawner.").unwrap_or(&var_ref.name)
                    )
                } else if var_ref.name == "source" {
                    "Use: --source <value>".to_string()
                } else {
                    format!("Variable '{}' is not a recognized built-in variable", var_ref.name)
                };

                let template_info = match template_name {
                    Some(n) => format!("\n  In template: {} (line {})", n, line_number),
                    None => format!("\n  At line: {}", line_number),
                };

                return Err(AikiError::TemplateVariableNotFound {
                    variable: var_ref.name.clone(),
                    hint,
                    template_info,
                });
            }
        }

        last_pos = var_ref.end;
    }

    // Add remaining text
    result.push_str(&text[last_pos..]);

    Ok(result)
}

/// Find all variable references in text (for validation)
///
/// Finds all `{{var}}` patterns and returns the variable names.
/// Delegates parsing to the shared `interpolation::parse_template` core.
pub fn find_variables(text: &str) -> Vec<String> {
    crate::parsing::interpolation::parse_template(text)
        .into_iter()
        .map(|r| r.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_substitute_builtin() {
        let mut ctx = VariableContext::new();
        ctx.set_builtin("assignee", "claude-code");
        ctx.set_builtin("priority", "p1");

        let result = substitute("Assigned to: {{assignee}}, Priority: {{priority}}", &ctx).unwrap();
        assert_eq!(result, "Assigned to: claude-code, Priority: p1");
    }

    #[test]
    fn test_substitute_data() {
        let mut ctx = VariableContext::new();
        ctx.set_data("scope.name", "Task (abc123)");
        ctx.set_data("scope.id", "abc123");
        ctx.set_data("files", "src/auth.rs, src/crypto.rs");

        let result = substitute("Review {{data.scope.name}} (files: {{data.files}})", &ctx).unwrap();
        assert_eq!(result, "Review Task (abc123) (files: src/auth.rs, src/crypto.rs)");
    }

    #[test]
    fn test_substitute_source() {
        let mut ctx = VariableContext::new();
        ctx.set_source("file:ops/now/feature.md");

        let result = substitute("Build from {{source}}", &ctx).unwrap();
        assert_eq!(result, "Build from file:ops/now/feature.md");
    }

    #[test]
    fn test_substitute_missing_variable() {
        let ctx = VariableContext::new();

        let result = substitute("Hello {{data.missing}}", &ctx);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("data.missing"));
        assert!(err.to_string().contains("--data missing="));
    }

    #[test]
    fn test_substitute_no_recursion() {
        let mut ctx = VariableContext::new();
        ctx.set_data("value", "{{data.other}}");

        let result = substitute("Result: {{data.value}}", &ctx).unwrap();
        // The value should be literal, not recursively evaluated
        assert_eq!(result, "Result: {{data.other}}");
    }

    #[test]
    fn test_substitute_empty_double_braces() {
        let ctx = VariableContext::new();

        let result = substitute("Empty {{}} braces", &ctx).unwrap();
        assert_eq!(result, "Empty {{}} braces");
    }

    #[test]
    fn test_substitute_unclosed_double_brace() {
        let ctx = VariableContext::new();

        let result = substitute("Unclosed {{brace", &ctx).unwrap();
        assert_eq!(result, "Unclosed {{brace");
    }

    #[test]
    fn test_substitute_single_brace_preserved() {
        let ctx = VariableContext::new();

        // Single braces are now just literal text (not variables)
        let result = substitute("Single {brace} preserved", &ctx).unwrap();
        assert_eq!(result, "Single {brace} preserved");
    }

    #[test]
    fn test_substitute_single_closing_brace() {
        let ctx = VariableContext::new();

        let result = substitute("Single } brace", &ctx).unwrap();
        assert_eq!(result, "Single } brace");
    }

    #[test]
    fn test_find_variables() {
        let vars = find_variables("Review {{data.scope}} assigned to {{assignee}} from {{source}}");
        assert_eq!(vars, vec!["data.scope", "assignee", "source"]);
    }

    #[test]
    fn test_find_variables_single_brace_ignored() {
        // Single braces are not variables anymore
        let vars = find_variables("{single} and {{real}} and {also_single}");
        assert_eq!(vars, vec!["real"]);
    }

    #[test]
    fn test_variable_context_builder() {
        let mut ctx = VariableContext::new();
        ctx.set_builtin("id", "abc123");
        ctx.set_data("scope.name", "Task (@)");
        ctx.set_data("scope.id", "@");
        ctx.set_source("file:plan.md");

        assert_eq!(ctx.resolve("id"), Some("abc123".to_string()));
        assert_eq!(ctx.resolve("data.scope.name"), Some("Task (@)".to_string()));
        assert_eq!(ctx.resolve("data.scope.id"), Some("@".to_string()));
        assert_eq!(ctx.resolve("source"), Some("file:plan.md".to_string()));
        assert_eq!(ctx.resolve("unknown"), None);
    }

    #[test]
    fn test_source_data_task() {
        let mut ctx = VariableContext::new();
        ctx.set_source("task:abc123xyz");

        assert_eq!(ctx.resolve("source"), Some("task:abc123xyz".to_string()));
        assert_eq!(ctx.resolve("source.id"), Some("abc123xyz".to_string()));
        assert_eq!(ctx.resolve("source.type"), Some("task".to_string()));
    }

    #[test]
    fn test_source_data_file() {
        let mut ctx = VariableContext::new();
        ctx.set_source("file:ops/now/plan.md");

        assert_eq!(ctx.resolve("source"), Some("file:ops/now/plan.md".to_string()));
        assert_eq!(ctx.resolve("source.id"), Some("ops/now/plan.md".to_string()));
        assert_eq!(ctx.resolve("source.path"), Some("ops/now/plan.md".to_string()));
        assert_eq!(ctx.resolve("source.type"), Some("file".to_string()));
    }

    #[test]
    fn test_source_data_comment() {
        let mut ctx = VariableContext::new();
        ctx.set_source("comment:c1a2b3c4");

        assert_eq!(ctx.resolve("source.id"), Some("c1a2b3c4".to_string()));
        assert_eq!(ctx.resolve("source.type"), Some("comment".to_string()));
    }

    #[test]
    fn test_source_data_issue() {
        let mut ctx = VariableContext::new();
        ctx.set_source("issue:GH-123");

        assert_eq!(ctx.resolve("source.id"), Some("GH-123".to_string()));
        assert_eq!(ctx.resolve("source.type"), Some("issue".to_string()));
    }

    #[test]
    fn test_source_data_prompt() {
        let mut ctx = VariableContext::new();
        ctx.set_source("prompt:nzwtoqqr");

        assert_eq!(ctx.resolve("source.id"), Some("nzwtoqqr".to_string()));
        assert_eq!(ctx.resolve("source.type"), Some("prompt".to_string()));
    }

    #[test]
    fn test_source_data_unknown_prefix() {
        let mut ctx = VariableContext::new();
        ctx.set_source("custom:some-value");

        assert_eq!(ctx.resolve("source.value"), Some("some-value".to_string()));
        assert_eq!(ctx.resolve("source.type"), Some("custom".to_string()));
    }

    #[test]
    fn test_source_data_direct_set() {
        let mut ctx = VariableContext::new();
        ctx.set_source_data("custom_key", "custom_value");

        assert_eq!(ctx.resolve("source.custom_key"), Some("custom_value".to_string()));
    }

    #[test]
    fn test_substitute_source_data() {
        let mut ctx = VariableContext::new();
        ctx.set_source("task:parent123");

        let result = substitute("Child of task:{{source.id}}", &ctx).unwrap();
        assert_eq!(result, "Child of task:parent123");
    }

    #[test]
    fn test_set_source_clears_stale_data() {
        let mut ctx = VariableContext::new();

        // Set source to a task
        ctx.set_source("task:abc123");
        assert_eq!(ctx.resolve("source.id"), Some("abc123".to_string()));
        assert_eq!(ctx.resolve("source.type"), Some("task".to_string()));

        // Change source to a file - old id should update
        ctx.set_source("file:ops/plan.md");
        assert_eq!(ctx.resolve("source.id"), Some("ops/plan.md".to_string()));
        assert_eq!(ctx.resolve("source.path"), Some("ops/plan.md".to_string()));
        assert_eq!(ctx.resolve("source.type"), Some("file".to_string()));
    }

    #[test]
    fn test_substitute_source_data_missing() {
        let ctx = VariableContext::new();

        let result = substitute("Task ID: {{source.id}}", &ctx);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("source.id"));
    }

    #[test]
    fn test_parent_data() {
        let mut ctx = VariableContext::new();
        ctx.set_parent("id", "parent123");
        ctx.set_parent("name", "Review task");
        ctx.set_parent("priority", "p1");

        assert_eq!(ctx.resolve("parent.id"), Some("parent123".to_string()));
        assert_eq!(ctx.resolve("parent.name"), Some("Review task".to_string()));
        assert_eq!(ctx.resolve("parent.priority"), Some("p1".to_string()));
        assert_eq!(ctx.resolve("parent.unknown"), None);
    }

    #[test]
    fn test_substitute_parent_data() {
        let mut ctx = VariableContext::new();
        ctx.set_parent("id", "abc123");
        ctx.set_parent("name", "Review Code");

        let result = substitute("Subtask of {{parent.name}} (id: {{parent.id}})", &ctx).unwrap();
        assert_eq!(result, "Subtask of Review Code (id: abc123)");
    }

    #[test]
    fn test_substitute_parent_data_missing() {
        let ctx = VariableContext::new();

        let result = substitute("Parent: {{parent.id}}", &ctx);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("parent.id"));
        assert!(err.to_string().contains("only available in subtask templates"));
    }

    #[test]
    fn test_item_data() {
        let mut ctx = VariableContext::new();
        ctx.set_item("text", "Fix the null check");
        ctx.set_item("file", "src/auth.rs");
        ctx.set_item("line", "42");
        ctx.set_item("severity", "error");

        assert_eq!(ctx.resolve("item.text"), Some("Fix the null check".to_string()));
        assert_eq!(ctx.resolve("item.file"), Some("src/auth.rs".to_string()));
        assert_eq!(ctx.resolve("item.line"), Some("42".to_string()));
        assert_eq!(ctx.resolve("item.severity"), Some("error".to_string()));
        assert_eq!(ctx.resolve("item.unknown"), None);
    }

    #[test]
    fn test_substitute_item_data() {
        let mut ctx = VariableContext::new();
        ctx.set_item("text", "Missing null check");
        ctx.set_item("file", "src/main.rs");
        ctx.set_item("line", "123");

        let result = substitute("Fix: {{item.file}}:{{item.line}} - {{item.text}}", &ctx).unwrap();
        assert_eq!(result, "Fix: src/main.rs:123 - Missing null check");
    }

    #[test]
    fn test_substitute_item_data_missing() {
        let ctx = VariableContext::new();

        let result = substitute("Item: {{item.text}}", &ctx);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("item.text"));
        assert!(err.to_string().contains("dynamic subtask templates"));
    }

    #[test]
    fn test_substitute_with_whitespace() {
        let mut ctx = VariableContext::new();
        ctx.set_data("name", "World");

        // Whitespace inside {{ }} should be trimmed
        let result = substitute("Hello {{ data.name }}!", &ctx).unwrap();
        assert_eq!(result, "Hello World!");
    }

    #[test]
    fn test_coerce_value_boolean() {
        assert_eq!(coerce_value("true"), serde_json::json!(true));
        assert_eq!(coerce_value("false"), serde_json::json!(false));
        assert_eq!(coerce_value("TRUE"), serde_json::json!(true));
        assert_eq!(coerce_value("False"), serde_json::json!(false));
    }

    #[test]
    fn test_coerce_value_integer() {
        assert_eq!(coerce_value("42"), serde_json::json!(42));
        assert_eq!(coerce_value("-7"), serde_json::json!(-7));
        assert_eq!(coerce_value("0"), serde_json::json!(0));
    }

    #[test]
    fn test_coerce_value_float() {
        assert_eq!(coerce_value("3.14"), serde_json::json!(3.14));
        assert_eq!(coerce_value("-2.5"), serde_json::json!(-2.5));
    }

    #[test]
    fn test_coerce_value_string() {
        assert_eq!(coerce_value("hello"), serde_json::json!("hello"));
        assert_eq!(coerce_value(""), serde_json::json!(""));
        assert_eq!(coerce_value("123abc"), serde_json::json!("123abc"));
        assert_eq!(coerce_value("truthy"), serde_json::json!("truthy")); // not "true"
    }

    #[test]
    fn test_coerce_to_string_normalizes() {
        assert_eq!(coerce_to_string("TRUE"), "true");
        assert_eq!(coerce_to_string("False"), "false");
        assert_eq!(coerce_to_string("42"), "42");
        assert_eq!(coerce_to_string("hello"), "hello");
    }

    #[test]
    fn test_spawner_data() {
        let mut ctx = VariableContext::new();
        // Spawner data is stored as "spawner.id", "spawner.priority" etc. in the data map
        ctx.set_data("spawner.id", "abc123spawnerid");
        ctx.set_data("spawner.priority", "p1");
        ctx.set_data("spawner.approved", "false");

        assert_eq!(ctx.resolve("spawner.id"), Some("abc123spawnerid".to_string()));
        assert_eq!(ctx.resolve("spawner.priority"), Some("p1".to_string()));
        assert_eq!(ctx.resolve("spawner.approved"), Some("false".to_string()));
        assert_eq!(ctx.resolve("spawner.unknown"), None);
    }

    #[test]
    fn test_substitute_spawner_data() {
        let mut ctx = VariableContext::new();
        ctx.set_data("spawner.id", "review123");
        ctx.set_data("spawner.priority", "p0");

        let result = substitute("Fix {{spawner.id}} (priority: {{spawner.priority}})", &ctx).unwrap();
        assert_eq!(result, "Fix review123 (priority: p0)");
    }

    #[test]
    fn test_substitute_spawner_data_missing() {
        let ctx = VariableContext::new();

        let result = substitute("Spawner: {{spawner.id}}", &ctx);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("spawner.id"));
        assert!(err.to_string().contains("spawned task templates"));
    }
}
