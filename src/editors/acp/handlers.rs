//! Event handlers for ACP proxy
//!
//! This module contains functions that fire Aiki events based on
//! intercepted ACP protocol messages:
//! - Session lifecycle events (start, end)
//! - File change events (pre/post)
//! - Prompt manipulation

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use agent_client_protocol::{
    SessionUpdate, ToolCall, ToolCallId, ToolCallLocation, ToolCallStatus, ToolCallUpdate, ToolKind,
};
use serde_json::json;

use super::protocol::{
    session_id, AgentInfo, ClientInfo, JsonRpcId, JsonRpcMessage, SessionId, SessionNotification,
};
use super::state::{check_autoreply_limit, increment_autoreply_counter, AutoreplyMessage};
use crate::cache::debug_log;
use crate::error::{AikiError, Result};
use crate::event_bus;
use crate::events::result::HookResult;
use crate::events::{
    AikiChangeCompletedPayload, AikiChangePermissionAskedPayload, AikiEvent,
    AikiSessionStartPayload, AikiTurnCompletedPayload, AikiTurnStartedPayload, ChangeOperation,
    DeleteOperation, MoveOperation, WriteOperation,
};
use crate::provenance::record::AgentType;
use crate::session::AikiSession;

/// A JSON-RPC autoreply message to be sent to the agent
///
/// Stores the structured data for a session/prompt autoreply request.
/// The JSON is generated on-demand when needed.
#[derive(Debug, Clone)]
pub struct Autoreply {
    /// The session ID to send the prompt to
    pub session_id: SessionId,
    /// The text content of the autoreply
    pub text: String,
    /// The raw request ID string (for JSON serialization)
    pub raw_request_id: String,
    /// The normalized request ID (for HashMap tracking)
    pub normalized_request_id: JsonRpcId,
}

impl Autoreply {
    /// Create a new session/prompt autoreply request
    ///
    /// # Arguments
    /// * `session_id` - The session ID to send the prompt to
    /// * `autoreply_text` - The text content to send as the prompt
    /// * `counter` - The autoreply counter for this session (for unique ID generation)
    #[must_use]
    pub fn new(session_id: &SessionId, autoreply_text: String, counter: usize) -> Self {
        // Generate unique request ID (raw string without quotes)
        let raw_id = format!("aiki-autoreply-{}-{}", session_id, counter);
        // Normalize for HashMap tracking (with quotes for string IDs)
        let normalized_id = JsonRpcId::from_value(&serde_json::Value::String(raw_id.clone()));

        Self {
            session_id: Arc::clone(session_id),
            text: autoreply_text,
            raw_request_id: raw_id,
            normalized_request_id: normalized_id,
        }
    }

    /// Generate the JSON-RPC request and serialize it to a string
    pub fn as_json(&self) -> Result<String> {
        let autoreply_request = json!({
            "jsonrpc": "2.0",
            "id": &self.raw_request_id,  // Use raw string, not normalized
            "method": "session/prompt",
            "params": {
                "sessionId": &self.session_id,
                "prompt": [{
                    "type": "text",
                    "text": &self.text
                }]
            }
        });

        serde_json::to_string(&autoreply_request)
            .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to serialize autoreply: {}", e)))
    }

    /// Get the normalized request ID for HashMap tracking
    #[must_use]
    pub fn normalized_request_id(&self) -> &JsonRpcId {
        &self.normalized_request_id
    }

    /// Get the raw request ID string for display/debugging
    #[must_use]
    pub fn raw_request_id_display(&self) -> &str {
        &self.raw_request_id
    }
}

// ============================================================================
// Prompt manipulation utilities
// ============================================================================

