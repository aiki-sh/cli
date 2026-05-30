use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;

use crate::flows::context::TextLines;

/// Hook control statement - top level unit of execution
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HookStatement {
    /// Conditional if/then/else
    If(IfStatement),
    /// Switch/case statement
    Switch(SwitchStatement),
    /// Invoke another plugin's handler for the current event.
    /// Handled by the composer (not the engine) because it requires
    /// the composer's HookLoader and call stack for cycle detection.
    Hook(HookAction),
    /// Action to execute
    Action(Action),
}

/// Conditional if statement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IfStatement {
    /// Condition to evaluate
    #[serde(rename = "if")]
    pub condition: String,

    /// Statements to execute if condition is true
    pub then: Vec<HookStatement>,

    /// Optional statements to execute if condition is false
    #[serde(default, rename = "else")]
    pub else_: Option<Vec<HookStatement>>,
}

/// Switch/case statement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwitchStatement {
    /// Expression to evaluate and match against cases
    #[serde(rename = "switch")]
    pub expression: String,

    /// Map of case values to statements
    pub cases: HashMap<String, Vec<HookStatement>>,

    /// Optional default case if no cases match
    #[serde(default)]
    pub default: Option<Vec<HookStatement>>,
}

/// Strongly-typed shortcuts for on_failure behavior
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnFailureShortcut {
    /// Continue: Log failure and continue execution (default)
    Continue,
    /// Stop: Add failure and stop hook silently
    Stop,
    /// Block: Add failure and block operation with exit code 2
    Block,
}

/// On-failure behavior for actions
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OnFailure {
    /// Shortcut: "continue", "stop", or "block"
    Shortcut(OnFailureShortcut),
    /// Full statements list for complex failure handling
    Statements(Vec<HookStatement>),
}

impl Default for OnFailure {
    fn default() -> Self {
        OnFailure::Shortcut(OnFailureShortcut::Continue)
    }
}

/// Hook action — invokes another plugin's handler for the current event.
///
/// YAML: `- hook: "aiki/context-inject"`
///
/// Handled by the composer (not the engine) because it requires
/// the composer's HookLoader and call stack for cycle detection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookAction {
    /// Plugin path to invoke (e.g., "aiki/context-inject")
    pub hook: String,
}

/// Deserialize a Vec<HookStatement> that treats null/unit as empty vec.
/// Needed because serde(flatten) doesn't apply #[serde(default)] when
/// the value is explicitly null (e.g., `session.resumed:` with no value in YAML).
fn deserialize_null_as_empty_vec<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<HookStatement>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<Vec<HookStatement>>::deserialize(deserializer).map(|opt| opt.unwrap_or_default())
}

/// Event handler fields shared by Hook and CompositionBlock.
///
/// Contains all event-specific handler lists. Both Hook (for own handlers)
/// and CompositionBlock (for inline handlers in before/after) use these fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventHandlers {
    // Session Lifecycle Events
    #[serde(rename = "session.started", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub session_started: Vec<HookStatement>,
    #[serde(rename = "session.resumed", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub session_resumed: Vec<HookStatement>,
    #[serde(rename = "session.will_compact", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub session_will_compact: Vec<HookStatement>,
    #[serde(rename = "session.compacted", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub session_compacted: Vec<HookStatement>,
    #[serde(rename = "session.cleared", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub session_cleared: Vec<HookStatement>,
    #[serde(rename = "session.ended", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub session_ended: Vec<HookStatement>,

    // Turn Lifecycle Events
    #[serde(rename = "turn.started", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub turn_started: Vec<HookStatement>,
    #[serde(rename = "turn.completed", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub turn_completed: Vec<HookStatement>,

    // Read Operation Events
    #[serde(rename = "read.permission_asked", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub read_permission_asked: Vec<HookStatement>,
    #[serde(rename = "read.completed", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub read_completed: Vec<HookStatement>,

    // Change Operation Events
    #[serde(rename = "change.permission_asked", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub change_permission_asked: Vec<HookStatement>,
    #[serde(rename = "change.completed", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub change_completed: Vec<HookStatement>,

    // Shell Command Events
    #[serde(rename = "shell.permission_asked", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub shell_permission_asked: Vec<HookStatement>,
    #[serde(rename = "shell.completed", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub shell_completed: Vec<HookStatement>,

    // Web Access Events
    #[serde(rename = "web.permission_asked", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub web_permission_asked: Vec<HookStatement>,
    #[serde(rename = "web.completed", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub web_completed: Vec<HookStatement>,

    // MCP Tool Events
    #[serde(rename = "mcp.permission_asked", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub mcp_permission_asked: Vec<HookStatement>,
    #[serde(rename = "mcp.completed", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub mcp_completed: Vec<HookStatement>,

    // Commit Integration Events
    #[serde(rename = "commit.message_started", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub commit_message_started: Vec<HookStatement>,

    // Repo Transition Events
    #[serde(rename = "repo.changed", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub repo_changed: Vec<HookStatement>,

    // Task Lifecycle Events
    #[serde(rename = "task.started", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub task_started: Vec<HookStatement>,
    #[serde(rename = "task.closed", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub task_closed: Vec<HookStatement>,

    // Legacy
    #[serde(rename = "Stop", default, deserialize_with = "deserialize_null_as_empty_vec")]
    pub stop: Vec<HookStatement>,
}

