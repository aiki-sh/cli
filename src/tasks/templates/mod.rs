//! Task template system for defining reusable workflows
//!
//! Templates are markdown files with YAML frontmatter that define:
//! - Task structure: parent task + subtasks
//! - Instructions: what the agent should do in each step
//! - Defaults: type, assignee, priority, custom data
//! - Variables: placeholders filled at runtime
//! - Conditionals: dynamic content based on context
//!
//! # Template Format
//!
//! ```markdown
//! ---
//! version: "1.0.0"
//! description: Template description
//! type: review
//! assignee: claude-code
//! priority: p2
//! data:
//!   custom_key: value
//! ---
//!
//! # Task Name with {{variables}}
//!
//! Task instructions...
//!
//! {% if data.target_type == "file" %}
//! Review the document for completeness.
//! {% else %}
//! Review the code for bugs.
//! {% endif %}
//!
//! # Subtasks
//!
//! ## Subtask Name
//!
//! Subtask instructions...
//! ```
//!
//! # Syntax
//!
//! ## Variables (Tera-style)
//!
//! - `{{data.key}}` - Data variable substitution
//! - `{{item.field}}` - Iteration item fields
//! - `{{source.id}}` - Source reference ID (value after the prefix)
//!
//! ## Conditionals
//!
//! - `{% if condition %}...{% endif %}`
//! - `{% if condition %}...{% else %}...{% endif %}`
//! - `{% if condition %}...{% elif condition %}...{% else %}...{% endif %}`
//!
//! Supported conditions:
//! - Equality: `data.type == "file"`, `priority != "p0"`
//! - Numeric: `data.count > 10`, `data.score >= 80`
//! - Truthy: `data.flag` (exists and is truthy)
//! - Negation: `not data.skip`
//! - Boolean: `a and b`, `a or b`, `(a or b) and c`
//!
//! ## Loops
//!
//! - `{% for item in collection %}...{% endfor %}`
//! - `{% for item in collection %}...{% else %}...{% endfor %}` (else for empty collections)
//!
//! Loop metadata variables:
//! - `loop.iteration` - 1-based iteration (1, 2, 3, ...)
//! - `loop.index` - 0-based iteration (0, 1, 2, ...)
//!
//! # Usage
//!
//! ```bash
//! # Create task from template
//! aiki task create --template review --data scope="@"
//!
//! # Create and start in one command
//! aiki task start --template myorg/refactor --data scope="src/auth.rs"
//!
//! # List available templates
//! aiki task template list
//! ```

pub mod builtin;
pub mod conditionals;
pub mod manifest;
pub mod parser;
pub mod resolver;
pub mod spawn_config;
pub mod sync;
pub mod types;
pub mod variables;

/// Directory name under `.aiki/` for user-defined task templates (runtime discovery).
///
/// Used to construct paths like `{repo_root}/.aiki/{TASKS_DIR_NAME}/`.
pub const TASKS_DIR_NAME: &str = "tasks";

/// Relative path from repo root to the built-in template source directory.
///
/// The `include_dir!` macro in `builtin.rs` must use a matching string literal
/// (macros cannot reference constants).
#[allow(dead_code)]
pub const BUILTIN_TEMPLATES_SOURCE: &str = "cli/src/tasks/templates/core";

#[allow(unused_imports)]
pub use builtin::default_plugin_templates;
#[allow(unused_imports)]
pub use resolver::{
    convert_data, create_review_task_from_template, create_subtask_entries_from_template,
    create_tasks_from_template, find_templates_dir, has_inline_loops, has_subtask_refs,
    list_templates, load_template, load_template_quiet, normalize_template_ref, parse_priority,
    substitute_parent_id, SubtaskEntry, PARENT_ID_PLACEHOLDER,
};
pub use types::TaskTemplate;
pub use variables::{
    coerce_to_string, find_variables, substitute_with_template_name, VariableContext,
};

/// Helper: construct the user tasks directory path relative to a repo root.
///
/// Returns `{repo_root}/.aiki/{TASKS_DIR_NAME}`.
#[allow(dead_code)]
pub fn user_tasks_dir(repo_root: &std::path::Path) -> std::path::PathBuf {
    repo_root.join(".aiki").join(TASKS_DIR_NAME)
}
