use std::path::PathBuf;
use thiserror::Error;

/// Format the error message for tasks missing instructions in a thread
fn format_missing_instructions(missing: &[(String, String)]) -> String {
    let count = missing.len();
    let mut msg = format!(
        "{count} task(s) in this thread are missing instructions:"
    );
    for (id, title) in missing {
        msg.push_str(&format!("\n  - {id} — {title}"));
    }
    msg.push_str(
        "\n\n\
         Every task MUST have instructions before you run it — even tasks you create yourself.\n\
         Instructions record your intent and context so that:\n\
         - If the session crashes or is interrupted, another agent can pick up the work\n\
         - If the task is retried, the new agent has full context without your conversation history\n\
         - Reviewers can understand what was intended vs what was done",
    );
    // TODO(run-single-shot): restore `aiki run <id> --instructions` suggestion
    // when ops/now/run-single-shot.md lands (adds --instructions flag to Run).
    if count == 1 {
        let id = &missing[0].0;
        msg.push_str(&format!(
            "\n\n\
             Set instructions before running:\n  \
             aiki task set {id} -i \"<describe what the agent should do>\""
        ));
    } else {
        msg.push_str(
            "\n\n\
             Set instructions on each task before running:\n  \
             aiki task set <id> -i \"<describe what the agent should do>\"",
        );
    }
    msg
}

/// Format a call stack for display in error messages
fn format_call_stack(stack: &[String]) -> String {
    if stack.is_empty() {
        return String::from("  (empty)");
    }

    let mut result = String::new();
    for (i, path) in stack.iter().enumerate() {
        if i == 0 {
            result.push_str(&format!("  {}", path));
        } else {
            result.push_str(&format!("\n  → {} (before)", path));
        }
    }
    result
}

/// Aiki-specific errors with structured error types
#[derive(Error, Debug)]
pub enum AikiError {
    // File errors
    #[error("File not found: {0}")]
    FileNotFound(PathBuf),

    #[error("File not found in working copy and no parents available")]
    FileNotFoundNoParents,

    #[error("File not found in working copy or its parent")]
    FileNotFoundInParent,