/// A composition block used in before/after positions.
///
/// Always a mapping with optional `include:` (plugins for all events)
/// and event-specific inline handler lists.
/// Each block retains its source hook identity for self.* resolution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompositionBlock {
    /// The hook identity this block came from (for self.* resolution in inline handlers).
    /// Set during include expansion; None for the hookfile's own block (resolved at execution time).
    #[serde(skip)]
    pub source_hook: Option<String>,

    /// Plugin references to run for all events in this phase
    #[serde(default)]
    pub include: Vec<String>,

    /// Event handlers for this block
    #[serde(flatten)]
    pub handlers: EventHandlers,
}

/// A segment of own handlers tagged with their source hook identity.
///
/// Preserves self.* context when handlers from different plugins are
/// sequenced together via top-level include expansion.
/// Stores the full Hook so that the correct event's handlers can be
/// selected at execution time.
#[derive(Debug, Clone)]
pub struct HandlerSegment {
    /// The hook identity for self.* resolution
    pub source_hook: String,
    /// The included hook (handlers selected per-event at execution time)
    pub hook: Hook,
}

/// A complete hook definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hook {
    /// Hook name (optional in YAML; autogenerated from path if missing)
    #[serde(default)]
    pub name: String,

    /// Optional description
    #[serde(default)]
    pub description: Option<String>,

    /// Hook version
    #[serde(default = "default_version")]
    pub version: String,

    // ========================================================================
    // Hook Composition
    // ========================================================================
    /// Plugins to include (expand their blocks/segments into this hook's lists).
    /// Top-level structural composition: prepends included plugin's before/after
    /// blocks and handler segments.
    #[serde(default)]
    pub include: Vec<String>,

    /// Composition block list: blocks run before this hook's own handlers.
    /// Vec because include expansion prepends blocks without merging.
    /// YAML hookfiles write `before:` as a single mapping (one CompositionBlock).
    /// The custom deserializer wraps it into a Vec with one entry.
    #[serde(default, deserialize_with = "deserialize_single_block_as_vec")]
    pub before: Vec<CompositionBlock>,

    /// Composition block list: blocks run after this hook's own handlers.
    /// Vec because include expansion prepends blocks without merging.
    #[serde(default, deserialize_with = "deserialize_single_block_as_vec")]
    pub after: Vec<CompositionBlock>,

    /// Handler segments from include expansion (not deserialized from YAML).
    /// Populated at runtime when top-level includes are expanded.
    #[serde(skip)]
    pub handler_segments: Vec<HandlerSegment>,

    // ========================================================================
    // Event Handlers (own handlers)
    // ========================================================================
    /// Event handler fields (flattened for YAML compatibility)
    #[serde(flatten)]
    pub handlers: EventHandlers,
}

fn default_version() -> String {
    "1".to_string()
}

/// Custom deserializer that wraps a single CompositionBlock into a Vec.
///
/// In YAML, `before:` is written as a single mapping (one CompositionBlock).
/// This deserializer accepts either a single mapping or an array of mappings,
/// producing a Vec<CompositionBlock> in both cases.
fn deserialize_single_block_as_vec<'de, D>(
    deserializer: D,
) -> std::result::Result<Vec<CompositionBlock>, D::Error>
where
    D: Deserializer<'de>,
{
    // Use an untagged enum to try both forms
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SingleOrVec {
        Single(CompositionBlock),
        Vec(Vec<CompositionBlock>),
    }

    match SingleOrVec::deserialize(deserializer)? {
        SingleOrVec::Single(block) => Ok(vec![block]),
        SingleOrVec::Vec(blocks) => {
            // Validate: Vec form should not be used for the old list-of-strings format.
            // If someone writes `before: ["aiki/foo"]`, the CompositionBlock deserialization
            // will fail because strings aren't valid blocks. This is the intended clean break.
            Ok(blocks)
        }
    }
}

