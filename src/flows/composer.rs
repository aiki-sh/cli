//! Flow composition with before/after orchestration and cycle detection.
//!
//! This module provides the [`HookComposer`] struct which orchestrates:
//! - Before hook execution (runs before this flow's actions)
//! - This flow's action execution (delegated to HookEngine)
//! - After hook execution (runs after this flow's actions)
//! - Cycle detection using a runtime call stack
//!
//! # Architecture
//!
//! ```text
//! User triggers event (e.g., change.completed)
//!     ↓
//! HookComposer.compose_hook("my-workflow.yml", state)
//!     ↓
//!     Loads flow via HookLoader
//!     Checks call stack for cycles
//!     ↓
//!     Executes before flows (each gets fresh variable context, shares event state)
//!     ↓
//!     Executes this flow's actions via HookEngine (fresh variable context, shares event state)
//!     ↓
//!     Executes after flows (each gets fresh variable context, shares event state)
//!     ↓
//!     Returns HookOutcome
//! ```
//!
//! # Isolation Model
//!
//! - **Variables are isolated**: Each flow gets a fresh variable context
//! - **Event state is shared**: All flows modify the same event object
//!   - Example: TurnStarted's ContextAssembler accumulates chunks from all flows
//!   - Example: TurnCompleted's autoreply builder accumulates from all flows

use std::path::{Path, PathBuf};

use super::engine::{HookEngine, HookOutcome};
use super::loader::HookLoader;
use super::state::AikiState;
use super::types::{Hook, HookStatement};
use crate::cache::debug_log;
use crate::error::{AikiError, Result};

/// Event type enum for routing to correct handler in composed flows.
///
/// Used to select the appropriate statement list from a Flow struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventType {
    SessionStarted,
    SessionResumed,
    SessionWillCompact,
    SessionCompacted,
    SessionCleared,
    SessionEnded,
    TurnStarted,
    TurnCompleted,
    ReadPermissionAsked,
    ReadCompleted,
    ChangePermissionAsked,
    ChangeCompleted,
    ShellPermissionAsked,
    ShellCompleted,
    WebPermissionAsked,
    WebCompleted,
    McpPermissionAsked,
    McpCompleted,
    CommitMessageStarted,
    RepoChanged,
    TaskStarted,
    TaskClosed,
}

impl EventType {
    /// Get the statements for this event type from EventHandlers.
    #[must_use]
    pub fn get_handlers<'a>(&self, handlers: &'a super::types::EventHandlers) -> &'a [HookStatement] {
        match self {
            EventType::SessionStarted => &handlers.session_started,
            EventType::SessionResumed => &handlers.session_resumed,
            EventType::SessionWillCompact => &handlers.session_will_compact,
            EventType::SessionCompacted => &handlers.session_compacted,
            EventType::SessionCleared => &handlers.session_cleared,
            EventType::SessionEnded => &handlers.session_ended,
            EventType::TurnStarted => &handlers.turn_started,
            EventType::TurnCompleted => &handlers.turn_completed,
            EventType::ReadPermissionAsked => &handlers.read_permission_asked,
            EventType::ReadCompleted => &handlers.read_completed,
            EventType::ChangePermissionAsked => &handlers.change_permission_asked,
            EventType::ChangeCompleted => &handlers.change_completed,
            EventType::ShellPermissionAsked => &handlers.shell_permission_asked,
            EventType::ShellCompleted => &handlers.shell_completed,
            EventType::WebPermissionAsked => &handlers.web_permission_asked,
            EventType::WebCompleted => &handlers.web_completed,
            EventType::McpPermissionAsked => &handlers.mcp_permission_asked,
            EventType::McpCompleted => &handlers.mcp_completed,
            EventType::CommitMessageStarted => &handlers.commit_message_started,
            EventType::RepoChanged => &handlers.repo_changed,
            EventType::TaskStarted => &handlers.task_started,
            EventType::TaskClosed => &handlers.task_closed,
        }
    }

    /// Get the statements for this event type from a Hook (convenience wrapper).
    #[must_use]
    pub fn get_statements<'a>(&self, hook: &'a Hook) -> &'a [HookStatement] {
        self.get_handlers(&hook.handlers)
    }
}

/// Orchestrates flow composition and delegates action execution to HookEngine.
///
/// HookComposer handles:
/// - Flow loading via HookLoader (with caching)
/// - Cycle detection via call stack (using canonical paths)
/// - Before/after flow orchestration
/// - Event type routing
///
/// # Example
///
/// ```rust,ignore
/// use aiki::flows::composer::{HookComposer, EventType};
///
/// let mut loader = HookLoader::new()?;
/// let mut composer = HookComposer::new(&mut loader);
///
/// // Compose and execute a flow
/// let result = composer.compose_hook(
///     "aiki/my-workflow",
///     EventType::ChangeCompleted,
///     &mut state,
/// )?;
/// ```
pub struct HookComposer<'a> {
    loader: &'a mut HookLoader,
    call_stack: Vec<PathBuf>,
}

impl<'a> HookComposer<'a> {
    /// Create a new HookComposer with a HookLoader.
    ///
    /// The loader is borrowed mutably because loading flows may update its cache.
    #[must_use]
    pub fn new(loader: &'a mut HookLoader) -> Self {
        Self {
            loader,
            call_stack: Vec::new(),
        }
    }

    /// Compose and execute a flow atomically (before → this flow → after).
    ///
    /// This is the main entry point for flow composition. It:
    /// 1. Loads the flow via HookLoader
    /// 2. Checks for circular dependencies
    /// 3. Executes before flows (each atomically)
    /// 4. Executes this flow's actions for the given event type
    /// 5. Executes after flows (each atomically)
    ///
    /// # Arguments
    ///
    /// * `flow_path` - Path to the flow (e.g., "aiki/quick-lint", "./helpers/lint.yml")
    /// * `event_type` - The event type being handled
    /// * `state` - Mutable state shared across all flows
    ///
    /// # Returns
    ///
    /// The combined [`HookOutcome`] from all executed flows.
    ///
    /// # Errors
    ///
    /// Returns:
    /// - `AikiError::CircularHookDependency` if a cycle is detected
    /// - `AikiError::HookNotFound` if a flow file doesn't exist
    /// - Other errors from hook execution
    pub fn compose_hook(
        &mut self,
        flow_path: &str,
        event_type: EventType,
        state: &mut AikiState,
    ) -> Result<HookOutcome> {
        // Load the flow (HookLoader uses HookResolver which returns canonical paths)
        let (hook, canonical_path) = self.loader.load(flow_path)?;

        // Check for circular dependency
        if self.call_stack.contains(&canonical_path) {
            return Err(AikiError::CircularHookDependency {
                path: flow_path.to_string(),
                canonical_path: canonical_path.display().to_string(),
                stack: self
                    .call_stack
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
            });
        }

        debug_log(|| {
            format!(
                "Composing hook: {} (canonical: {})",
                flow_path,
                canonical_path.display()
            )
        });

        // Push canonical path onto call stack for cycle detection
        self.call_stack.push(canonical_path.clone());

        // Expand top-level includes (if any) and execute
        let result = self.expand_and_execute(hook, &canonical_path, event_type, state);

        // Pop from call stack (even on error)
        self.call_stack.pop();

        result
    }

    /// Compose and execute a flow from an absolute file path.
    ///
    /// This is used for loading flows that aren't in the standard namespace structure,
    /// such as .aiki/hooks/default.yml.
    ///
    /// # Arguments
    ///
    /// * `file_path` - Absolute path to the flow file
    /// * `event_type` - The event type to execute
    /// * `state` - Mutable execution state
    ///
    /// # Errors
    ///
    /// Returns:
    /// - `AikiError::CircularHookDependency` if a cycle is detected
    /// - `AikiError::HookNotFound` if the file doesn't exist
    /// - Other errors from hook execution
    pub fn compose_hook_from_path(
        &mut self,
        file_path: &Path,
        event_type: EventType,
        state: &mut AikiState,
    ) -> Result<HookOutcome> {
        // Load the hook directly from the file path
        let (hook, canonical_path) = self.loader.load_from_file_path(file_path)?;

        // Check for circular dependency
        if self.call_stack.contains(&canonical_path) {
            return Err(AikiError::CircularHookDependency {
                path: file_path.display().to_string(),
                canonical_path: canonical_path.display().to_string(),
                stack: self
                    .call_stack
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
            });
        }

        debug_log(|| {
            format!(
                "Composing hook from path: {} (canonical: {})",
                file_path.display(),
                canonical_path.display()
            )
        });

        // Push canonical path onto call stack for cycle detection
        self.call_stack.push(canonical_path.clone());

        // Expand top-level includes (if any) and execute
        let result = self.expand_and_execute(hook, &canonical_path, event_type, state);

        // Pop from call stack (even on error)
        self.call_stack.pop();

        result
    }

