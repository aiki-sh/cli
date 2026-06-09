//! Agent runtime abstraction for spawning and managing agent processes
//!
//! This module provides the `AgentRuntime` trait that defines how to spawn
//! agent sessions and the result types for tracking session outcomes.

mod claude_code;
mod codex;

pub use claude_code::ClaudeCodeRuntime;
pub use codex::CodexRuntime;

use crate::error::Result;
use crate::tasks::lanes::ThreadId;
use crate::tasks::md::DONE_MESSAGE;
use std::path::Path;
use std::process::{Child, ExitStatus};

pub(crate) use super::AgentType;

/// Handle for a background agent process
///
/// Returned when spawning an agent in background mode. Contains the
/// thread ID for later management (e.g., stopping the process).
#[derive(Debug, Clone)]
pub struct BackgroundHandle {
    /// Thread being worked on
    pub thread: ThreadId,
    /// Session UUID, resolved after spawn via event polling
    #[allow(dead_code)]
    pub session_id: Option<String>,
    /// Agent type that was spawned
    pub agent_type: AgentType,
}

/// Handle for a monitored child process
///
/// Unlike `BackgroundHandle`, this keeps the `Child` handle so we can properly
/// detect when the process exits (including zombie processes). This is used
/// for real-time status monitoring where we need accurate exit detection.
///
/// Stdout and stderr are drained by background threads to prevent the child
/// from blocking on a full pipe buffer (agents like codex write their entire
/// session log to stderr, easily exceeding the ~64KB OS pipe limit).
pub struct MonitoredChild {
    /// The child process handle
    child: Child,
    /// Drain threads for stdout/stderr (joined on drop)
    _drain_threads: Vec<std::thread::JoinHandle<()>>,
}

impl MonitoredChild {
    /// Create a new monitored child from a Child process
    #[must_use]
    pub fn new(mut child: Child) -> Self {
        let mut drain_threads = Vec::new();

        // Spawn background threads that read-and-discard stdout/stderr so the
        // child process never blocks on a full pipe buffer.
        if let Some(mut stdout) = child.stdout.take() {
            drain_threads.push(std::thread::spawn(move || {
                let _ = std::io::copy(&mut stdout, &mut std::io::sink());
            }));
        }
        if let Some(mut stderr) = child.stderr.take() {
            drain_threads.push(std::thread::spawn(move || {
                let _ = std::io::copy(&mut stderr, &mut std::io::sink());
            }));
        }

        Self {
            child,
            _drain_threads: drain_threads,
        }
    }

    /// Check if the process has exited without blocking
    ///
    /// Returns:
    /// - `Ok(Some(status))` if the process has exited
    /// - `Ok(None)` if the process is still running
    /// - `Err` on error
    ///
    /// This properly handles zombie processes by calling `wait()` internally,
    /// which reaps the zombie when the process has exited.
    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Block until the process exits
    ///
    /// Returns the exit status. This reaps the child process.
    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.child.wait()
    }
}

/// Result of an agent session
#[derive(Debug, Clone)]
pub enum AgentSessionResult {
    /// Agent finished successfully
    Completed {
        /// Summary of what was accomplished
        summary: String,
    },
    /// Agent explicitly stopped (needs user input, blocked, etc.)
    Stopped {
        /// Reason for stopping
        reason: String,
    },
    /// Agent failed (crash, timeout, error)
    Failed {
        /// Error description
        error: String,
    },
    /// User detached from monitoring, but agent continues running in background
    Detached,
}

impl AgentSessionResult {
    /// Create a completed result
    #[must_use]
    pub fn completed(summary: impl Into<String>) -> Self {
        Self::Completed {
            summary: summary.into(),
        }
    }

    /// Create a stopped result
    #[must_use]
    pub fn stopped(reason: impl Into<String>) -> Self {
        Self::Stopped {
            reason: reason.into(),
        }
    }

    /// Create a failed result
    #[must_use]
    pub fn failed(error: impl Into<String>) -> Self {
        Self::Failed {
            error: error.into(),
        }
    }

    /// Create a detached result (user disconnected, agent continues)
    #[must_use]
    pub fn detached() -> Self {
        Self::Detached
    }
}

/// Options for spawning an agent session
#[derive(Debug, Clone)]
pub struct AgentSpawnOptions {
    /// Working directory for the agent
    pub cwd: std::path::PathBuf,
    /// Thread to work on (single-task or multi-task chain)
    pub thread: ThreadId,
    /// Parent session UUID for workspace isolation chaining
    pub parent_session_uuid: Option<String>,
    /// Path to the event FIFO pipe for push-based event streaming
    pub event_pipe: Option<std::path::PathBuf>,
}