/// Extract text content from a prompt array
///
/// Iterates through prompt items and collects all text from items with `type: "text"`.
pub fn extract_text_from_prompt_array(prompt_array: &[serde_json::Value]) -> Vec<String> {
    prompt_array
        .iter()
        .filter_map(|item| {
            if item.get("type").and_then(|v| v.as_str()) == Some("text") {
                item.get("text")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Concatenate text chunks with double newline separators
pub fn concatenate_text_chunks(chunks: &[String]) -> String {
    chunks.join("\n\n")
}

/// Build a modified prompt array by replacing only the first text entry
///
/// Preserves the original array order and all non-text resources (images, etc.).
/// Replaces the first text item with the modified text, removing all other text items.
/// This maintains the original ordering of resources while updating the user's prompt.
pub fn build_modified_prompt(
    original_prompt: &[serde_json::Value],
    modified_text: &str,
) -> Vec<serde_json::Value> {
    let mut new_prompt = Vec::new();
    let mut replaced_first_text = false;

    for item in original_prompt {
        let is_text = item.get("type").and_then(|v| v.as_str()) == Some("text");

        if is_text {
            if !replaced_first_text {
                // Clone the first text block to preserve all fields (annotations, _meta, etc.)
                // then mutate only the "text" field
                let mut modified_item = item.clone();
                if let Some(obj) = modified_item.as_object_mut() {
                    obj.insert("text".to_string(), json!(modified_text));
                }
                new_prompt.push(modified_item);
                replaced_first_text = true;
            }
            // Skip all other text entries (they were concatenated into modified_text)
        } else {
            // Preserve all non-text items in their original position
            new_prompt.push(item.clone());
        }
    }

    // If there were no text items, append a minimal text block at the end
    if !replaced_first_text {
        new_prompt.push(json!({
            "type": "text",
            "text": modified_text
        }));
    }

    new_prompt
}

/// Extract autoreply from context
pub fn extract_autoreply(response: &HookResult) -> Option<String> {
    response.context.as_ref().filter(|s| !s.is_empty()).cloned()
}

// ============================================================================
// Session management
// ============================================================================

/// Create an AikiSession for ACP protocol tracking
///
/// In ACP mode, `agent_pid` can be provided by the agent in the session/start message.
/// This enables PID-based session detection for subprocesses spawned by the agent.
/// Mode is determined by `AIKI_SESSION_MODE` env var:
/// - "background" → Background mode
/// - anything else → Interactive mode (default)
pub fn create_session(
    agent_type: AgentType,
    session_id: impl Into<String>,
    agent_version: Option<&str>,
) -> AikiSession {
    use crate::session::SessionMode;
    // Determine mode from AIKI_SESSION_MODE env var
    let mode = match std::env::var("AIKI_SESSION_MODE").as_deref() {
        Ok("background") => SessionMode::Background,
        _ => SessionMode::Interactive,
    };
    AikiSession::new(
        agent_type,
        session_id,
        agent_version,
        crate::provenance::DetectionMethod::ACP,
        mode,
    )
}

/// Create an AikiSession with agent_pid for ACP protocol tracking
///
/// When `agent_pid` is provided, it's stored in the session file to enable
/// PID-based session detection for subprocesses spawned by the agent.
/// Mode is determined by `AIKI_SESSION_MODE` env var:
/// - "background" → Background mode
/// - anything else → Interactive mode (default)
pub fn create_session_with_pid(
    agent_type: AgentType,
    session_id: impl Into<String>,
    agent_version: Option<&str>,
    agent_pid: Option<u32>,
) -> AikiSession {
    use crate::session::SessionMode;
    // Determine mode from AIKI_SESSION_MODE env var
    let mode = match std::env::var("AIKI_SESSION_MODE").as_deref() {
        Ok("background") => SessionMode::Background,
        _ => SessionMode::Interactive,
    };
    AikiSession::new(
        agent_type,
        session_id,
        agent_version,
        crate::provenance::DetectionMethod::ACP,
        mode,
    )
    .with_parent_pid(agent_pid)
}

// ============================================================================
// Tool call context tracking
// ============================================================================

/// Context for tracking tool calls across multiple updates
#[derive(Clone)]
pub struct ToolCallContext {
    pub kind: ToolKind,
    pub paths: Vec<PathBuf>,
    pub content: Vec<agent_client_protocol::ToolCallContent>,
}

/// Extract file paths from tool call locations
pub fn paths_from_locations(locations: &[ToolCallLocation]) -> Vec<PathBuf> {
    locations.iter().map(|loc| loc.path.clone()).collect()
}

/// Convert ToolKind to canonical tool name string
pub fn tool_kind_to_name(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "Read",
        ToolKind::Edit => "Edit",
        ToolKind::Delete => "Delete",
        ToolKind::Move => "Move",
        ToolKind::Search => "Search",
        ToolKind::Execute => "Execute",
        ToolKind::Think => "Think",
        ToolKind::Fetch => "Fetch",
        ToolKind::SwitchMode => "SwitchMode",
        ToolKind::Other => "Other",
    }
}

/// Extract edit details from ACP tool call context
///
/// Extracts old_text/new_text from ToolCallContent::Diff variants.
/// The ACP protocol provides file diffs in the content field when tools
/// modify files, allowing us to detect user edits.
pub fn extract_edit_details(context: &ToolCallContext) -> Vec<crate::events::EditDetail> {
    use agent_client_protocol::ToolCallContent;

    let mut edit_details = Vec::new();

    for content_item in &context.content {
        if let ToolCallContent::Diff { diff } = content_item {
            // Convert PathBuf to string for file_path
            let file_path = diff.path.to_string_lossy().to_string();

            // old_text is Option<String>, use empty string if None (new file)
            let old_string = diff.old_text.clone().unwrap_or_default();

            // new_text is the modified content
            let new_string = diff.new_text.clone();

            edit_details.push(crate::events::EditDetail::new(
                file_path, old_string, new_string,
            ));
        }
    }

    if !edit_details.is_empty() {
        debug_log(|| {
            format!(
                "[acp] Extracted {} edit details from tool call content",
                edit_details.len()
            )
        });
    }

    edit_details
}

// ============================================================================
// Permission request parsing
// ============================================================================

/// Permission request context extracted from session/request_permission
pub struct PermissionRequestContext {
    pub kind: ToolKind,
    pub paths: Vec<String>,
}

/// Parses session/request_permission params to extract tool kind and file paths.
/// Returns None for non-file-modifying operations (Read, Bash, etc.).
pub fn parse_permission_request(msg: &JsonRpcMessage) -> Option<PermissionRequestContext> {
    let params = msg.params.as_ref()?;

    // Log tool_call_id for debugging
    if let Some(tool_call_id) = params.get("toolCallId") {
        debug_log(|| {
            format!(
                "[acp] Found permission request for tool_call_id: {:?}",
                tool_call_id
            )
        });
    }

    // Extract tool kind
    let kind_val = params.get("kind").or_else(|| params.get("toolKind"))?;
    let kind_str = kind_val.as_str()?;

    let kind = match kind_str {
        "edit" => ToolKind::Edit,
        "delete" => ToolKind::Delete,
        "move" => ToolKind::Move,
        _ => return None, // Not a file-modifying operation
    };

    // Extract file paths from various possible locations in the params
    let mut paths = Vec::new();

    // Try "paths" array
    if let Some(paths_array) = params.get("paths").and_then(|v| v.as_array()) {
        for path_val in paths_array {
            if let Some(path_str) = path_val.as_str() {
                paths.push(path_str.to_string());
            }
        }
    }

    // Try "filePath" or "file_path" single value
    if paths.is_empty() {
        if let Some(file_path) = params
            .get("filePath")
            .or_else(|| params.get("file_path"))
            .and_then(|v| v.as_str())
        {
            paths.push(file_path.to_string());
        }
    }

    // Try "locations" array (ACP tool call format)
    if paths.is_empty() {
        if let Some(locations) = params.get("locations").and_then(|v| v.as_array()) {
            for loc in locations {
                if let Some(path) = loc.get("path").and_then(|v| v.as_str()) {
                    paths.push(path.to_string());
                }
            }
        }
    }

    Some(PermissionRequestContext { kind, paths })
}

// ============================================================================
// Session update handling
// ============================================================================

/// Handle session/update notification from agent
///
/// Extracts tool_call information and dispatches provenance recording via event bus.
/// This is called for every session/update from the agent to the IDE.
pub fn handle_session_update(
    msg: &JsonRpcMessage,
    agent_type: &AgentType,
    client_info: &Option<ClientInfo>,
    agent_info: &Option<AgentInfo>,
    cwd: &Option<PathBuf>,
    tool_call_contexts: &mut HashMap<ToolCallId, ToolCallContext>,
) -> Result<()> {
    // Parse session/update params
    let params = msg
        .params
        .as_ref()
        .ok_or_else(|| AikiError::Other(anyhow::anyhow!("session/update missing params")))?;

    let notification: SessionNotification =
        serde_json::from_value(params.clone()).map_err(|e| {
            AikiError::Other(anyhow::anyhow!(
                "Failed to parse SessionNotification: {}",
                e
            ))
        })?;

    let session_id_str = notification.session_id.to_string();

    match &notification.update {
        SessionUpdate::ToolCall(tool_call) => process_tool_call(
            &session_id_str,
            tool_call,
            agent_type,
            client_info,
            agent_info,
            cwd,
            tool_call_contexts,
        ),
        SessionUpdate::ToolCallUpdate(update) => process_tool_call_update(
            &session_id_str,
            update,
            agent_type,
            client_info,
            agent_info,
            cwd,
            tool_call_contexts,
        ),
        _ => Ok(()),
    }
}

fn process_tool_call(
    session_id_str: &str,
    tool_call: &ToolCall,
    agent_type: &AgentType,
    client_info: &Option<ClientInfo>,
    agent_info: &Option<AgentInfo>,
    cwd: &Option<PathBuf>,
    tool_call_contexts: &mut HashMap<ToolCallId, ToolCallContext>,
) -> Result<()> {
    let context = ToolCallContext {
        kind: tool_call.kind,
        paths: paths_from_locations(&tool_call.locations),
        content: tool_call.content.clone(),
    };

    let status = tool_call.status;

    // Store context for potential updates
    tool_call_contexts.insert(tool_call.id.clone(), context.clone());
    if matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed) {
        tool_call_contexts.remove(&tool_call.id);
    }

    if status == ToolCallStatus::Completed {
        record_post_change_events(
            session_id_str,
            agent_type,
            client_info,
            agent_info,
            cwd,
            context,
        )?;
    }

    Ok(())
}

fn process_tool_call_update(
    session_id_str: &str,
    tool_call: &ToolCallUpdate,
    agent_type: &AgentType,
    client_info: &Option<ClientInfo>,
    agent_info: &Option<AgentInfo>,
    cwd: &Option<PathBuf>,
    tool_call_contexts: &mut HashMap<ToolCallId, ToolCallContext>,
) -> Result<()> {
    let entry = tool_call_contexts
        .entry(tool_call.id.clone())
        .or_insert_with(|| ToolCallContext {
            kind: tool_call.fields.kind.unwrap_or(ToolKind::Other),
            paths: Vec::new(),
            content: Vec::new(),
        });

    if let Some(kind) = tool_call.fields.kind {
        entry.kind = kind;
    }

    if let Some(locations) = &tool_call.fields.locations {
        entry.paths = paths_from_locations(locations);
    }

    if let Some(content) = &tool_call.fields.content {
        entry.content = content.clone();
    }

    let status = tool_call.fields.status;
    let should_record =
        matches!(status, Some(ToolCallStatus::Completed)) && !entry.paths.is_empty();
    let context = if should_record {
        Some(entry.clone())
    } else {
        None
    };

    if matches!(
        status,
        Some(ToolCallStatus::Completed | ToolCallStatus::Failed)
    ) {
        tool_call_contexts.remove(&tool_call.id);
    }

    if let Some(context) = context {
        record_post_change_events(
            session_id_str,
            agent_type,
            client_info,
            agent_info,
            cwd,
            context,
        )?;
    }

    Ok(())
}

/// Record change.completed events after a tool call completes
pub fn record_post_change_events(
    session_id_str: &str,
    agent_type: &AgentType,
    client_info: &Option<ClientInfo>,
    agent_info: &Option<AgentInfo>,
    cwd: &Option<PathBuf>,
    context: ToolCallContext,
) -> Result<()> {
    if !matches!(
        context.kind,
        ToolKind::Edit | ToolKind::Delete | ToolKind::Move
    ) {
        return Ok(());
    }

    if context.paths.is_empty() {
        return Ok(());
    }

    // Get working directory (required)
    let working_dir = cwd
        .as_ref()
        .ok_or_else(|| AikiError::Other(anyhow::anyhow!("Working directory not available")))?
        .clone();

    // Get tool name from kind
    let tool_name = tool_kind_to_name(context.kind);

    // Convert all paths to strings
    let file_paths: Vec<String> = context
        .paths
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    // Create session with agent version and client info
    let agent_version = agent_info.as_ref().and_then(|a| a.version.as_deref());
    let session = create_session(*agent_type, session_id_str.to_string(), agent_version)
        .with_client_info(
            client_info.as_ref().map(|c| c.name.as_str()),
            client_info.as_ref().and_then(|c| c.version.as_deref()),
        );

    // Build operation based on tool kind
    let operation = match context.kind {
        ToolKind::Edit => {
            let edit_details = extract_edit_details(&context);
            ChangeOperation::Write(WriteOperation {
                file_paths,
                edit_details,
            })
        }
        ToolKind::Delete => ChangeOperation::Delete(DeleteOperation { file_paths }),
        ToolKind::Move => {
            // Use MoveOperation::from_move_paths to properly expand directory moves
            if file_paths.len() >= 2 {
                ChangeOperation::Move(MoveOperation::from_move_paths(file_paths))
            } else {
                // Single path - treat as write
                ChangeOperation::Write(WriteOperation {
                    file_paths,
                    edit_details: vec![],
                })
            }
        }
        _ => return Ok(()), // Should not reach here due to earlier check
    };

    // Create and dispatch the change.completed event
    // Note: Turn info is not available in ACP context; provenance will use defaults
    let event = AikiEvent::ChangeCompleted(AikiChangeCompletedPayload {
        session,
        cwd: working_dir,
        timestamp: chrono::Utc::now(),
        tool_name: tool_name.to_string(),
        success: true,
        turn: crate::events::Turn::unknown(),
        operation,
    });

    // Dispatch to event bus (non-blocking - errors are logged but don't fail the proxy)
    if let Err(e) = event_bus::dispatch(event) {
        eprintln!("Warning: Event bus dispatch failed: {}", e);
    }

    Ok(())
}

// ============================================================================
// Session lifecycle events
// ============================================================================

/// Fire session.started event
///
/// If `agent_pid` is provided, it will be stored in the session file to enable
/// PID-based session detection for subprocesses spawned by the agent.
pub fn fire_session_start_event(
    session_id_str: &str,
    agent_type: &AgentType,
    cwd: &Option<PathBuf>,
    agent_pid: Option<u32>,
) -> Result<()> {
    // Get working directory (required)
    let working_dir = cwd
        .as_ref()
        .ok_or_else(|| AikiError::Other(anyhow::anyhow!("Working directory not available")))?
        .clone();

    // Create session with agent_pid for PID-based session detection
    let session = create_session_with_pid(
        *agent_type,
        session_id_str.to_string(),
        None::<&str>,
        agent_pid,
    );
    let event = AikiEvent::SessionStarted(AikiSessionStartPayload {
        session,
        cwd: working_dir,
        timestamp: chrono::Utc::now(),
    });

    // Dispatch to event bus (non-blocking - errors are logged but don't fail the proxy)
    if let Err(e) = event_bus::dispatch(event) {
        eprintln!("Warning: session.started event bus dispatch failed: {}", e);
    } else {
        debug_log(|| {
            format!(
                "[acp] Fired session.started event for session: {} (agent_pid: {:?})",
                session_id_str, agent_pid
            )
        });
    }

    Ok(())
}

/// Fire change.permission_asked event before file-modifying tool executes
pub fn fire_pre_file_change_event(
    session_id_str: &str,
    agent_type: &AgentType,
    cwd: &Option<PathBuf>,
    kind: ToolKind,
    file_paths: Vec<String>,
) -> Result<()> {
    // Get working directory (required)
    let working_dir = cwd
        .as_ref()
        .ok_or_else(|| AikiError::Other(anyhow::anyhow!("Working directory not available")))?
        .clone();

    // Get tool name from kind
    let tool_name = tool_kind_to_name(kind);

    // Build operation based on tool kind (matching record_post_change_events logic)
    let operation = match kind {
        ToolKind::Edit => ChangeOperation::Write(WriteOperation {
            file_paths,
            edit_details: vec![], // Edit details not available at permission time
        }),
        ToolKind::Delete => ChangeOperation::Delete(DeleteOperation { file_paths }),
        ToolKind::Move => {
            // Use MoveOperation::from_move_paths to properly expand directory moves
            if file_paths.len() >= 2 {
                ChangeOperation::Move(MoveOperation::from_move_paths(file_paths))
            } else {
                // Single path - treat as write (destination only)
                ChangeOperation::Write(WriteOperation {
                    file_paths,
                    edit_details: vec![],
                })
            }
        }
        _ => {
            // Shouldn't reach here since we filter in parse_permission_request
            ChangeOperation::Write(WriteOperation {
                file_paths,
                edit_details: vec![],
            })
        }
    };

    // Create and dispatch change.permission_asked event
    let session = create_session(*agent_type, session_id_str.to_string(), None::<&str>);
    let event = AikiEvent::ChangePermissionAsked(AikiChangePermissionAskedPayload {
        session,
        cwd: working_dir,
        timestamp: chrono::Utc::now(),
        tool_name: tool_name.to_string(),
        operation,
    });

    // Dispatch to event bus (non-blocking - errors are logged but don't fail the proxy)
    if let Err(e) = event_bus::dispatch(event) {
        eprintln!(
            "Warning: change.permission_asked event bus dispatch failed: {}",
            e
        );
    } else {
        debug_log(|| {
            format!(
                "[acp] Fired change.permission_asked ({}) event for session: {}",
                tool_name, session_id_str
            )
        });
    }

    Ok(())
}

// ============================================================================
// Prompt handling
// ============================================================================

/// Handle session/prompt request and fire turn.started event
///
/// This intercepts the user's prompt, fires a turn.started event, and potentially
/// modifies the prompt before forwarding to the agent. Implements graceful
/// degradation - on any error, forwards the original message.
///
/// Note: Request tracking (TrackPrompt) is done by the caller before this function
/// to ensure turn.completed fires even if turn.started processing fails.
pub fn handle_session_prompt(
    agent_stdin: &Arc<Mutex<std::process::ChildStdin>>,
    msg: &JsonRpcMessage,
    params: &serde_json::Value,
    agent_type: &AgentType,
    cwd: &Option<PathBuf>,
) -> Result<()> {
    // Extract session_id
    let sid = session_id(
        params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
    );

    // Extract all text content from prompt array
    let prompt_array = params
        .get("prompt")
        .and_then(|v| v.as_array())
        .ok_or_else(|| AikiError::Other(anyhow::anyhow!("Missing prompt array")))?;

    let text_chunks = extract_text_from_prompt_array(prompt_array);
    let original_text = concatenate_text_chunks(&text_chunks);

    // Get working directory with fallback
    let working_dir = cwd
        .as_ref()
        .cloned()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));

    // Fire turn.started event
    let session = create_session(*agent_type, sid.to_string(), None::<&str>);
    let event = AikiEvent::TurnStarted(AikiTurnStartedPayload {
        session,
        cwd: working_dir,
        timestamp: chrono::Utc::now(),
        turn: crate::events::Turn::unknown(), // Set by handle_turn_started
        prompt: original_text.clone(),
        injected_refs: vec![],
    });

    let response = event_bus::dispatch(event)?;

    // Emit failures to stderr (user-visible)
    for failure in &response.failures {
        use crate::events::result::Failure;
        let Failure(s) = failure;
        eprintln!("[aiki] ❌ {}", s);
    }

    // Check if blocked
    if response.is_blocking() {
        return Err(AikiError::Other(anyhow::anyhow!(
            "turn.started validation blocked prompt"
        )));
    }

    // Build final prompt: messages + context + original
    let formatted_messages = response.format_messages();
    let prepended_context = response.context.as_deref().unwrap_or("");

    let final_prompt = match (
        !formatted_messages.is_empty(),
        !prepended_context.is_empty(),
    ) {
        (true, true) => {
            // Both messages and context: combine them
            format!(
                "{}\n\n{}\n\n{}",
                formatted_messages, prepended_context, original_text
            )
        }
        (true, false) => {
            // Only messages
            format!("{}\n\n{}", formatted_messages, original_text)
        }
        (false, true) => {
            // Only context
            format!("{}\n\n{}", prepended_context, original_text)
        }
        (false, false) => {
            // Neither: use original prompt
            original_text.to_string()
        }
    };

    // Modify the JSON params to replace prompt text
    // We rebuild the prompt array with a single text entry containing the final prompt,
    // while preserving all non-text resources (images, etc.) to avoid sending duplicate
    // content when the IDE sends multiple text chunks.
    let mut modified_msg = msg.clone();
    if let Some(params_mut) = modified_msg.params.as_mut() {
        if let Some(params_obj) = params_mut.as_object_mut() {
            if let Some(prompt_arr) = params_obj.get_mut("prompt").and_then(|v| v.as_array_mut()) {
                let new_prompt = build_modified_prompt(prompt_arr, &final_prompt);
                *prompt_arr = new_prompt;
            }
        }
    }

    // Note: Request tracking is now done in the caller (before this function is called)
    // to ensure turn.completed fires even if turn.started processing fails (graceful degradation)

    // Forward modified message to agent
    let modified_line = serde_json::to_string(&modified_msg).map_err(|e| {
        AikiError::Other(anyhow::anyhow!(
            "Failed to serialize modified prompt: {}",
            e
        ))
    })?;
    // Serialize outside lock to minimize critical section
    let data = format!("{}\n", modified_line).into_bytes();
    {
        // ✅ FIX for Issue #5: Handle mutex poisoning gracefully
        let mut stdin = match agent_stdin.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                eprintln!("Warning: Mutex poisoned (another thread panicked), attempting recovery");
                poisoned.into_inner()
            }
        };
        stdin.write_all(&data)?;
        stdin.flush()?;
    }

    debug_log(|| {
        format!(
            "[acp] Fired turn.started event for session: {}, modified: {}",
            sid,
            final_prompt != original_text
        )
    });

    Ok(())
}