impl Hook {
    /// Check if this hook has any event handlers defined.
    pub fn has_handlers(&self) -> bool {
        self.handlers.has_any()
    }
}

impl EventHandlers {
    /// Check if any event handler has statements.
    pub fn has_any(&self) -> bool {
        !self.session_started.is_empty()
            || !self.session_resumed.is_empty()
            || !self.session_will_compact.is_empty()
            || !self.session_compacted.is_empty()
            || !self.session_cleared.is_empty()
            || !self.session_ended.is_empty()
            || !self.turn_started.is_empty()
            || !self.turn_completed.is_empty()
            || !self.read_permission_asked.is_empty()
            || !self.read_completed.is_empty()
            || !self.change_permission_asked.is_empty()
            || !self.change_completed.is_empty()
            || !self.shell_permission_asked.is_empty()
            || !self.shell_completed.is_empty()
            || !self.web_permission_asked.is_empty()
            || !self.web_completed.is_empty()
            || !self.mcp_permission_asked.is_empty()
            || !self.mcp_completed.is_empty()
            || !self.commit_message_started.is_empty()
            || !self.repo_changed.is_empty()
            || !self.task_started.is_empty()
            || !self.task_closed.is_empty()
            || !self.stop.is_empty()
    }
}

/// Continue hook execution action (generates Failure and continues)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinueAction {
    #[serde(rename = "continue")]
    pub failure: String,
}

/// Stop hook execution action (generates Failure and stops silently)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StopAction {
    #[serde(rename = "stop")]
    pub failure: String,
}

/// Block editor operation action (generates Failure and blocks with exit 2)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockAction {
    #[serde(rename = "block")]
    pub failure: String,
}

/// Session end action - terminates the current session gracefully
///
/// Used for task-driven sessions that should auto-end when their driving task closes.
/// Sends SIGTERM to the parent process (the agent) after a short delay to allow
/// the hook to complete.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEndAction {
    /// Reason for ending (logged)
    #[serde(rename = "session.end")]
    pub reason: String,

    #[serde(default)]
    pub on_failure: OnFailure,
}

/// An action to execute in a hook (no hook control)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Action {
    /// Let binding (function call or variable aliasing)
    Let(LetAction),
    /// Call action (call a function without storing result)
    Call(CallAction),
    /// Shell command
    Shell(ShellAction),
    /// JJ command
    Jj(JjAction),
    /// Log message
    Log(LogAction),
    /// Context injection (for PrePrompt events)
    Context(ContextAction),
    /// Autoreply (for SessionEnd events)
    Autoreply(AutoreplyAction),
    /// Commit message (for PrepareCommitMessage events)
    CommitMessage(CommitMessageAction),
    /// Run a task by spawning an agent session
    TaskRun(TaskRunAction),
    /// Create and run a review task
    Review(ReviewAction),
    /// Continue hook execution (generates Failure and returns FailedContinue)
    Continue(ContinueAction),
    /// Stop hook execution (emits warning and stops silently)
    Stop(StopAction),
    /// Block editor operation (emits error and blocks with exit 2)
    Block(BlockAction),
    /// End the current session gracefully (for task-driven sessions)
    SessionEnd(SessionEndAction),
}

/// Shell command action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellAction {
    pub shell: String,

    #[serde(default)]
    pub timeout: Option<String>,

    #[serde(default)]
    pub on_failure: OnFailure,

    /// Optional variable name to store the result
    #[serde(default)]
    pub alias: Option<String>,
}

/// JJ command action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JjAction {
    pub jj: String,

    #[serde(default)]
    pub timeout: Option<String>,

    #[serde(default)]
    pub on_failure: OnFailure,

    /// Optional variable name to store the result
    #[serde(default)]
    pub alias: Option<String>,

    /// Optional author to set for this command via JJ_USER and JJ_EMAIL
    /// Format: "Name <email>"
    #[serde(default)]
    pub with_author: Option<String>,

    /// Optional function that returns {author, message}
    /// Automatically sets with_author and makes {{message}} available
    #[serde(default)]
    pub with_author_and_message: Option<String>,
}