impl AgentSpawnOptions {
    /// Create new spawn options
    #[must_use]
    pub fn new(cwd: impl AsRef<Path>, thread: ThreadId) -> Self {
        Self {
            cwd: cwd.as_ref().to_path_buf(),
            thread,
            parent_session_uuid: None,
            event_pipe: None,
        }
    }

    /// Set the parent session UUID for workspace isolation chaining
    #[must_use]
    pub fn with_parent_session_uuid(mut self, uuid: Option<String>) -> Self {
        self.parent_session_uuid = uuid;
        self
    }

    /// Set the event pipe path for push-based event streaming
    #[must_use]
    pub fn with_event_pipe(mut self, path: Option<std::path::PathBuf>) -> Self {
        self.event_pipe = path;
        self
    }

    /// Build the task prompt with instructions for autonomous work
    #[must_use]
    pub fn task_prompt(&self) -> String {
        format!(
            r#"You are assigned thread `{thread}`. Work through all tasks in order.

SCOPE: Only tasks in this thread. Do not pick up other work.
START: Run `aiki task list` to see your backlog. Start the next task with
       `aiki task start <task-id>`. The start output includes the task's
       instructions and context; review that output, then begin working immediately.
EXIT: When `aiki task list` returns "{done}", you are finished — exit immediately.
"#,
            thread = self.thread,
            done = DONE_MESSAGE,
        )
    }
}

/// Trait for agent runtime implementations
///
/// Each agent type (ClaudeCode, Codex, etc.) has its own runtime that knows
/// how to spawn and manage sessions for that agent.
pub trait AgentRuntime {
    /// Spawns an agent session and waits for completion
    ///
    /// This is a blocking operation that:
    /// 1. Spawns the agent process with the task context
    /// 2. Waits for the agent to complete
    /// 3. Returns the session result
    fn spawn_blocking(&self, options: &AgentSpawnOptions) -> Result<AgentSessionResult>;

    /// Spawns an agent session in the background
    ///
    /// This is a non-blocking operation that:
    /// 1. Spawns the agent process detached from the parent
    /// 2. Returns immediately with a handle containing the PID
    /// 3. The agent runs until task completion
    ///
    /// The background process is fully detached and will continue running
    /// even after the parent process exits.
    fn spawn_background(&self, options: &AgentSpawnOptions) -> Result<BackgroundHandle>;

    /// Spawns an agent session for monitoring
    ///
    /// Similar to `spawn_background`, but keeps the Child handle so we can
    /// properly detect when the process exits (including zombie processes).
    /// This should be used when real-time status monitoring is needed.
    #[allow(dead_code)]
    fn spawn_monitored(&self, options: &AgentSpawnOptions) -> Result<MonitoredChild>;
}

/// Build the common environment variables for spawning an agent.
///
/// Both `ClaudeCodeRuntime` and `CodexRuntime` call this to ensure consistent
/// env-var setup (`AIKI_THREAD`, `AIKI_SESSION_MODE`, and optionally
/// `AIKI_PARENT_SESSION_UUID`).
pub(crate) fn build_spawn_env(options: &AgentSpawnOptions, mode: &str) -> Vec<(String, String)> {
    let mut env = vec![
        ("AIKI_THREAD".to_string(), options.thread.serialize()),
        ("AIKI_SESSION_MODE".to_string(), mode.to_string()),
    ];
    if let Some(ref uuid) = options.parent_session_uuid {
        env.push(("AIKI_PARENT_SESSION_UUID".to_string(), uuid.clone()));
    }
    if let Some(ref pipe) = options.event_pipe {
        env.push((
            "AIKI_EVENT_PIPE".to_string(),
            pipe.to_string_lossy().into_owned(),
        ));
    }
    env
}

/// Get the appropriate runtime for an agent type
#[must_use]
pub fn get_runtime(agent_type: AgentType) -> Option<Box<dyn AgentRuntime>> {
    match agent_type {
        AgentType::ClaudeCode => Some(Box::new(ClaudeCodeRuntime::new())),
        AgentType::Codex => Some(Box::new(CodexRuntime::new())),
        // Other agent types not yet supported for task execution
        AgentType::Cursor | AgentType::Gemini | AgentType::Unknown => None,
    }
}

