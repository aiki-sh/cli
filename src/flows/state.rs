use std::collections::HashMap;

/// Result of executing an action
#[derive(Debug, Clone)]
pub struct ActionResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Aiki execution state for hook processing
///
/// This holds the mutable state that accumulates during hook execution:
/// - The original triggering event (immutable)
/// - Let-bound variables computed during execution
/// - Metadata about action results
/// - Current hook context
/// - Prompt assembler (for PrePrompt events)
#[derive(Debug, Clone)]
pub struct AikiState {
    /// The original event that triggered this execution
    pub event: crate::events::AikiEvent,

    /// Let-bound variables (user-defined, accessed without event. prefix)
    let_vars: HashMap<String, String>,

    /// Structured metadata for variables (stores ActionResult for each variable)
    variable_metadata: HashMap<String, ActionResult>,

    /// Current hook name (e.g., "aiki/core") for self references
    pub hook_name: Option<String>,

    /// Context assembler for events that build messages
    /// - session.started: accumulates context for session initialization
    /// - turn.started: accumulates prompt modifications and context
    /// - turn.completed: accumulates autoreply content
    context_assembler: Option<crate::flows::context::ContextAssembler>,

    /// Failure messages emitted by the hook
    failures: Vec<crate::events::result::Failure>,

    /// PIDs to send SIGTERM to after all hooks complete
    /// Used by session.end action to defer termination until hooks are done
    pending_session_ends: Vec<u32>,

    /// Cached expression evaluator for compile-once/eval-many condition evaluation.
    /// Persists compiled ASTs across multiple condition evaluations within a session.
    expression_evaluator: crate::expressions::ExpressionEvaluator,
}

impl AikiState {
    #[must_use]
    pub fn new(event: impl Into<crate::events::AikiEvent>) -> Self {
        let event = event.into();

        // Initialize context assembler based on event type
        let context_assembler = match &event {
            crate::events::AikiEvent::SessionStarted(_)
            | crate::events::AikiEvent::SessionResumed(_)
            | crate::events::AikiEvent::SessionCompacted(_)
            | crate::events::AikiEvent::SessionCleared(_) => {
                // session.started/resumed/compacted/cleared: build additional context from scratch
                Some(crate::flows::context::ContextAssembler::new(None, "\n\n"))
            }
            crate::events::AikiEvent::TurnStarted(e) => {
                // turn.started: start with original prompt
                Some(crate::flows::context::ContextAssembler::new(
                    Some(e.prompt.clone()),
                    "\n\n",
                ))
            }
            crate::events::AikiEvent::TurnCompleted(_) => {
                // turn.completed: build autoreply from scratch
                Some(crate::flows::context::ContextAssembler::new(None, "\n\n"))
            }
            _ => None,
        };

        Self {
            event,
            let_vars: HashMap::new(),
            variable_metadata: HashMap::new(),
            hook_name: None,
            context_assembler,
            failures: Vec::new(),
            pending_session_ends: Vec::new(),
            expression_evaluator: crate::expressions::ExpressionEvaluator::new(),
        }
    }

    /// Helper to get the current working directory
    #[must_use]
    pub fn cwd(&self) -> &std::path::Path {
        self.event.cwd()
    }

    /// Get a mutable reference to the cached expression evaluator.
    pub fn expression_evaluator(&mut self) -> &mut crate::expressions::ExpressionEvaluator {
        &mut self.expression_evaluator
    }

    /// Helper to get the agent type
    #[must_use]
    #[allow(dead_code)] // Part of AikiState API
    pub fn agent_type(&self) -> crate::provenance::AgentType {
        self.event.agent_type()
    }

    /// Get a variable value by name
    #[must_use]
    pub fn get_variable(&self, name: &str) -> Option<&String> {
        self.let_vars.get(name)
    }

    /// Set a variable value
    pub fn set_variable(&mut self, name: String, value: String) {
        self.let_vars.insert(name, value);
    }

    /// Iterate over all variables (for VariableResolver)
    pub fn iter_variables(&self) -> impl Iterator<Item = (&String, &String)> {
        self.let_vars.iter()
    }

    /// Store an action result with its metadata
    ///
    /// This stores both the primary value and structured metadata for a variable.
    pub fn store_action_result(&mut self, name: String, result: ActionResult) {
        // Store the primary value
        self.let_vars.insert(name.clone(), result.stdout.clone());

        // Store structured metadata
        self.variable_metadata.insert(name, result);
    }

    /// Get metadata for a variable (test-only)
    #[cfg(test)]
    pub fn get_metadata(&self, name: &str) -> Option<&ActionResult> {
        self.variable_metadata.get(name)
    }

    /// Get mutable reference to the context assembler
    /// Only available for session.started, turn.started, and turn.completed events
    pub fn get_context_assembler_mut(
        &mut self,
    ) -> crate::error::Result<&mut crate::flows::context::ContextAssembler> {
        self.context_assembler.as_mut().ok_or_else(|| {
            crate::error::AikiError::Other(anyhow::anyhow!(
                "Context assembler not available (not a session.started, turn.started, or turn.completed event)"
            ))
        })
    }

