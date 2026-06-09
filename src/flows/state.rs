use std::cell::OnceCell;
use std::collections::HashMap;
use std::rc::Rc;

/// Result of executing an action
#[derive(Debug, Clone)]
pub struct ActionResult {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl ActionResult {
    pub fn success() -> Self {
        Self {
            success: true,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        }
    }
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

    /// Set by the `end_session` action to signal that the session should end.
    /// The turn.completed handler reads this to set Decision::Block, which
    /// the Codex stop output builder translates to `{ "continue": false }`.
    pub end_session: bool,

    /// Cached expression evaluator for compile-once/eval-many condition evaluation.
    /// Persists compiled ASTs across multiple condition evaluations within a session.
    expression_evaluator: crate::expressions::ExpressionEvaluator,

    /// Lazily cached thread-session match for task.closed events.
    /// Stores `Some(None)` after a miss so failed lookups are also memoized.
    task_closed_thread_session: Rc<OnceCell<Option<crate::session::ThreadSessionInfo>>>,
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
            end_session: false,
            expression_evaluator: crate::expressions::ExpressionEvaluator::new(),
            task_closed_thread_session: Rc::new(OnceCell::new()),
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

    /// Resolve the task-thread session once for a task.closed event and cache the result.
    #[must_use]
    pub fn resolve_task_closed_thread_session(&self) -> Option<crate::session::ThreadSessionInfo> {
        self.task_closed_thread_session
            .get_or_init(|| match &self.event {
                crate::events::AikiEvent::TaskClosed(e) => {
                    crate::session::find_thread_session(&e.task.id)
                }
                _ => None,
            })
            .clone()
    }

    /// Get the shared per-event cache used by task.closed lazy session variables.
    #[must_use]
    pub fn task_closed_thread_session_cache(
        &self,
    ) -> Rc<OnceCell<Option<crate::session::ThreadSessionInfo>>> {
        Rc::clone(&self.task_closed_thread_session)
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

    /// Returns true if an autoreply (or context) has been queued in this hook run.
    #[must_use]
    pub fn has_pending_autoreply(&self) -> bool {
        self.context_assembler
            .as_ref()
            .is_some_and(|a| !a.is_empty())
    }

    /// Build the final context from accumulated chunks without the original prompt/body.
    ///
    /// This is the right default for hook adapters that already receive the
    /// original user prompt separately and only need injected context.
    /// Returns None if:
    /// - This event doesn't have a context assembler, OR
    /// - The assembler is empty (no Context actions were executed)
    #[must_use]
    pub fn build_context(&self) -> Option<String> {
        self.context_assembler.as_ref().and_then(|assembler| {
            if assembler.is_empty() {
                None
            } else {
                Some(assembler.build_without_original())
            }
        })
    }

    /// Build the final context from accumulated chunks including original prompt/body.
    ///
    /// This preserves the legacy prompt-rewrite behavior for callers that need it.
    #[must_use]
    #[allow(dead_code)]
    pub fn build_context_with_original_prompt(&self) -> Option<String> {
        self.context_assembler.as_ref().and_then(|assembler| {
            if assembler.is_empty() {
                None
            } else {
                Some(assembler.build_with_original())
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AikiTaskClosedPayload, TaskEventPayload};
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    // Use the process-wide mutex from global.rs to avoid races with other modules
    fn aiki_home_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::global::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    struct EnvGuard {
        original: Option<String>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => env::set_var(crate::global::AIKI_HOME_ENV, value),
                None => env::remove_var(crate::global::AIKI_HOME_ENV),
            }
        }
    }

    fn setup_global_aiki_home() -> (TempDir, EnvGuard) {
        let aiki_home = TempDir::new().unwrap();
        fs::create_dir_all(aiki_home.path().join("sessions")).unwrap();

        let original = env::var(crate::global::AIKI_HOME_ENV).ok();
        env::set_var(crate::global::AIKI_HOME_ENV, aiki_home.path());

        (aiki_home, EnvGuard { original })
    }

    fn create_task_closed_event(task_id: &str) -> crate::events::AikiEvent {
        AikiTaskClosedPayload {
            task: TaskEventPayload {
                id: task_id.to_string(),
                name: "Execution Test".to_string(),
                task_type: "review".to_string(),
                status: "closed".to_string(),
                assignee: None,
                outcome: Some("done".to_string()),
                source: None,
                files: Some(vec!["src/test.rs".to_string()]),
                changes: Some(vec!["abc123".to_string()]),
            },
            cwd: PathBuf::from("/tmp/test"),
            timestamp: chrono::Utc::now(),
        }
        .into()
    }

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
            crate::provenance::DetectionMethod::Hook,
            SessionMode::Interactive,
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
                if let ChangeOperation::Write(ref w) = e.operation {
                    assert_eq!(w.file_paths, vec!["/test/file.rs".to_string()]);
                } else {
                    panic!("Expected Write operation");
                }
            }
            _ => panic!("Expected ChangeCompleted event"),
        }
        assert_eq!(ctx.cwd(), std::path::Path::new("/test"));
    }

    #[test]
    fn test_task_closed_thread_session_lookup_is_cached() {
        let _lock = aiki_home_lock();
        let (_aiki_home, _guard) = setup_global_aiki_home();

        let task_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let thread_head = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let session_path = crate::global::global_sessions_dir().join("sessioncachetest");
        fs::write(
            &session_path,
            format!(
                "thread={}:{}\nparent_pid=4242\nsession_id=sessioncachetest\nmode=background\n",
                thread_head, task_id
            ),
        )
        .unwrap();

        let state = AikiState::new(create_task_closed_event(task_id));

        let first = state
            .resolve_task_closed_thread_session()
            .expect("expected initial session lookup to succeed");
        assert_eq!(first.pid, 4242);
        assert_eq!(first.thread.head, thread_head);
        assert_eq!(first.thread.tail, task_id);
        assert_eq!(first.mode, crate::session::SessionMode::Background);

        fs::remove_file(&session_path).unwrap();

        let second = state
            .resolve_task_closed_thread_session()
            .expect("expected cached session lookup to survive file removal");
        assert_eq!(second.pid, first.pid);
        assert_eq!(second.thread.serialize(), first.thread.serialize());
        assert_eq!(second.mode, first.mode);
    }
}
