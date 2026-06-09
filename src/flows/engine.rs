use crate::jj::jj_cmd;
use anyhow::Context;
use std::process::Command;
use std::time::Duration;

use crate::cache::debug_log;
use crate::events::AikiEvent;
use crate::validation::is_valid_flow_identifier;

use super::state::{ActionResult, AikiState};
use super::types::{
    Action, AutoreplyAction, AutoreplyContent, CallAction, CommitMessageAction, CommitMessageOp,
    ContextAction, HookStatement, IfStatement, JjAction, LetAction, LogAction, OnFailure,
    OnFailureShortcut, ReviewAction, ShellAction, SwitchStatement, TaskRunAction,
};
use super::variables::VariableResolver;
use crate::error::{AikiError, Result};
use crate::flows::context::ContextChunk;

/// Result of hook execution
#[derive(Debug, Clone)]
pub enum HookOutcome {
    /// All actions succeeded
    Success,
    /// Action failed with on_failure: continue (logged, hook continued)
    FailedContinue,
    /// Action failed with on_failure: stop (silent failure, hook stopped)
    FailedStop,
    /// Action failed with on_failure: block (block editor operation)
    FailedBlock,
}

/// Executes hook actions
pub struct HookEngine;

impl HookEngine {
    /// Create a variable resolver with consistent variable availability
    ///
    /// Makes variables available both with and without `event.` prefix:
    /// - {{event.file_paths}} (for event variables)
    /// - {{file_path}} (for event variables, let-bound variables)
    /// - {{description}} (for let-bound variables)
    /// Create a variable resolver with proper variable scoping
    ///
    /// Variable scopes:
    /// - Event variables (from actual events): {{event.file_paths}}, {{event.agent_type}}
    /// - Let variables (user-defined): {{description}}, {{my_var}} (no event. prefix)
    /// - System variables: {{cwd}}
    /// - Environment variables: {{HOME}}, {{PATH}}
    fn create_resolver(state: &AikiState) -> VariableResolver {
        let mut resolver = VariableResolver::new();

        // Add event-specific variables based on event type
        match &state.event {
            crate::events::AikiEvent::TurnStarted(e) => {
                resolver.add_var("event.prompt".to_string(), e.prompt.clone());
                resolver.add_var("event.source".to_string(), e.turn.source.clone());
                resolver.add_var("event.turn".to_string(), e.turn.number.to_string());
                resolver.add_var("event.turn_id".to_string(), e.turn.id.clone());
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
            }

            // Read operations
            crate::events::AikiEvent::ReadPermissionAsked(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
                resolver.add_var("event.tool_name".to_string(), e.tool_name.clone());
                resolver.add_var("event.file_paths".to_string(), e.file_paths.join(" "));
                if let Some(ref pattern) = e.pattern {
                    resolver.add_var("event.pattern".to_string(), pattern.clone());
                }
            }
            crate::events::AikiEvent::ReadCompleted(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
                resolver.add_var("event.tool_name".to_string(), e.tool_name.clone());
                resolver.add_var("event.file_paths".to_string(), e.file_paths.join(" "));
                resolver.add_var(
                    "event.file_count".to_string(),
                    e.file_paths.len().to_string(),
                );
                resolver.add_var("event.success".to_string(), e.success.to_string());
            }
            crate::events::AikiEvent::SessionStarted(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
            }
            crate::events::AikiEvent::TurnCompleted(e) => {
                resolver.add_var("event.response".to_string(), e.response.clone());
                resolver.add_var("event.source".to_string(), e.turn.source.clone());
                resolver.add_var("event.turn".to_string(), e.turn.number.to_string());
                resolver.add_var("event.turn_id".to_string(), e.turn.id.clone());
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
                resolver.add_var(
                    "event.modified_files".to_string(),
                    e.modified_files
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
            crate::events::AikiEvent::SessionEnded(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
            }
            crate::events::AikiEvent::SessionResumed(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
            }
            crate::events::AikiEvent::SessionWillCompact(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
            }
            crate::events::AikiEvent::SessionCompacted(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
            }
            crate::events::AikiEvent::SessionCleared(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
            }
            crate::events::AikiEvent::ShellPermissionAsked(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
                resolver.add_var("event.command".to_string(), e.command.clone());
            }
            crate::events::AikiEvent::ShellCompleted(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
                resolver.add_var("event.command".to_string(), e.command.clone());
                resolver.add_var("event.success".to_string(), e.success.to_string());
                // Optional fields - only add if agent provides them
                if let Some(exit_code) = e.exit_code {
                    resolver.add_var("event.exit_code".to_string(), exit_code.to_string());
                }
                if let Some(ref stdout) = e.stdout {
                    resolver.add_var("event.stdout".to_string(), stdout.clone());
                }
                if let Some(ref stderr) = e.stderr {
                    resolver.add_var("event.stderr".to_string(), stderr.clone());
                }
            }
            crate::events::AikiEvent::McpPermissionAsked(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
                resolver.add_var("event.tool_name".to_string(), e.tool_name.clone());
                resolver.add_var("event.parameters".to_string(), e.parameters.to_string());
            }
            crate::events::AikiEvent::McpCompleted(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
                resolver.add_var("event.tool_name".to_string(), e.tool_name.clone());
                resolver.add_var("event.success".to_string(), e.success.to_string());
                if let Some(ref result) = e.result {
                    resolver.add_var("event.result".to_string(), result.clone());
                }
            }
            crate::events::AikiEvent::WebPermissionAsked(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
                resolver.add_var("event.operation".to_string(), e.operation.to_string());
                if let Some(ref url) = e.url {
                    resolver.add_var("event.url".to_string(), url.clone());
                }
                if let Some(ref query) = e.query {
                    resolver.add_var("event.query".to_string(), query.clone());
                }
            }
            crate::events::AikiEvent::WebCompleted(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
                resolver.add_var("event.operation".to_string(), e.operation.to_string());
                if let Some(ref url) = e.url {
                    resolver.add_var("event.url".to_string(), url.clone());
                }
                if let Some(ref query) = e.query {
                    resolver.add_var("event.query".to_string(), query.clone());
                }
                resolver.add_var("event.success".to_string(), e.success.to_string());
            }
            crate::events::AikiEvent::CommitMessageStarted(e) => {
                // Add commit message file path if available
                if let Some(ref path) = e.commit_msg_file {
                    resolver.add_var(
                        "event.commit_msg_file".to_string(),
                        path.display().to_string(),
                    );
                }
            }

            // Change operations (unified mutations: write, delete, move)
            crate::events::AikiEvent::ChangePermissionAsked(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
                resolver.add_var(
                    "event.operation".to_string(),
                    e.operation.operation_name().to_string(),
                );
                resolver.add_var("event.tool_name".to_string(), e.tool_name.clone());

                // Computed properties for operation type checks (return "true" or "")
                resolver.add_var(
                    "event.write".to_string(),
                    e.operation.is_write().to_string(),
                );
                resolver.add_var(
                    "event.delete".to_string(),
                    e.operation.is_delete().to_string(),
                );
                resolver.add_var("event.move".to_string(), e.operation.is_move().to_string());

                // Operation-specific variables
                match &e.operation {
                    crate::events::ChangeOperation::Write(op) => {
                        resolver.add_var("event.file_paths".to_string(), op.file_paths.join(" "));
                        resolver.add_var(
                            "event.file_count".to_string(),
                            op.file_paths.len().to_string(),
                        );
                        if !op.edit_details.is_empty() {
                            resolver.add_var(
                                "event.edit_details".to_string(),
                                serde_json::to_string(&op.edit_details).unwrap_or_default(),
                            );
                        }
                    }
                    crate::events::ChangeOperation::Delete(op) => {
                        resolver.add_var("event.file_paths".to_string(), op.file_paths.join(" "));
                        resolver.add_var(
                            "event.file_count".to_string(),
                            op.file_paths.len().to_string(),
                        );
                    }
                    crate::events::ChangeOperation::Move(op) => {
                        resolver.add_var(
                            "event.file_paths".to_string(),
                            op.destination_paths.join(" "),
                        );
                        resolver
                            .add_var("event.source_paths".to_string(), op.source_paths.join(" "));
                        resolver.add_var(
                            "event.destination_paths".to_string(),
                            op.destination_paths.join(" "),
                        );
                        resolver.add_var(
                            "event.file_count".to_string(),
                            op.destination_paths.len().to_string(),
                        );
                    }
                }
            }
            crate::events::AikiEvent::ChangeCompleted(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
                resolver.add_var(
                    "event.operation".to_string(),
                    e.operation.operation_name().to_string(),
                );
                resolver.add_var("event.tool_name".to_string(), e.tool_name.clone());
                resolver.add_var("event.success".to_string(), e.success.to_string());

                // Computed properties for operation type checks (return "true" or "")
                resolver.add_var(
                    "event.write".to_string(),
                    e.operation.is_write().to_string(),
                );
                resolver.add_var(
                    "event.delete".to_string(),
                    e.operation.is_delete().to_string(),
                );
                resolver.add_var("event.move".to_string(), e.operation.is_move().to_string());

                // Operation-specific variables
                match &e.operation {
                    crate::events::ChangeOperation::Write(op) => {
                        resolver.add_var("event.file_paths".to_string(), op.file_paths.join(" "));
                        resolver.add_var(
                            "event.file_count".to_string(),
                            op.file_paths.len().to_string(),
                        );
                        if !op.edit_details.is_empty() {
                            resolver.add_var(
                                "event.edit_details".to_string(),
                                serde_json::to_string(&op.edit_details).unwrap_or_default(),
                            );
                        }
                    }
                    crate::events::ChangeOperation::Delete(op) => {
                        resolver.add_var("event.file_paths".to_string(), op.file_paths.join(" "));
                        resolver.add_var(
                            "event.file_count".to_string(),
                            op.file_paths.len().to_string(),
                        );
                    }
                    crate::events::ChangeOperation::Move(op) => {
                        resolver.add_var(
                            "event.file_paths".to_string(),
                            op.destination_paths.join(" "),
                        );
                        resolver
                            .add_var("event.source_paths".to_string(), op.source_paths.join(" "));
                        resolver.add_var(
                            "event.destination_paths".to_string(),
                            op.destination_paths.join(" "),
                        );
                        resolver.add_var(
                            "event.file_count".to_string(),
                            op.destination_paths.len().to_string(),
                        );
                    }
                }
            }

            crate::events::AikiEvent::Unsupported => {
                // No event-specific variables for unsupported events
            }

            // Model transition events
            crate::events::AikiEvent::ModelChanged(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
                resolver.add_var("event.new_model".to_string(), e.new_model.clone());
                if let Some(ref prev) = e.previous_model {
                    resolver.add_var("event.previous_model".to_string(), prev.clone());
                }
            }

            // Task lifecycle events
            crate::events::AikiEvent::RepoChanged(e) => {
                resolver.add_var(
                    "event.session.uuid".to_string(),
                    e.session.uuid().to_string(),
                );
                resolver.add_var(
                    "event.session.external_id".to_string(),
                    e.session.external_id().to_string(),
                );
                resolver.add_var("event.repo.root".to_string(), e.repo.root.clone());
                resolver.add_var(
                    "event.repo.path".to_string(),
                    e.repo.path.display().to_string(),
                );
                resolver.add_var("event.repo.id".to_string(), e.repo.id.clone());
                if let Some(ref prev) = e.previous_repo {
                    resolver.add_var("event.previous_repo.root".to_string(), prev.root.clone());
                    resolver.add_var(
                        "event.previous_repo.path".to_string(),
                        prev.path.display().to_string(),
                    );
                    resolver.add_var("event.previous_repo.id".to_string(), prev.id.clone());
                }
            }
            crate::events::AikiEvent::TaskStarted(e) => {
                // Task info is nested under event.task.*
                resolver.add_var("event.task.id".to_string(), e.task.id.clone());
                resolver.add_var("event.task.name".to_string(), e.task.name.clone());
                resolver.add_var("event.task.type".to_string(), e.task.task_type.clone());
                resolver.add_var("event.task.status".to_string(), e.task.status.clone());
                if let Some(ref assignee) = e.task.assignee {
                    resolver.add_var("event.task.assignee".to_string(), assignee.clone());
                }
            }
            crate::events::AikiEvent::TaskClosed(e) => {
                // Task info is nested under event.task.*
                resolver.add_var("event.task.id".to_string(), e.task.id.clone());
                resolver.add_var("event.task.name".to_string(), e.task.name.clone());
                resolver.add_var("event.task.type".to_string(), e.task.task_type.clone());
                resolver.add_var("event.task.status".to_string(), e.task.status.clone());
                if let Some(ref assignee) = e.task.assignee {
                    resolver.add_var("event.task.assignee".to_string(), assignee.clone());
                }
                if let Some(ref outcome) = e.task.outcome {
                    resolver.add_var("event.task.outcome".to_string(), outcome.clone());
                }
                if let Some(ref source) = e.task.source {
                    resolver.add_var("event.task.source".to_string(), source.clone());
                }

                // Register lazy provenance variables - only query JJ when accessed
                let cwd = e.cwd.clone();
                let task_id = e.task.id.clone();
                resolver.add_lazy_var("event.task.changes", move || {
                    let changes = crate::jj::get_changes_for_task(&cwd, &task_id);
                    changes.join(" ")
                });

                let cwd2 = e.cwd.clone();
                let task_id2 = e.task.id.clone();
                resolver.add_lazy_var("event.task.files", move || {
                    let files = crate::jj::get_files_for_task(&cwd2, &task_id2);
                    files.join(" ")
                });
            }
        }

        // Add agent type as event.agent_type
        let agent_str = match state.event.agent_type() {
            crate::provenance::AgentType::ClaudeCode => "claude",
            crate::provenance::AgentType::Codex => "codex",
            crate::provenance::AgentType::Cursor => "cursor",
            crate::provenance::AgentType::Gemini => "gemini",
            crate::provenance::AgentType::Unknown => "unknown",
        };
        resolver.add_var("event.agent_type".to_string(), agent_str.to_string());

        // Add let variables (accessible via $key without event. prefix)
        for (key, value) in state.iter_variables() {
            resolver.add_var(key.clone(), value.clone());
        }

        // Add cwd using helper method
        resolver.add_var("cwd", state.cwd().to_string_lossy().to_string());

        // Expose whether an autoreply has been queued earlier in this hook run.
        // Used to guard session.end from firing when a follow-up turn is pending.
        resolver.add_var(
            "has_pending_autoreply".to_string(),
            state.has_pending_autoreply().to_string(),
        );

        // Set up lazy per-key env var lookup instead of collecting all env vars
        // This ensures runtime set_var/remove_var mutations are immediately visible
        resolver.set_env_lookup(|name| std::env::var(name).ok());

        // Add session variables for task.closed events
        // These look up the session driven by the closed task's thread
        if let crate::events::AikiEvent::TaskClosed(e) = &state.event {
            let task_id = e.task.id.clone();
            let session_cache = state.task_closed_thread_session_cache();
            let resolve_thread_session: std::rc::Rc<
                dyn Fn() -> Option<crate::session::ThreadSessionInfo>,
            > = {
                let task_id = task_id.clone();
                let session_cache = session_cache.clone();
                std::rc::Rc::new(move || {
                    session_cache
                        .get_or_init(|| crate::session::find_thread_session(&task_id))
                        .clone()
                })
            };

            debug_log(|| format!("task.closed: event.task.id={}", task_id));

            // Lazy lookup of thread session info — matches on thread tail
            let resolve_thread_session_for_tail = resolve_thread_session.clone();
            resolver.add_lazy_var("session.thread.tail", move || {
                let result = if let Some(session_info) = resolve_thread_session_for_tail() {
                    session_info.thread.tail.clone()
                } else {
                    String::new()
                };
                debug_log(|| format!("task.closed: session.thread.tail resolved to '{}'", result));
                result
            });

            let resolve_thread_session_for_head = resolve_thread_session.clone();
            resolver.add_lazy_var("session.thread.head", move || {
                let result = if let Some(session_info) = resolve_thread_session_for_head() {
                    session_info.thread.head.clone()
                } else {
                    String::new()
                };
                debug_log(|| format!("task.closed: session.thread.head resolved to '{}'", result));
                result
            });

            let resolve_thread_session_for_thread = resolve_thread_session.clone();
            resolver.add_lazy_var("session.thread", move || {
                let result = if let Some(session_info) = resolve_thread_session_for_thread() {
                    session_info.thread.serialize()
                } else {
                    String::new()
                };
                debug_log(|| format!("task.closed: session.thread resolved to '{}'", result));
                result
            });

            let resolve_thread_session_for_mode = resolve_thread_session.clone();
            resolver.add_lazy_var("session.mode", move || {
                // Read the session file to get the actual mode
                let result = if let Some(session_info) = resolve_thread_session_for_mode() {
                    session_info.mode.to_string().to_string()
                } else {
                    String::new()
                };
                debug_log(|| format!("task.closed: session.mode resolved to '{}'", result));
                result
            });

            let resolve_thread_session_for_agent = resolve_thread_session;
            resolver.add_lazy_var("session.agent", move || {
                let result = if let Some(session_info) = resolve_thread_session_for_agent() {
                    session_info.agent_type.clone()
                } else {
                    String::new()
                };
                debug_log(|| format!("task.closed: session.agent resolved to '{}'", result));
                result
            });
        }

        // Add session variables for session-bearing events (turn.completed, etc.)
        // These read directly from the session on the event payload.
        if let Ok(session) = extract_session(&state.event) {
            // Only add these if not already set (task.closed sets them via reverse lookup above)
            if !matches!(&state.event, crate::events::AikiEvent::TaskClosed(_)) {
                let mode = session.mode();
                resolver.add_var("session.mode".to_string(), mode.to_string());

                let thread = session.thread();

                if let Some(thread) = thread {
                    let tail = thread.tail.clone();
                    let head = thread.head.clone();
                    let serialized = thread.serialize();
                    resolver.add_var("session.thread.tail".to_string(), tail.clone());
                    resolver.add_var("session.thread.head".to_string(), head);
                    resolver.add_var("session.thread".to_string(), serialized);

                    // session.thread.tail.status — lazy lookup from task graph
                    let tail_for_status = tail;
                    let cwd_for_status = state.cwd().to_path_buf();
                    resolver.add_lazy_var("session.thread.tail.status", move || {
                        crate::tasks::read_events(&cwd_for_status)
                            .ok()
                            .and_then(|events| {
                                let graph = crate::tasks::materialize_graph(&events);
                                graph.tasks.get(&tail_for_status).map(|t| t.status.to_string())
                            })
                            .unwrap_or_default()
                    });
                } else {
                    resolver.add_var("session.thread.tail".to_string(), String::new());
                    resolver.add_var("session.thread.head".to_string(), String::new());
                    resolver.add_var("session.thread".to_string(), String::new());
                    resolver.add_var("session.thread.tail.status".to_string(), String::new());
                }

                // session.task.* — lazy lookup of the thread tail task from the task graph
                if let Some(thread) = thread {
                    let tail_id = thread.tail.clone();
                    let cwd = state.cwd().to_path_buf();
                    // Cache the task lookup so we only load the graph once
                    let task_cache: std::rc::Rc<
                        std::cell::OnceCell<Option<(String, String)>>,
                    > = std::rc::Rc::new(std::cell::OnceCell::new());

                    let lookup_task = {
                        let tail_id = tail_id.clone();
                        let cwd = cwd.clone();
                        let cache = task_cache.clone();
                        move || -> Option<(String, String)> {
                            cache.get_or_init(|| {
                                let events = crate::tasks::read_events(&cwd).ok()?;
                                let graph = crate::tasks::materialize_graph(&events);
                                let task = graph.tasks.get(&tail_id)?;
                                Some((
                                    task.status.to_string(),
                                    task.task_type.clone().unwrap_or_default(),
                                ))
                            }).clone()
                        }
                    };

                    resolver.add_var("session.task.id".to_string(), tail_id);

                    let lookup_status = lookup_task.clone();
                    resolver.add_lazy_var("session.task.status", move || {
                        lookup_status()
                            .map(|(status, _)| status)
                            .unwrap_or_default()
                    });

                    let lookup_type = lookup_task;
                    resolver.add_lazy_var("session.task.type", move || {
                        lookup_type()
                            .map(|(_, task_type)| task_type)
                            .unwrap_or_default()
                    });
                } else {
                    resolver.add_var("session.task.id".to_string(), String::new());
                    resolver.add_var("session.task.status".to_string(), String::new());
                    resolver.add_var("session.task.type".to_string(), String::new());
                }
            }
        }

        resolver
    }
    /// Execute a list of statements sequentially
    ///
    /// Returns the flow result.
    pub fn execute_statements(
        statements: &[HookStatement],
        state: &mut AikiState,
    ) -> Result<HookOutcome> {
        let mut had_continue_failure = false;

        for statement in statements {
            let result = Self::execute_statement(statement, state)?;

            // Handle flow control results
            match result {
                HookOutcome::Success => {
                    // Continue to next statement
                }
                HookOutcome::FailedContinue => {
                    had_continue_failure = true;
                    // Continue to next statement
                }
                HookOutcome::FailedStop => {
                    return Ok(HookOutcome::FailedStop);
                }
                HookOutcome::FailedBlock => {
                    return Ok(HookOutcome::FailedBlock);
                }
            }
        }

        // All actions completed
        if had_continue_failure {
            Ok(HookOutcome::FailedContinue)
        } else {
            Ok(HookOutcome::Success)
        }
    }

    /// Execute a single statement
    fn execute_statement(statement: &HookStatement, state: &mut AikiState) -> Result<HookOutcome> {
        match statement {
            HookStatement::If(if_stmt) => Self::execute_if(if_stmt, state),
            HookStatement::Switch(switch_stmt) => Self::execute_switch(switch_stmt, state),
            HookStatement::Hook(hook_action) => {
                // hook: actions should be intercepted by HookComposer, not the engine.
                // If we reach here, it means the engine was called directly without
                // the composer's statement interceptor.
                Err(AikiError::Other(anyhow::anyhow!(
                    "hook: action '{}' must be handled by HookComposer, not HookEngine directly. \
                     Use execute_statements_with_hooks() instead.",
                    hook_action.hook
                )))
            }
            HookStatement::Action(action) => {
                // Execute the action
                let result = Self::execute_action(action, state)?;

                // Store action results for reference by subsequent actions
                Self::store_action_result(action, &result, state);

                // Handle action failures with on_failure behavior
                if !result.success {
                    Self::handle_action_failure(action, &result, state)
                } else {
                    Ok(HookOutcome::Success)
                }
            }
        }
    }

    /// Execute a single action
    fn execute_action(action: &Action, state: &mut AikiState) -> Result<ActionResult> {
        match action {
            Action::Shell(shell_action) => Self::execute_shell(shell_action, state),
            Action::Jj(jj_action) => Self::execute_jj(jj_action, state),
            Action::Log(log_action) => Self::execute_log(log_action, state),
            Action::Let(let_action) => Self::execute_let(let_action, state),
            Action::Call(call_action) => Self::execute_call(call_action, state),
            Action::Context(context_action) => Self::execute_context(context_action, state),
            Action::Autoreply(autoreply_action) => Self::execute_autoreply(autoreply_action, state),
            Action::CommitMessage(commit_msg_action) => {
                Self::execute_commit_message(commit_msg_action, state)
            }
            Action::TaskRun(task_run_action) => Self::execute_task_run(task_run_action, state),
            Action::Review(review_action) => Self::execute_review(review_action, state),
            Action::Continue(continue_action) => Self::execute_continue(continue_action, state),
            Action::Stop(stop_action) => Self::execute_stop(stop_action, state),
            Action::Block(block_action) => Self::execute_block(block_action, state),
            Action::SessionEnd(session_end_action) => {
                Self::execute_session_end(session_end_action, state)
            }
            Action::AutoreplyToEndSession(action) => {
                Self::execute_autoreply_to_end_session(action, state)
            }
            Action::AutoreplyToFixConflicts(action) => {
                Self::execute_autoreply_to_fix_conflicts(action, state)
            }
        }
    }

    /// Handle action failure
    fn handle_action_failure(
        action: &Action,
        result: &ActionResult,
        state: &mut AikiState,
    ) -> Result<HookOutcome> {
        let on_failure_behavior = match action {
            Action::Shell(shell_action) => &shell_action.on_failure,
            Action::Jj(jj_action) => &jj_action.on_failure,
            Action::Let(let_action) => &let_action.on_failure,
            Action::Call(call_action) => &call_action.on_failure,
            Action::Context(context_action) => &context_action.on_failure,
            Action::Autoreply(autoreply_action) => &autoreply_action.on_failure,
            Action::CommitMessage(commit_msg_action) => &commit_msg_action.on_failure,
            Action::TaskRun(task_run_action) => &task_run_action.on_failure,
            Action::Review(review_action) => &review_action.on_failure,
            Action::Log(_) => return Ok(HookOutcome::Success),
            Action::Continue(_) => return Ok(HookOutcome::FailedContinue),
            Action::Stop(_) => return Ok(HookOutcome::FailedStop),
            Action::Block(_) => return Ok(HookOutcome::FailedBlock),
            Action::SessionEnd(session_end_action) => &session_end_action.on_failure,
            Action::AutoreplyToEndSession(action) => &action.on_failure,
            Action::AutoreplyToFixConflicts(action) => &action.on_failure,
        };

        let failure_text = if !result.stderr.is_empty() {
            result.stderr.clone()
        } else {
            "Action failed".to_string()
        };

        match on_failure_behavior {
            OnFailure::Shortcut(shortcut) => match shortcut {
                OnFailureShortcut::Continue => {
                    eprintln!("[aiki] Action failed but continuing: {}", failure_text);
                    state.add_failure(crate::events::result::Failure(failure_text));
                    Ok(HookOutcome::FailedContinue)
                }
                OnFailureShortcut::Stop => {
                    state.add_failure(crate::events::result::Failure(failure_text));
                    Ok(HookOutcome::FailedStop)
                }
                OnFailureShortcut::Block => {
                    state.add_failure(crate::events::result::Failure(failure_text));
                    Ok(HookOutcome::FailedBlock)
                }
            },
            OnFailure::Statements(on_failure_statements) => {
                if on_failure_statements.is_empty() {
                    eprintln!(
                        "[aiki] Action failed with empty on_failure list, continuing: {}",
                        failure_text
                    );
                    state.add_failure(crate::events::result::Failure(failure_text));
                    return Ok(HookOutcome::FailedContinue);
                }

                let failures_before = state.failures().len();

                // Store failure context for on_failure handlers
                if let Some(exit_code) = result.exit_code {
                    state.set_variable("EXIT_CODE".to_string(), exit_code.to_string());
                } else {
                    state.set_variable("EXIT_CODE".to_string(), String::new());
                }
                state.set_variable("STDOUT".to_string(), result.stdout.clone());
                state.set_variable("STDERR".to_string(), result.stderr.clone());

                // Execute on_failure statements (no timing)
                let callback_result = Self::execute_statements(on_failure_statements, state)?;

                // Add default failure record if on_failure handlers didn't add any
                let failures_after = state.failures().len();
                if failures_after == failures_before {
                    state.add_failure(crate::events::result::Failure(failure_text));
                }

                // Translate Success to FailedContinue since we had a failure but handled it
                Ok(match callback_result {
                    HookOutcome::Success => HookOutcome::FailedContinue,
                    other => other,
                })
            }
        }
    }

    /// Store action result as variables for subsequent actions
    ///
    /// For Let actions: stores the variable and its structured metadata
    /// For Shell/Jj/Log with alias: stores the variable with its result
    fn store_action_result(action: &Action, result: &ActionResult, state: &mut AikiState) {
        match action {
            Action::Let(let_action) => {
                // Parse the variable name from "variable = expression"
                if let Some(variable_name) = let_action.let_.split('=').next() {
                    let variable_name = variable_name.trim();
                    state.store_action_result(variable_name.to_string(), result.clone());
                }
            }
            Action::Shell(shell_action) => {
                if let Some(alias) = &shell_action.alias {
                    state.store_action_result(alias.clone(), result.clone());
                }
            }
            Action::Jj(jj_action) => {
                if let Some(alias) = &jj_action.alias {
                    state.store_action_result(alias.clone(), result.clone());
                }
            }
            Action::Log(log_action) => {
                if let Some(alias) = &log_action.alias {
                    state.store_action_result(alias.clone(), result.clone());
                }
            }
            Action::Call(_) => {
                // Call actions don't store results (they're fire-and-forget)
            }
            Action::Context(_) => {
                // Context actions accumulate in state.context directly
                // No need to store results
            }
            Action::Autoreply(_) => {
                // Autoreply actions modify the autoreply_assembler in state directly
                // No need to store results
            }
            Action::CommitMessage(_) => {
                // commit_message actions don't produce storable results
            }
            Action::TaskRun(_) => {
                // task.run actions don't produce storable results
            }
            Action::Review(_) => {
                // review actions don't produce storable results
            }
            Action::Continue(_) | Action::Stop(_) | Action::Block(_) => {
                // Flow control actions add messages and control execution flow
                // No need to store results
            }
            Action::SessionEnd(_) => {
                // session.end actions don't produce storable results
            }
            Action::AutoreplyToEndSession(_) => {
                // autoreply_to_end_session sets state.end_session flag, no storable result
            }
            Action::AutoreplyToFixConflicts(_) => {
                // autoreply_to_fix_conflicts adds to context assembler, no storable result
            }
        }
    }

    /// Execute a shell command
    fn execute_shell(action: &ShellAction, state: &mut AikiState) -> Result<ActionResult> {
        // Create variable resolver with consistent variable availability
        let mut resolver = Self::create_resolver(state);

        // Resolve variables in command
        let command = resolver.resolve(&action.shell)?;

        debug_log(|| format!("[flows] Executing shell: {}", command));

        // Execute command
        let output = if let Some(timeout_str) = &action.timeout {
            // Parse timeout (e.g., "30s", "1m")
            let timeout = parse_timeout(timeout_str)?;
            execute_with_timeout(&command, state.cwd(), timeout)?
        } else {
            Command::new("sh")
                .arg("-c")
                .arg(&command)
                .current_dir(state.cwd())
                .output()
                .context("Failed to execute shell command")?
        };

        Ok(ActionResult {
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    /// Execute a JJ command
    fn execute_jj(action: &JjAction, state: &mut AikiState) -> Result<ActionResult> {
        // Handle with_author_and_message if provided - it sets both author and message
        if let Some(ref metadata_fn) = action.with_author_and_message {
            let resolved_metadata = if metadata_fn.trim().starts_with("self.") {
                // Execute the metadata function
                let call_action = CallAction {
                    call: metadata_fn.trim().to_string(),
                    on_failure: OnFailure::default(),
                };
                let result = Self::execute_call(&call_action, state)?;
                result.stdout.trim().to_string()
            } else {
                // Resolve variable reference (bare name or {{template}})
                let mut resolver = Self::create_resolver(state);
                resolver.resolve_or_lookup(metadata_fn)?
            };

            // Parse the JSON result
            let json: serde_json::Value = serde_json::from_str(&resolved_metadata)
                .context("Failed to parse metadata function result as JSON")?;

            let author = json["author"]
                .as_str()
                .ok_or_else(|| {
                    AikiError::Other(anyhow::anyhow!("Metadata missing 'author' field"))
                })?
                .to_string();

            let message = json["message"]
                .as_str()
                .ok_or_else(|| {
                    AikiError::Other(anyhow::anyhow!("Metadata missing 'message' field"))
                })?
                .to_string();

            // Store message in context so it can be referenced as {{message}}
            state.store_action_result(
                "message".to_string(),
                ActionResult {
                    success: true,
                    exit_code: Some(0),
                    stdout: message,
                    stderr: String::new(),
                },
            );

            // Create a new JjAction with with_author set
            let mut new_action = action.clone();
            new_action.with_author = Some(author);
            new_action.with_author_and_message = None; // Clear to avoid infinite loop

            // Execute the modified action
            return Self::execute_jj(&new_action, state);
        }

        // Create variable resolver with consistent variable availability
        let mut resolver = Self::create_resolver(state);

        // Resolve variables in command
        let jj_args = resolver.resolve(&action.jj)?;

        debug_log(|| format!("[flows] Executing jj: {}", jj_args));

        // Parse arguments using proper shell word splitting (handles quoted args)
        let args = shell_words::split(&jj_args)
            .with_context(|| format!("Failed to parse jj arguments: {}", jj_args))?;

        // Parse with_author if provided
        let (jj_user, jj_email) = if let Some(ref author) = action.with_author {
            let resolved_author = if author.trim().starts_with("self.") {
                // It's a function call - execute it now (maintains execution order)
                let call_action = CallAction {
                    call: author.trim().to_string(),
                    on_failure: OnFailure::default(),
                };
                let result = Self::execute_call(&call_action, state)?;
                result.stdout.trim().to_string()
            } else {
                // It's a variable reference (bare name or {{template}})
                resolver.resolve_or_lookup(author)?
            };
            parse_author(&resolved_author)?
        } else {
            (None, None)
        };

        if let (Some(ref user), Some(ref email)) = (&jj_user, &jj_email) {
            debug_log(|| format!("[flows] Setting JJ_USER={}, JJ_EMAIL={}", user, email));
        }

        // Execute JJ command (using direct argv, no shell invocation)
        let output = if let Some(timeout_str) = &action.timeout {
            let timeout = parse_timeout(timeout_str)?;
            execute_with_timeout_argv_with_env(
                "jj",
                &args,
                state.cwd(),
                timeout,
                jj_user,
                jj_email,
            )?
        } else {
            let mut cmd = jj_cmd();
            cmd.args(&args).current_dir(state.cwd());

            // Set JJ_USER and JJ_EMAIL if provided
            if let Some(user) = jj_user {
                cmd.env("JJ_USER", user);
            }
            if let Some(email) = jj_email {
                cmd.env("JJ_EMAIL", email);
            }

            cmd.output().context("Failed to execute jj command")?
        };

        Ok(ActionResult {
            success: output.status.success(),
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }

    /// Execute a log action
    fn execute_log(action: &LogAction, state: &mut AikiState) -> Result<ActionResult> {
        // Create variable resolver with consistent variable availability
        let mut resolver = Self::create_resolver(state);

        // Resolve variables in message
        let message = resolver.resolve(&action.log)?;

        // Print to stderr (so it appears in hook output)
        eprintln!("[aiki] {}", message);

        // Return the message in stdout so it can be stored as a variable
        Ok(ActionResult {
            success: true,
            exit_code: Some(0),
            stdout: message,
            stderr: String::new(),
        })
    }

    /// Execute a task.run action - spawns an agent session to work on a task
    fn execute_task_run(action: &TaskRunAction, state: &mut AikiState) -> Result<ActionResult> {
        use crate::agents::AgentType;
        use crate::tasks::runner::{task_run, TaskRunOptions};

        // Create variable resolver
        let mut resolver = Self::create_resolver(state);

        // Resolve task_id (supports variable interpolation)
        let task_id = resolver.resolve(&action.task_run.task_id)?;
        if task_id.is_empty() {
            return Ok(ActionResult {
                success: false,
                exit_code: Some(1),
                stdout: String::new(),
                stderr: "task.run: task_id is required".to_string(),
            });
        }

        // Resolve optional agent override
        let agent_override = action
            .task_run
            .agent
            .as_ref()
            .map(|a| resolver.resolve(a))
            .transpose()?;

        // Build options
        let mut options = TaskRunOptions::new();
        if let Some(ref agent_str) = agent_override {
            if let Some(agent_type) = AgentType::from_str(agent_str) {
                options = options.with_agent(agent_type);
            } else {
                return Ok(ActionResult {
                    success: false,
                    exit_code: Some(1),
                    stdout: String::new(),
                    stderr: format!("task.run: unknown agent type '{}'", agent_str),
                });
            }
        }

        // Get cwd from state
        let cwd = state.cwd();

        // Run the task
        match task_run(&cwd, &task_id, options) {
            Ok(()) => Ok(ActionResult {
                success: true,
                exit_code: Some(0),
                stdout: format!("Task {} completed", task_id),
                stderr: String::new(),
            }),
            Err(e) => Ok(ActionResult {
                success: false,
                exit_code: Some(1),
                stdout: String::new(),
                stderr: e.to_string(),
            }),
        }
    }

    /// Execute a review action - creates and runs a code review task
    ///
    /// This is a thin wrapper around `aiki review`. Internally,
    /// `review: { task_id: X, template: Y }` is equivalent to
    /// `aiki review X --template Y --async`. Flows always use async mode.
    fn execute_review(action: &ReviewAction, state: &mut AikiState) -> Result<ActionResult> {
        use crate::reviews::{create_review, detect_target, CreateReviewParams};
        use crate::tasks::runner::{task_run_async, TaskRunOptions};

        // Create variable resolver
        let mut resolver = Self::create_resolver(state);

        // Resolve optional task_id for scope
        let task_id = action
            .review
            .task_id
            .as_ref()
            .map(|id| resolver.resolve(id))
            .transpose()?;

        // Resolve optional agent override
        let agent_override = action
            .review
            .agent
            .as_ref()
            .map(|a| resolver.resolve(a))
            .transpose()?;

        // Resolve optional template name
        let template = action
            .review
            .template
            .as_ref()
            .map(|t| resolver.resolve(t))
            .transpose()?;

        // Get cwd from state
        let cwd = state.cwd();

        // Detect target scope (flows are always task-id-centric, no --implementation)
        let (scope, _worker) = match detect_target(&cwd, task_id.as_deref(), false) {
            Ok(r) => r,
            Err(AikiError::NothingToReview) => {
                return Ok(ActionResult {
                    success: true,
                    exit_code: Some(0),
                    stdout: String::new(),
                    stderr: "Nothing to review - no closed tasks in session.".to_string(),
                });
            }
            Err(e) => {
                return Ok(ActionResult {
                    success: false,
                    exit_code: Some(1),
                    stdout: String::new(),
                    stderr: e.to_string(),
                });
            }
        };

        // Create review task using shared logic (same as CLI)
        let result = match create_review(
            &cwd,
            CreateReviewParams {
                scope,
                agent_override,
                template,
                fix_template: None,
                autorun: false,
            },
        ) {
            Ok(r) => r,
            Err(AikiError::NothingToReview) => {
                // No tasks to review - this is a success case for flows
                return Ok(ActionResult {
                    success: true,
                    exit_code: Some(0),
                    stdout: String::new(),
                    stderr: "Nothing to review - no closed tasks in session.".to_string(),
                });
            }
            Err(e) => {
                return Ok(ActionResult {
                    success: false,
                    exit_code: Some(1),
                    stdout: String::new(),
                    stderr: format!("Failed to create review task: {}", e),
                });
            }
        };

        // Run the review task asynchronously (flows can't block)
        let options = TaskRunOptions::new();
        match task_run_async(&cwd, &result.review_task_id, options) {
            Ok(_handle) => Ok(ActionResult {
                success: true,
                exit_code: Some(0),
                stdout: format!("Review task {} started", result.review_task_id),
                stderr: String::new(),
            }),
            Err(e) => Ok(ActionResult {
                success: false,
                exit_code: Some(1),
                stdout: String::new(),
                stderr: format!("Failed to start review task: {}", e),
            }),
        }
    }

    /// Execute a continue action (generates Failure and continues)
    fn execute_continue(
        action: &crate::flows::types::ContinueAction,
        state: &mut AikiState,
    ) -> Result<ActionResult> {
        // Create variable resolver
        let mut resolver = Self::create_resolver(state);

        // Resolve variables in failure text
        let failure = resolver.resolve(&action.failure)?;

        // Always add failure, using default text if empty
        let failure_text = if !failure.is_empty() {
            failure
        } else {
            "Action triggered continue (no message provided)".to_string()
        };
        state.add_failure(crate::events::result::Failure(failure_text.clone()));

        // Return failure to trigger continue behavior through handle_action_failure
        Ok(ActionResult {
            success: false,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: failure_text,
        })
    }

    /// Execute a stop action (generates Failure and returns failure)
    fn execute_stop(
        action: &crate::flows::types::StopAction,
        state: &mut AikiState,
    ) -> Result<ActionResult> {
        // Create variable resolver
        let mut resolver = Self::create_resolver(state);

        // Resolve variables in failure text
        let failure = resolver.resolve(&action.failure)?;

        // Always add failure, using default text if empty
        let failure_text = if !failure.is_empty() {
            failure
        } else {
            "Action triggered stop (no message provided)".to_string()
        };
        state.add_failure(crate::events::result::Failure(failure_text.clone()));

        // Return failure to trigger stop behavior
        Ok(ActionResult {
            success: false,
            exit_code: Some(1),
            stdout: String::new(),
            stderr: failure_text,
        })
    }

    /// Execute a block action (generates Failure and returns failure)
    fn execute_block(
        action: &crate::flows::types::BlockAction,
        state: &mut AikiState,
    ) -> Result<ActionResult> {
        // Create variable resolver
        let mut resolver = Self::create_resolver(state);

        // Resolve variables in failure text
        let failure = resolver.resolve(&action.failure)?;

        // Always add failure, using default text if empty
        let failure_text = if !failure.is_empty() {
            failure
        } else {
            "Action triggered block (no message provided)".to_string()
        };
        state.add_failure(crate::events::result::Failure(failure_text.clone()));

        // Return failure to trigger block behavior
        Ok(ActionResult {
            success: false,
            exit_code: Some(2),
            stdout: String::new(),
            stderr: failure_text,
        })
    }

    /// Execute a sigterm_to_end_session action - terminates the current session via SIGTERM
    ///
    /// This action is used for agents (like Codex) whose SDK doesn't fire the Stop
    /// hook in exec mode, so cooperative termination via autoreply_to_end_session
    /// cannot work. Sends SIGTERM to the agent process after hooks complete.
    fn execute_session_end(
        action: &crate::flows::types::SessionEndAction,
        state: &mut AikiState,
    ) -> Result<ActionResult> {
        use crate::cache::debug_log;

        // Create variable resolver
        let mut resolver = Self::create_resolver(state);

        // Resolve variables in reason text
        let reason = resolver.resolve(&action.reason)?;

        debug_log(|| format!("sigterm_to_end_session: {}", reason));

        // Get session info. For task.closed events, reverse-lookup from session files.
        // For session-bearing events, extract directly from the event payload.
        let session_info = state.resolve_task_closed_thread_session();
        let parent_pid = extract_session(&state.event)
            .ok()
            .and_then(|s| s.parent_pid())
            .or_else(|| session_info.as_ref().map(|info| info.pid));

        if let Some(pid) = parent_pid {
            // Fire synthetic turn.completed → session.ended to complete the
            // session lifecycle. Codex exec doesn't fire Stop, so the last
            // turn's turn.completed (workspace absorption) was never run.
            // session.ended handles history recording and session file cleanup.
            if let Some(ref info) = session_info {
                let session = info.to_aiki_session();
                let cwd = state.cwd().to_path_buf();

                // 1. Synthetic turn.completed — absorbs workspace changes
                debug_log(|| format!("sigterm_to_end_session: Dispatching synthetic turn.completed for {}", info.session_id));
                let turn_event = crate::events::AikiEvent::TurnCompleted(
                    crate::events::AikiTurnCompletedPayload {
                        session: session.clone(),
                        cwd: cwd.clone(),
                        timestamp: chrono::Utc::now(),
                        turn: crate::events::Turn::unknown(),
                        response: String::new(),
                        modified_files: vec![],
                        tasks: Default::default(),
                        tokens: None,
                        model: None,
                    },
                );
                if let Err(e) = crate::event_bus::dispatch(turn_event) {
                    debug_log(|| format!("sigterm_to_end_session: turn.completed dispatch failed (non-fatal): {}", e));
                }

                // 2. Synthetic session.ended — history, session file cleanup
                debug_log(|| format!("sigterm_to_end_session: Dispatching synthetic session.ended for {}", info.session_id));
                let ended_event = crate::events::AikiEvent::SessionEnded(
                    crate::events::AikiSessionEndedPayload {
                        session,
                        cwd,
                        timestamp: chrono::Utc::now(),
                        reason: reason.clone(),
                        tokens: None,
                    },
                );
                if let Err(e) = crate::event_bus::dispatch(ended_event) {
                    debug_log(|| format!("sigterm_to_end_session: session.ended dispatch failed (non-fatal): {}", e));
                }
            }

            // Defer SIGTERM until after all hooks complete
            // The actual termination happens in execute_pending_session_ends()
            // which is called by the event handler after hook execution
            debug_log(|| format!("sigterm_to_end_session: Deferring SIGTERM to PID {}", pid));
            state.add_pending_session_end(pid);

            Ok(ActionResult {
                success: true,
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            })
        } else {
            // No parent PID found - log warning but don't fail
            debug_log(|| "sigterm_to_end_session: No parent PID available, skipping termination".to_string());
            Ok(ActionResult {
                success: true,
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            })
        }
    }

    /// Execute an autoreply_to_end_session action (cooperative termination).
    ///
    /// Sets `state.end_session = true` so that the turn.completed handler
    /// returns `Decision::Block`. The editor output builder translates this
    /// into the agent-specific stop signal (e.g., `{ "continue": false }`
    /// for Codex).
    fn execute_autoreply_to_end_session(
        action: &crate::flows::types::AutoreplyToEndSessionAction,
        state: &mut AikiState,
    ) -> Result<ActionResult> {
        use crate::cache::debug_log;

        let mut resolver = Self::create_resolver(state);
        let reason = resolver.resolve(&action.reason)?;

        debug_log(|| format!("autoreply_to_end_session: {}", reason));
        state.end_session = true;

        Ok(ActionResult {
            success: true,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    /// Execute an autoreply_to_fix_conflicts action.
    ///
    /// Builds a conflict resolution autoreply message from the conflicted file
    /// list and adds it to the context assembler, just like a regular autoreply.
    fn execute_autoreply_to_fix_conflicts(
        action: &crate::flows::types::AutoreplyToFixConflictsAction,
        state: &mut AikiState,
    ) -> Result<ActionResult> {
        use crate::cache::debug_log;
        use crate::events::AikiEvent;

        if !matches!(&state.event, AikiEvent::TurnCompleted(_)) {
            return Err(AikiError::Other(anyhow::anyhow!(
                "autoreply_to_fix_conflicts can only be used in turn.completed events"
            )));
        }

        let mut resolver = Self::create_resolver(state);
        let conflicted_files = resolver.resolve(&action.conflicted_files)?;

        debug_log(|| format!("autoreply_to_fix_conflicts: {}", conflicted_files));

        let message = format!(
            "CONFLICT RESOLUTION REQUIRED\n\
             \n\
             Absorption introduced conflicts in the working copy.\n\
             Conflicted files:\n\
             {conflicted_files}\n\
             \n\
             To resolve: edit the conflicted files to remove JJ conflict\n\
             markers, then continue working. JJ parses your edits back\n\
             automatically on snapshot. Alternatively, run\n\
             `aiki resolve <change-id>` to delegate conflict resolution.\n\
             \n\
             JJ conflict marker format (NOT Git's format):\n\
             \x20 <<<<<<< Conflict N of M\n\
             \x20 %%%%%%% Changes from base to side #1 (this is a DIFF, not content)\n\
             \x20 +++++++ Contents of side #2 (this is literal content)\n\
             \x20 >>>>>>> Conflict N of M ends\n\
             \n\
             The %%%%%%% section shows a diff (additions/removals from base),\n\
             NOT a middle version like Git's =======. Apply the diff's intent\n\
             to side #2's content to produce the merged result."
        );

        let chunk = crate::flows::context::ContextChunk {
            prepend: None,
            append: Some(crate::flows::context::TextLines::Single(message)),
        };

        let assembler = state.get_context_assembler_mut()?;
        assembler.add_chunk(chunk);

        Ok(ActionResult {
            success: true,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    /// Execute a context action
    ///
    /// This action accumulates context that will be prepended to prompts/autoreplies.
    /// Works for session lifecycle events plus turn.started and turn.completed.
    fn execute_context(action: &ContextAction, state: &mut AikiState) -> Result<ActionResult> {
        use crate::events::AikiEvent;

        // Verify this is an event type that supports context injection
        if !matches!(
            &state.event,
            AikiEvent::SessionStarted(_)
                | AikiEvent::SessionResumed(_)
                | AikiEvent::SessionCleared(_)
                | AikiEvent::TurnStarted(_)
                | AikiEvent::TurnCompleted(_)
        ) {
            return Err(AikiError::Other(anyhow::anyhow!(
                "context action can only be used in session.started, session.resumed, session.cleared, turn.started, or turn.completed events"
            )));
        }

        // Create variable resolver
        let mut resolver = Self::create_resolver(state);

        // Convert ContextContent to ContextChunk and resolve variables
        let chunk = match &action.context {
            crate::flows::types::ContextContent::Simple(text) => {
                // Simple form defaults to append
                ContextChunk {
                    prepend: None,
                    append: Some(crate::flows::context::TextLines::Single(text.clone())),
                }
            }
            crate::flows::types::ContextContent::Explicit { prepend, append } => {
                crate::flows::context::ContextChunk {
                    prepend: prepend.clone(),
                    append: append.clone(),
                }
            }
        }
        .resolve_variables(|s| resolver.resolve(s))?;

        // Validate chunk before adding to assembler
        chunk.validate()?;

        // Add chunk to message assembler
        let assembler = state.get_context_assembler_mut()?;
        assembler.add_chunk(chunk);

        Ok(ActionResult {
            success: true,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    /// Execute an autoreply action
    ///
    /// This action adds content to the autoreply assembler for turn.completed events.
    /// Only works for turn.completed events that have an autoreply_assembler.
    fn execute_autoreply(action: &AutoreplyAction, state: &mut AikiState) -> Result<ActionResult> {
        use crate::events::AikiEvent;

        // Verify this is a TurnCompleted event
        if !matches!(&state.event, AikiEvent::TurnCompleted(_)) {
            return Err(AikiError::Other(anyhow::anyhow!(
                "autoreply action can only be used in turn.completed events"
            )));
        }

        // Create variable resolver
        let mut resolver = Self::create_resolver(state);

        // Convert AutoreplyContent to ContextChunk and resolve variables
        let chunk = match &action.autoreply {
            AutoreplyContent::Simple(text) => {
                // Simple form: just text content
                ContextChunk {
                    prepend: None,
                    append: Some(crate::flows::context::TextLines::Single(text.clone())),
                }
            }
            AutoreplyContent::Explicit { prepend, append } => crate::flows::context::ContextChunk {
                prepend: prepend.clone(),
                append: append.clone(),
            },
        }
        .resolve_variables(|s| resolver.resolve(s))?;

        // Validate chunk before adding to assembler
        chunk.validate()?;

        // Add chunk to message assembler
        let assembler = state.get_context_assembler_mut()?;
        assembler.add_chunk(chunk);

        Ok(ActionResult {
            success: true,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    /// Execute a commit_message action
    ///
    /// This action modifies the commit message file in place.
    /// Only works for commit.message_started events that have a commit_msg_file.
    fn execute_commit_message(
        action: &CommitMessageAction,
        state: &mut AikiState,
    ) -> Result<ActionResult> {
        use crate::events::AikiEvent;
        use std::fs;

        // Get commit message file from event
        let commit_msg_file = match &state.event {
            AikiEvent::CommitMessageStarted(e) => e
                .commit_msg_file
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("No commit message file in event"))?,
            _ => {
                return Err(AikiError::Other(anyhow::anyhow!(
                    "commit_message can only be used in commit.message_started events"
                )))
            }
        };

        // Read current message
        let content = fs::read_to_string(commit_msg_file)?;

        // Create variable resolver
        let mut resolver = Self::create_resolver(state);
        let op = &action.commit_message;

        // Apply operations
        let new_content = Self::apply_commit_message_edits(&content, op, &mut resolver)?;

        // Write atomically
        fs::write(commit_msg_file, new_content)?;

        Ok(ActionResult {
            success: true,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    /// Apply commit message edit operations
    fn apply_commit_message_edits(
        content: &str,
        op: &CommitMessageOp,
        resolver: &mut VariableResolver,
    ) -> Result<String> {
        let mut result = content.to_string();

        // Prepend to subject line (before first line)
        if let Some(ref prepend_subject) = op.prepend_subject {
            let text = resolver.resolve(prepend_subject)?;
            if !text.is_empty() {
                result = format!("{}{}", text, result);
            }
        }

        // Append to body (before trailers)
        if let Some(ref body) = op.append_body {
            let text = resolver.resolve(body)?;
            if !text.is_empty() {
                result = Self::append_to_body(&result, &text);
            }
        }

        // Append trailer (after existing trailers)
        if let Some(ref trailer) = op.append_trailer {
            let text = resolver.resolve(trailer)?;
            if !text.is_empty() {
                result = Self::append_trailer(&result, &text);
            }
        }

        // Append footer (after everything)
        if let Some(ref append_footer) = op.append_footer {
            let text = resolver.resolve(append_footer)?;
            if !text.is_empty() {
                // Ensure blank line before appending
                if !result.ends_with('\n') {
                    result.push('\n');
                }
                if !result.ends_with("\n\n") {
                    result.push('\n');
                }
                result.push_str(&text);
                if !text.ends_with('\n') {
                    result.push('\n');
                }
            }
        }

        Ok(result)
    }

    /// Append text to message body (before trailers)
    fn append_to_body(content: &str, text: &str) -> String {
        // Find where trailers start (lines like "Key: value")
        let lines: Vec<&str> = content.lines().collect();
        let mut trailer_start = lines.len();

        // Scan backwards to find first trailer
        for (i, line) in lines.iter().enumerate().rev() {
            if line.is_empty() {
                continue;
            }
            if Self::is_trailer_line(line) {
                trailer_start = i;
            } else {
                break;
            }
        }

        if trailer_start == lines.len() {
            // No trailers, append to end
            let mut result = content.to_string();
            if !result.ends_with('\n') {
                result.push('\n');
            }
            result.push('\n');
            result.push_str(text);
            if !text.ends_with('\n') {
                result.push('\n');
            }
            result
        } else {
            // Insert before trailers
            let mut result = String::new();
            for (i, line) in lines.iter().enumerate() {
                if i == trailer_start {
                    // Add blank line if needed
                    if i > 0 && !lines[i - 1].is_empty() {
                        result.push('\n');
                    }
                    result.push_str(text);
                    if !text.ends_with('\n') {
                        result.push('\n');
                    }
                    result.push('\n');
                }
                result.push_str(line);
                result.push('\n');
            }
            result
        }
    }

    /// Append Git trailer (after existing trailers)
    fn append_trailer(content: &str, text: &str) -> String {
        let mut result = content.to_string();

        // Ensure there's a newline at the end
        if !result.ends_with('\n') {
            result.push('\n');
        }

        // Check if we need to add a blank line before the trailer
        // Git convention: trailers should be separated from the body by a blank line
        let lines: Vec<&str> = content.lines().collect();

        if lines.is_empty() {
            // Empty content, add blank line
            result.push('\n');
        } else {
            // Check the last two lines to see if there's already a blank line
            let last_non_empty = lines.iter().rev().find(|l| !l.is_empty());

            if let Some(last) = last_non_empty {
                // If last non-empty line is not a trailer, we need a blank line separator
                if !Self::is_trailer_line(last) {
                    // Check if there's already a blank line at the end
                    let ends_with_blank =
                        lines.last().map_or(false, |l| l.is_empty()) || result.ends_with("\n\n");

                    if !ends_with_blank {
                        result.push('\n');
                    }
                }
            } else {
                // All lines are empty, add blank line
                result.push('\n');
            }
        }

        result.push_str(text);
        if !text.ends_with('\n') {
            result.push('\n');
        }

        result
    }

    /// Check if a line looks like a Git trailer
    fn is_trailer_line(line: &str) -> bool {
        // Git trailers are lines like "Key: value" or "Key #value"
        // They typically have a capital letter at start and contain : or #
        if let Some(colon_pos) = line.find(':') {
            let key = &line[..colon_pos];
            // Key should start with capital letter, contain only word chars and hyphens
            !key.is_empty()
                && key.chars().next().map_or(false, |c| c.is_uppercase())
                && key.chars().all(|c| c.is_alphanumeric() || c == '-')
        } else {
            false
        }
    }

    /// Execute a let binding action
    ///
    /// Supports two modes:
    /// 1. Function call: `let metadata = aiki/core.build_metadata`
    /// 2. Variable aliasing: `let desc = description`

    /// Execute a conditional if/then/else statement
    fn execute_if(stmt: &IfStatement, state: &mut AikiState) -> Result<HookOutcome> {
        let condition_result = Self::evaluate_condition(&stmt.condition, state)?;

        debug_log(|| {
            format!(
                "[flows] If condition '{}' evaluated to: {}",
                stmt.condition, condition_result
            )
        });

        let statements_to_execute = if condition_result {
            debug_log(|| "[flows] Executing 'then' branch");
            &stmt.then
        } else if let Some(else_statements) = &stmt.else_ {
            debug_log(|| "[flows] Executing 'else' branch");
            else_statements
        } else {
            debug_log(|| "[flows] No else branch, condition false - no-op");
            return Ok(HookOutcome::Success);
        };

        Self::execute_statements(statements_to_execute, state)
    }

    /// Execute a switch/case statement
    fn execute_switch(stmt: &SwitchStatement, state: &mut AikiState) -> Result<HookOutcome> {
        let mut resolver = Self::create_resolver(state);
        let switch_value = resolver.resolve(&stmt.expression)?;

        debug_log(|| {
            format!(
                "[flows] Switch expression '{}' evaluated to: {}",
                stmt.expression, switch_value
            )
        });

        let statements_to_execute = if let Some(case_statements) = stmt.cases.get(&switch_value) {
            debug_log(|| format!("[flows] Switch matched case: {}", switch_value));
            case_statements
        } else if let Some(default_statements) = &stmt.default {
            debug_log(|| {
                format!(
                    "[flows] Switch using default case (no match for '{}')",
                    switch_value
                )
            });
            default_statements
        } else {
            debug_log(|| {
                format!(
                    "[flows] Switch: no match for '{}' and no default case",
                    switch_value
                )
            });
            return Ok(HookOutcome::Success);
        };

        Self::execute_statements(statements_to_execute, state)
    }

    /// Evaluate a condition expression using Rhai.
    ///
    /// Supports: ==, !=, >, <, >=, <=, &&, ||, field access (event.task.type),
    /// $var prefix (deprecated, auto-stripped), and/or/not word operators.
    ///
    /// Before Rhai evaluation, `self.function` calls are resolved and their
    /// results substituted into the expression as string literals.
    fn evaluate_condition(condition: &str, state: &mut AikiState) -> Result<bool> {
        let condition = condition.trim();

        debug_log(|| format!("[flows] Evaluating condition with Rhai: '{}'", condition));

        // Warn about deprecated $var syntax
        if crate::expressions::uses_dollar_syntax(condition) {
            eprintln!(
                "[aiki] Warning: `$var` syntax is deprecated, use `var` instead: {}",
                condition
            );
        }

        // Step 1: Pre-process self.function calls (can't be evaluated by Rhai)
        let condition = Self::resolve_self_functions_in_condition(condition, state)?;

        // Step 2: Build Rhai scope from state variables
        let mut resolver = Self::create_resolver(state);
        let variables = resolver.collect_variables();
        let var_map: std::collections::BTreeMap<String, String> = variables.into_iter().collect();
        let mut scope = crate::expressions::build_scope_from_flat(&var_map);

        // Step 3: Evaluate with Rhai
        // ExpressionEvaluator::evaluate uses lenient mode — Rhai errors are
        // caught internally (logged + default to false), so it always returns Ok.
        let result = state
            .expression_evaluator()
            .evaluate(&condition, &mut scope)
            .expect("ExpressionEvaluator::evaluate is infallible in lenient mode");
        debug_log(|| format!("[flows] Rhai condition result: {}", result));
        Ok(result)
    }

    /// Pre-process self.function calls in a condition expression.
    ///
    /// Finds `self.function` or `self.function.field` references and replaces
    /// them with their resolved string values (quoted for Rhai).
    fn resolve_self_functions_in_condition(
        condition: &str,
        state: &mut AikiState,
    ) -> Result<String> {
        if !condition.contains("self.") {
            return Ok(condition.to_string());
        }

        // Find self.function patterns outside of string literals.
        // We scan character-by-character to track quote state, collecting
        // the positions of `self.` references that need resolution.
        let bytes = condition.as_bytes();
        let len = bytes.len();
        let mut positions: Vec<(usize, usize)> = Vec::new(); // (start, end) pairs
        let mut i = 0;
        let mut in_string: Option<u8> = None; // b'"' or b'\''

        while i < len {
            let c = bytes[i];

            // Track string literal boundaries
            if in_string.is_some() {
                if c == b'\\' && i + 1 < len {
                    i += 2; // skip escaped char
                    continue;
                }
                if Some(c) == in_string {
                    in_string = None;
                }
                i += 1;
                continue;
            }

            if c == b'"' || c == b'\'' {
                in_string = Some(c);
                i += 1;
                continue;
            }

            // Check for "self." outside string literals
            if i + 5 <= len && &condition[i..i + 5] == "self." {
                let after_self = &condition[i + 5..];
                let func_end = after_self
                    .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '.')
                    .unwrap_or(after_self.len());
                positions.push((i, i + 5 + func_end));
                i += 5 + func_end;
            } else {
                i += 1;
            }
        }

        if positions.is_empty() {
            return Ok(condition.to_string());
        }

        // Resolve in reverse order so earlier indices remain valid
        let mut result = condition.to_string();
        for &(start, end) in positions.iter().rev() {
            let self_ref = &condition[start..end];
            let value = Self::resolve_self_function(self_ref, state)?;

            // Preserve type for Rhai: numeric values and booleans unquoted
            let replacement = if value.parse::<i64>().is_ok()
                || value.parse::<f64>().is_ok()
                || value == "true"
                || value == "false"
            {
                value.clone()
            } else {
                format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
            };
            result = format!("{}{}{}", &result[..start], replacement, &result[end..]);
        }

        Ok(result)
    }

    /// Resolve a single self.function reference to its string value.
    fn resolve_self_function(expr: &str, state: &mut AikiState) -> Result<String> {
        // Split into "self" and the rest
        let parts: Vec<&str> = expr.splitn(2, '.').collect();
        if parts.len() != 2 {
            return Err(AikiError::Other(anyhow::anyhow!(
                "Invalid inline function call syntax: '{}'",
                expr
            )));
        }
        let remaining = parts[1];

        // Check if there's a field access after the function name
        if let Some(field_start) = remaining.find('.') {
            let function_name = remaining[..field_start].trim();
            let field_path = remaining[field_start + 1..].trim();

            let call_action = CallAction {
                call: format!("self.{}", function_name),
                on_failure: OnFailure::default(),
            };

            let result = Self::execute_call(&call_action, state)?;

            // Parse result as JSON and extract the field
            let json_value: serde_json::Value = serde_json::from_str(&result.stdout)
                .context("Failed to parse function result as JSON")?;

            let mut current = &json_value;
            for field in field_path.split('.') {
                current = current.get(field).ok_or_else(|| {
                    AikiError::Other(anyhow::anyhow!(
                        "Field '{}' not found in JSON result",
                        field
                    ))
                })?;
            }

            Ok(match current {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Number(n) => n.to_string(),
                serde_json::Value::Bool(b) => b.to_string(),
                serde_json::Value::Null => "null".to_string(),
                _ => current.to_string(),
            })
        } else {
            // No field access - execute the function and return stdout
            let call_action = CallAction {
                call: expr.to_string(),
                on_failure: OnFailure::default(),
            };

            let result = Self::execute_call(&call_action, state)?;
            Ok(result.stdout.trim().to_string())
        }
    }

    fn execute_let(action: &LetAction, state: &mut AikiState) -> Result<ActionResult> {
        // Parse the let binding: "variable = expression"
        let parts: Vec<&str> = action.let_.splitn(2, '=').collect();
        if parts.len() != 2 {
            return Err(AikiError::InvalidLetSyntax(action.let_.to_string()));
        }

        let variable_name = parts[0].trim();
        let expression = parts[1].trim();

        // Validate variable name
        if !is_valid_flow_identifier(variable_name) {
            return Err(AikiError::InvalidVariableName(variable_name.to_string()));
        }

        debug_log(|| format!("[flows] Let binding: {} = {}", variable_name, expression));

        // Check if this is variable interpolation/aliasing or a function call
        if expression.contains("{{") || expression.starts_with('$') {
            // Mode 2: Variable interpolation ({{var}}) or legacy aliasing ($var)
            Self::execute_let_alias(variable_name, expression, state)
        } else {
            // Mode 1: Function call
            Self::execute_let_function(variable_name, expression, state)
        }
    }

    /// Execute a let binding for variable interpolation: `let desc = {{event.file_paths}}`
    fn execute_let_alias(
        variable_name: &str,
        expression: &str,
        state: &AikiState,
    ) -> Result<ActionResult> {
        // Create variable resolver with consistent variable availability
        let mut resolver = Self::create_resolver(state);

        // Resolve the variable reference
        let value = resolver.resolve(expression)?;

        debug_log(|| format!("[flows] Variable alias: {} = {}", variable_name, value));

        // Return the value as stdout
        Ok(ActionResult {
            success: true,
            exit_code: Some(0),
            stdout: value,
            stderr: String::new(),
        })
    }

    /// Execute a let binding for function call: `let metadata = aiki/core.build_metadata`
    /// Supports `self.function` syntax to reference functions in the current flow
    fn execute_let_function(
        variable_name: &str,
        function_path: &str,
        state: &AikiState,
    ) -> Result<ActionResult> {
        debug_log(|| {
            format!(
                "[flows] Function call: {} = {}",
                variable_name, function_path
            )
        });

        // Handle self.function syntax
        let resolved_path = if function_path.starts_with("self.") {
            // Extract function name from self.function
            let function_name = function_path
                .strip_prefix("self.")
                .expect("BUG: starts_with('self.') check passed but strip_prefix failed");

            // Get current hook name from context
            let hook_name = state.hook_name.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "Cannot use 'self.{}' - no hook context available",
                    function_name
                )
            })?;

            // Convert hook name (e.g., "aiki/core") to module.function
            // Extract module from hook name: aiki/core -> core
            let module = hook_name.split('/').last().unwrap_or(hook_name);
            format!("aiki/{}.{}", module, function_name)
        } else {
            function_path.to_string()
        };

        // Parse function path: namespace/module.function
        // For now, we only support aiki/* namespace
        if !resolved_path.starts_with("aiki/") {
            return Err(AikiError::UnsupportedFunctionNamespace(
                resolved_path.to_string(),
            ));
        }

        // Extract module.function part
        let module_function = resolved_path
            .strip_prefix("aiki/")
            .expect("BUG: starts_with('aiki/') check passed but strip_prefix failed");

        // Split into module and function
        let parts: Vec<&str> = module_function.splitn(2, '.').collect();
        if parts.len() != 2 {
            return Err(AikiError::MissingFunction(function_path.to_string()));
        }

        let module = parts[0];
        let function = parts[1];

        // Route to appropriate function
        match (module, function) {
            // ========================================================================
            // Change event functions (unified mutations: write, delete, move)
            // ========================================================================
            ("core", "build_write_metadata") => {
                // build_write_metadata requires change.completed event with Write operation
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::build_write_metadata(event, Some(state))
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "build_write_metadata can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "build_delete_metadata") => {
                // build_delete_metadata requires change.completed event with Delete operation
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::build_delete_metadata(event, Some(state))
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "build_delete_metadata can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "build_move_metadata") => {
                // build_move_metadata requires change.completed event with Move operation
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::build_move_metadata(event, Some(state))
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "build_move_metadata can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "build_human_metadata_change_pre") => {
                // build_human_metadata_change_pre requires change.permission_asked event
                match &state.event {
                    AikiEvent::ChangePermissionAsked(event) => {
                        crate::flows::core::build_human_metadata_change_pre(event, Some(state))
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "build_human_metadata_change_pre can only be called for change.permission_asked events"
                    ))),
                }
            }
            ("core", "build_human_metadata_change_post") => {
                // build_human_metadata_change_post requires change.completed event
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::build_human_metadata_change_post(event, Some(state))
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "build_human_metadata_change_post can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "classify_edits_change") => {
                // classify_edits_change requires change.completed event with Write operation
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::classify_edits_change(event)
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "classify_edits_change can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "prepare_separation_change") => {
                // prepare_separation_change requires change.completed event with Write operation
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::prepare_separation_change(event)
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "prepare_separation_change can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "write_ai_files_change") => {
                // write_ai_files_change requires change.completed event with Write operation
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::write_ai_files_change(event, Some(state))
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "write_ai_files_change can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "restore_original_files_change") => {
                // restore_original_files_change requires change.completed event with Write operation
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::restore_original_files_change(event, Some(state))
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "restore_original_files_change can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "generate_coauthors") => {
                // generate_coauthors requires PrepareCommitMessage event
                let AikiEvent::CommitMessageStarted(event) = &state.event else {
                    return Err(AikiError::Other(anyhow::anyhow!(
                        "generate_coauthors can only be called for commit.message_started events"
                    )));
                };
                crate::flows::core::generate_coauthors(event)
            }
            // ========================================================================
            // Task system functions
            // ========================================================================
            ("core", "task_list_size") => {
                // task_list_size can be called from any event context
                // It reads from the task branch and returns the ready queue size
                // Filtered by agent type to show only tasks visible to this agent
                let agent_type = state.event.agent_type();
                crate::flows::core::task_list_size_for_agent(state.cwd(), &agent_type)
            }
            ("core", "task_in_progress") => {
                // task_in_progress can be called from any event context
                // It reads from the task branch and returns in-progress task IDs
                crate::flows::core::task_in_progress(state.cwd())
            }
            // ========================================================================
            // Workspace isolation functions
            // ========================================================================
            ("core", "workspace_ensure_isolated") => {
                let session = extract_session(&state.event)?;
                crate::flows::core::workspace_ensure_isolated(session, state.cwd())
            }
            ("core", "workspace_absorb_all") => {
                let session = extract_session(&state.event)?;
                crate::flows::core::workspace_absorb_all(session)
            }
            _ => Err(AikiError::FunctionNotFoundInNamespace(
                function.to_string(),
                module.to_string(),
            )),
        }
    }

    /// Execute a call action: `call: self.write_ai_files`
    /// This is like execute_let_function but doesn't store the result in a variable
    fn execute_call(action: &CallAction, state: &mut AikiState) -> Result<ActionResult> {
        let function_path = &action.call;

        debug_log(|| format!("[flows] Call action: {}", function_path));

        // Resolve function path relative to current hook context.
        // "call: self.foo" resolves to "aiki/<module>.foo"
        // "call: aiki/core.foo" is used as-is (fully qualified)
        // Bare names (e.g., "call: foo") are rejected — must use self. or full namespace
        let resolved_path = if function_path.starts_with("aiki/") {
            // Already fully qualified
            function_path.to_string()
        } else if function_path.starts_with("self.") {
            // Strip "self." prefix to get the bare function name
            let function_name = function_path
                .strip_prefix("self.")
                .expect("BUG: starts_with(\"self.\") check passed but strip_prefix failed");

            // Get current hook name from state
            let hook_name = state.hook_name.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "Cannot use 'self.{}' - no hook context available",
                    function_name
                )
            })?;

            // Convert hook name (e.g., "aiki/core") to module.function
            // Extract module from hook name: aiki/core -> core
            let module = hook_name.split('/').last().unwrap_or(hook_name);
            format!("aiki/{}.{}", module, function_name)
        } else {
            // Bare names are rejected — must use self. or full namespace (aiki/...)
            return Err(AikiError::InvalidFunctionPath(function_path.to_string()));
        };

        // Parse function path: namespace/module.function
        // For now, we only support aiki/* namespace
        if !resolved_path.starts_with("aiki/") {
            return Err(AikiError::UnsupportedFunctionNamespace(
                resolved_path.to_string(),
            ));
        }

        // Extract module.function part
        let module_function = resolved_path
            .strip_prefix("aiki/")
            .expect("BUG: starts_with('aiki/') check passed but strip_prefix failed");

        // Split into module and function
        let parts: Vec<&str> = module_function.splitn(2, '.').collect();
        if parts.len() != 2 {
            return Err(AikiError::MissingFunction(function_path.to_string()));
        }

        let module = parts[0];
        let function = parts[1];

        // Route to appropriate function (same routing as execute_let_function)
        match (module, function) {
            // ========================================================================
            // Change event functions (unified mutations: write, delete, move)
            // ========================================================================
            ("core", "build_write_metadata") => {
                // build_write_metadata requires change.completed event with Write operation
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::build_write_metadata(event, Some(state))
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "build_write_metadata can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "build_delete_metadata") => {
                // build_delete_metadata requires change.completed event with Delete operation
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::build_delete_metadata(event, Some(state))
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "build_delete_metadata can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "build_move_metadata") => {
                // build_move_metadata requires change.completed event with Move operation
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::build_move_metadata(event, Some(state))
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "build_move_metadata can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "build_human_metadata_change_pre") => {
                // build_human_metadata_change_pre requires change.permission_asked event
                match &state.event {
                    AikiEvent::ChangePermissionAsked(event) => {
                        crate::flows::core::build_human_metadata_change_pre(event, Some(state))
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "build_human_metadata_change_pre can only be called for change.permission_asked events"
                    ))),
                }
            }
            ("core", "build_human_metadata_change_post") => {
                // build_human_metadata_change_post requires change.completed event
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::build_human_metadata_change_post(event, Some(state))
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "build_human_metadata_change_post can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "classify_edits_change") => {
                // classify_edits_change requires change.completed event with Write operation
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::classify_edits_change(event)
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "classify_edits_change can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "prepare_separation_change") => {
                // prepare_separation_change requires change.completed event with Write operation
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::prepare_separation_change(event)
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "prepare_separation_change can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "write_ai_files_change") => {
                // write_ai_files_change requires change.completed event with Write operation
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::write_ai_files_change(event, Some(state))
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "write_ai_files_change can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "restore_original_files_change") => {
                // restore_original_files_change requires change.completed event with Write operation
                match &state.event {
                    AikiEvent::ChangeCompleted(event) => {
                        crate::flows::core::restore_original_files_change(event, Some(state))
                    }
                    _ => Err(AikiError::Other(anyhow::anyhow!(
                        "restore_original_files_change can only be called for change.completed events"
                    ))),
                }
            }
            ("core", "generate_coauthors") => {
                // generate_coauthors requires PrepareCommitMessage event
                let AikiEvent::CommitMessageStarted(event) = &state.event else {
                    return Err(AikiError::Other(anyhow::anyhow!(
                        "generate_coauthors can only be called for commit.message_started events"
                    )));
                };
                crate::flows::core::generate_coauthors(event)
            }
            // ========================================================================
            // Task system functions
            // ========================================================================
            ("core", "task_list_size") => {
                // task_list_size can be called from any event context
                // Filtered by agent type to show only tasks visible to this agent
                let agent_type = state.event.agent_type();
                crate::flows::core::task_list_size_for_agent(state.cwd(), &agent_type)
            }
            ("core", "task_in_progress") => {
                // task_in_progress can be called from any event context
                crate::flows::core::task_in_progress(state.cwd())
            }
            // ========================================================================
            // Workspace isolation functions
            // ========================================================================
            ("core", "workspace_ensure_isolated") => {
                let session = extract_session(&state.event)?;
                crate::flows::core::workspace_ensure_isolated(session, state.cwd())
            }
            ("core", "workspace_absorb_all") => {
                let session = extract_session(&state.event)?;
                crate::flows::core::workspace_absorb_all(session)
            }
            _ => Err(AikiError::FunctionNotFoundInNamespace(
                function.to_string(),
                module.to_string(),
            )),
        }
    }
}

/// Extract the session reference from an AikiEvent.
///
/// Most events carry a session field. Returns an error for events
/// that don't have one (TaskStarted, TaskClosed, Unsupported).
fn extract_session(event: &AikiEvent) -> Result<&crate::session::AikiSession> {
    match event {
        AikiEvent::SessionStarted(e) => Ok(&e.session),
        AikiEvent::SessionResumed(e) => Ok(&e.session),
        AikiEvent::SessionEnded(e) => Ok(&e.session),
        AikiEvent::TurnStarted(e) => Ok(&e.session),
        AikiEvent::TurnCompleted(e) => Ok(&e.session),
        AikiEvent::ReadPermissionAsked(e) => Ok(&e.session),
        AikiEvent::ReadCompleted(e) => Ok(&e.session),
        AikiEvent::ChangePermissionAsked(e) => Ok(&e.session),
        AikiEvent::ChangeCompleted(e) => Ok(&e.session),
        AikiEvent::ShellPermissionAsked(e) => Ok(&e.session),
        AikiEvent::ShellCompleted(e) => Ok(&e.session),
        AikiEvent::WebPermissionAsked(e) => Ok(&e.session),
        AikiEvent::WebCompleted(e) => Ok(&e.session),
        AikiEvent::McpPermissionAsked(e) => Ok(&e.session),
        AikiEvent::McpCompleted(e) => Ok(&e.session),
        AikiEvent::ModelChanged(e) => Ok(&e.session),
        AikiEvent::RepoChanged(e) => Ok(&e.session),
        _ => Err(AikiError::Other(anyhow::anyhow!(
            "workspace functions require an event with a session"
        ))),
    }
}

/// Parse timeout string (e.g., "30s", "1m", "2h")
fn parse_timeout(timeout_str: &str) -> Result<Duration> {
    let timeout_str = timeout_str.trim();

    if let Some(seconds_str) = timeout_str.strip_suffix('s') {
        let seconds: u64 = seconds_str.parse().context("Invalid timeout value")?;
        Ok(Duration::from_secs(seconds))
    } else if let Some(minutes_str) = timeout_str.strip_suffix('m') {
        let minutes: u64 = minutes_str.parse().context("Invalid timeout value")?;
        Ok(Duration::from_secs(minutes * 60))
    } else if let Some(hours_str) = timeout_str.strip_suffix('h') {
        let hours: u64 = hours_str.parse().context("Invalid timeout value")?;
        Ok(Duration::from_secs(hours * 3600))
    } else {
        Err(AikiError::InvalidTimeoutFormat(timeout_str.to_string()))
    }
}

/// Parse author string in "Name <email>" format
/// Returns (Some(name), Some(email)) or (None, None) on parse error
fn parse_author(author: &str) -> Result<(Option<String>, Option<String>)> {
    let author = author.trim();

    // Parse "Name <email>" format
    if let Some(email_start) = author.find('<') {
        if let Some(email_end) = author.find('>') {
            if email_start < email_end {
                let name = author[..email_start].trim().to_string();
                let email = author[email_start + 1..email_end].trim().to_string();
                return Ok((Some(name), Some(email)));
            }
        }
    }

    Err(AikiError::Other(anyhow::anyhow!(
        "Invalid author format '{}'. Expected 'Name <email>'",
        author
    )))
}

/// Execute command with timeout using direct argv and optional environment variables
fn execute_with_timeout_argv_with_env(
    program: &str,
    args: &[String],
    cwd: &std::path::Path,
    timeout: Duration,
    jj_user: Option<String>,
    jj_email: Option<String>,
) -> Result<std::process::Output> {
    use std::panic;
    use std::sync::mpsc;
    use std::thread;

    let cwd = cwd.to_path_buf();
    let program = program.to_string();
    let args = args.to_vec();

    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        // Catch panics in command execution to prevent poisoning
        let result = panic::catch_unwind(|| {
            let mut cmd = Command::new(&program);
            cmd.args(&args).current_dir(&cwd);

            // Set JJ_USER and JJ_EMAIL if provided
            if let Some(user) = jj_user {
                cmd.env("JJ_USER", user);
            }
            if let Some(email) = jj_email {
                cmd.env("JJ_EMAIL", email);
            }

            cmd.output()
        });

        // Send result or error - channel will be dropped if recv already timed out
        let output_result = match result {
            Ok(output_result) => output_result,
            Err(panic_err) => {
                eprintln!("PANIC in command execution thread: {:?}", panic_err);
                Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Command execution thread panicked",
                ))
            }
        };
        let _ = tx.send(output_result);
    });

    Ok(rx
        .recv_timeout(timeout)
        .context("Command timed out")?
        .context("Failed to execute command")?)
}

/// Execute command with timeout (legacy shell-based version)
fn execute_with_timeout(
    command: &str,
    cwd: &std::path::Path,
    timeout: Duration,
) -> Result<std::process::Output> {
    use std::panic;
    use std::sync::mpsc;
    use std::thread;

    let cwd = cwd.to_path_buf();
    let command = command.to_string();

    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        // Catch panics in command execution to prevent poisoning
        let result = panic::catch_unwind(|| {
            Command::new("sh")
                .arg("-c")
                .arg(&command)
                .current_dir(&cwd)
                .output()
        });

        // Send result or error - channel will be dropped if recv already timed out
        let output_result = match result {
            Ok(output_result) => output_result,
            Err(panic_err) => {
                eprintln!("PANIC in shell command execution thread: {:?}", panic_err);
                Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Shell command execution thread panicked",
                ))
            }
        };
        let _ = tx.send(output_result);
    });

    Ok(rx
        .recv_timeout(timeout)
        .context("Command timed out")?
        .context("Failed to execute command")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{
        AikiChangeCompletedPayload, AikiTaskClosedPayload, ChangeOperation, TaskEventPayload,
        WriteOperation,
    };
    use crate::provenance::record::AgentType;
    use crate::session::{AikiSession, SessionMode};

    // Helper to create a simple test event
    fn create_test_event() -> AikiEvent {
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session".to_string(),
            None::<&str>,
            crate::provenance::DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        AikiEvent::ChangeCompleted(AikiChangeCompletedPayload {
            session,
            cwd: std::path::PathBuf::from("/tmp"),
            timestamp: chrono::Utc::now(),
            tool_name: "Edit".to_string(),
            success: true,
            turn: crate::events::Turn::unknown(),
            operation: ChangeOperation::Write(WriteOperation {
                file_paths: vec!["/tmp/file.rs".to_string()],
                edit_details: vec![],
            }),
        })
    }

    // Helper to create a test event with custom file_path
    fn create_test_event_with_file(file_path: &str) -> AikiEvent {
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session".to_string(),
            None::<&str>,
            crate::provenance::DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        AikiEvent::ChangeCompleted(AikiChangeCompletedPayload {
            session,
            cwd: std::path::PathBuf::from("/tmp"),
            timestamp: chrono::Utc::now(),
            tool_name: "Edit".to_string(),
            success: true,
            turn: crate::events::Turn::unknown(),
            operation: ChangeOperation::Write(WriteOperation {
                file_paths: vec![file_path.to_string()],
                edit_details: vec![],
            }),
        })
    }

    fn create_session_resumed_event() -> AikiEvent {
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session".to_string(),
            None::<&str>,
            crate::provenance::DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        AikiEvent::SessionResumed(crate::events::AikiSessionResumedPayload {
            session,
            cwd: std::path::PathBuf::from("/tmp"),
            timestamp: chrono::Utc::now(),
        })
    }

    fn create_session_cleared_event() -> AikiEvent {
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session".to_string(),
            None::<&str>,
            crate::provenance::DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        AikiEvent::SessionCleared(crate::events::AikiSessionClearedPayload {
            session,
            cwd: std::path::PathBuf::from("/tmp"),
            timestamp: chrono::Utc::now(),
        })
    }

    // Helper function for tests that still use Action lists (wraps them in HookStatements)
    fn execute_actions(actions: &[Action], state: &mut AikiState) -> Result<HookOutcome> {
        let statements: Vec<HookStatement> = actions
            .iter()
            .map(|action| HookStatement::Action(action.clone()))
            .collect();
        HookEngine::execute_statements(&statements, state)
    }

    #[test]
    fn test_parse_timeout_seconds() {
        assert_eq!(parse_timeout("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_timeout("1s").unwrap(), Duration::from_secs(1));
    }

    #[test]
    fn test_parse_timeout_minutes() {
        assert_eq!(parse_timeout("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_timeout("1m").unwrap(), Duration::from_secs(60));
    }

    #[test]
    fn test_parse_timeout_hours() {
        assert_eq!(parse_timeout("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_timeout("2h").unwrap(), Duration::from_secs(7200));
    }

    #[test]
    fn test_parse_timeout_invalid() {
        assert!(parse_timeout("30").is_err());
        assert!(parse_timeout("abc").is_err());
        assert!(parse_timeout("30x").is_err());
    }

    #[test]
    fn test_execute_log_action() {
        let action = LogAction {
            log: "Test message".to_string(),
            alias: None,
        };

        let mut state = AikiState::new(create_test_event());

        let result = HookEngine::execute_log(&action, &mut state).unwrap();
        assert!(result.success);
    }

    #[test]
    fn test_execute_log_with_variables() {
        let action = LogAction {
            log: "File: {{event.file_paths}}".to_string(),
            alias: None,
        };

        let mut state = AikiState::new(create_test_event_with_file("test.rs"));

        let result = HookEngine::execute_log(&action, &mut state).unwrap();
        assert!(result.success);
    }

    #[test]
    fn test_execute_shell_echo() {
        let action = ShellAction {
            shell: "echo 'test'".to_string(),
            timeout: None,
            on_failure: OnFailure::default(),
            alias: None,
        };

        let mut state = AikiState::new(create_test_event());

        let result = HookEngine::execute_shell(&action, &mut state).unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("test"));
    }

    #[test]
    fn test_execute_shell_with_variables() {
        let action = ShellAction {
            shell: "echo {{event.file_paths}}".to_string(),
            timeout: None,
            on_failure: OnFailure::default(),
            alias: None,
        };

        let mut state = AikiState::new(create_test_event_with_file("test.rs"));

        let result = HookEngine::execute_shell(&action, &mut state).unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("test.rs"));
    }

    #[test]
    fn test_execute_actions_sequential() {
        let actions = vec![
            Action::Log(LogAction {
                log: "Step 1".to_string(),
                alias: None,
            }),
            Action::Shell(ShellAction {
                shell: "echo 'Step 2'".to_string(),
                timeout: None,
                on_failure: OnFailure::default(),
                alias: None,
            }),
            Action::Log(LogAction {
                log: "Step 3".to_string(),
                alias: None,
            }),
        ];

        let mut state = AikiState::new(create_test_event());

        let result = execute_actions(&actions, &mut state).unwrap();
        assert!(matches!(result, HookOutcome::Success));
    }

    #[test]
    fn test_execute_actions_fail_mode_continue() {
        let actions = vec![
            Action::Shell(ShellAction {
                shell: "false".to_string(), // This command fails
                timeout: None,
                on_failure: OnFailure::default(), // But we continue
                alias: None,
            }),
            Action::Log(LogAction {
                log: "This should still run".to_string(),
                alias: None,
            }),
        ];

        let mut state = AikiState::new(create_test_event());

        let result = execute_actions(&actions, &mut state).unwrap();
        // Should return FailedContinue since first action failed but flow continued
        assert!(matches!(result, HookOutcome::FailedContinue));
    }

    #[test]
    fn test_execute_actions_fail_mode_stop() {
        let actions = vec![
            Action::Shell(ShellAction {
                shell: "false".to_string(), // This command fails
                timeout: None,
                on_failure: OnFailure::Statements(vec![HookStatement::Action(Action::Stop(
                    crate::flows::types::StopAction {
                        failure: "Action failed".to_string(),
                    },
                ))]), // Stop on failure
                alias: None,
            }),
            Action::Log(LogAction {
                log: "This should NOT run".to_string(),
                alias: None,
            }),
        ];

        let event = create_test_event();
        let mut state = AikiState::new(event);

        let result = execute_actions(&actions, &mut state).unwrap();
        // Should return FailedStop since action failed with on_failure: stop
        assert!(matches!(result, HookOutcome::FailedStop));
    }

    #[test]
    fn test_is_valid_variable_name() {
        // Valid names
        assert!(is_valid_flow_identifier("description"));
        assert!(is_valid_flow_identifier("desc"));
        assert!(is_valid_flow_identifier("_private"));
        assert!(is_valid_flow_identifier("var123"));
        assert!(is_valid_flow_identifier("my_var"));
        assert!(is_valid_flow_identifier("CamelCase"));

        // Invalid names
        assert!(!is_valid_flow_identifier(""));
        assert!(!is_valid_flow_identifier("123var")); // starts with number
        assert!(!is_valid_flow_identifier("my-var")); // contains hyphen
        assert!(!is_valid_flow_identifier("my.var")); // contains dot
        assert!(!is_valid_flow_identifier("my var")); // contains space
        assert!(!is_valid_flow_identifier("$var")); // starts with $
    }

    #[test]
    fn test_execute_let_variable_aliasing() {
        let action = LetAction {
            let_: "desc = {{event.file_paths}}".to_string(),
            on_failure: OnFailure::default(),
        };

        let mut state = AikiState::new(create_test_event_with_file("test.rs"));

        let result = HookEngine::execute_let(&action, &mut state).unwrap();
        assert!(result.success);
        assert_eq!(result.stdout, "test.rs");
    }

    #[test]
    fn test_execute_let_invalid_syntax() {
        let action = LetAction {
            let_: "invalid_syntax".to_string(), // Missing '='
            on_failure: OnFailure::default(),
        };

        let event = create_test_event();
        let mut state = AikiState::new(event);

        let result = HookEngine::execute_let(&action, &mut state);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid let syntax"));
    }

    #[test]
    fn test_execute_let_invalid_variable_names() {
        let invalid_names = vec![
            "123var = value", // starts with number
            "my-var = value", // contains hyphen
            "my.var = value", // contains dot
            "my var = value", // contains space
            "$var = value",   // starts with $
            " = value",       // empty name
        ];

        for let_str in invalid_names {
            let action = LetAction {
                let_: let_str.to_string(),
                on_failure: OnFailure::default(),
            };

            let event = create_test_event();
            let mut state = AikiState::new(event);

            let result = HookEngine::execute_let(&action, &mut state);
            assert!(result.is_err(), "Should reject: {}", let_str);
            assert!(
                result.unwrap_err().to_string().contains("Invalid variable"),
                "Should mention invalid variable for: {}",
                let_str
            );
        }
    }

    #[test]
    fn test_execute_let_whitespace_trimming() {
        let action = LetAction {
            let_: "  description  =  {{event.file_paths}}  ".to_string(),
            on_failure: OnFailure::default(),
        };

        let mut state = AikiState::new(create_test_event_with_file("test.rs"));

        let result = HookEngine::execute_let(&action, &mut state).unwrap();
        assert!(result.success);
        assert_eq!(result.stdout, "test.rs");
    }

    #[test]
    fn test_let_variable_storage() {
        let actions = vec![
            Action::Let(LetAction {
                let_: "desc = {{event.file_paths}}".to_string(),
                on_failure: OnFailure::default(),
            }),
            Action::Log(LogAction {
                log: "Variable: {{desc}}".to_string(),
                alias: None,
            }),
        ];

        let mut state = AikiState::new(create_test_event_with_file("test.rs"));

        let result = execute_actions(&actions, &mut state).unwrap();
        assert!(matches!(result, HookOutcome::Success));

        // Check that the variable was stored
        assert_eq!(state.get_variable("desc"), Some(&"test.rs".to_string()));
    }

    #[test]
    fn test_shell_alias_stores_structured_metadata() {
        let actions = vec![Action::Shell(ShellAction {
            shell: "echo 'test output'".to_string(),
            timeout: None,
            on_failure: OnFailure::default(),
            alias: Some("result".to_string()),
        })];

        let event = create_test_event();
        let mut state = AikiState::new(event);

        let result = execute_actions(&actions, &mut state).unwrap();
        assert!(matches!(result, HookOutcome::Success));

        // Check that the variable was stored
        assert!(state.get_variable("result").is_some());
        assert!(state
            .get_variable("result")
            .unwrap()
            .contains("test output"));

        // Check that structured metadata was stored
        assert!(state.get_metadata("result").is_some());
        assert!(state.get_metadata("result").unwrap().success);
    }

    #[test]
    fn test_let_creates_structured_metadata() {
        let actions = vec![Action::Let(LetAction {
            let_: "desc = {{event.file_paths}}".to_string(),
            on_failure: OnFailure::default(),
        })];

        let mut state = AikiState::new(create_test_event_with_file("test.rs"));

        let _result = execute_actions(&actions, &mut state).unwrap();

        // Check that structured metadata was stored
        assert!(state.get_metadata("desc").is_some());
        let metadata = state.get_metadata("desc").unwrap();
        assert!(metadata.success);
        assert_eq!(metadata.stdout, "test.rs");
    }

    #[test]
    fn test_actions_without_alias_dont_store_variables() {
        let actions = vec![Action::Shell(ShellAction {
            shell: "echo 'test'".to_string(),
            timeout: None,
            on_failure: OnFailure::default(),
            alias: None, // No alias
        })];

        let event = create_test_event();
        let mut state = AikiState::new(event);

        let _result = execute_actions(&actions, &mut state).unwrap();

        // Check that no extra variables were stored (except for any built-ins)
        // The metadata should be empty since no alias was provided
        #[cfg(test)]
        {
            assert!(state.get_metadata("result").is_none());
        }
    }

    #[test]
    fn test_let_with_context_vars() {
        // This test verifies that build_write_metadata works with typed events.
        // The type system now guarantees that change.completed events have all required fields.
        let action = LetAction {
            let_: "metadata = aiki/core.build_write_metadata".to_string(),
            on_failure: OnFailure::default(),
        };

        let event = create_test_event();
        let mut state = AikiState::new(event);
        state.hook_name = Some("aiki/core".to_string());

        // This should succeed because ChangeCompletedPayload has session and tool_name
        let result = HookEngine::execute_let(&action, &mut state).unwrap();
        assert!(result.success);
        // Result is JSON with author and message fields
        assert!(result.stdout.contains("author"));
        assert!(result.stdout.contains("message"));
    }

    #[test]
    fn test_let_creates_copy_not_reference() {
        // Verify aliasing behavior creates copies
        let actions = vec![
            Action::Let(LetAction {
                let_: "original = {{event.file_paths}}".to_string(),
                on_failure: OnFailure::default(),
            }),
            Action::Let(LetAction {
                let_: "copy = {{original}}".to_string(),
                on_failure: OnFailure::default(),
            }),
        ];

        let mut state = AikiState::new(create_test_event());

        let _result = execute_actions(&actions, &mut state).unwrap();

        // Both should have the same value
        assert_eq!(
            state.get_variable("original"),
            Some(&"/tmp/file.rs".to_string())
        );
        assert_eq!(
            state.get_variable("copy"),
            Some(&"/tmp/file.rs".to_string())
        );

        // Modify original
        state.set_variable("original".to_string(), "modified".to_string());

        // Copy should still have original value (it's a copy, not a reference)
        assert_eq!(
            state.get_variable("copy"),
            Some(&"/tmp/file.rs".to_string())
        );
        assert_eq!(
            state.get_variable("original"),
            Some(&"modified".to_string())
        );
    }

    #[test]
    fn test_let_variable_shadowing() {
        // Verify that reassigning variables works correctly
        let actions = vec![
            Action::Let(LetAction {
                let_: "x = {{event.tool_name}}".to_string(),
                on_failure: OnFailure::default(),
            }),
            Action::Let(LetAction {
                let_: "x = {{event.session.external_id}}".to_string(),
                on_failure: OnFailure::default(),
            }),
        ];

        let mut state = AikiState::new(create_test_event());

        let _result = execute_actions(&actions, &mut state).unwrap();

        // Second assignment should overwrite first
        assert_eq!(state.get_variable("x"), Some(&"test-session".to_string()));
    }

    #[test]
    fn test_let_aliasing_copies_all_structured_metadata() {
        let actions = vec![
            Action::Let(LetAction {
                let_: "file = {{event.file_paths}}".to_string(),
                on_failure: OnFailure::default(),
            }),
            Action::Let(LetAction {
                let_: "copy = {{file}}".to_string(),
                on_failure: OnFailure::default(),
            }),
        ];

        let mut state = AikiState::new(create_test_event_with_file("test.rs"));

        let _result = execute_actions(&actions, &mut state).unwrap();

        // Both should have the value
        assert_eq!(state.get_variable("file"), Some(&"test.rs".to_string()));
        assert_eq!(state.get_variable("copy"), Some(&"test.rs".to_string()));

        // Both should have structured metadata
        assert!(state.get_metadata("file").is_some());
        assert!(state.get_metadata("copy").is_some());
    }

    #[test]
    fn test_let_self_reference() {
        let action = LetAction {
            let_: "metadata = self.build_write_metadata".to_string(),
            on_failure: OnFailure::default(),
        };

        let event = create_test_event();
        let mut state = AikiState::new(event);
        state.hook_name = Some("aiki/core".to_string());

        let result = HookEngine::execute_let(&action, &mut state).unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("author"));
        assert!(result.stdout.contains("message"));
    }

    #[test]
    fn test_let_self_reference_without_flow_context() {
        let action = LetAction {
            let_: "metadata = self.build_write_metadata".to_string(),
            on_failure: OnFailure::default(),
        };

        // No flow_name set
        let event = create_test_event();
        let mut state = AikiState::new(event);

        let result = HookEngine::execute_let(&action, &mut state);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no hook context available"));
    }

    #[test]
    fn test_let_variables_work_in_shell_actions() {
        let actions = vec![
            Action::Let(LetAction {
                let_: "my_var = {{event.file_paths}}".to_string(),
                on_failure: OnFailure::default(),
            }),
            Action::Shell(ShellAction {
                shell: "echo $my_var".to_string(),
                timeout: None,
                on_failure: OnFailure::default(),
                alias: None,
            }),
        ];

        let mut state = AikiState::new(create_test_event_with_file("test.rs"));

        let result = execute_actions(&actions, &mut state).unwrap();
        assert!(matches!(result, HookOutcome::Success));

        // Check that the variable was stored
        assert!(state.get_variable("my_var").is_some());
        assert_eq!(state.get_variable("my_var"), Some(&"test.rs".to_string()));
    }

    #[test]
    fn test_let_variables_work_in_jj_actions() {
        let actions = vec![
            Action::Let(LetAction {
                let_: "msg = {{event.file_paths}}".to_string(),
                on_failure: OnFailure::default(),
            }),
            Action::Jj(JjAction {
                jj: "log -r {{msg}}".to_string(),
                timeout: None,
                on_failure: OnFailure::default(),
                alias: None,
                with_author: None,
                with_author_and_message: None,
            }),
        ];

        let event = create_test_event();
        let mut state = AikiState::new(event);

        let result = execute_actions(&actions, &mut state).unwrap();
        // Should succeed (we don't validate jj commands in tests)
        assert!(matches!(
            result,
            HookOutcome::Success | HookOutcome::FailedContinue
        ));
    }

    #[test]
    fn test_let_variables_work_in_log_actions() {
        let actions = vec![
            Action::Let(LetAction {
                let_: "file = {{event.file_paths}}".to_string(),
                on_failure: OnFailure::default(),
            }),
            Action::Log(LogAction {
                log: "Processing {{file}}".to_string(),
                alias: None,
            }),
        ];

        let mut state = AikiState::new(create_test_event_with_file("test.rs"));

        let result = execute_actions(&actions, &mut state).unwrap();
        assert!(matches!(result, HookOutcome::Success));
    }

    #[test]
    fn test_append_trailer_adds_blank_line_before_first_trailer() {
        let content = "Commit title\n\nCommit body text.";
        let trailer = "Co-authored-by: Test <test@example.com>";

        let result = HookEngine::append_trailer(content, trailer);

        // Should have blank line before trailer
        assert!(
            result.contains("\n\nCo-authored-by:"),
            "Should have blank line before trailer"
        );
        assert_eq!(
            result,
            "Commit title\n\nCommit body text.\n\nCo-authored-by: Test <test@example.com>\n"
        );
    }

    #[test]
    fn test_append_trailer_no_duplicate_blank_line() {
        let content = "Commit title\n\nCommit body text.\n";
        let trailer = "Co-authored-by: Test <test@example.com>";

        let result = HookEngine::append_trailer(content, trailer);

        // Should add blank line since content doesn't end with blank line
        assert!(
            result.contains("text.\n\nCo-authored-by:"),
            "Should have blank line before trailer"
        );
    }

    #[test]
    fn test_append_trailer_to_existing_trailer() {
        let content = "Commit title\n\nCommit body.\n\nSigned-off-by: Author <author@example.com>";
        let trailer = "Co-authored-by: Test <test@example.com>";

        let result = HookEngine::append_trailer(content, trailer);

        // Should NOT add blank line before second trailer (trailers stay together)
        assert!(
            result.contains("Signed-off-by: Author <author@example.com>\nCo-authored-by:"),
            "Should not have blank line between trailers"
        );
    }

    #[test]
    fn test_append_trailer_preserves_existing_blank_line() {
        let content = "Commit title\n\nCommit body text.\n\n";
        let trailer = "Co-authored-by: Test <test@example.com>";

        let result = HookEngine::append_trailer(content, trailer);

        // Should not add another blank line since one already exists
        assert!(
            !result.contains("\n\n\nCo-authored-by:"),
            "Should not have double blank lines"
        );
        assert!(
            result.contains("text.\n\nCo-authored-by:"),
            "Should preserve existing blank line"
        );
    }

    #[test]
    fn test_if_condition_true_executes_then_branch() {
        let statements = vec![
            // Set a variable using log action (which doesn't require function namespace)
            HookStatement::Action(Action::Log(LogAction {
                log: "true".to_string(),
                alias: Some("status".to_string()),
            })),
            // Conditional that should execute then branch
            HookStatement::If(IfStatement {
                condition: "$status == true".to_string(),
                then: vec![HookStatement::Action(Action::Log(LogAction {
                    log: "then branch executed".to_string(),
                    alias: Some("result".to_string()),
                }))],
                else_: None,
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

        assert!(matches!(result, HookOutcome::Success));
        assert_eq!(
            state.get_variable("result"),
            Some(&"then branch executed".to_string())
        );
    }

    #[test]
    fn test_if_condition_false_executes_else_branch() {
        let statements = vec![
            // Set a variable using log action
            HookStatement::Action(Action::Log(LogAction {
                log: "false".to_string(),
                alias: Some("status".to_string()),
            })),
            // Conditional that should execute else branch
            HookStatement::If(IfStatement {
                condition: "$status == true".to_string(),
                then: vec![HookStatement::Action(Action::Log(LogAction {
                    log: "then branch executed".to_string(),
                    alias: Some("result".to_string()),
                }))],
                else_: Some(vec![HookStatement::Action(Action::Log(LogAction {
                    log: "else branch executed".to_string(),
                    alias: Some("result".to_string()),
                }))]),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

        assert!(matches!(result, HookOutcome::Success));
        assert_eq!(
            state.get_variable("result"),
            Some(&"else branch executed".to_string())
        );
    }

    #[test]
    fn test_if_condition_false_no_else_branch() {
        let statements = vec![
            HookStatement::Action(Action::Log(LogAction {
                log: "false".to_string(),
                alias: Some("status".to_string()),
            })),
            HookStatement::If(IfStatement {
                condition: "$status == true".to_string(),
                then: vec![HookStatement::Action(Action::Log(LogAction {
                    log: "then branch executed".to_string(),
                    alias: Some("result".to_string()),
                }))],
                else_: None,
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

        assert!(matches!(result, HookOutcome::Success));
        // result should not be set since neither branch executed
        assert!(state.get_variable("result").is_none());
    }

    #[test]
    fn test_if_json_field_access() {
        let actions = vec![
            // Create JSON variable
            Action::Let(LetAction {
                let_: "detection = aiki/core.classify_edits_change".to_string(),
                on_failure: OnFailure::default(),
            }),
            // Check JSON field (will fail in test since classify_edits_change returns error)
        ];

        let mut state = AikiState::new(create_test_event());
        state.hook_name = Some("aiki/core".to_string());

        let _result = execute_actions(&actions, &mut state).unwrap();

        // This test just verifies syntax doesn't crash
    }

    #[test]
    fn test_if_nested_conditionals() {
        let statements = vec![
            HookStatement::Action(Action::Log(LogAction {
                log: "true".to_string(),
                alias: Some("outer".to_string()),
            })),
            HookStatement::Action(Action::Log(LogAction {
                log: "true".to_string(),
                alias: Some("inner".to_string()),
            })),
            HookStatement::If(IfStatement {
                condition: "$outer == true".to_string(),
                then: vec![HookStatement::If(IfStatement {
                    condition: "$inner == true".to_string(),
                    then: vec![HookStatement::Action(Action::Log(LogAction {
                        log: "nested then executed".to_string(),
                        alias: Some("result".to_string()),
                    }))],
                    else_: None,
                })],
                else_: None,
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

        assert!(matches!(result, HookOutcome::Success));
        assert_eq!(
            state.get_variable("result"),
            Some(&"nested then executed".to_string())
        );
    }

    #[test]
    fn test_evaluate_condition_equality() {
        let mut state = AikiState::new(create_test_event());
        state.store_action_result(
            "test".to_string(),
            ActionResult {
                success: true,
                exit_code: Some(0),
                stdout: "value".to_string(),
                stderr: String::new(),
            },
        );

        // Test equality (right side must be quoted for Rhai)
        assert!(HookEngine::evaluate_condition(r#"$test == "value""#, &mut state).unwrap());
        assert!(!HookEngine::evaluate_condition(r#"$test == "other""#, &mut state).unwrap());

        // Test inequality
        assert!(!HookEngine::evaluate_condition(r#"$test != "value""#, &mut state).unwrap());
        assert!(HookEngine::evaluate_condition(r#"$test != "other""#, &mut state).unwrap());
    }

    #[test]
    fn test_if_condition_truthy_values() {
        let mut state = AikiState::new(create_test_event());

        // Empty string is falsy
        state.store_action_result(
            "empty".to_string(),
            ActionResult {
                success: true,
                exit_code: Some(0),
                stdout: "".to_string(),
                stderr: String::new(),
            },
        );
        assert!(!HookEngine::evaluate_condition("$empty", &mut state).unwrap());

        // Non-empty string is truthy
        state.store_action_result(
            "nonempty".to_string(),
            ActionResult {
                success: true,
                exit_code: Some(0),
                stdout: "some content".to_string(),
                stderr: String::new(),
            },
        );
        assert!(HookEngine::evaluate_condition("$nonempty", &mut state).unwrap());

        // "false" literal is falsy
        state.store_action_result(
            "false_str".to_string(),
            ActionResult {
                success: true,
                exit_code: Some(0),
                stdout: "false".to_string(),
                stderr: String::new(),
            },
        );
        assert!(!HookEngine::evaluate_condition("$false_str", &mut state).unwrap());

        // "true" literal is truthy
        state.store_action_result(
            "true_str".to_string(),
            ActionResult {
                success: true,
                exit_code: Some(0),
                stdout: "true".to_string(),
                stderr: String::new(),
            },
        );
        assert!(HookEngine::evaluate_condition("$true_str", &mut state).unwrap());
    }

    #[test]
    fn test_switch_matches_case() {
        use std::collections::HashMap;

        let mut cases = HashMap::new();
        cases.insert(
            "ExactMatch".to_string(),
            vec![HookStatement::Action(Action::Log(LogAction {
                log: "exact match case".to_string(),
                alias: Some("result".to_string()),
            }))],
        );
        cases.insert(
            "PartialMatch".to_string(),
            vec![HookStatement::Action(Action::Log(LogAction {
                log: "partial match case".to_string(),
                alias: Some("result".to_string()),
            }))],
        );

        let statements = vec![
            HookStatement::Action(Action::Log(LogAction {
                log: "ExactMatch".to_string(),
                alias: Some("status".to_string()),
            })),
            HookStatement::Switch(SwitchStatement {
                expression: "{{status}}".to_string(),
                cases,
                default: None,
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

        assert!(matches!(result, HookOutcome::Success));
        assert_eq!(
            state.get_variable("result"),
            Some(&"exact match case".to_string())
        );
    }

    #[test]
    fn test_switch_uses_default_case() {
        use std::collections::HashMap;

        let mut cases = HashMap::new();
        cases.insert(
            "ExactMatch".to_string(),
            vec![HookStatement::Action(Action::Log(LogAction {
                log: "exact match case".to_string(),
                alias: Some("result".to_string()),
            }))],
        );

        let statements = vec![
            HookStatement::Action(Action::Log(LogAction {
                log: "NoMatch".to_string(),
                alias: Some("status".to_string()),
            })),
            HookStatement::Switch(SwitchStatement {
                expression: "{{status}}".to_string(),
                cases,
                default: Some(vec![HookStatement::Action(Action::Log(LogAction {
                    log: "default case".to_string(),
                    alias: Some("result".to_string()),
                }))]),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

        assert!(matches!(result, HookOutcome::Success));
        assert_eq!(
            state.get_variable("result"),
            Some(&"default case".to_string())
        );
    }

    #[test]
    fn test_switch_no_match_no_default() {
        use std::collections::HashMap;

        let mut cases = HashMap::new();
        cases.insert(
            "ExactMatch".to_string(),
            vec![HookStatement::Action(Action::Log(LogAction {
                log: "exact match case".to_string(),
                alias: Some("result".to_string()),
            }))],
        );

        let statements = vec![
            HookStatement::Action(Action::Log(LogAction {
                log: "NoMatch".to_string(),
                alias: Some("status".to_string()),
            })),
            HookStatement::Switch(SwitchStatement {
                expression: "{{status}}".to_string(),
                cases,
                default: None,
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

        // No match and no default = success (no-op)
        assert!(matches!(result, HookOutcome::Success));
        // result variable should not be set
        assert!(state.get_variable("result").is_none());
    }

    #[test]
    fn test_switch_with_json_field_access() {
        use std::collections::HashMap;

        let mut cases = HashMap::new();
        cases.insert(
            "true".to_string(),
            vec![HookStatement::Action(Action::Log(LogAction {
                log: "all exact match".to_string(),
                alias: Some("result".to_string()),
            }))],
        );
        cases.insert(
            "false".to_string(),
            vec![HookStatement::Action(Action::Log(LogAction {
                log: "not all exact match".to_string(),
                alias: Some("result".to_string()),
            }))],
        );

        // Create a simple JSON object to test field access
        let statements = vec![
            HookStatement::Action(Action::Log(LogAction {
                log: "{\"all_exact_match\": \"true\"}".to_string(),
                alias: Some("detection".to_string()),
            })),
            HookStatement::Switch(SwitchStatement {
                expression: "{{detection.all_exact_match}}".to_string(),
                cases,
                default: None,
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

        assert!(matches!(result, HookOutcome::Success));
        // Note: Variable resolver will parse the JSON and extract the field
        // The actual result depends on the resolver implementation
    }

    #[test]
    fn test_prefilechange_flow_with_jj_diff_output() {
        // Simulate the PreFileChange flow: `jj diff -r @ --name-only` returns file names
        let statements = vec![
            // Simulate jj diff output with files using echo
            HookStatement::Action(Action::Shell(ShellAction {
                shell: "echo 'src/main.rs\nsrc/lib.rs'".to_string(),
                timeout: None,
                on_failure: OnFailure::default(),
                alias: Some("changed_files".to_string()),
            })),
            // If there are changed files (non-empty), execute the then branch
            HookStatement::If(IfStatement {
                condition: "$changed_files".to_string(),
                then: vec![HookStatement::Action(Action::Log(LogAction {
                    log: "User has changes to stash".to_string(),
                    alias: Some("stash_result".to_string()),
                }))],
                else_: Some(vec![HookStatement::Action(Action::Log(LogAction {
                    log: "No changes to stash".to_string(),
                    alias: Some("stash_result".to_string()),
                }))]),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let _result = HookEngine::execute_statements(&statements, &mut state).unwrap();

        // Should execute the then branch because changed_files is non-empty
        assert_eq!(
            state.get_variable("stash_result").unwrap(),
            "User has changes to stash"
        );
    }

    #[test]
    fn test_prefilechange_flow_with_empty_jj_diff() {
        // Simulate jj diff with no changes (empty output)
        let statements = vec![
            // Simulate empty jj diff output using true (which produces no output)
            HookStatement::Action(Action::Shell(ShellAction {
                shell: "true".to_string(), // Exits 0 but produces no output
                timeout: None,
                on_failure: OnFailure::default(),
                alias: Some("changed_files".to_string()),
            })),
            HookStatement::If(IfStatement {
                condition: "$changed_files".to_string(),
                then: vec![HookStatement::Action(Action::Log(LogAction {
                    log: "Should not execute".to_string(),
                    alias: Some("result".to_string()),
                }))],
                else_: Some(vec![HookStatement::Action(Action::Log(LogAction {
                    log: "No changes detected".to_string(),
                    alias: Some("result".to_string()),
                }))]),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let _result = HookEngine::execute_statements(&statements, &mut state).unwrap();

        // Should execute the else branch because changed_files is empty
        assert_eq!(state.get_variable("result").unwrap(), "No changes detected");
    }

    #[test]
    fn test_shell_action_error_with_continue() {
        // Test that shell action with on_failure: continue allows flow to proceed
        let actions = vec![
            Action::Shell(ShellAction {
                shell: "false".to_string(), // Fails
                timeout: None,
                on_failure: OnFailure::default(),
                alias: None,
            }),
            Action::Log(LogAction {
                log: "Still executed".to_string(),
                alias: Some("result".to_string()),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedContinue
        assert!(matches!(result, HookOutcome::FailedContinue));

        // Second action should still execute
        assert_eq!(
            state.get_variable("result"),
            Some(&"Still executed".to_string())
        );
    }

    #[test]
    fn test_shell_action_error_with_stop() {
        // Test that shell action with on_failure: stop halts execution
        let actions = vec![
            Action::Shell(ShellAction {
                shell: "false".to_string(), // Fails
                timeout: None,
                on_failure: OnFailure::Statements(vec![HookStatement::Action(Action::Stop(
                    crate::flows::types::StopAction {
                        failure: "Shell command failed".to_string(),
                    },
                ))]),
                alias: None,
            }),
            Action::Log(LogAction {
                log: "Should not execute".to_string(),
                alias: Some("result".to_string()),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedStop
        assert!(matches!(result, HookOutcome::FailedStop));

        // Second action should NOT execute
        assert!(state.get_variable("result").is_none());
    }

    #[test]
    fn test_shell_action_error_with_block() {
        // Test that shell action with on_failure: block halts execution
        let actions = vec![
            Action::Shell(ShellAction {
                shell: "false".to_string(), // Fails
                timeout: None,
                on_failure: OnFailure::Statements(vec![HookStatement::Action(Action::Block(
                    crate::flows::types::BlockAction {
                        failure: "Action failed with block".to_string(),
                    },
                ))]),
                alias: None,
            }),
            Action::Log(LogAction {
                log: "Should not execute".to_string(),
                alias: Some("result".to_string()),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedBlock
        assert!(matches!(result, HookOutcome::FailedBlock));

        // Second action should NOT execute
        assert!(state.get_variable("result").is_none());
    }

    #[test]
    fn test_self_function_error_propagation() {
        use crate::events::{AikiEvent, AikiTurnStartedPayload};

        // Create TurnStarted event
        let session = AikiSession::new(
            crate::provenance::AgentType::ClaudeCode,
            "test-session".to_string(),
            None::<&str>,
            crate::provenance::DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        let event = AikiEvent::TurnStarted(AikiTurnStartedPayload {
            session,
            prompt: "test".to_string(),
            timestamp: chrono::Utc::now(),
            cwd: std::path::PathBuf::from("/tmp"),
            turn: crate::events::Turn::unknown(),
            injected_refs: vec![],
        });

        // Create action that calls a self.* function that doesn't exist
        let actions = vec![Action::Call(CallAction {
            call: "self.nonexistent_function".to_string(),
            on_failure: OnFailure::default(),
        })];

        let mut state = AikiState::new(event);
        state.hook_name = Some("aiki/core".to_string());

        let result = execute_actions(&actions, &mut state);

        // Should fail
        assert!(result.is_err());
    }

    #[test]
    fn test_context_action_allowed_for_session_resumed() {
        let action = Action::Context(ContextAction {
            context: crate::flows::types::ContextContent::Simple("resumed context".to_string()),
            on_failure: OnFailure::default(),
        });

        let mut state = AikiState::new(create_session_resumed_event());
        let result = execute_actions(&[action], &mut state).unwrap();

        assert!(matches!(result, HookOutcome::Success));
        assert_eq!(
            state.build_context_with_original_prompt().as_deref(),
            Some("resumed context")
        );
    }

    #[test]
    fn test_context_action_allowed_for_session_cleared() {
        let action = Action::Context(ContextAction {
            context: crate::flows::types::ContextContent::Simple("cleared context".to_string()),
            on_failure: OnFailure::default(),
        });

        let mut state = AikiState::new(create_session_cleared_event());
        let result = execute_actions(&[action], &mut state).unwrap();

        assert!(matches!(result, HookOutcome::Success));
        assert_eq!(
            state.build_context_with_original_prompt().as_deref(),
            Some("cleared context")
        );
    }

    #[test]
    fn test_standalone_stop_action() {
        // Test that a standalone stop action halts execution
        let actions = vec![
            Action::Log(LogAction {
                log: "Before stop".to_string(),
                alias: Some("before".to_string()),
            }),
            Action::Stop(crate::flows::types::StopAction {
                failure: "Stopping execution".to_string(),
            }),
            Action::Log(LogAction {
                log: "After stop - should not execute".to_string(),
                alias: Some("after".to_string()),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedStop
        assert!(matches!(result, HookOutcome::FailedStop));

        // First action should execute
        assert_eq!(
            state.get_variable("before"),
            Some(&"Before stop".to_string())
        );

        // Action after stop should NOT execute
        assert!(state.get_variable("after").is_none());

        // Failure message should be added
        assert!(state
            .failures()
            .iter()
            .any(|f| f.0.contains("Stopping execution")));
    }

    #[test]
    fn test_standalone_block_action() {
        // Test that a standalone block action halts execution
        let actions = vec![
            Action::Log(LogAction {
                log: "Before block".to_string(),
                alias: Some("before".to_string()),
            }),
            Action::Block(crate::flows::types::BlockAction {
                failure: "Blocking execution".to_string(),
            }),
            Action::Log(LogAction {
                log: "After block - should not execute".to_string(),
                alias: Some("after".to_string()),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedBlock
        assert!(matches!(result, HookOutcome::FailedBlock));

        // First action should execute
        assert_eq!(
            state.get_variable("before"),
            Some(&"Before block".to_string())
        );

        // Action after block should NOT execute
        assert!(state.get_variable("after").is_none());

        // Failure message should be added
        assert!(state
            .failures()
            .iter()
            .any(|f| f.0.contains("Blocking execution")));
    }

    #[test]
    fn test_standalone_continue_action() {
        // Test that a standalone continue action does not halt execution
        let actions = vec![
            Action::Log(LogAction {
                log: "Before continue".to_string(),
                alias: Some("before".to_string()),
            }),
            Action::Continue(crate::flows::types::ContinueAction {
                failure: "Continuing execution".to_string(),
            }),
            Action::Log(LogAction {
                log: "After continue - should execute".to_string(),
                alias: Some("after".to_string()),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedContinue (standalone continue action records a failure)
        assert!(matches!(result, HookOutcome::FailedContinue));

        // Both actions should execute
        assert_eq!(
            state.get_variable("before"),
            Some(&"Before continue".to_string())
        );
        assert_eq!(
            state.get_variable("after"),
            Some(&"After continue - should execute".to_string())
        );

        // Failure message should be added
        assert!(state
            .failures()
            .iter()
            .any(|f| f.0.contains("Continuing execution")));
    }

    #[test]
    fn test_nested_on_failure_handlers() {
        // Test that nested on_failure handlers are executed correctly
        let actions = vec![
            Action::Shell(ShellAction {
                shell: "false".to_string(), // This fails
                timeout: None,
                on_failure: OnFailure::Statements(vec![HookStatement::Action(Action::Shell(
                    ShellAction {
                        shell: "false".to_string(), // This also fails
                        timeout: None,
                        on_failure: OnFailure::Statements(vec![HookStatement::Action(
                            Action::Block(crate::flows::types::BlockAction {
                                failure: "Nested failure handler executed".to_string(),
                            }),
                        )]),
                        alias: None,
                    },
                ))]),
                alias: None,
            }),
            Action::Log(LogAction {
                log: "Should not execute".to_string(),
                alias: Some("after".to_string()),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedBlock from nested handler
        assert!(matches!(result, HookOutcome::FailedBlock));

        // Action after first shell should NOT execute
        assert!(state.get_variable("after").is_none());

        // Failure message from nested handler should be added
        assert!(state
            .failures()
            .iter()
            .any(|f| f.0.contains("Nested failure handler")));
    }

    #[test]
    fn test_empty_flow_control_messages() {
        // Test that empty messages now generate default messages
        let actions = vec![
            Action::Continue(crate::flows::types::ContinueAction {
                failure: "".to_string(), // Empty message
            }),
            Action::Stop(crate::flows::types::StopAction {
                failure: "".to_string(), // Empty message
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedStop from the stop action
        assert!(matches!(result, HookOutcome::FailedStop));

        // Default messages should be added for both actions
        assert_eq!(state.failures().len(), 2);
        assert!(state
            .failures()
            .iter()
            .any(|f| f.0.contains("no message provided")));
    }

    #[test]
    fn test_empty_block_message() {
        // Test that empty block messages now generate default messages
        let actions = vec![Action::Block(crate::flows::types::BlockAction {
            failure: "".to_string(), // Empty message
        })];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedBlock
        assert!(matches!(result, HookOutcome::FailedBlock));

        // Default message should be added
        assert_eq!(state.failures().len(), 1);
        assert!(state
            .failures()
            .iter()
            .any(|f| f.0.contains("no message provided")));
    }

    #[test]
    fn test_on_failure_with_block_simple() {
        // Simple test: shell fails, on_failure has block
        let actions = vec![Action::Shell(ShellAction {
            shell: "false".to_string(),
            timeout: None,
            on_failure: OnFailure::Statements(vec![HookStatement::Action(Action::Block(
                crate::flows::types::BlockAction {
                    failure: "Blocking after failure".to_string(),
                },
            ))]),
            alias: None,
        })];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        eprintln!("Simple block result: {:?}", result);
        eprintln!("Failures: {:?}", state.failures());

        // Should return FailedBlock
        assert!(matches!(result, HookOutcome::FailedBlock));
    }

    #[test]
    fn test_nested_action_on_failure_in_if_branch() {
        // Test that actions nested inside if branches execute their own on_failure handlers
        let statements = vec![
            HookStatement::Action(Action::Log(LogAction {
                log: "true".to_string(),
                alias: Some("condition".to_string()),
            })),
            HookStatement::If(IfStatement {
                condition: "$condition == true".to_string(),
                then: vec![
                    // This shell action should fail and trigger its own on_failure (continue)
                    HookStatement::Action(Action::Shell(ShellAction {
                        shell: "false".to_string(),
                        timeout: None,
                        on_failure: OnFailure::Statements(vec![HookStatement::Action(
                            Action::Continue(crate::flows::types::ContinueAction {
                                failure: "Nested shell failed but continuing".to_string(),
                            }),
                        )]),
                        alias: Some("nested_shell".to_string()),
                    })),
                ],
                else_: None,
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

        eprintln!("Nested if on_failure result: {:?}", result);
        eprintln!("Failures: {:?}", state.failures());

        // The nested shell fails but its on_failure handler (continue action) executes successfully
        // Successful on_failure handlers return FailedContinue (handled the failure, continuing)
        assert!(matches!(result, HookOutcome::FailedContinue));

        // Verify the failure message was added
        let failures = state.failures();
        assert!(failures
            .iter()
            .any(|f| f.0.contains("Nested shell failed but continuing")));
    }

    #[test]
    fn test_nested_action_on_failure_in_switch_case() {
        // Test that actions nested inside switch cases execute their own on_failure handlers
        let statements = vec![
            HookStatement::Action(Action::Log(LogAction {
                log: "case1".to_string(),
                alias: Some("value".to_string()),
            })),
            HookStatement::Switch(SwitchStatement {
                expression: "{{value}}".to_string(),
                cases: vec![(
                    "case1".to_string(),
                    vec![
                        // This shell action should fail and trigger its own on_failure (block)
                        HookStatement::Action(Action::Shell(ShellAction {
                            shell: "false".to_string(),
                            timeout: None,
                            on_failure: OnFailure::Statements(vec![HookStatement::Action(
                                Action::Block(crate::flows::types::BlockAction {
                                    failure: "Nested shell in switch blocked".to_string(),
                                }),
                            )]),
                            alias: Some("nested_switch_shell".to_string()),
                        })),
                    ],
                )]
                .into_iter()
                .collect(),
                default: None,
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

        eprintln!("Nested switch on_failure result: {:?}", result);
        eprintln!("Failures: {:?}", state.failures());

        // The nested shell fails and its on_failure handler (block) stops execution
        // So the overall flow should return FailedBlock
        assert!(matches!(result, HookOutcome::FailedBlock));

        // Verify the block message was added
        let failures = state.failures();
        assert!(failures
            .iter()
            .any(|f| f.0.contains("Nested shell in switch blocked")));
    }

    #[test]
    fn test_deeply_nested_on_failure_handlers() {
        // Test that deeply nested actions (if inside if) execute their on_failure handlers
        let statements = vec![
            HookStatement::Action(Action::Log(LogAction {
                log: "true".to_string(),
                alias: Some("outer_condition".to_string()),
            })),
            HookStatement::Action(Action::Log(LogAction {
                log: "true".to_string(),
                alias: Some("inner_condition".to_string()),
            })),
            HookStatement::If(IfStatement {
                condition: "$outer_condition == true".to_string(),
                then: vec![HookStatement::If(IfStatement {
                    condition: "$inner_condition == true".to_string(),
                    then: vec![
                        // Deeply nested shell with its own on_failure
                        HookStatement::Action(Action::Shell(ShellAction {
                            shell: "false".to_string(),
                            timeout: None,
                            on_failure: OnFailure::Statements(vec![HookStatement::Action(
                                Action::Continue(crate::flows::types::ContinueAction {
                                    failure: "Deeply nested failure".to_string(),
                                }),
                            )]),
                            alias: Some("deep_shell".to_string()),
                        })),
                    ],
                    else_: None,
                })],
                else_: None,
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

        eprintln!("Deeply nested on_failure result: {:?}", result);
        eprintln!("Failures: {:?}", state.failures());

        // The deeply nested shell fails and its on_failure handler (continue action) executes
        // Continue actions should return FailedContinue to match shortcut behavior
        assert!(matches!(result, HookOutcome::FailedContinue));

        // Verify the failure message was added
        let failures = state.failures();
        assert!(failures
            .iter()
            .any(|f| f.0.contains("Deeply nested failure")));
    }

    #[test]
    fn test_if_branch_propagates_failed_continue() {
        // Test that FailedContinue inside an if branch is properly propagated
        // This is a regression test for the bug where HookOutcome::FailedContinue
        // was converted to ActionResult { success: true }, causing the parent
        // execute_statements to not track the failure
        let statements = vec![HookStatement::If(IfStatement {
            condition: "true".to_string(),
            then: vec![HookStatement::Action(Action::Shell(ShellAction {
                shell: "false".to_string(),
                timeout: None,
                on_failure: OnFailure::default(), // No on_failure handler, should default to continue
                alias: Some("failing_shell".to_string()),
            }))],
            else_: None,
        })];

        let mut state = AikiState::new(create_test_event());
        let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

        eprintln!("If branch with failing shell result: {:?}", result);

        // The shell failed and had no on_failure, so the if branch should have FailedContinue
        // which should propagate up to the parent execute_statements
        assert!(
            matches!(result, HookOutcome::FailedContinue),
            "Expected FailedContinue but got {:?}",
            result
        );
    }

    #[test]
    fn test_switch_branch_propagates_failed_continue() {
        // Test that FailedContinue inside a switch branch is properly propagated
        use std::collections::HashMap;

        let mut cases = HashMap::new();
        cases.insert(
            "test".to_string(),
            vec![HookStatement::Action(Action::Shell(ShellAction {
                shell: "false".to_string(),
                timeout: None,
                on_failure: OnFailure::default(), // No on_failure handler, should default to continue
                alias: Some("failing_shell".to_string()),
            }))],
        );

        let statements = vec![HookStatement::Switch(SwitchStatement {
            expression: "test".to_string(),
            cases,
            default: None,
        })];

        let mut state = AikiState::new(create_test_event());
        let result = HookEngine::execute_statements(&statements, &mut state).unwrap();

        eprintln!("Switch branch with failing shell result: {:?}", result);

        // The shell failed and had no on_failure, so the switch case should have FailedContinue
        // which should propagate up to the parent execute_statements
        assert!(
            matches!(result, HookOutcome::FailedContinue),
            "Expected FailedContinue but got {:?}",
            result
        );
    }

    #[test]
    fn test_if_branch_failure_triggers_parent_on_failure() {
        // This test is no longer valid because IfStatement doesn't have on_failure field.
        // If/Switch statements don't have failure handlers - only Actions do.
        // Keeping this test disabled to document the behavior change.
        // If you need failure handling around an if statement, wrap it in a shell action
        // or handle failures within the branch actions themselves.
    }

    #[test]
    fn test_switch_branch_failure_triggers_parent_on_failure() {
        // This test is no longer valid because SwitchStatement doesn't have on_failure field.
        // If/Switch statements don't have failure handlers - only Actions do.
        // Keeping this test disabled to document the behavior change.
        // If you need failure handling around a switch statement, wrap it in a shell action
        // or handle failures within the case actions themselves.
    }

    #[test]
    fn test_on_failure_shortcut_continue() {
        // Test that on_failure: "continue" works correctly
        let actions = vec![
            Action::Shell(ShellAction {
                shell: "false".to_string(),
                timeout: None,
                on_failure: OnFailure::Shortcut(OnFailureShortcut::Continue),
                alias: None,
            }),
            Action::Log(LogAction {
                log: "This should execute".to_string(),
                alias: Some("after".to_string()),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedContinue (failure occurred but flow continued)
        assert!(matches!(result, HookOutcome::FailedContinue));

        // Subsequent action should execute
        assert_eq!(
            state.get_variable("after"),
            Some(&"This should execute".to_string())
        );

        // Failure should be recorded
        assert_eq!(state.failures().len(), 1);
    }

    #[test]
    fn test_on_failure_explicit_continue_action() {
        // Test that on_failure: [continue: "message"] produces the same HookOutcome as shortcut form
        // This verifies the fix for the inconsistency where explicit actions returned Success
        let actions = vec![
            Action::Shell(ShellAction {
                shell: "false".to_string(),
                timeout: None,
                on_failure: OnFailure::Statements(vec![HookStatement::Action(Action::Continue(
                    crate::flows::types::ContinueAction {
                        failure: "Explicit continue message".to_string(),
                    },
                ))]),
                alias: None,
            }),
            Action::Log(LogAction {
                log: "This should execute".to_string(),
                alias: Some("after".to_string()),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedContinue (same as shortcut form)
        // The explicit continue action should behave identically to on_failure: continue
        assert!(
            matches!(result, HookOutcome::FailedContinue),
            "Expected FailedContinue but got {:?}. Explicit continue actions should match shortcut behavior.",
            result
        );

        // Subsequent action should execute
        assert_eq!(
            state.get_variable("after"),
            Some(&"This should execute".to_string())
        );

        // Failure should be recorded with our custom message
        assert_eq!(state.failures().len(), 1);
        assert!(state
            .failures()
            .iter()
            .any(|f| f.0.contains("Explicit continue message")));
    }

    #[test]
    fn test_on_failure_shortcut_stop() {
        // Test that on_failure: "stop" works correctly
        let actions = vec![
            Action::Shell(ShellAction {
                shell: "false".to_string(),
                timeout: None,
                on_failure: OnFailure::Shortcut(OnFailureShortcut::Stop),
                alias: None,
            }),
            Action::Log(LogAction {
                log: "This should NOT execute".to_string(),
                alias: Some("after".to_string()),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedStop
        assert!(matches!(result, HookOutcome::FailedStop));

        // Subsequent action should NOT execute
        assert!(state.get_variable("after").is_none());

        // Failure should be recorded
        assert_eq!(state.failures().len(), 1);
    }

    #[test]
    fn test_on_failure_shortcut_block() {
        // Test that on_failure: "block" works correctly
        let actions = vec![
            Action::Shell(ShellAction {
                shell: "false".to_string(),
                timeout: None,
                on_failure: OnFailure::Shortcut(OnFailureShortcut::Block),
                alias: None,
            }),
            Action::Log(LogAction {
                log: "This should NOT execute".to_string(),
                alias: Some("after".to_string()),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedBlock
        assert!(matches!(result, HookOutcome::FailedBlock));

        // Subsequent action should NOT execute
        assert!(state.get_variable("after").is_none());

        // Failure should be recorded
        assert_eq!(state.failures().len(), 1);
    }

    #[test]
    fn test_empty_on_failure_actions_list() {
        // Test that empty on_failure actions list is treated as continue
        let actions = vec![
            Action::Shell(ShellAction {
                shell: "false".to_string(),
                timeout: None,
                on_failure: OnFailure::Statements(vec![]), // Empty list
                alias: None,
            }),
            Action::Log(LogAction {
                log: "This should execute".to_string(),
                alias: Some("after".to_string()),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedContinue (empty list treated as continue)
        assert!(matches!(result, HookOutcome::FailedContinue));

        // Subsequent action should execute
        assert_eq!(
            state.get_variable("after"),
            Some(&"This should execute".to_string())
        );

        // Failure should be recorded with default message
        assert_eq!(state.failures().len(), 1);
        assert!(state
            .failures()
            .iter()
            .any(|f| f.0.contains("Action failed")));
    }

    #[test]
    fn test_on_failure_mixed_shortcuts_and_actions() {
        // Test mixing shortcuts and action lists in nested failures
        let actions = vec![
            Action::Shell(ShellAction {
                shell: "false".to_string(),
                timeout: None,
                on_failure: OnFailure::Statements(vec![
                    // First try to recover with another shell
                    HookStatement::Action(Action::Shell(ShellAction {
                        shell: "false".to_string(), // This also fails
                        timeout: None,
                        on_failure: OnFailure::Shortcut(OnFailureShortcut::Block), // Use shortcut
                        alias: None,
                    })),
                ]),
                alias: None,
            }),
            Action::Log(LogAction {
                log: "This should NOT execute".to_string(),
                alias: Some("after".to_string()),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedBlock from nested shortcut
        assert!(matches!(result, HookOutcome::FailedBlock));

        // Subsequent action should NOT execute
        assert!(state.get_variable("after").is_none());

        // Should have failures from both levels
        assert!(state.failures().len() >= 1);
    }

    #[test]
    fn test_on_failure_default_is_continue() {
        // Test that default on_failure behavior is continue
        let actions = vec![
            Action::Shell(ShellAction {
                shell: "false".to_string(),
                timeout: None,
                on_failure: OnFailure::default(), // Should default to continue
                alias: None,
            }),
            Action::Log(LogAction {
                log: "This should execute".to_string(),
                alias: Some("after".to_string()),
            }),
        ];

        let mut state = AikiState::new(create_test_event());
        let result = execute_actions(&actions, &mut state).unwrap();

        // Should return FailedContinue (default is continue)
        assert!(matches!(result, HookOutcome::FailedContinue));

        // Subsequent action should execute
        assert_eq!(
            state.get_variable("after"),
            Some(&"This should execute".to_string())
        );

        // Failure should be recorded
        assert_eq!(state.failures().len(), 1);
    }

    #[test]
    fn test_numeric_comparison_greater_than() {
        let mut state = AikiState::new(create_test_event());
        state.set_variable("count".to_string(), "5".to_string());

        // 5 > 3 should be true
        let result = HookEngine::evaluate_condition("$count > 3", &mut state).unwrap();
        assert!(result, "5 > 3 should be true");

        // 5 > 10 should be false
        let result = HookEngine::evaluate_condition("$count > 10", &mut state).unwrap();
        assert!(!result, "5 > 10 should be false");
    }

    #[test]
    fn test_numeric_comparison_less_than() {
        let mut state = AikiState::new(create_test_event());
        state.set_variable("count".to_string(), "5".to_string());

        // 5 < 10 should be true
        let result = HookEngine::evaluate_condition("$count < 10", &mut state).unwrap();
        assert!(result, "5 < 10 should be true");

        // 5 < 3 should be false
        let result = HookEngine::evaluate_condition("$count < 3", &mut state).unwrap();
        assert!(!result, "5 < 3 should be false");
    }

    #[test]
    fn test_numeric_comparison_greater_equal() {
        let mut state = AikiState::new(create_test_event());
        state.set_variable("count".to_string(), "5".to_string());

        // 5 >= 5 should be true
        let result = HookEngine::evaluate_condition("$count >= 5", &mut state).unwrap();
        assert!(result, "5 >= 5 should be true");

        // 5 >= 3 should be true
        let result = HookEngine::evaluate_condition("$count >= 3", &mut state).unwrap();
        assert!(result, "5 >= 3 should be true");

        // 5 >= 10 should be false
        let result = HookEngine::evaluate_condition("$count >= 10", &mut state).unwrap();
        assert!(!result, "5 >= 10 should be false");
    }

    #[test]
    fn test_numeric_comparison_less_equal() {
        let mut state = AikiState::new(create_test_event());
        state.set_variable("count".to_string(), "5".to_string());

        // 5 <= 5 should be true
        let result = HookEngine::evaluate_condition("$count <= 5", &mut state).unwrap();
        assert!(result, "5 <= 5 should be true");

        // 5 <= 10 should be true
        let result = HookEngine::evaluate_condition("$count <= 10", &mut state).unwrap();
        assert!(result, "5 <= 10 should be true");

        // 5 <= 3 should be false
        let result = HookEngine::evaluate_condition("$count <= 3", &mut state).unwrap();
        assert!(!result, "5 <= 3 should be false");
    }

    #[test]
    fn test_numeric_comparison_decimal() {
        let mut state = AikiState::new(create_test_event());
        state.set_variable("value".to_string(), "3.14".to_string());

        // 3.14 > 3.0 should be true
        let result = HookEngine::evaluate_condition("$value > 3.0", &mut state).unwrap();
        assert!(result, "3.14 > 3.0 should be true");

        // 3.14 < 3.2 should be true
        let result = HookEngine::evaluate_condition("$value < 3.2", &mut state).unwrap();
        assert!(result, "3.14 < 3.2 should be true");
    }

    #[test]
    fn test_numeric_comparison_invalid_number() {
        let mut state = AikiState::new(create_test_event());
        state.set_variable("text".to_string(), "not_a_number".to_string());

        // Rhai: type mismatch returns false (lenient mode)
        let result = HookEngine::evaluate_condition("$text > 5", &mut state).unwrap();
        assert!(
            !result,
            "Comparing non-numeric value should return false (lenient)"
        );
    }

    #[test]
    fn test_numeric_comparison_with_nested_field() {
        let mut state = AikiState::new(create_test_event());
        // Use dotted key to simulate how create_resolver sets event fields
        state.set_variable("event.file_count".to_string(), "3".to_string());

        // event.file_count > 1 should be true
        let result = HookEngine::evaluate_condition("event.file_count > 1", &mut state).unwrap();
        assert!(result, "event.file_count (3) > 1 should be true");

        // event.file_count > 5 should be false
        let result = HookEngine::evaluate_condition("event.file_count > 5", &mut state).unwrap();
        assert!(!result, "event.file_count (3) > 5 should be false");
    }

    // ── Mutex + helpers for session-file-based tests (env mutation) ──

    // Use the process-wide mutex from global.rs to avoid races with other modules
    fn session_test_lock() -> std::sync::MutexGuard<'static, ()> {
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
                Some(v) => std::env::set_var(crate::global::AIKI_HOME_ENV, v),
                None => std::env::remove_var(crate::global::AIKI_HOME_ENV),
            }
        }
    }

    fn setup_aiki_home() -> (tempfile::TempDir, EnvGuard) {
        let aiki_home = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(aiki_home.path().join("sessions")).unwrap();
        let original = std::env::var(crate::global::AIKI_HOME_ENV).ok();
        std::env::set_var(crate::global::AIKI_HOME_ENV, aiki_home.path());
        (aiki_home, EnvGuard { original })
    }

    fn create_task_closed_event(task_id: &str) -> AikiEvent {
        AikiTaskClosedPayload {
            task: TaskEventPayload {
                id: task_id.to_string(),
                name: "Test task".to_string(),
                task_type: "feature".to_string(),
                status: "closed".to_string(),
                assignee: None,
                outcome: Some("done".to_string()),
                source: None,
                files: None,
                changes: None,
            },
            cwd: std::path::PathBuf::from("/tmp/test"),
            timestamp: chrono::Utc::now(),
        }
        .into()
    }

    // 32-char lowercase IDs for thread tests
    const HEAD_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const TAIL_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    // --- 5a: task.closed resolves session.thread.{tail,head} and session.thread ---
    #[test]
    fn test_task_closed_resolves_session_thread_tail() {
        let _lock = session_test_lock();
        let (aiki_home, _guard) = setup_aiki_home();

        // Write session file with thread=head:tail
        let sessions_dir = aiki_home.path().join("sessions");
        let content = format!(
            "thread={}:{}\nparent_pid=12345\nsession_id=sess-5a\nmode=interactive\n",
            HEAD_ID, TAIL_ID,
        );
        std::fs::write(sessions_dir.join("sess-5a"), &content).unwrap();

        // Close the TAIL task → should match the session
        let event = create_task_closed_event(TAIL_ID);
        let state = AikiState::new(event);
        let mut resolver = HookEngine::create_resolver(&state);

        assert_eq!(
            resolver.resolve("{{session.thread.tail}}").unwrap(),
            TAIL_ID,
        );
        assert_eq!(
            resolver.resolve("{{session.thread.head}}").unwrap(),
            HEAD_ID,
        );
        assert_eq!(
            resolver.resolve("{{session.thread}}").unwrap(),
            format!("{}:{}", HEAD_ID, TAIL_ID),
        );
    }

    // --- 5b: No session file on disk → empty strings ---
    #[test]
    fn test_task_closed_session_thread_empty_when_no_session() {
        let _lock = session_test_lock();
        let (_aiki_home, _guard) = setup_aiki_home();
        // No session files written

        let event = create_task_closed_event(TAIL_ID);
        let state = AikiState::new(event);
        let mut resolver = HookEngine::create_resolver(&state);

        assert_eq!(resolver.resolve("{{session.thread.tail}}").unwrap(), "",);
        assert_eq!(resolver.resolve("{{session.mode}}").unwrap(), "",);
    }

    // --- 5c: Only tail triggers session.end (head does not) ---
    #[test]
    fn test_task_closed_only_tail_triggers_session_end() {
        let _lock = session_test_lock();
        let (aiki_home, _guard) = setup_aiki_home();

        let sessions_dir = aiki_home.path().join("sessions");
        let content = format!(
            "thread={}:{}\nparent_pid=99999\nsession_id=sess-5c\nmode=interactive\n",
            HEAD_ID, TAIL_ID,
        );
        std::fs::write(sessions_dir.join("sess-5c"), &content).unwrap();

        let session_end = crate::flows::types::SessionEndAction {
            reason: "task done".to_string(),
            on_failure: crate::flows::types::OnFailure::default(),
        };

        // Close HEAD task → session lookup should NOT match (tail-only matching)
        {
            let event = create_task_closed_event(HEAD_ID);
            let mut state = AikiState::new(event);

            // Verify the thread session lookup returns None for the head task
            assert!(
                state.resolve_task_closed_thread_session().is_none(),
                "Closing the HEAD task should NOT match a session (tail-only match)",
            );

            // session.end action should succeed but not register any pending termination
            let result = HookEngine::execute_session_end(&session_end, &mut state).unwrap();
            assert!(result.success);
            // No pending session end should have been registered (no PID found)
        }

        // Close TAIL task → should match → session.end fires
        {
            let event = create_task_closed_event(TAIL_ID);
            let mut state = AikiState::new(event);

            // Verify the thread session lookup returns Some for the tail task
            let session_info = state.resolve_task_closed_thread_session();
            assert!(
                session_info.is_some(),
                "Closing the TAIL task should match the session",
            );
            assert_eq!(session_info.unwrap().pid, 99999);

            // session.end action should register a pending termination
            let result = HookEngine::execute_session_end(&session_end, &mut state).unwrap();
            assert!(result.success);
        }
    }

    // --- 5d: Single-task thread (head==tail) triggers session.end ---
    #[test]
    fn test_task_closed_single_task_thread_triggers_session_end() {
        let _lock = session_test_lock();
        let (aiki_home, _guard) = setup_aiki_home();

        let sessions_dir = aiki_home.path().join("sessions");
        // Single-task thread: thread=<id> (no colon → head==tail)
        let content = format!(
            "thread={}\nparent_pid=77777\nsession_id=sess-5d\nmode=background\n",
            HEAD_ID,
        );
        std::fs::write(sessions_dir.join("sess-5d"), &content).unwrap();

        let event = create_task_closed_event(HEAD_ID);
        let mut state = AikiState::new(event);

        // Single-task thread: closing the only task matches
        let session_info = state.resolve_task_closed_thread_session();
        assert!(
            session_info.is_some(),
            "Single-task thread should match when closing that task",
        );
        let info = session_info.unwrap();
        assert_eq!(info.thread.head, HEAD_ID);
        assert_eq!(info.thread.tail, HEAD_ID);
        assert!(info.thread.is_single());
        assert_eq!(info.pid, 77777);

        // Verify session.thread resolves to just the ID (no colon for single-task)
        let mut resolver = HookEngine::create_resolver(&state);
        assert_eq!(resolver.resolve("{{session.thread}}").unwrap(), HEAD_ID,);

        // session.end should register pending termination
        let session_end = crate::flows::types::SessionEndAction {
            reason: "single-task session done".to_string(),
            on_failure: crate::flows::types::OnFailure::default(),
        };
        let result = HookEngine::execute_session_end(&session_end, &mut state).unwrap();
        assert!(result.success);
    }

    // --- 5e: Background mode blocks the hook for non-orchestrator tasks ---
    //
    // Background sessions running non-orchestrator tasks should NOT be SIGTERM'd.
    #[test]
    fn test_task_closed_background_non_orchestrator_blocks_hook() {
        let _lock = session_test_lock();
        let (aiki_home, _guard) = setup_aiki_home();

        // Write session file with mode=background (as spawn_blocking now sets)
        let sessions_dir = aiki_home.path().join("sessions");
        let content = format!(
            "thread={}\nparent_pid=88888\nsession_id=sess-5e\nmode=background\n",
            HEAD_ID,
        );
        std::fs::write(sessions_dir.join("sess-5e"), &content).unwrap();

        // Close a non-orchestrator task (feature type) that matches the session's thread tail
        let event = create_task_closed_event(HEAD_ID);
        let mut state = AikiState::new(event);

        // Verify the session IS found (thread tail matches)
        let session_info = state.resolve_task_closed_thread_session();
        assert!(
            session_info.is_some(),
            "Session should be found by thread tail"
        );
        let info = session_info.unwrap();
        assert_eq!(info.mode, crate::session::SessionMode::Background);

        // The hook condition now allows interactive OR orchestrator tasks
        let condition = r#"event.task.id == session.thread.tail && (session.mode == "interactive" || event.task.type == "orchestrator")"#;
        let result = HookEngine::evaluate_condition(condition, &mut state).unwrap();
        assert!(
            !result,
            "Hook condition must be FALSE for background sessions with non-orchestrator tasks"
        );
    }

    // --- 5f: Interactive mode allows the full hook condition ---
    #[test]
    fn test_task_closed_interactive_mode_allows_hook_condition() {
        let _lock = session_test_lock();
        let (aiki_home, _guard) = setup_aiki_home();

        let sessions_dir = aiki_home.path().join("sessions");
        let content = format!(
            "thread={}\nparent_pid=99999\nsession_id=sess-5f\nmode=interactive\n",
            HEAD_ID,
        );
        std::fs::write(sessions_dir.join("sess-5f"), &content).unwrap();

        let event = create_task_closed_event(HEAD_ID);
        let mut state = AikiState::new(event);

        let condition = r#"event.task.id == session.thread.tail && (session.mode == "interactive" || event.task.type == "orchestrator")"#;
        let result = HookEngine::evaluate_condition(condition, &mut state).unwrap();
        assert!(
            result,
            "Hook condition must be TRUE for interactive sessions"
        );
    }

    // --- 5g: Background orchestrator sessions are terminated ---
    //
    // When an orchestrator task closes, its background session should be SIGTERM'd.
    // This is the fix for spawn_blocking hangs in `aiki build`.
    #[test]
    fn test_task_closed_background_orchestrator_allows_hook() {
        let _lock = session_test_lock();
        let (aiki_home, _guard) = setup_aiki_home();

        let sessions_dir = aiki_home.path().join("sessions");
        let content = format!(
            "thread={}\nparent_pid=77777\nsession_id=sess-5g\nmode=background\n",
            HEAD_ID,
        );
        std::fs::write(sessions_dir.join("sess-5g"), &content).unwrap();

        // Create an orchestrator task closed event
        let event: AikiEvent = AikiTaskClosedPayload {
            task: TaskEventPayload {
                id: HEAD_ID.to_string(),
                name: "Loop orchestrator".to_string(),
                task_type: "orchestrator".to_string(),
                status: "closed".to_string(),
                assignee: None,
                outcome: Some("done".to_string()),
                source: None,
                files: None,
                changes: None,
            },
            cwd: std::path::PathBuf::from("/tmp/test"),
            timestamp: chrono::Utc::now(),
        }
        .into();
        let mut state = AikiState::new(event);

        let condition = r#"event.task.id == session.thread.tail && (session.mode == "interactive" || event.task.type == "orchestrator")"#;
        let result = HookEngine::evaluate_condition(condition, &mut state).unwrap();
        assert!(
            result,
            "Hook condition must be TRUE for orchestrator tasks in background sessions"
        );
    }
}