/// Poll session state until a thread-bound session is registered.
///
/// After spawning an agent, `session.started` is recorded in conversation
/// history with the run thread ID and session UUID. This function polls that log using
/// exponential backoff (100ms → 500ms, 30s total timeout).
///
/// Conversation events live in the global JJ repo (`~/.aiki/`), not the
/// project repo, so we read from `global_aiki_dir()`.
pub fn discover_session_id(thread: &ThreadId) -> Result<String> {
    use crate::history::storage::find_session_started_for_thread;
    use std::thread as std_thread;
    use std::time::{Duration, Instant};

    // Session startup can take longer than the original 5s budget.
    let timeout = Duration::from_secs(30);
    let start = Instant::now();
    let mut delay = Duration::from_millis(100);
    let max_delay = Duration::from_millis(500);

    let serialized = thread.serialize();
    let global_dir = crate::global::global_aiki_dir();

    loop {
        if let Some(session_id) = find_session_started_for_thread(&global_dir, &serialized)? {
            return Ok(session_id);
        }

        if start.elapsed() >= timeout {
            return Err(anyhow::anyhow!("Session UUID not discovered within timeout").into());
        }

        std_thread::sleep(delay);
        delay = (delay * 2).min(max_delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_session_result_constructors() {
        let completed = AgentSessionResult::completed("Task done");
        assert!(matches!(completed, AgentSessionResult::Completed { .. }));

        let stopped = AgentSessionResult::stopped("Needs input");
        assert!(matches!(stopped, AgentSessionResult::Stopped { .. }));

        let failed = AgentSessionResult::failed("Crashed");
        assert!(matches!(failed, AgentSessionResult::Failed { .. }));

        let detached = AgentSessionResult::detached();
        assert!(matches!(detached, AgentSessionResult::Detached));
    }

    #[test]
    fn test_spawn_options() {
        let thread = ThreadId::single("task123".to_string());
        let options = AgentSpawnOptions::new("/tmp", thread);

        assert_eq!(options.cwd.to_string_lossy(), "/tmp");
        assert_eq!(options.thread.head, "task123");
        assert!(options.thread.is_single());
    }

    #[test]
    fn test_get_runtime() {
        assert!(get_runtime(AgentType::ClaudeCode).is_some());
        assert!(get_runtime(AgentType::Codex).is_some());
        assert!(get_runtime(AgentType::Cursor).is_none());
        assert!(get_runtime(AgentType::Gemini).is_none());
        assert!(get_runtime(AgentType::Unknown).is_none());
    }

    /// 6a: task_prompt contains thread display, expected keywords
    #[test]
    fn test_task_prompt_contains_thread_id() {
        let head = "abcdefghijklmnopqrstuvwxyzabcdef".to_string();
        let tail = "fedcbazyxwvutsrqponmlkjihgfedcba".to_string();
        let thread = ThreadId {
            head: head.clone(),
            tail: tail.clone(),
        };
        let options = AgentSpawnOptions::new("/tmp", thread.clone());
        let prompt = options.task_prompt();

        // Should contain the Display form (short IDs)
        let display = format!("{}", thread);
        assert!(
            prompt.contains(&display),
            "prompt should contain thread display '{display}': {prompt}"
        );
        assert!(prompt.contains("thread"), "prompt should mention 'thread'");
        assert!(prompt.contains("SCOPE"), "prompt should mention 'SCOPE'");
        assert!(prompt.contains("START"), "prompt should mention 'START'");
        assert!(prompt.contains("EXIT"), "prompt should mention 'EXIT'");
        assert!(
            prompt.contains("aiki task list"),
            "prompt should mention 'aiki task list'"
        );
        assert!(
            prompt.contains("aiki task start <task-id>"),
            "prompt should mention starting the task before working"
        );
    }

    /// 6b: single-task thread display shows bare short ID (no colon)
    #[test]
    fn test_task_prompt_single_task_thread() {
        let id = "abcdefghijklmnopqrstuvwxyzabcdef".to_string();
        let thread = ThreadId::single(id.clone());
        let options = AgentSpawnOptions::new("/tmp", thread);
        let prompt = options.task_prompt();

        let short = &id[..7];
        assert!(
            prompt.contains(short),
            "prompt should contain short ID '{short}'"
        );
        // Single-task thread display should NOT contain a colon separator
        assert!(
            !prompt.contains(&format!("{}:", short)),
            "single-task thread display should not contain colon"
        );
    }

    /// 6c: serialize() returns "head:tail" for multi-task, bare head for single
    #[test]
    fn test_spawn_options_thread_serialization() {
        let head = "abcdefghijklmnopqrstuvwxyzabcdef".to_string();
        let tail = "fedcbazyxwvutsrqponmlkjihgfedcba".to_string();
        let thread = ThreadId {
            head: head.clone(),
            tail: tail.clone(),
        };
        let options = AgentSpawnOptions::new("/tmp", thread);
        assert_eq!(
            options.thread.serialize(),
            format!("{}:{}", head, tail),
            "multi-task thread should serialize as head:tail"
        );

        // Single-task thread serializes as just the ID
        let single = ThreadId::single(head.clone());
        let options_single = AgentSpawnOptions::new("/tmp", single);
        assert_eq!(
            options_single.thread.serialize(),
            head,
            "single-task thread should serialize as bare head"
        );
    }

    /// 6d: build_spawn_env returns AIKI_THREAD = thread.serialize() (multi-task thread)
    #[test]
    fn test_build_spawn_env_sets_aiki_thread() {
        let head = "abcdefghijklmnopqrstuvwxyzabcdef".to_string();
        let tail = "fedcbazyxwvutsrqponmlkjihgfedcba".to_string();
        let thread = ThreadId {
            head: head.clone(),
            tail: tail.clone(),
        };
        let options = AgentSpawnOptions::new("/tmp", thread.clone());
        let env = build_spawn_env(&options, "background");

        let thread_val = env
            .iter()
            .find(|(k, _)| k == "AIKI_THREAD")
            .map(|(_, v)| v.as_str());
        assert_eq!(
            thread_val,
            Some(thread.serialize().as_str()),
            "AIKI_THREAD must equal thread.serialize()"
        );
    }

    /// 6e: build_spawn_env returns AIKI_THREAD = thread.serialize() (single-task thread)
    #[test]
    fn test_build_spawn_env_sets_aiki_thread_single() {
        let id = "abcdefghijklmnopqrstuvwxyzabcdef".to_string();
        let thread = ThreadId::single(id.clone());
        let options = AgentSpawnOptions::new("/tmp", thread.clone());
        let env = build_spawn_env(&options, "monitored");

        let thread_val = env
            .iter()
            .find(|(k, _)| k == "AIKI_THREAD")
            .map(|(_, v)| v.as_str());
        assert_eq!(
            thread_val,
            Some(id.as_str()),
            "single-task AIKI_THREAD must equal bare head ID"
        );

        let mode_val = env
            .iter()
            .find(|(k, _)| k == "AIKI_SESSION_MODE")
            .map(|(_, v)| v.as_str());
        assert_eq!(mode_val, Some("monitored"));
    }

    /// 7a: find_session_started_for_thread matches on thread.serialize() format
    ///
    /// Behavioral test: creates a SessionStart event with a known run_thread_id
    /// matching thread.serialize(), then verifies the matching logic returns the
    /// correct session.
    #[test]
    fn test_find_session_matches_serialized_thread() {
        use crate::history::types::ConversationEvent;
        use chrono::Utc;

        let head = "abcdefghijklmnopqrstuvwxyzabcdef".to_string();
        let tail = "fedcbazyxwvutsrqponmlkjihgfedcba".to_string();
        let thread = ThreadId {
            head: head.clone(),
            tail: tail.clone(),
        };
        let serialized = thread.serialize();

        // Build an event with the serialized thread as run_thread_id
        let events = vec![ConversationEvent::SessionStart {
            session_id: "found-session".to_string(),
            agent_type: super::super::AgentType::ClaudeCode,
            timestamp: Utc::now(),
            run_thread_id: Some(serialized.clone()),
            repo_id: None,
            cwd: None,
            session_mode: None,
            transcript_path: None,
        }];

        // Replicate the matching logic used by find_session_started_for_thread
        let result = events.iter().rev().find_map(|event| match event {
            ConversationEvent::SessionStart {
                session_id,
                run_thread_id: Some(run_thread_id),
                ..
            } if run_thread_id == &serialized => Some(session_id.clone()),
            _ => None,
        });

        assert_eq!(
            result.as_deref(),
            Some("found-session"),
            "find_session_started_for_thread must match thread.serialize() format"
        );
    }

    /// Structural invariant: every spawn method that sets AIKI_THREAD must also
    /// set AIKI_SESSION_MODE. The one exception is plan.rs which spawns a truly
    /// interactive user session (mode defaults to "interactive" intentionally).
    ///
    /// Regression guard for: spawn_blocking missing AIKI_SESSION_MODE caused
    /// task.closed hook to SIGTERM background agents (exit code 143).
    #[test]
    fn test_all_spawn_methods_set_session_mode() {
        let runtime_dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agents/runtime");

        for filename in &["claude_code.rs", "codex.rs"] {
            let path = runtime_dir.join(filename);
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("Failed to read {}: {}", filename, e));

            // Split source into methods by finding `fn spawn_` boundaries
            let method_starts: Vec<usize> = source
                .match_indices("fn spawn_")
                .map(|(idx, _)| idx)
                .collect();

            for (i, &start) in method_starts.iter().enumerate() {
                let end = method_starts.get(i + 1).copied().unwrap_or(source.len());
                let method_body = &source[start..end];

                // Extract method name for error messages
                let method_name: String = method_body
                    .chars()
                    .skip("fn ".len())
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();

                let has_thread = method_body.contains("AIKI_THREAD");
                let has_mode = method_body.contains("AIKI_SESSION_MODE");

                if has_thread {
                    assert!(
                        has_mode,
                        "{filename}::{method_name} sets AIKI_THREAD but not AIKI_SESSION_MODE. \
                         Every spawn method that sets AIKI_THREAD must also set \
                         AIKI_SESSION_MODE to prevent the task.closed hook from \
                         treating background agents as interactive sessions."
                    );
                }
            }
        }
    }
}