/// Handle session.ended event and autoreply
///
/// Fires when the agent completes a turn (stopReason: end_turn).
/// Dispatches session.ended event to flows, and if they return an autoreply,
/// sends it back to the agent (up to MAX_AUTOREPLIES times per session).
pub fn handle_session_end(
    sid: &SessionId,
    agent_type: &AgentType,
    cwd: &Option<PathBuf>,
    response_text: &str,
    autoreply_counters: &mut HashMap<SessionId, usize>,
    max_autoreplies: usize,
    autoreply_tx: &mpsc::Sender<AutoreplyMessage>,
    prompt_requests: &mut HashMap<JsonRpcId, SessionId>,
) -> Result<()> {
    // Get working directory with fallback
    let working_dir = cwd
        .as_ref()
        .cloned()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")));

    // Fire turn.completed event with accumulated response text
    let session = create_session(*agent_type, sid.to_string(), None::<&str>);
    let event = AikiEvent::TurnCompleted(AikiTurnCompletedPayload {
        session,
        cwd: working_dir,
        timestamp: chrono::Utc::now(),
        turn: crate::events::Turn::unknown(), // Set by handle_turn_completed
        response: response_text.to_string(),
        modified_files: Vec::new(), // Files tracked separately via change.done events
        tasks: Default::default(), // Populated by handle_turn_completed
    });

    let response = event_bus::dispatch(event)?;

    // Emit failures to stderr (user-visible only)
    for failure in &response.failures {
        use crate::events::result::Failure;
        let Failure(s) = failure;
        eprintln!("[aiki] ❌ {}", s);
    }

    // Check for autoreply (agent-visible via next prompt)
    // Only send autoreply when the flow explicitly sets autoreply context.
    // Failure messages are already shown to user via stderr above.
    let formatted_messages = response.format_messages();
    let autoreply_context = extract_autoreply(&response);

    // Only autoreply if there's explicit autoreply context from the flow
    let autoreply_text = autoreply_context.map(|context| {
        if formatted_messages.is_empty() {
            context
        } else {
            // Combine failure messages with autoreply context
            format!("{}\n\n{}", formatted_messages, context)
        }
    });

    if let Some(autoreply_text) = autoreply_text {
        // Get current autoreply count for this session
        let current_count = autoreply_counters.get(sid).copied().unwrap_or(0);

        if !check_autoreply_limit(current_count, max_autoreplies) {
            // Increment counter for this session
            let new_count = increment_autoreply_counter(autoreply_counters, sid);

            debug_log(|| {
                format!(
                    "[acp] turn.completed autoreply #{} for session {}: {} chars",
                    new_count,
                    sid,
                    autoreply_text.len()
                )
            });

            // Create autoreply message (JSON generated on-demand when sent)
            let autoreply_msg = Autoreply::new(sid, autoreply_text, new_count);

            // Extract debug info before moving
            let debug_request_id = Some(autoreply_msg.raw_request_id_display().to_string());

            // ✅ FIX for Issue #2: Insert into HashMap BEFORE sending to channel
            // This prevents a race condition where the agent responds before we've
            // registered the request ID, causing the turn.completed event to be lost.
            // The correct order is: prepare state first, then trigger the action.
            prompt_requests.insert(
                autoreply_msg.normalized_request_id().clone(),
                Arc::clone(sid),
            );

            // Send via channel to autoreply forwarder thread
            // Autoreplies sent to agent only (not IDE) to avoid race condition
            autoreply_tx
                .send(AutoreplyMessage::SendAutoreply(autoreply_msg))
                .map_err(|e| {
                    AikiError::Other(anyhow::anyhow!("Failed to send autoreply: {}", e))
                })?;

            if let Some(request_id) = debug_request_id {
                debug_log(|| {
                    format!(
                        "[acp] Queued autoreply #{} for session: {} with request_id: {}",
                        new_count, sid, request_id
                    )
                });
            }
        } else if current_count >= max_autoreplies {
            eprintln!(
                "Warning: Maximum autoreplies ({}) reached for session {}, ignoring autoreply from flow",
                max_autoreplies, sid
            );
        }
    } else {
        debug_log(|| {
            format!(
                "[acp] Fired turn.completed event for session: {}, no autoreply",
                sid
            )
        });
    }

    Ok(())
}