    // Agent/vendor errors
    #[error(
        "Unknown agent type: '{0}'. Supported values: 'claude-code', 'codex', 'cursor', 'gemini'"
    )]
    UnknownAgentType(String),

    #[error("Unsupported agent type: {0:?}")]
    UnsupportedAgentType(String),

    #[error(
        "Unknown assignee: '{0}'. Valid values: 'claude-code', 'codex', 'cursor', 'gemini', 'human'"
    )]
    UnknownAssignee(String),

    // Hook execution errors
    #[error("Invalid let syntax: '{0}'. Expected 'variable = expression'")]
    InvalidLetSyntax(String),

    #[error("Invalid variable name: '{0}'. Variable names must start with a letter or underscore, and contain only letters, numbers, and underscores")]
    InvalidVariableName(String),

    // Message assembly errors
    #[error("Invalid message chunk: {0}")]
    InvalidContextChunk(String),

    #[error("Missing function: {0}")]
    MissingFunction(String),

    #[error("Function '{0}' not found in namespace '{1}'")]
    FunctionNotFoundInNamespace(String, String),

    #[error("Unsupported function namespace in '{0}'. Only 'aiki/*' functions are supported")]
    UnsupportedFunctionNamespace(String),

    #[error("Invalid function path '{0}': must use 'self.<function>' or fully qualified 'aiki/<module>.<function>'")]
    InvalidFunctionPath(String),

    #[error("Invalid timeout format: {0}. Use 's', 'm', or 'h' suffix")]
    InvalidTimeoutFormat(String),

    // Scope errors
    #[error("Unknown scope: '{0}'. Supported values: 'staged'")]
    UnknownScope(String),

    #[error("Unknown format: '{0}'. Supported values: 'plain', 'git', 'json'")]
    UnknownFormat(String),

    // Command execution errors
    #[error("jj command failed: {0}")]
    JjCommandFailed(String),

    #[error("Failed to create isolated workspace: {0}")]
    WorkspaceCreationFailed(String),

    #[error("Failed to absorb workspace changes: {0}")]
    WorkspaceAbsorbFailed(String),

    #[error("Lock failed: {0}")]
    LockFailed(String),

    // Hook composition errors (Milestone 1.3)
    #[error(
        "Not in an Aiki project. No .aiki/ directory found searching upward from: {searched_from}"
    )]
    NotInAikiProject { searched_from: PathBuf },

    #[error("Invalid hook path: '{path}'. {reason}")]
    InvalidHookPath { path: String, reason: String },

    #[error("Hook not found: '{path}'. Resolved to: {resolved_path}")]
    HookNotFound {
        path: String,
        resolved_path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Circular hook dependency detected: '{path}' (canonical: {canonical_path})\n\nHook execution chain:\n{}", format_call_stack(.stack))]
    CircularHookDependency {
        path: String,
        canonical_path: String,
        stack: Vec<String>,
    },

    // ACP/Zed integration errors
    #[error(
        "ACP binary not found for agent '{agent_type}'.

Zed installation not found or agent not installed.
Please ensure:
  1. Zed editor is installed (https://zed.dev)
  2. You've created a {agent_type} thread in Zed (cmd-? → '+' → {agent_type})
  3. Zed has completed its one-time package installation

Alternatively, install the agent globally:
  npm install -g {package_name}"
    )]
    AcpBinaryNotFound {
        agent_type: String,
        executable_name: String,
        package_name: String,
    },

    #[error("Zed installation not found at {0}. Install Zed from https://zed.dev")]
    ZedNotInstalled(PathBuf),

    #[error("Agent '{0}' not installed by Zed. Create a thread with this agent in Zed first (cmd-? → '+' → agent)")]
    ZedAgentNotInstalled(String),

    #[error("Node.js not found. Zed-installed agents require Node.js. Install from: https://nodejs.org or 'brew install node'")]
    NodeJsNotFound,

    #[error("Unsupported platform: {0}")]
    UnsupportedPlatform(String),

    // Plugin errors
    #[error("Failed to auto-fetch plugin '{plugin}': {reason}")]
    AutoFetchFailed { plugin: String, reason: String },

    /// Like `AutoFetchFailed` but from a disk-persisted marker. The warning was
    /// already printed on the first failure; callers should skip silently.
    #[error("Plugin '{plugin}' previously failed to fetch: {reason}")]
    AutoFetchCached { plugin: String, reason: String },

    #[error("{plugin} is a dependency of: {dependents}. Use --force to remove anyway.")]
    PluginHasDependents { plugin: String, dependents: String },

    // Argument validation errors
    #[error("{0}")]
    InvalidArgument(String),

    #[error("Missing required argument: {0}")]
    MissingArgument(String),

    // Task system errors
    #[error("Task not found: '{0}'")]
    TaskNotFound(String),

    #[error("Ambiguous task ID prefix '{prefix}' — matches {count} tasks:\n{matches}")]
    AmbiguousTaskId {
        prefix: String,
        count: usize,
        matches: String,
    },

    #[error("Task '{root}' has no subtask '{subtask}'")]
    SubtaskNotFound { root: String, subtask: String },

    #[error("Task ID prefix '{prefix}' is too short (minimum 3 characters)")]
    PrefixTooShort { prefix: String },

    #[error("No tasks in ready queue")]
    NoTasksReady,

    #[error("{0}")]
    TaskCommentRequired(String),

    #[error("Task '{0}' is already closed")]
    TaskAlreadyClosed(String),

    #[error("Invalid outcome: '{0}'. Valid values: {}", .1.join(", "))]
    InvalidOutcome(String, Vec<String>),

    #[error("Invalid task source: '{0}'. Sources must have a prefix: 'file:', 'task:', 'comment:', 'issue:', or 'prompt:'")]
    InvalidTaskSource(String),

    #[error("Invalid --data format: '{0}'. Expected: --data key=value")]
    InvalidDataFormat(String),

    #[error("Invalid data key: '{0}'. Keys must be lowercase with underscores (e.g., 'my_key')")]
    InvalidDataKey(String),

    #[error("Invalid slug '{0}': must be 1-48 chars of lowercase letters, digits, and hyphens (no leading/trailing hyphens)")]
    InvalidSlug(String),

    #[error("Slug '{slug}' already exists under parent {parent_id} (task: {existing_task})")]
    DuplicateSlug {
        slug: String,
        parent_id: String,
        existing_task: String,
    },

    #[error("Invalid link target for '{kind}': '{target}' is not a task. {kind} links require a task ID as target")]
    InvalidLinkTarget { kind: String, target: String },

    #[error("Link would create a cycle in '{kind}' links")]
    LinkCycle { kind: String },

    #[error("Task '{0}' has no assignee and no --agent specified")]
    TaskNoAssignee(String),

    #[error("No agent CLIs found. Install claude-code or codex to continue.")]
    NoAgentsAvailable,

    #[error("Agent '{agent}' is not available.\n  {hint}")]
    AgentNotInstalled { agent: String, hint: String },

    #[error("Failed to spawn agent: {0}")]
    AgentSpawnFailed(String),

    #[error("Cannot resolve --source prompt: no active session found. Use --source prompt:<change_id> to specify explicitly.")]
    NoActiveSessionForPromptSource,

    #[error("Cannot resolve --source prompt: no prompt events found for this session.")]
    NoPromptEventsForSession,

    // Template system errors
    #[error("Template not found: '{name}'\n  Expected: {expected_path}{suggestions}")]
    TemplateNotFound {
        name: String,
        expected_path: String,
        suggestions: String,
    },

    #[error("Variable '{{{variable}}}' referenced but not provided{template_info}\n  {hint}")]
    TemplateVariableNotFound {
        variable: String,
        hint: String,
        template_info: String,
    },

    #[error("Variable '{variable}' not found: {hint}")]
    VariableNotFound { variable: String, hint: String },

    #[error("Invalid template frontmatter\n  File: {file}\n  {details}")]
    TemplateFrontmatterInvalid { file: String, details: String },

    #[error("Invalid template structure\n  File: {file}\n  {details}")]
    TemplateStructureInvalid { file: String, details: String },

    #[error("No tasks directory found at: {path}")]
    TemplatesDirectoryNotFound { path: String },

    #[error("Template processing failed: {details}")]
    TemplateProcessingFailed { details: String },

    // Undo errors
    #[error("Task has no file changes to undo")]
    TaskNoChanges,

    #[error("Cannot undo - files have been modified since task completed:\n{0}")]
    UndoConflict(String),

    #[error("Cannot undo - in-progress tasks have modified the same files:\n{0}")]
    UndoInProgressConflict(String),

    #[error("Epic has no completed subtasks to undo")]
    NoCompletedSubtasks,

    // Review system errors
    #[error("Unknown review scope type: '{0}'. Valid values: 'task', 'plan', 'implementation', 'session'")]
    UnknownReviewScope(String),

    #[error("Nothing to review - no closed tasks in session")]
    NothingToReview,

    #[error("{0}")]
    ReviewIssuesMissing(String),

    // Plugin errors
    #[error("Invalid plugin reference: '{reference}'. {reason}")]
    InvalidPluginRef { reference: String, reason: String },

    #[error("Plugin {0} is not installed")]
    PluginNotInstalled(String),

    #[error("Plugin operation failed for '{plugin}': {details}")]
    PluginOperationFailed { plugin: String, details: String },

    // Thread validation errors
    #[error("{}", format_missing_instructions(.missing))]
    ThreadMissingInstructions {
        missing: Vec<(String, String)>, // Vec of (task_id, title)
    },

    // Generic wrapper for underlying errors
    #[error(transparent)]
    Other(#[from] anyhow::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Result type alias for Aiki operations
pub type Result<T> = std::result::Result<T, AikiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unknown_agent_type() {
        let err = AikiError::UnknownAgentType("vscode".to_string());
        assert_eq!(
            err.to_string(),
            "Unknown agent type: 'vscode'. Supported values: 'claude-code', 'codex', 'cursor', 'gemini'"
        );
    }

    #[test]
    fn test_invalid_let_syntax() {
        let err = AikiError::InvalidLetSyntax("x".to_string());
        assert_eq!(
            err.to_string(),
            "Invalid let syntax: 'x'. Expected 'variable = expression'"
        );
    }

    #[test]
    fn test_file_not_found() {
        let err = AikiError::FileNotFound(PathBuf::from("/tmp/test.txt"));
        assert!(err.to_string().contains("/tmp/test.txt"));
    }

    #[test]
    fn test_not_in_aiki_project() {
        let err = AikiError::NotInAikiProject {
            searched_from: PathBuf::from("/home/user/project"),
        };
        assert!(err.to_string().contains("/home/user/project"));
        assert!(err.to_string().contains(".aiki/"));
    }

    #[test]
    fn test_invalid_hook_path() {
        let err = AikiError::InvalidHookPath {
            path: "invalid".to_string(),
            reason: "Must start with aiki/, vendor/, @/, ./, ../, or /".to_string(),
        };
        assert!(err.to_string().contains("invalid"));
        assert!(err.to_string().contains("Must start with"));
    }

    #[test]
    fn test_circular_hook_dependency() {
        let err = AikiError::CircularHookDependency {
            path: "aiki/flow-a".to_string(),
            canonical_path: "/project/.aiki/hooks/aiki/flow-a.yml".to_string(),
            stack: vec![
                "my-workflow.yml".to_string(),
                "aiki/flow-a.yml".to_string(),
                "aiki/flow-b.yml".to_string(),
            ],
        };
        let msg = err.to_string();
        assert!(msg.contains("Circular hook dependency"));
        assert!(msg.contains("aiki/flow-a"));
        assert!(msg.contains("my-workflow.yml"));
        assert!(msg.contains("→"));
    }

    #[test]
    fn test_format_call_stack_empty() {
        let result = format_call_stack(&[]);
        assert_eq!(result, "  (empty)");
    }

    #[test]
    fn test_format_call_stack_single() {
        let result = format_call_stack(&["my-flow.yml".to_string()]);
        assert_eq!(result, "  my-flow.yml");
    }

    #[test]
    fn test_format_call_stack_multiple() {
        let result = format_call_stack(&[
            "top.yml".to_string(),
            "middle.yml".to_string(),
            "bottom.yml".to_string(),
        ]);
        assert!(result.contains("top.yml"));
        assert!(result.contains("→ middle.yml"));
        assert!(result.contains("→ bottom.yml"));
    }

    #[test]
    fn test_ambiguous_task_id_display() {
        let err = AikiError::AmbiguousTaskId {
            prefix: "mvslrsp".to_string(),
            count: 2,
            matches: "  mvslrspm — Task A\n  mvslrspo — Task B".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("Ambiguous task ID prefix 'mvslrsp'"));
        assert!(msg.contains("matches 2 tasks"));
        assert!(msg.contains("Task A"));
        assert!(msg.contains("Task B"));
    }

    #[test]
    fn test_subtask_not_found_display() {
        let err = AikiError::SubtaskNotFound {
            root: "mvslrspmoynoxyyywqyutmovxpvztkls".to_string(),
            subtask: "99".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("has no subtask '99'"));
    }

    #[test]
    fn test_prefix_too_short_display() {
        let err = AikiError::PrefixTooShort {
            prefix: "mv".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("'mv'"));
        assert!(msg.contains("minimum 3 characters"));
    }
}