/// Log message action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogAction {
    pub log: String,

    /// Optional variable name to store the result
    #[serde(default)]
    pub alias: Option<String>,
}

/// Let binding action (function call or variable aliasing)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LetAction {
    /// The let binding in format "variable = expression"
    #[serde(rename = "let")]
    pub let_: String,

    /// What to do when the action fails
    #[serde(default)]
    pub on_failure: OnFailure,
}

/// Call action (calls a function without storing result)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallAction {
    /// The function to call in format "self.function_name" or "aiki/module.function"
    /// Example: "self.write_ai_files"
    #[serde(rename = "call")]
    pub call: String,

    /// What to do when the action fails
    #[serde(default)]
    pub on_failure: OnFailure,
}

/// Context action (for PrePrompt events)
/// Injects context that is prepended to the user's prompt
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextAction {
    /// The context content to inject
    pub context: ContextContent,

    #[serde(default)]
    pub on_failure: OnFailure,
}

/// Content for context action
/// Can be a simple string (defaults to append) or explicit prepend/append
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContextContent {
    /// Simple form: defaults to append
    /// YAML: `context: "text"`
    Simple(String),

    /// Explicit form with prepend/append
    /// YAML: `context: { prepend: "...", append: "..." }`
    /// Supports both scalar and array forms:
    /// `context: { prepend: "single line" }`
    /// `context: { prepend: ["line 1", "line 2"] }`
    Explicit {
        #[serde(default)]
        prepend: Option<TextLines>,
        #[serde(default)]
        append: Option<TextLines>,
    },
}

/// Autoreply action (for PostResponse events)
/// Sends an additional message to the agent after it completes its response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoreplyAction {
    /// The autoreply content (MessageChunk)
    pub autoreply: AutoreplyContent,

    #[serde(default)]
    pub on_failure: OnFailure,
}

/// Content for autoreply action
/// Can be a simple string or explicit prepend/append
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AutoreplyContent {
    /// Short form: simple text message
    /// YAML: `autoreply: "text"`
    Simple(String),

    /// Explicit form with prepend/append
    /// YAML: `autoreply: { prepend: "...", append: "..." }`
    /// Supports both scalar and array forms:
    /// `autoreply: { prepend: "single line" }`
    /// `autoreply: { prepend: ["line 1", "line 2"] }`
    Explicit {
        #[serde(default)]
        prepend: Option<TextLines>,
        #[serde(default)]
        append: Option<TextLines>,
    },
}

/// Commit message action (for PrepareCommitMessage events)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitMessageAction {
    pub commit_message: CommitMessageOp,

    #[serde(default)]
    pub on_failure: OnFailure,
}

/// Operations for commit messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitMessageOp {
    /// Append Git trailers (after existing trailers)
    #[serde(default)]
    pub append_trailer: Option<String>,

    /// Append to message body (before trailers)
    #[serde(default)]
    pub append_body: Option<String>,

    /// Prepend to subject line (before first line)
    #[serde(default)]
    pub prepend_subject: Option<String>,

    /// Append footer (after everything)
    #[serde(default)]
    pub append_footer: Option<String>,
}

/// Task run action - spawns an agent session to work on a task
/// YAML: `task.run: { task_id: "abc123" }` or `task.run: { task_id: "$var" }`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunAction {
    /// The task run configuration
    #[serde(rename = "task.run")]
    pub task_run: TaskRunConfig,

    #[serde(default)]
    pub on_failure: OnFailure,
}

/// Configuration for task.run action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRunConfig {
    /// Task ID to run (supports variable interpolation)
    pub task_id: String,

    /// Optional agent override (claude-code, codex)
    #[serde(default)]
    pub agent: Option<String>,
}

/// Review action - creates and runs a code review task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewAction {
    /// The review configuration
    pub review: ReviewConfig,

    #[serde(default)]
    pub on_failure: OnFailure,
}

/// Configuration for review action
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewConfig {
    /// Task ID to review (supports variable interpolation)
    #[serde(default)]
    pub task_id: Option<String>,

    /// Optional agent override (claude-code, codex)
    #[serde(default)]
    pub agent: Option<String>,

    /// Optional template override (default: aiki/review)
    #[serde(default)]
    pub template: Option<String>,
}