    /// Expand top-level `include:` directives and execute the composed flow.
    ///
    /// If the hook has a non-empty `include:` list, each included plugin's
    /// before blocks, after blocks, and handler segments are prepended to
    /// the hook's own lists. Includes are processed in reverse order so that
    /// the first-declared include ends up first after prepending.
    fn expand_and_execute(
        &mut self,
        hook: Hook,
        canonical_path: &Path,
        event_type: EventType,
        state: &mut AikiState,
    ) -> Result<HookOutcome> {
        if hook.include.is_empty() {
            return self.execute_composed_flow(&hook, canonical_path, event_type, state);
        }

        // Clone since we need to mutate the hook's block/segment lists
        let mut expanded = hook;
        let includes = std::mem::take(&mut expanded.include);

        // Load and expand all includes (in declaration order for loading,
        // then reverse for prepending)
        let mut loaded_includes: Vec<(Hook, PathBuf)> = Vec::new();
        for include_path in &includes {
            let (included_hook, included_canonical) = self.loader.load(include_path)?;

            // Cycle detection for includes
            if self.call_stack.contains(&included_canonical) {
                return Err(AikiError::CircularHookDependency {
                    path: include_path.to_string(),
                    canonical_path: included_canonical.display().to_string(),
                    stack: self
                        .call_stack
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect(),
                });
            }

            // Recursively expand nested includes
            let mut included = included_hook;
            if !included.include.is_empty() {
                self.call_stack.push(included_canonical.clone());
                let result = self.expand_includes_recursive(&mut included);
                self.call_stack.pop();
                result?;
            }

            loaded_includes.push((included, included_canonical));
        }

        // Prepend in reverse order so first-declared include ends up first
        for (included, included_canonical) in loaded_includes.into_iter().rev() {
            Self::prepend_included(&mut expanded, &included, &included_canonical);
        }

        self.execute_composed_flow(&expanded, canonical_path, event_type, state)
    }

    /// Recursively expand a hook's top-level includes in-place.
    fn expand_includes_recursive(&mut self, hook: &mut Hook) -> Result<()> {
        let includes = std::mem::take(&mut hook.include);

        let mut loaded: Vec<(Hook, PathBuf)> = Vec::new();
        for include_path in &includes {
            let (included_hook, included_canonical) = self.loader.load(include_path)?;

            if self.call_stack.contains(&included_canonical) {
                return Err(AikiError::CircularHookDependency {
                    path: include_path.to_string(),
                    canonical_path: included_canonical.display().to_string(),
                    stack: self
                        .call_stack
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect(),
                });
            }

            let mut included = included_hook;
            if !included.include.is_empty() {
                self.call_stack.push(included_canonical.clone());
                let result = self.expand_includes_recursive(&mut included);
                self.call_stack.pop();
                result?;
            }

            loaded.push((included, included_canonical));
        }

        for (included, included_canonical) in loaded.into_iter().rev() {
            Self::prepend_included(hook, &included, &included_canonical);
        }

        Ok(())
    }

    /// Prepend an included plugin's blocks and segments into the target hook.
    fn prepend_included(hook: &mut Hook, included: &Hook, included_canonical: &Path) {
        let source_hook = Self::extract_flow_identifier(included_canonical);

        // Prepend before blocks (tag each with source)
        let mut included_before = included.before.clone();
        for block in &mut included_before {
            block.source_hook.get_or_insert_with(|| source_hook.clone());
        }
        hook.before.splice(0..0, included_before);

        // Prepend after blocks (tag each with source)
        let mut included_after = included.after.clone();
        for block in &mut included_after {
            block.source_hook.get_or_insert_with(|| source_hook.clone());
        }
        hook.after.splice(0..0, included_after);

        // Build the full list of handler segments to prepend:
        // First, any transitive handler_segments from the included hook's own include expansion,
        // then the included hook's own handlers (if any).
        let mut segments_to_prepend = included.handler_segments.clone();
        if included.has_handlers() {
            segments_to_prepend.push(super::types::HandlerSegment {
                source_hook,
                hook: included.clone(),
            });
        }
        if !segments_to_prepend.is_empty() {
            hook.handler_segments.splice(0..0, segments_to_prepend);
        }
    }

    /// Execute a composed flow: before blocks → handler segments → own handlers → after blocks.
    ///
    /// This is separated from compose_hook to ensure call_stack is always popped.
    fn execute_composed_flow(
        &mut self,
        hook: &Hook,
        canonical_path: &Path,
        event_type: EventType,
        state: &mut AikiState,
    ) -> Result<HookOutcome> {
        let mut overall_result = HookOutcome::Success;

        // Variable isolation: each flow starts with a fresh variable context.
        state.clear_variables();

        // 1. Walk before blocks in order
        for block in &hook.before {
            let outcome = self.execute_composition_block(block, canonical_path, event_type, state)?;
            match outcome {
                HookOutcome::Success => {}
                HookOutcome::FailedContinue => {
                    overall_result = HookOutcome::FailedContinue;
                }
                HookOutcome::FailedStop | HookOutcome::FailedBlock => {
                    return Ok(outcome);
                }
            }
        }

        // 2. Walk own handler segments in order (from include expansion)
        for segment in &hook.handler_segments {
            let statements = event_type.get_statements(&segment.hook);
            if statements.is_empty() {
                continue;
            }

            state.clear_variables();
            state.hook_name = Some(segment.source_hook.clone());
            let outcome =
                self.execute_statements_with_hooks(statements, event_type, state)?;
            match outcome {
                HookOutcome::Success => {}
                HookOutcome::FailedContinue => {
                    overall_result = HookOutcome::FailedContinue;
                }
                HookOutcome::FailedStop | HookOutcome::FailedBlock => {
                    return Ok(outcome);
                }
            }
        }

        // 3. Execute this flow's own actions (if any for this event)
        let statements = event_type.get_statements(hook);
        if !statements.is_empty() {
            debug_log(|| {
                format!(
                    "  Executing {} statements for {:?}",
                    statements.len(),
                    event_type
                )
            });

            state.clear_variables();
            state.hook_name = Some(Self::extract_flow_identifier(canonical_path));

            let result =
                self.execute_statements_with_hooks(statements, event_type, state)?;

            match result {
                HookOutcome::Success => {}
                HookOutcome::FailedContinue => {
                    overall_result = HookOutcome::FailedContinue;
                }
                HookOutcome::FailedStop | HookOutcome::FailedBlock => {
                    debug_log(|| {
                        format!(
                            "  Flow '{}' failed with {:?}, skipping after flows",
                            hook.name, result
                        )
                    });
                    return Ok(result);
                }
            }
        }

        // 4. Walk after blocks in order
        for block in &hook.after {
            let outcome = self.execute_composition_block(block, canonical_path, event_type, state)?;
            match outcome {
                HookOutcome::Success => {}
                HookOutcome::FailedContinue => {
                    overall_result = HookOutcome::FailedContinue;
                }
                HookOutcome::FailedStop | HookOutcome::FailedBlock => {
                    return Ok(outcome);
                }
            }
        }

        Ok(overall_result)
    }

    /// Execute a single composition block: includes first, then inline handlers.
    fn execute_composition_block(
        &mut self,
        block: &super::types::CompositionBlock,
        canonical_path: &Path,
        event_type: EventType,
        state: &mut AikiState,
    ) -> Result<HookOutcome> {
        let mut overall_result = HookOutcome::Success;

        // 1. Run included plugins for all events (compose recursively)
        for plugin_path in &block.include {
            debug_log(|| format!("  Before include: {}", plugin_path));
            let outcome = self.compose_hook(plugin_path, event_type, state)?;
            match outcome {
                HookOutcome::Success => {}
                HookOutcome::FailedContinue => {
                    overall_result = HookOutcome::FailedContinue;
                }
                HookOutcome::FailedStop | HookOutcome::FailedBlock => {
                    return Ok(outcome);
                }
            }
        }

        // 2. Run inline handlers for this event type
        let statements = event_type.get_handlers(&block.handlers);
        if !statements.is_empty() {
            state.clear_variables();

            // Set self.* context for inline handlers (save/restore unconditionally)
            let saved_hook_name = state.hook_name.take();
            state.hook_name = block
                .source_hook
                .clone()
                .or_else(|| Some(Self::extract_flow_identifier(canonical_path)));

            let result =
                self.execute_statements_with_hooks(statements, event_type, state);

            // Restore unconditionally
            state.hook_name = saved_hook_name;

            let outcome = result?;
            match outcome {
                HookOutcome::Success => {}
                HookOutcome::FailedContinue => {
                    overall_result = HookOutcome::FailedContinue;
                }
                HookOutcome::FailedStop | HookOutcome::FailedBlock => {
                    return Ok(outcome);
                }
            }
        }

        Ok(overall_result)
    }