    /// Build the final context from accumulated chunks
    /// Works for session.started, turn.started (builds prompt), and turn.completed (builds autoreply)
    /// Returns None if:
    /// - This event doesn't have a context assembler, OR
    /// - The assembler is empty (no Context actions were executed)
    #[must_use]
    pub fn build_context(&self) -> Option<String> {
        self.context_assembler.as_ref().and_then(|assembler| {
            if assembler.is_empty() {
                None
            } else {
                Some(assembler.build())
            }
        })
    }

    /// Add a failure to the failures list
    pub fn add_failure(&mut self, failure: crate::events::result::Failure) {
        self.failures.push(failure);
    }

    /// Take all failures (consumes and returns them, leaving empty Vec)
    pub fn take_failures(&mut self) -> Vec<crate::events::result::Failure> {
        std::mem::take(&mut self.failures)
    }

    /// Get a reference to the failures
    #[must_use]
    pub fn failures(&self) -> &[crate::events::result::Failure] {
        &self.failures
    }

    /// Get the number of failures
    #[must_use]
    #[allow(dead_code)] // Part of AikiState API
    pub fn failures_count(&self) -> usize {
        self.failures.len()
    }

    /// Clear all let-bound variables and their metadata.
    ///
    /// Used by HookComposer to provide variable isolation between composed hooks.
    /// Each hook starts with a fresh variable context.
    pub fn clear_variables(&mut self) {
        self.let_vars.clear();
        self.variable_metadata.clear();
    }

    /// Save current variables and metadata for later restoration.
    ///
    /// Used by `hook:` action to isolate variables: save caller's vars,
    /// clear for target, then restore after.
    pub fn save_variables(&self) -> (HashMap<String, String>, HashMap<String, ActionResult>) {
        (self.let_vars.clone(), self.variable_metadata.clone())
    }

    /// Restore previously saved variables and metadata.
    ///
    /// Must be called unconditionally (even on error) to prevent variable drift.
    pub fn restore_variables(
        &mut self,
        saved: (HashMap<String, String>, HashMap<String, ActionResult>),
    ) {
        self.let_vars = saved.0;
        self.variable_metadata = saved.1;
    }

    /// Register a PID to receive SIGTERM after hooks complete
    ///
    /// Used by session.end action to defer termination until all hooks are done.
    /// This ensures hooks can finish their work before the session is terminated.
    pub fn add_pending_session_end(&mut self, pid: u32) {
        self.pending_session_ends.push(pid);
    }

    /// Execute all pending session terminations
    ///
    /// Sends SIGTERM to all registered PIDs. Called after all hooks complete.
    /// This is synchronous - the termination happens before returning.
    #[cfg(unix)]
    pub fn execute_pending_session_ends(&mut self) {
        use crate::cache::debug_log;

        for pid in self.pending_session_ends.drain(..) {
            debug_log(|| format!("Sending SIGTERM to session PID {}", pid));
            // SAFETY: libc::kill is safe to call with any pid value
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
    }

    /// Execute all pending session terminations (non-Unix stub)
    #[cfg(not(unix))]
    pub fn execute_pending_session_ends(&mut self) {
        use crate::cache::debug_log;

        if !self.pending_session_ends.is_empty() {
            debug_log(|| "session.end: SIGTERM not supported on this platform".to_string());
            self.pending_session_ends.clear();
        }
    }

    /// Check if there are pending session terminations
    #[must_use]
    pub fn has_pending_session_ends(&self) -> bool {
        !self.pending_session_ends.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_action_result_success() {
        let result = ActionResult {
            success: true,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(result.success);
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn test_action_result_failure() {
        let result = ActionResult {
            success: false,
            exit_code: Some(1),
            stdout: String::new(),
            stderr: "error".to_string(),
        };
        assert!(!result.success);
        assert_eq!(result.exit_code, Some(1));
        assert_eq!(result.stderr, "error");
    }

    #[test]
    fn test_execution_context_with_event() {
        use crate::events::{
            AikiChangeCompletedPayload, AikiEvent, ChangeOperation, WriteOperation,
        };
        use crate::provenance::record::AgentType;
        use crate::session::{AikiSession, SessionMode};

        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session".to_string(),
            None::<&str>,
            crate::provenance::DetectionMethod::Hook, SessionMode::Interactive,
        );
        let event = AikiEvent::ChangeCompleted(AikiChangeCompletedPayload {
            session,
            cwd: std::path::PathBuf::from("/test"),
            timestamp: chrono::Utc::now(),
            tool_name: "Edit".to_string(),
            success: true,
            turn: crate::events::Turn::unknown(),
            operation: ChangeOperation::Write(WriteOperation {
                file_paths: vec!["/test/file.rs".to_string()],
                edit_details: vec![],
            }),
        });
        let ctx = AikiState::new(event);

        // Verify we can access event fields through the enum
        match &ctx.event {
            AikiEvent::ChangeCompleted(e) => {
                assert_eq!(e.operation.file_paths(), vec!["/test/file.rs".to_string()]);
            }
            _ => panic!("Expected ChangeCompleted event"),
        }
        assert_eq!(ctx.cwd(), std::path::Path::new("/test"));
    }
}