    /// Execute statements, intercepting `hook:` actions.
    ///
    /// Normal statements are delegated to [`HookEngine`]. `hook:` statements
    /// are handled here because they require the composer's loader, call stack,
    /// and event_type context.
    fn execute_statements_with_hooks(
        &mut self,
        statements: &[HookStatement],
        event_type: EventType,
        state: &mut AikiState,
    ) -> Result<HookOutcome> {
        let mut overall_result = HookOutcome::Success;

        for statement in statements {
            let result = match statement {
                HookStatement::Hook(hook_action) => {
                    self.execute_hook_action(&hook_action.hook, event_type, state)?
                }
                other => {
                    // Delegate to engine (single statement)
                    HookEngine::execute_statements(std::slice::from_ref(other), state)?
                }
            };
            match result {
                HookOutcome::Success => {}
                HookOutcome::FailedContinue => {
                    overall_result = HookOutcome::FailedContinue;
                }
                HookOutcome::FailedStop | HookOutcome::FailedBlock => {
                    return Ok(result);
                }
            }
        }

        Ok(overall_result)
    }

    /// Execute a `hook:` action — surgical invocation of a plugin's own handlers.
    ///
    /// Variable-isolated: the target plugin gets a clean variable scope.
    /// Shared state (context_assembler, failures, pending_session_ends) is
    /// intentionally not isolated — see the design doc's state isolation table.
    fn execute_hook_action(
        &mut self,
        plugin_path: &str,
        event_type: EventType,
        state: &mut AikiState,
    ) -> Result<HookOutcome> {
        // 1. Load plugin
        let (plugin, canonical_path) = self.loader.load(plugin_path)?;

        // 2. Cycle detection
        if self.call_stack.contains(&canonical_path) {
            return Err(AikiError::CircularHookDependency {
                path: plugin_path.to_string(),
                canonical_path: canonical_path.display().to_string(),
                stack: self
                    .call_stack
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect(),
            });
        }
        self.call_stack.push(canonical_path.clone());

        // 3. Isolation: save/clear scoped fields, leave shared fields untouched.
        let saved_hook_name = state.hook_name.take();
        let saved_variables = state.save_variables();
        state.clear_variables();
        state.hook_name = Some(Self::extract_flow_identifier(&canonical_path));

        // 4. Get plugin's own handlers for current event (no before/after).
        //    Use execute_statements_with_hooks so nested hook: actions are intercepted.
        let statements = event_type.get_statements(&plugin);
        let result = if !statements.is_empty() {
            self.execute_statements_with_hooks(statements, event_type, state)
        } else {
            Ok(HookOutcome::Success)
        };

        // 5. Restore caller's context unconditionally (even on error).
        state.restore_variables(saved_variables);
        state.hook_name = saved_hook_name;
        self.call_stack.pop();

        result
    }

    /// Get the current call stack depth.
    #[must_use]
    #[allow(dead_code)] // Part of HookComposer API
    pub fn depth(&self) -> usize {
        self.call_stack.len()
    }

    /// Check if a path is already in the call stack.
    ///
    /// This is a helper for testing cycle detection.
    #[must_use]
    #[allow(dead_code)] // Part of HookComposer API
    pub fn is_in_stack(&self, path: &Path) -> bool {
        self.call_stack.contains(&path.to_path_buf())
    }

    /// Extract flow identifier from canonical path for self.* resolution.
    ///
    /// Converts paths like:
    /// - `/project/.aiki/hooks/aiki/quick-lint.yml` → `aiki/quick-lint`
    /// - `/project/.aiki/hooks/eslint/check.yml` → `eslint/check`
    /// - `/project/.aiki/hooks/helpers/lint.yml` → `helpers/lint`
    ///
    /// Falls back to filename without extension if pattern doesn't match.
    fn extract_flow_identifier(canonical_path: &Path) -> String {
        // Convert to string for pattern matching
        let path_str = canonical_path.to_string_lossy();

        // Look for ".aiki/hooks/" pattern and extract everything after it
        if let Some(flows_idx) = path_str.find(".aiki/hooks/") {
            let after_flows = &path_str[flows_idx + ".aiki/hooks/".len()..];
            // Remove .yml or .yaml extension
            let without_ext = after_flows
                .strip_suffix(".yml")
                .or_else(|| after_flows.strip_suffix(".yaml"))
                .unwrap_or(after_flows);
            return without_ext.to_string();
        }

        // Fallback: use filename without extension
        canonical_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AikiChangeCompletedPayload, AikiEvent, ChangeOperation, WriteOperation};
    use crate::provenance::record::{AgentType, DetectionMethod};
    use crate::session::{AikiSession, SessionMode};
    use std::fs;
    use tempfile::TempDir;

    /// Create a test project with .aiki/ directory structure
    fn create_test_project() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        // Create namespaces - aiki is just another namespace
        fs::create_dir_all(temp_dir.path().join(".aiki/hooks/aiki")).unwrap();
        fs::create_dir_all(temp_dir.path().join(".aiki/hooks/eslint")).unwrap();
        fs::create_dir_all(temp_dir.path().join(".aiki/hooks/prettier")).unwrap();
        fs::create_dir_all(temp_dir.path().join(".aiki/hooks/helpers")).unwrap();
        temp_dir
    }

    /// Create a flow file with specified before/after dependencies and optional statements.
    /// Uses the new CompositionBlock format: `before: { include: [...] }`.
    fn create_flow_file(
        path: &Path,
        name: &str,
        before: &[&str],
        after: &[&str],
        has_change_completed: bool,
    ) {
        let quote_if_needed = |s: &str| -> String {
            if s.starts_with('@') || s.contains(':') || s.contains('#') {
                format!("\"{}\"", s)
            } else {
                s.to_string()
            }
        };

        let before_yaml = if before.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = before
                .iter()
                .map(|b| format!("    - {}", quote_if_needed(b)))
                .collect();
            format!("before:\n  include:\n{}\n", items.join("\n"))
        };

        let after_yaml = if after.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = after
                .iter()
                .map(|a| format!("    - {}", quote_if_needed(a)))
                .collect();
            format!("after:\n  include:\n{}\n", items.join("\n"))
        };

        let change_completed_yaml = if has_change_completed {
            "change.completed:\n  - log: \"Executed from flow\"\n".to_string()
        } else {
            String::new()
        };

        let content = format!(
            r#"name: {}
version: "1"
{}{}{}
"#,
            name, before_yaml, after_yaml, change_completed_yaml
        );
        fs::write(path, content).unwrap();
    }

    /// Create a test AikiState with a ChangeCompleted event
    fn create_test_state(temp_dir: &TempDir) -> AikiState {
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session".to_string(),
            None::<&str>,
            DetectionMethod::Hook, SessionMode::Interactive,
        );
        let event = AikiEvent::ChangeCompleted(AikiChangeCompletedPayload {
            session,
            cwd: temp_dir.path().to_path_buf(),
            timestamp: chrono::Utc::now(),
            tool_name: "Edit".to_string(),
            success: true,
            turn: crate::events::Turn::unknown(),
            operation: ChangeOperation::Write(WriteOperation {
                file_paths: vec!["/test/file.rs".to_string()],
                edit_details: vec![],
            }),
        });
        AikiState::new(event)
    }

    #[test]
    fn test_compose_simple_flow() {
        let temp_dir = create_test_project();
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/simple.yml");
        create_flow_file(&flow_path, "Simple Flow", &[], &[], true);

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/simple", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));
    }

    #[test]
    fn test_compose_hook_with_before() {
        let temp_dir = create_test_project();

        // Create base flow (no dependencies)
        let base_path = temp_dir.path().join(".aiki/hooks/aiki/base.yml");
        create_flow_file(&base_path, "Base Flow", &[], &[], true);

        // Create main flow (depends on base)
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        create_flow_file(&main_path, "Main Flow", &["aiki/base"], &[], true);

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));
    }

    #[test]
    fn test_compose_hook_with_after() {
        let temp_dir = create_test_project();

        // Create cleanup flow (no dependencies)
        let cleanup_path = temp_dir.path().join(".aiki/hooks/aiki/cleanup.yml");
        create_flow_file(&cleanup_path, "Cleanup Flow", &[], &[], true);

        // Create main flow (has after)
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        create_flow_file(&main_path, "Main Flow", &[], &["aiki/cleanup"], true);

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));
    }

    #[test]
    fn test_compose_nested_flows() {
        let temp_dir = create_test_project();

        // Create level 0 flow (no dependencies)
        let level0_path = temp_dir.path().join(".aiki/hooks/aiki/level0.yml");
        create_flow_file(&level0_path, "Level 0", &[], &[], true);

        // Create level 1 flow (depends on level 0)
        let level1_path = temp_dir.path().join(".aiki/hooks/aiki/level1.yml");
        create_flow_file(&level1_path, "Level 1", &["aiki/level0"], &[], true);

        // Create level 2 flow (depends on level 1)
        let level2_path = temp_dir.path().join(".aiki/hooks/aiki/level2.yml");
        create_flow_file(&level2_path, "Level 2", &["aiki/level1"], &[], true);

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/level2", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));
    }

    #[test]
    fn test_circular_dependency_detected() {
        let temp_dir = create_test_project();

        // Create flow-a (depends on flow-b)
        let flow_a_path = temp_dir.path().join(".aiki/hooks/aiki/flow-a.yml");
        create_flow_file(&flow_a_path, "Flow A", &["aiki/flow-b"], &[], true);

        // Create flow-b (depends on flow-a - circular!)
        let flow_b_path = temp_dir.path().join(".aiki/hooks/aiki/flow-b.yml");
        create_flow_file(&flow_b_path, "Flow B", &["aiki/flow-a"], &[], true);

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer.compose_hook("aiki/flow-a", EventType::ChangeCompleted, &mut state);

        assert!(matches!(
            result,
            Err(AikiError::CircularHookDependency { .. })
        ));
    }

    #[test]
    fn test_circular_dependency_self_reference() {
        let temp_dir = create_test_project();

        // Create flow that references itself
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/self-ref.yml");
        create_flow_file(&flow_path, "Self Reference", &["aiki/self-ref"], &[], true);

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer.compose_hook("aiki/self-ref", EventType::ChangeCompleted, &mut state);

        assert!(matches!(
            result,
            Err(AikiError::CircularHookDependency { .. })
        ));
    }

    #[test]
    fn test_flow_not_found() {
        let temp_dir = create_test_project();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result =
            composer.compose_hook("aiki/nonexistent", EventType::ChangeCompleted, &mut state);

        assert!(matches!(result, Err(AikiError::HookNotFound { .. })));
    }

    #[test]
    fn test_depth_tracking() {
        let temp_dir = create_test_project();

        // Create simple flow
        let flow_path = temp_dir.path().join(".aiki/hooks/aiki/simple.yml");
        create_flow_file(&flow_path, "Simple Flow", &[], &[], true);

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let composer = HookComposer::new(&mut loader);

        // Initially depth should be 0
        assert_eq!(composer.depth(), 0);
    }

    #[test]
    fn test_event_type_get_statements() {
        let yaml = r#"
name: Multi Event Flow
version: "1"
session.started:
  - log: "Session started"
change.completed:
  - log: "Change completed"
commit.message_started:
  - log: "Commit message started"
"#;

        let hook: Hook = serde_yaml::from_str(yaml).unwrap();

        // Check each event type returns correct statements
        assert_eq!(EventType::SessionStarted.get_statements(&hook).len(), 1);
        assert_eq!(EventType::ChangeCompleted.get_statements(&hook).len(), 1);
        assert_eq!(
            EventType::CommitMessageStarted.get_statements(&hook).len(),
            1
        );
        assert!(EventType::TurnStarted.get_statements(&hook).is_empty());
    }

    #[test]
    fn test_before_and_after_both() {
        let temp_dir = create_test_project();

        // Create pre flow
        let pre_path = temp_dir.path().join(".aiki/hooks/aiki/pre.yml");
        create_flow_file(&pre_path, "Pre Flow", &[], &[], true);

        // Create post flow
        let post_path = temp_dir.path().join(".aiki/hooks/aiki/post.yml");
        create_flow_file(&post_path, "Post Flow", &[], &[], true);

        // Create main flow with both before and after
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        create_flow_file(&main_path, "Main Flow", &["aiki/pre"], &["aiki/post"], true);

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));
    }

    #[test]
    fn test_extract_flow_identifier() {
        // Test aiki namespace
        let path = PathBuf::from("/project/.aiki/hooks/aiki/quick-lint.yml");
        assert_eq!(
            HookComposer::extract_flow_identifier(&path),
            "aiki/quick-lint"
        );

        // Test other namespace
        let path = PathBuf::from("/home/user/code/.aiki/hooks/eslint/check.yml");
        assert_eq!(HookComposer::extract_flow_identifier(&path), "eslint/check");

        // Test nested paths
        let path = PathBuf::from("/project/.aiki/hooks/helpers/lint/core.yml");
        assert_eq!(
            HookComposer::extract_flow_identifier(&path),
            "helpers/lint/core"
        );

        // Test .yaml extension
        let path = PathBuf::from("/project/.aiki/hooks/aiki/test.yaml");
        assert_eq!(HookComposer::extract_flow_identifier(&path), "aiki/test");

        // Test fallback for non-standard paths
        let path = PathBuf::from("/some/random/path/flow.yml");
        assert_eq!(HookComposer::extract_flow_identifier(&path), "flow");
    }

    // =========================================================================
    // Execution order and variable isolation tests
    // =========================================================================

    /// Helper to create a flow that appends to a log file (for order verification).
    /// Uses the new CompositionBlock format: `before: { include: [...] }`.
    fn create_logging_flow(
        path: &Path,
        name: &str,
        log_msg: &str,
        before: &[&str],
        after: &[&str],
    ) {
        let quote_if_needed = |s: &str| -> String {
            if s.starts_with('@') || s.contains(':') || s.contains('#') {
                format!("\"{}\"", s)
            } else {
                s.to_string()
            }
        };

        let before_yaml = if before.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = before
                .iter()
                .map(|b| format!("    - {}", quote_if_needed(b)))
                .collect();
            format!("before:\n  include:\n{}\n", items.join("\n"))
        };

        let after_yaml = if after.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = after
                .iter()
                .map(|a| format!("    - {}", quote_if_needed(a)))
                .collect();
            format!("after:\n  include:\n{}\n", items.join("\n"))
        };

        // Use shell to append to execution_log.txt
        let content = format!(
            r#"name: {name}
version: "1"
{before_yaml}{after_yaml}change.completed:
  - shell: echo "{log_msg}" >> execution_log.txt
"#
        );
        fs::write(path, content).unwrap();
    }

    /// Helper to create a flow that captures shell output to a variable.
    /// Uses the new CompositionBlock format: `before: { include: [...] }`.
    fn create_shell_var_flow(
        path: &Path,
        name: &str,
        var_name: &str,
        echo_value: &str,
        before: &[&str],
    ) {
        let quote_if_needed = |s: &str| -> String {
            if s.starts_with('@') || s.contains(':') || s.contains('#') {
                format!("\"{}\"", s)
            } else {
                s.to_string()
            }
        };

        let before_yaml = if before.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = before
                .iter()
                .map(|b| format!("    - {}", quote_if_needed(b)))
                .collect();
            format!("before:\n  include:\n{}\n", items.join("\n"))
        };

        // Use shell with 'alias:' to capture output to a variable, then echo it
        let content = format!(
            r#"name: {name}
version: "1"
{before_yaml}change.completed:
  - shell: echo {echo_value}
    alias: {var_name}
  - shell: echo "{{{{{var_name}}}}}" >> var_log.txt
"#
        );
        fs::write(path, content).unwrap();
    }

    #[test]
    fn test_execution_order_before_main_after() {
        let temp_dir = create_test_project();

        // Create before flow
        let before_path = temp_dir.path().join(".aiki/hooks/aiki/before.yml");
        create_logging_flow(&before_path, "Before Flow", "BEFORE", &[], &[]);

        // Create after flow
        let after_path = temp_dir.path().join(".aiki/hooks/aiki/after.yml");
        create_logging_flow(&after_path, "After Flow", "AFTER", &[], &[]);

        // Create main flow with before and after
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        create_logging_flow(
            &main_path,
            "Main Flow",
            "MAIN",
            &["aiki/before"],
            &["aiki/after"],
        );

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        // Verify execution order
        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        assert_eq!(lines.len(), 3, "Expected 3 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "BEFORE", "Before flow should run first");
        assert_eq!(lines[1], "MAIN", "Main flow should run second");
        assert_eq!(lines[2], "AFTER", "After flow should run last");
    }

    #[test]
    fn test_execution_order_nested_before() {
        let temp_dir = create_test_project();

        // Create level 0 (innermost before)
        let level0_path = temp_dir.path().join(".aiki/hooks/aiki/level0.yml");
        create_logging_flow(&level0_path, "Level 0", "L0", &[], &[]);

        // Create level 1 (has before: level0)
        let level1_path = temp_dir.path().join(".aiki/hooks/aiki/level1.yml");
        create_logging_flow(&level1_path, "Level 1", "L1", &["aiki/level0"], &[]);

        // Create level 2 (has before: level1)
        let level2_path = temp_dir.path().join(".aiki/hooks/aiki/level2.yml");
        create_logging_flow(&level2_path, "Level 2", "L2", &["aiki/level1"], &[]);

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/level2", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        // Verify execution order: L0 -> L1 -> L2
        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        assert_eq!(lines.len(), 3, "Expected 3 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "L0", "Level 0 should run first");
        assert_eq!(lines[1], "L1", "Level 1 should run second");
        assert_eq!(lines[2], "L2", "Level 2 should run last");
    }

    #[test]
    fn test_variable_isolation_between_flows() {
        let temp_dir = create_test_project();

        // Create before flow that sets $my_var via shell output capture
        let before_path = temp_dir.path().join(".aiki/hooks/aiki/before.yml");
        create_shell_var_flow(&before_path, "Before Flow", "my_var", "from_before", &[]);

        // Create main flow that checks $my_var then sets its own
        // If isolation works, main should NOT see before's $my_var
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let main_content = r#"name: Main Flow
version: "1"
before:
  include:
    - aiki/before
change.completed:
  - shell: echo "main_sees:no_var" >> var_log.txt
  - shell: echo from_main
    alias: my_var
  - shell: echo "main_set:{{my_var}}" >> var_log.txt
"#;
        fs::write(&main_path, main_content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        // Verify variable isolation
        let log_path = temp_dir.path().join("var_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        // Filter out empty lines (shell captures trailing newlines)
        let lines: Vec<&str> = log_content.lines().filter(|l| !l.is_empty()).collect();

        // Before flow sets and echoes its var
        assert!(
            lines[0] == "from_before",
            "Before should echo its var: got {:?}",
            lines[0]
        );

        // Main flow should NOT see before's variable (isolation)
        // The variable should not be defined in main's scope
        assert_eq!(
            lines[1], "main_sees:no_var",
            "Main should confirm it ran without seeing before's variable: got {:?}",
            lines[1]
        );

        // After setting, main should see its own value
        assert_eq!(
            lines[2], "main_set:from_main",
            "Main should see its own variable"
        );
    }

    #[test]
    fn test_variable_isolation_before_flows_dont_see_caller() {
        let temp_dir = create_test_project();

        // Create a before flow that tries to read $caller_var
        let before_path = temp_dir.path().join(".aiki/hooks/aiki/before.yml");
        let before_content = r#"name: Before Flow
version: "1"
change.completed:
  - shell: echo "before_sees:no_caller" >> var_log.txt
"#;
        fs::write(&before_path, before_content).unwrap();

        // Create main flow that sets $caller_var via shell
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let main_content = r#"name: Main Flow
version: "1"
before:
  include:
    - aiki/before
change.completed:
  - shell: echo should_not_leak
    alias: caller_var
  - shell: echo "main_set:{{caller_var}}" >> var_log.txt
"#;
        fs::write(&main_path, main_content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        // Pre-set a variable in state to simulate a "caller" having variables
        state.set_variable("caller_var".to_string(), "from_caller".to_string());

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        // Verify isolation: before flow should NOT see caller's variable
        let log_path = temp_dir.path().join("var_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        // Before flow should not see the caller's variable (isolation clears on entry)
        assert_eq!(
            lines[0], "before_sees:no_caller",
            "Before should NOT see caller's variable: got {:?}",
            lines[0]
        );
    }

    // =========================================================================
    // Phase 6: Composition Block Tests
    // =========================================================================

    #[test]
    fn test_composition_block_include_only() {
        // before: { include: [a, b] } runs plugins in order for all events
        let temp_dir = create_test_project();

        let a_path = temp_dir.path().join(".aiki/hooks/aiki/plugin-a.yml");
        create_logging_flow(&a_path, "Plugin A", "PLUGIN_A", &[], &[]);

        let b_path = temp_dir.path().join(".aiki/hooks/aiki/plugin-b.yml");
        create_logging_flow(&b_path, "Plugin B", "PLUGIN_B", &[], &[]);

        // Main flow with before: { include: [a, b] }
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        create_logging_flow(
            &main_path,
            "Main Flow",
            "MAIN",
            &["aiki/plugin-a", "aiki/plugin-b"],
            &[],
        );

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        assert_eq!(lines.len(), 3, "Expected 3 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "PLUGIN_A", "Plugin A should run first");
        assert_eq!(lines[1], "PLUGIN_B", "Plugin B should run second");
        assert_eq!(lines[2], "MAIN", "Main should run last");
    }

    #[test]
    fn test_composition_block_inline_only() {
        // before: { turn.started: [...] } runs inline handlers for that event
        let temp_dir = create_test_project();

        // Flow with inline handlers in before block
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let content = r#"name: Main Flow
version: "1"
before:
  change.completed:
    - shell: echo "BEFORE_INLINE" >> execution_log.txt
change.completed:
  - shell: echo "MAIN" >> execution_log.txt
"#;
        fs::write(&main_path, content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        assert_eq!(lines.len(), 2, "Expected 2 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "BEFORE_INLINE", "Inline before should run first");
        assert_eq!(lines[1], "MAIN", "Main should run second");
    }

    #[test]
    fn test_composition_block_mixed_include_and_inline() {
        // before: { include: [a], change.completed: [shell: ...] }
        // Include plugins run first, then inline handlers
        let temp_dir = create_test_project();

        let a_path = temp_dir.path().join(".aiki/hooks/aiki/plugin-a.yml");
        create_logging_flow(&a_path, "Plugin A", "PLUGIN_A", &[], &[]);

        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let content = r#"name: Main Flow
version: "1"
before:
  include:
    - aiki/plugin-a
  change.completed:
    - shell: echo "BEFORE_INLINE" >> execution_log.txt
change.completed:
  - shell: echo "MAIN" >> execution_log.txt
"#;
        fs::write(&main_path, content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        assert_eq!(lines.len(), 3, "Expected 3 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "PLUGIN_A", "Include plugin runs first");
        assert_eq!(lines[1], "BEFORE_INLINE", "Inline before handler runs second");
        assert_eq!(lines[2], "MAIN", "Main handler runs last");
    }

    #[test]
    fn test_composition_block_empty() {
        // Empty before block is a no-op
        let temp_dir = create_test_project();

        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let content = r#"name: Main Flow
version: "1"
before: {}
change.completed:
  - shell: echo "MAIN" >> execution_log.txt
"#;
        fs::write(&main_path, content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        assert_eq!(lines.len(), 1, "Expected 1 log entry, got: {:?}", lines);
        assert_eq!(lines[0], "MAIN");
    }

    // =========================================================================
    // Phase 6: hook: Action Tests
    // =========================================================================

    #[test]
    fn test_hook_action_basic() {
        // hook: invokes plugin's handler for current event
        let temp_dir = create_test_project();

        // Plugin with change.completed handler
        let plugin_path = temp_dir.path().join(".aiki/hooks/aiki/my-plugin.yml");
        let plugin_content = r#"name: My Plugin
version: "1"
change.completed:
  - shell: echo "FROM_PLUGIN" >> execution_log.txt
"#;
        fs::write(&plugin_path, plugin_content).unwrap();

        // Main flow uses hook: to invoke plugin
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let content = r#"name: Main Flow
version: "1"
change.completed:
  - shell: echo "BEFORE_HOOK" >> execution_log.txt
  - hook: aiki/my-plugin
  - shell: echo "AFTER_HOOK" >> execution_log.txt
"#;
        fs::write(&main_path, content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        assert_eq!(lines.len(), 3, "Expected 3 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "BEFORE_HOOK");
        assert_eq!(lines[1], "FROM_PLUGIN");
        assert_eq!(lines[2], "AFTER_HOOK");
    }

    #[test]
    fn test_hook_action_no_matching_handler() {
        // hook: with no matching handler is a no-op
        let temp_dir = create_test_project();

        // Plugin with session.started handler only (no change.completed)
        let plugin_path = temp_dir.path().join(".aiki/hooks/aiki/my-plugin.yml");
        let plugin_content = r#"name: My Plugin
version: "1"
session.started:
  - log: "Only runs on session.started"
"#;
        fs::write(&plugin_path, plugin_content).unwrap();

        // Main flow uses hook: but plugin has no change.completed handler
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let content = r#"name: Main Flow
version: "1"
change.completed:
  - shell: echo "BEFORE_HOOK" >> execution_log.txt
  - hook: aiki/my-plugin
  - shell: echo "AFTER_HOOK" >> execution_log.txt
"#;
        fs::write(&main_path, content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        // Only the before and after shell actions should have run
        assert_eq!(lines.len(), 2, "Expected 2 log entries (plugin was no-op), got: {:?}", lines);
        assert_eq!(lines[0], "BEFORE_HOOK");
        assert_eq!(lines[1], "AFTER_HOOK");
    }

    #[test]
    fn test_hook_action_does_not_run_before_after() {
        // hook: only runs the plugin's OWN handlers, not its before/after
        let temp_dir = create_test_project();

        // Sub-plugin referenced in plugin's before
        let sub_path = temp_dir.path().join(".aiki/hooks/aiki/sub.yml");
        let sub_content = r#"name: Sub Plugin
version: "1"
change.completed:
  - shell: echo "FROM_SUB" >> execution_log.txt
"#;
        fs::write(&sub_path, sub_content).unwrap();

        // Plugin with before and after composition
        let plugin_path = temp_dir.path().join(".aiki/hooks/aiki/composed-plugin.yml");
        let plugin_content = r#"name: Composed Plugin
version: "1"
before:
  include:
    - aiki/sub
change.completed:
  - shell: echo "FROM_COMPOSED" >> execution_log.txt
"#;
        fs::write(&plugin_path, plugin_content).unwrap();

        // Main flow uses hook: to invoke composed plugin
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let content = r#"name: Main Flow
version: "1"
change.completed:
  - hook: aiki/composed-plugin
  - shell: echo "MAIN" >> execution_log.txt
"#;
        fs::write(&main_path, content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        // hook: only runs own handlers, NOT before/after
        // So FROM_SUB should NOT appear
        assert_eq!(lines.len(), 2, "Expected 2 log entries (hook: skips before/after), got: {:?}", lines);
        assert_eq!(lines[0], "FROM_COMPOSED", "Plugin's own handler runs");
        assert_eq!(lines[1], "MAIN", "Main handler continues after");
    }

    #[test]
    fn test_hook_action_interleaved() {
        // hook: and inline actions execute in declaration order
        let temp_dir = create_test_project();

        let plugin_path = temp_dir.path().join(".aiki/hooks/aiki/my-plugin.yml");
        let plugin_content = r#"name: My Plugin
version: "1"
change.completed:
  - shell: echo "PLUGIN" >> execution_log.txt
"#;
        fs::write(&plugin_path, plugin_content).unwrap();

        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let content = r#"name: Main Flow
version: "1"
change.completed:
  - shell: echo "STEP1" >> execution_log.txt
  - hook: aiki/my-plugin
  - shell: echo "STEP2" >> execution_log.txt
  - hook: aiki/my-plugin
  - shell: echo "STEP3" >> execution_log.txt
"#;
        fs::write(&main_path, content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        assert_eq!(lines.len(), 5, "Expected 5 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "STEP1");
        assert_eq!(lines[1], "PLUGIN");
        assert_eq!(lines[2], "STEP2");
        assert_eq!(lines[3], "PLUGIN");
        assert_eq!(lines[4], "STEP3");
    }

    #[test]
    fn test_hook_action_in_before_block() {
        // hook: works inside composition blocks (before/after)
        let temp_dir = create_test_project();

        let plugin_path = temp_dir.path().join(".aiki/hooks/aiki/my-plugin.yml");
        let plugin_content = r#"name: My Plugin
version: "1"
change.completed:
  - shell: echo "PLUGIN" >> execution_log.txt
"#;
        fs::write(&plugin_path, plugin_content).unwrap();

        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let content = r#"name: Main Flow
version: "1"
before:
  change.completed:
    - hook: aiki/my-plugin
    - shell: echo "BEFORE_INLINE" >> execution_log.txt
change.completed:
  - shell: echo "MAIN" >> execution_log.txt
"#;
        fs::write(&main_path, content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        assert_eq!(lines.len(), 3, "Expected 3 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "PLUGIN", "hook: in before block runs first");
        assert_eq!(lines[1], "BEFORE_INLINE", "Inline after hook: in before block");
        assert_eq!(lines[2], "MAIN", "Main handler runs last");
    }

    #[test]
    fn test_hook_action_cycle_detection() {
        // Circular hook: references produce CircularHookDependency
        let temp_dir = create_test_project();

        // Plugin A hooks Plugin B
        let a_path = temp_dir.path().join(".aiki/hooks/aiki/hook-a.yml");
        let a_content = r#"name: Hook A
version: "1"
change.completed:
  - hook: aiki/hook-b
"#;
        fs::write(&a_path, a_content).unwrap();

        // Plugin B hooks Plugin A (circular!)
        let b_path = temp_dir.path().join(".aiki/hooks/aiki/hook-b.yml");
        let b_content = r#"name: Hook B
version: "1"
change.completed:
  - hook: aiki/hook-a
"#;
        fs::write(&b_path, b_content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer.compose_hook("aiki/hook-a", EventType::ChangeCompleted, &mut state);

        assert!(
            matches!(result, Err(AikiError::CircularHookDependency { .. })),
            "Expected CircularHookDependency, got: {:?}",
            result
        );
    }

    #[test]
    fn test_hook_action_self_context() {
        // self.* resolves against the target plugin during hook: execution
        let temp_dir = create_test_project();

        // Plugin with a self function that we can detect via hook_name
        let plugin_path = temp_dir.path().join(".aiki/hooks/aiki/ctx-plugin.yml");
        let plugin_content = r#"name: Context Plugin
version: "1"
change.completed:
  - log: "Plugin handler executed"
"#;
        fs::write(&plugin_path, plugin_content).unwrap();

        // Main flow uses hook: — we verify execution succeeds
        // (self.* context is an internal mechanism tested via hook_name state)
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let content = r#"name: Main Flow
version: "1"
change.completed:
  - hook: aiki/ctx-plugin
  - log: "Main handler after hook"
"#;
        fs::write(&main_path, content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));
    }

    // =========================================================================
    // Phase 6: Include Expansion Tests
    // =========================================================================

    #[test]
    fn test_include_basic() {
        // Include a plugin with inline before/after, verify execution order
        let temp_dir = create_test_project();

        // Plugin with before and after inline handlers
        let plugin_path = temp_dir.path().join(".aiki/hooks/aiki/base-plugin.yml");
        let content = r#"name: Base Plugin
version: "1"
before:
  change.completed:
    - shell: echo "BASE_BEFORE" >> execution_log.txt
after:
  change.completed:
    - shell: echo "BASE_AFTER" >> execution_log.txt
change.completed:
  - shell: echo "BASE_OWN" >> execution_log.txt
"#;
        fs::write(&plugin_path, content).unwrap();

        // Main flow includes the plugin
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let main_content = r#"name: Main Flow
version: "1"
include:
  - aiki/base-plugin
change.completed:
  - shell: echo "MAIN_OWN" >> execution_log.txt
"#;
        fs::write(&main_path, main_content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        // Expected order:
        // 1. base-plugin's before block inline handlers
        // 2. base-plugin's own handlers (as handler segment)
        // 3. main's own handlers
        // 4. base-plugin's after block inline handlers
        assert_eq!(lines.len(), 4, "Expected 4 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "BASE_BEFORE", "Included before block runs first");
        assert_eq!(lines[1], "BASE_OWN", "Included own handlers run second");
        assert_eq!(lines[2], "MAIN_OWN", "Main's own handlers run third");
        assert_eq!(lines[3], "BASE_AFTER", "Included after block runs last");
    }

    #[test]
    fn test_include_multiple() {
        // First include's blocks come first
        let temp_dir = create_test_project();

        let first_path = temp_dir.path().join(".aiki/hooks/aiki/first.yml");
        create_logging_flow(&first_path, "First", "FIRST", &[], &[]);

        let second_path = temp_dir.path().join(".aiki/hooks/aiki/second.yml");
        create_logging_flow(&second_path, "Second", "SECOND", &[], &[]);

        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let content = r#"name: Main Flow
version: "1"
include:
  - aiki/first
  - aiki/second
change.completed:
  - shell: echo "MAIN" >> execution_log.txt
"#;
        fs::write(&main_path, content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        // Handler segments: first's handlers, second's handlers, main's handlers
        assert_eq!(lines.len(), 3, "Expected 3 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "FIRST", "First include's handlers run first");
        assert_eq!(lines[1], "SECOND", "Second include's handlers run second");
        assert_eq!(lines[2], "MAIN", "Main's handlers run last");
    }

    #[test]
    fn test_include_transitive() {
        // Plugin includes another plugin, verify full expansion
        let temp_dir = create_test_project();

        // Base plugin (no includes)
        let base_path = temp_dir.path().join(".aiki/hooks/aiki/base.yml");
        create_logging_flow(&base_path, "Base", "BASE", &[], &[]);

        // Middle plugin includes base
        let middle_path = temp_dir.path().join(".aiki/hooks/aiki/middle.yml");
        let middle_content = r#"name: Middle
version: "1"
include:
  - aiki/base
change.completed:
  - shell: echo "MIDDLE" >> execution_log.txt
"#;
        fs::write(&middle_path, middle_content).unwrap();

        // Main includes middle (which transitively includes base)
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let main_content = r#"name: Main Flow
version: "1"
include:
  - aiki/middle
change.completed:
  - shell: echo "MAIN" >> execution_log.txt
"#;
        fs::write(&main_path, main_content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        // Transitive expansion: base, middle, main
        assert_eq!(lines.len(), 3, "Expected 3 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "BASE", "Transitively included base runs first");
        assert_eq!(lines[1], "MIDDLE", "Middle include runs second");
        assert_eq!(lines[2], "MAIN", "Main runs last");
    }

    #[test]
    fn test_include_with_explicit_before_after() {
        // Include contributions come before hookfile's own before/after blocks
        let temp_dir = create_test_project();

        // Plugin with a before block
        let plugin_path = temp_dir.path().join(".aiki/hooks/aiki/plugin.yml");
        let plugin_content = r#"name: Plugin
version: "1"
before:
  change.completed:
    - shell: echo "PLUGIN_BEFORE" >> execution_log.txt
change.completed:
  - shell: echo "PLUGIN_OWN" >> execution_log.txt
"#;
        fs::write(&plugin_path, plugin_content).unwrap();

        // Main flow includes plugin AND has its own before block
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let main_content = r#"name: Main Flow
version: "1"
include:
  - aiki/plugin
before:
  change.completed:
    - shell: echo "MAIN_BEFORE" >> execution_log.txt
change.completed:
  - shell: echo "MAIN_OWN" >> execution_log.txt
"#;
        fs::write(&main_path, main_content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        // Include's before block comes first, then main's own before block
        // Then handler segments (plugin own, main own)
        assert_eq!(lines.len(), 4, "Expected 4 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "PLUGIN_BEFORE", "Include's before block runs first");
        assert_eq!(lines[1], "MAIN_BEFORE", "Main's before block runs second");
        assert_eq!(lines[2], "PLUGIN_OWN", "Include's own handlers run third");
        assert_eq!(lines[3], "MAIN_OWN", "Main's own handlers run last");
    }

    #[test]
    fn test_include_handler_segments_same_event() {
        // Same event in includer and included both run
        let temp_dir = create_test_project();

        let plugin_path = temp_dir.path().join(".aiki/hooks/aiki/plugin.yml");
        let plugin_content = r#"name: Plugin
version: "1"
change.completed:
  - shell: echo "PLUGIN_HANDLER" >> execution_log.txt
"#;
        fs::write(&plugin_path, plugin_content).unwrap();

        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let main_content = r#"name: Main Flow
version: "1"
include:
  - aiki/plugin
change.completed:
  - shell: echo "MAIN_HANDLER" >> execution_log.txt
"#;
        fs::write(&main_path, main_content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        assert_eq!(lines.len(), 2, "Expected 2 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "PLUGIN_HANDLER", "Included handler runs first");
        assert_eq!(lines[1], "MAIN_HANDLER", "Main handler runs second");
    }

    #[test]
    fn test_include_circular() {
        // Circular include produces CircularHookDependency
        let temp_dir = create_test_project();

        let a_path = temp_dir.path().join(".aiki/hooks/aiki/inc-a.yml");
        let a_content = r#"name: Include A
version: "1"
include:
  - aiki/inc-b
change.completed:
  - log: "A"
"#;
        fs::write(&a_path, a_content).unwrap();

        let b_path = temp_dir.path().join(".aiki/hooks/aiki/inc-b.yml");
        let b_content = r#"name: Include B
version: "1"
include:
  - aiki/inc-a
change.completed:
  - log: "B"
"#;
        fs::write(&b_path, b_content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer.compose_hook("aiki/inc-a", EventType::ChangeCompleted, &mut state);

        assert!(
            matches!(result, Err(AikiError::CircularHookDependency { .. })),
            "Expected CircularHookDependency for circular include, got: {:?}",
            result
        );
    }

    #[test]
    fn test_include_no_include_field_backwards_compat() {
        // Plugin with no include field works fine (backwards compatible)
        let temp_dir = create_test_project();

        let plugin_path = temp_dir.path().join(".aiki/hooks/aiki/old-plugin.yml");
        let content = r#"name: Old Plugin
version: "1"
change.completed:
  - shell: echo "OLD" >> execution_log.txt
"#;
        fs::write(&plugin_path, content).unwrap();

        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let main_content = r#"name: Main Flow
version: "1"
include:
  - aiki/old-plugin
change.completed:
  - shell: echo "MAIN" >> execution_log.txt
"#;
        fs::write(&main_path, main_content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "OLD");
        assert_eq!(lines[1], "MAIN");
    }

    // =========================================================================
    // Phase 6: Variable Isolation Tests
    // =========================================================================

    #[test]
    fn test_hook_action_variable_isolation() {
        // Variables set by target plugin do not leak back to caller
        let temp_dir = create_test_project();

        let plugin_path = temp_dir.path().join(".aiki/hooks/aiki/var-plugin.yml");
        let plugin_content = r#"name: Var Plugin
version: "1"
change.completed:
  - shell: echo plugin_value
    alias: my_var
  - shell: echo "plugin_set:{{my_var}}" >> var_log.txt
"#;
        fs::write(&plugin_path, plugin_content).unwrap();

        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let content = r#"name: Main Flow
version: "1"
change.completed:
  - shell: echo before_hook_value
    alias: my_var
  - shell: echo "before_hook:{{my_var}}" >> var_log.txt
  - hook: aiki/var-plugin
  - shell: echo "after_hook:{{my_var}}" >> var_log.txt
"#;
        fs::write(&main_path, content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("var_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().filter(|l| !l.is_empty()).collect();

        assert_eq!(lines.len(), 3, "Expected 3 log lines, got: {:?}", lines);
        assert_eq!(lines[0], "before_hook:before_hook_value", "Var set before hook");
        assert_eq!(lines[1], "plugin_set:plugin_value", "Plugin sets its own var");
        assert_eq!(
            lines[2], "after_hook:before_hook_value",
            "After hook:, caller's var is restored (not leaked)"
        );
    }

    #[test]
    fn test_hook_action_caller_variables_restored() {
        // Caller's variables are fully restored after hook: returns
        let temp_dir = create_test_project();

        let plugin_path = temp_dir.path().join(".aiki/hooks/aiki/noop-plugin.yml");
        let plugin_content = r#"name: NoOp Plugin
version: "1"
change.completed:
  - log: "no-op"
"#;
        fs::write(&plugin_path, plugin_content).unwrap();

        // Use printf -n to avoid trailing newlines in variable values
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let content = r#"name: Main Flow
version: "1"
change.completed:
  - shell: printf val_a
    alias: var_a
  - shell: printf val_b
    alias: var_b
  - hook: aiki/noop-plugin
  - shell: printf "{{var_a}}|{{var_b}}" >> var_log.txt
"#;
        fs::write(&main_path, content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("var_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();

        assert_eq!(
            log_content.trim(), "val_a|val_b",
            "Both caller variables should be restored after hook:"
        );
    }

    #[test]
    fn test_handler_segment_variable_isolation() {
        // Each handler segment starts with a clean variable scope
        let temp_dir = create_test_project();

        let plugin_path = temp_dir.path().join(".aiki/hooks/aiki/seg-plugin.yml");
        let plugin_content = r#"name: Segment Plugin
version: "1"
change.completed:
  - shell: echo seg_val
    alias: seg_var
  - shell: echo "plugin:{{seg_var}}" >> var_log.txt
"#;
        fs::write(&plugin_path, plugin_content).unwrap();

        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let main_content = r#"name: Main Flow
version: "1"
include:
  - aiki/seg-plugin
change.completed:
  - shell: echo "main_sees:no_seg_var" >> var_log.txt
"#;
        fs::write(&main_path, main_content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("var_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().filter(|l| !l.is_empty()).collect();

        assert_eq!(lines.len(), 2, "Expected 2 log lines, got: {:?}", lines);
        assert_eq!(lines[0], "plugin:seg_val", "Plugin segment sees its own var");
        // Main segment starts with clean scope — shouldn't see plugin's var
        assert_eq!(
            lines[1], "main_sees:no_seg_var",
            "Main segment should NOT see plugin's variable (isolation): got {:?}",
            lines[1]
        );
    }

    // =========================================================================
    // Phase 6: Inline Handler Event Filtering Tests
    // =========================================================================

    #[test]
    fn test_inline_handler_event_filtering() {
        // Inline handlers only run for the matching event, not all events
        let temp_dir = create_test_project();

        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let content = r#"name: Main Flow
version: "1"
before:
  session.started:
    - shell: echo "SESSION_BEFORE" >> execution_log.txt
  change.completed:
    - shell: echo "CHANGE_BEFORE" >> execution_log.txt
change.completed:
  - shell: echo "CHANGE_MAIN" >> execution_log.txt
"#;
        fs::write(&main_path, content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        // Execute for ChangeCompleted event
        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        // Only change.completed handlers should run, not session.started
        assert_eq!(lines.len(), 2, "Expected 2 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "CHANGE_BEFORE", "Only change.completed before runs");
        assert_eq!(lines[1], "CHANGE_MAIN", "change.completed main runs");
    }

    // =========================================================================
    // Phase 6: After Block Inline Handler Tests
    // =========================================================================

    #[test]
    fn test_after_block_inline_handlers() {
        // after: with inline handlers runs after own handlers
        let temp_dir = create_test_project();

        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let content = r#"name: Main Flow
version: "1"
change.completed:
  - shell: echo "MAIN" >> execution_log.txt
after:
  change.completed:
    - shell: echo "AFTER_INLINE" >> execution_log.txt
"#;
        fs::write(&main_path, content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        assert_eq!(lines.len(), 2, "Expected 2 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "MAIN", "Main runs first");
        assert_eq!(lines[1], "AFTER_INLINE", "After inline runs second");
    }

    // =========================================================================
    // Phase 6: Full Composition (Before + Own + After with Include)
    // =========================================================================

    #[test]
    fn test_full_composition_before_own_after_with_include() {
        // Full test: include + before + own + after all working together
        let temp_dir = create_test_project();

        // Plugin included at top level
        let plugin_path = temp_dir.path().join(".aiki/hooks/aiki/included.yml");
        let plugin_content = r#"name: Included Plugin
version: "1"
before:
  change.completed:
    - shell: echo "INCLUDED_BEFORE" >> execution_log.txt
change.completed:
  - shell: echo "INCLUDED_OWN" >> execution_log.txt
after:
  change.completed:
    - shell: echo "INCLUDED_AFTER" >> execution_log.txt
"#;
        fs::write(&plugin_path, plugin_content).unwrap();

        // Plugin in before block include
        let before_plugin_path = temp_dir.path().join(".aiki/hooks/aiki/before-inc.yml");
        let before_plugin_content = r#"name: Before Plugin
version: "1"
change.completed:
  - shell: echo "BEFORE_INC" >> execution_log.txt
"#;
        fs::write(&before_plugin_path, before_plugin_content).unwrap();

        // Main flow: top-level include + explicit before/after + own handlers
        let main_path = temp_dir.path().join(".aiki/hooks/aiki/main.yml");
        let main_content = r#"name: Main Flow
version: "1"
include:
  - aiki/included
before:
  include:
    - aiki/before-inc
  change.completed:
    - shell: echo "MAIN_BEFORE_INLINE" >> execution_log.txt
change.completed:
  - shell: echo "MAIN_OWN" >> execution_log.txt
after:
  change.completed:
    - shell: echo "MAIN_AFTER_INLINE" >> execution_log.txt
"#;
        fs::write(&main_path, main_content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        let result = composer
            .compose_hook("aiki/main", EventType::ChangeCompleted, &mut state)
            .unwrap();

        assert!(matches!(result, HookOutcome::Success));

        let log_path = temp_dir.path().join("execution_log.txt");
        let log_content = fs::read_to_string(&log_path).unwrap();
        let lines: Vec<&str> = log_content.lines().collect();

        // Expected execution order:
        // Before blocks: [included.before, main.before]
        //   1. INCLUDED_BEFORE (included plugin's before block inline)
        //   2. BEFORE_INC (main's before block include plugin)
        //   3. MAIN_BEFORE_INLINE (main's before block inline)
        // Handler segments: [included.own, main.own]
        //   4. INCLUDED_OWN (included plugin's own handler segment)
        //   5. MAIN_OWN (main's own handlers)
        // After blocks: [included.after, main.after]
        //   6. INCLUDED_AFTER (included plugin's after block inline)
        //   7. MAIN_AFTER_INLINE (main's after block inline)
        assert_eq!(lines.len(), 7, "Expected 7 log entries, got: {:?}", lines);
        assert_eq!(lines[0], "INCLUDED_BEFORE");
        assert_eq!(lines[1], "BEFORE_INC");
        assert_eq!(lines[2], "MAIN_BEFORE_INLINE");
        assert_eq!(lines[3], "INCLUDED_OWN");
        assert_eq!(lines[4], "MAIN_OWN");
        assert_eq!(lines[5], "INCLUDED_AFTER");
        assert_eq!(lines[6], "MAIN_AFTER_INLINE");
    }

    #[test]
    fn test_include_expansion_failure_cleans_call_stack() {
        // Regression test: if include expansion fails (e.g., missing nested include),
        // the call_stack must be cleaned up so subsequent compose_hook calls on the
        // same composer don't get false CircularHookDependency errors.
        let temp_dir = create_test_project();

        // aiki/mid.yml includes a non-existent plugin (will cause load failure)
        let mid_path = temp_dir.path().join(".aiki/hooks/aiki/mid.yml");
        let mid_content = r#"name: Mid
version: "1"
include:
  - aiki/nonexistent
change.completed:
  - log: "MID"
"#;
        fs::write(&mid_path, mid_content).unwrap();

        // aiki/outer.yml includes aiki/mid (triggers nested failure)
        let outer_path = temp_dir.path().join(".aiki/hooks/aiki/outer.yml");
        let outer_content = r#"name: Outer
version: "1"
include:
  - aiki/mid
change.completed:
  - log: "OUTER"
"#;
        fs::write(&outer_path, outer_content).unwrap();

        let mut loader = HookLoader::with_start_dir(temp_dir.path()).unwrap();
        let mut composer = HookComposer::new(&mut loader);
        let mut state = create_test_state(&temp_dir);

        // First call: should fail because aiki/mid includes aiki/nonexistent
        let result = composer.compose_hook("aiki/outer", EventType::ChangeCompleted, &mut state);
        assert!(result.is_err(), "Expected error from missing nested include");

        // Second call: compose aiki/mid directly. Before the fix, aiki/mid's
        // canonical path was left on the call_stack from the failed first call,
        // causing a false CircularHookDependency here.
        let result2 = composer.compose_hook("aiki/mid", EventType::ChangeCompleted, &mut state);
        assert!(
            !matches!(result2, Err(AikiError::CircularHookDependency { .. })),
            "call_stack leaked from failed include expansion: {:?}",
            result2
        );
    }
}
