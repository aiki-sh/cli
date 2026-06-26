use serde::Deserialize;
use std::path::PathBuf;

use crate::cache::debug_log;
use crate::error::Result;
use crate::events::{
    parse_mcp_server, AikiChangeCompletedPayload, AikiEvent, AikiMcpCompletedPayload,
    AikiMcpPermissionAskedPayload, AikiSessionEndedPayload, AikiShellCompletedPayload,
    AikiShellPermissionAskedPayload, AikiTurnCompletedPayload, AikiTurnStartedPayload,
    ChangeOperation, WriteOperation,
};

use super::session::create_session;

// ============================================================================
// Hook Payload Structures (matches Cursor API)
// See: https://cursor.com/docs/agent/hooks
// ============================================================================

/// Cursor hook event - discriminated by eventName
#[derive(Deserialize, Debug)]
#[serde(tag = "eventName")]
enum CursorEvent {
    #[serde(rename = "beforeSubmitPrompt")]
    BeforeSubmitPrompt {
        #[serde(flatten)]
        payload: BeforeSubmitPromptPayload,
    },
    #[serde(rename = "stop")]
    Stop {
        #[serde(flatten)]
        payload: StopPayload,
    },
    #[serde(rename = "beforeShellExecution")]
    BeforeShellExecution {
        #[serde(flatten)]
        payload: BeforeShellExecutionPayload,
    },
    #[serde(rename = "afterShellExecution")]
    AfterShellExecution {
        #[serde(flatten)]
        payload: AfterShellExecutionPayload,
    },
    #[serde(rename = "beforeMCPExecution")]
    BeforeMcpExecution {
        #[serde(flatten)]
        payload: BeforeMcpExecutionPayload,
    },
    #[serde(rename = "afterMCPExecution")]
    AfterMcpExecution {
        #[serde(flatten)]
        payload: AfterMcpExecutionPayload,
    },
    #[serde(rename = "afterFileEdit")]
    AfterFileEdit {
        #[serde(flatten)]
        payload: AfterFileEditPayload,
    },
    #[serde(rename = "sessionEnd")]
    SessionEnd {
        #[serde(flatten)]
        payload: SessionEndPayload,
    },
}

/// beforeSubmitPrompt hook payload
#[derive(Deserialize, Debug)]
struct BeforeSubmitPromptPayload {
    #[serde(rename = "conversationId")]
    conversation_id: String,
    #[serde(rename = "cursorVersion")]
    cursor_version: String,
    #[serde(rename = "workspaceRoots")]
    workspace_roots: Vec<String>,
    #[serde(default)]
    prompt: String,
}

/// stop hook payload
#[derive(Deserialize, Debug)]
struct StopPayload {
    #[serde(rename = "conversationId")]
    conversation_id: String,
    #[serde(rename = "cursorVersion")]
    cursor_version: String,
    #[serde(rename = "workspaceRoots")]
    workspace_roots: Vec<String>,
}

/// beforeShellExecution hook payload
#[derive(Deserialize, Debug)]
struct BeforeShellExecutionPayload {
    #[serde(rename = "conversationId")]
    conversation_id: String,
    #[serde(rename = "cursorVersion")]
    cursor_version: String,
    command: String,
    cwd: String,
}

/// afterShellExecution hook payload
#[derive(Deserialize, Debug)]
struct AfterShellExecutionPayload {
    #[serde(rename = "conversationId")]
    conversation_id: String,
    #[serde(rename = "cursorVersion")]
    cursor_version: String,
    #[serde(rename = "workspaceRoots")]
    workspace_roots: Vec<String>,
    command: String,
    output: String,
}

/// beforeMCPExecution hook payload
#[derive(Deserialize, Debug)]
pub struct BeforeMcpExecutionPayload {
    #[serde(rename = "conversationId")]
    pub conversation_id: String,
    #[serde(rename = "cursorVersion")]
    pub cursor_version: String,
    #[serde(rename = "workspaceRoots")]
    pub workspace_roots: Vec<String>,
    #[serde(rename = "toolName")]
    pub tool_name: String,
    #[serde(rename = "toolInput")]
    pub tool_input: String,
}

/// afterMCPExecution hook payload
#[derive(Deserialize, Debug)]
struct AfterMcpExecutionPayload {
    #[serde(rename = "conversationId")]
    conversation_id: String,
    #[serde(rename = "cursorVersion")]
    cursor_version: String,
    #[serde(rename = "workspaceRoots")]
    workspace_roots: Vec<String>,
    #[serde(rename = "toolName")]
    tool_name: String,
    #[serde(rename = "resultJson")]
    result_json: String,
}

/// afterFileEdit hook payload
#[derive(Deserialize, Debug)]
struct AfterFileEditPayload {
    #[serde(rename = "conversationId")]
    conversation_id: String,
    #[serde(rename = "cursorVersion")]
    cursor_version: String,
    #[serde(rename = "workspaceRoots")]
    workspace_roots: Vec<String>,
    #[serde(rename = "filePath")]
    file_path: String,
    edits: Vec<EditPayload>,
}

/// Individual edit operation in Cursor's afterFileEdit hook
#[derive(Deserialize, Debug)]
struct EditPayload {
    old_string: String,
    new_string: String,
}

/// sessionEnd hook payload
///
/// Cursor fires this when the session terminates.
/// Reasons: "completed", "aborted", "error", "window_close", "user_close"
#[derive(Deserialize, Debug)]
struct SessionEndPayload {
    #[serde(rename = "conversationId")]
    conversation_id: String,
    #[serde(rename = "cursorVersion")]
    cursor_version: String,
    #[serde(rename = "workspaceRoots")]
    workspace_roots: Vec<String>,
    /// Reason for session termination
    #[serde(default)]
    reason: String,
}

// ============================================================================
// Event Building
// ============================================================================

/// Build AikiEvent from Cursor event read from stdin
pub fn build_aiki_event_from_stdin() -> Result<AikiEvent> {
    // Parse event - serde discriminates by eventName
    let event: CursorEvent = super::super::read_stdin_json()?;
    Ok(cursor_event_to_aiki(event))
}

/// Convert a parsed `CursorEvent` into an `AikiEvent`.
///
/// Split out from the stdin read so the deserialize-and-dispatch path can be
/// driven directly in tests (mirrors `claude_code::events::claude_event_to_aiki`).
fn cursor_event_to_aiki(event: CursorEvent) -> AikiEvent {
    match event {
        CursorEvent::BeforeSubmitPrompt { payload } => build_turn_started_event(payload),
        CursorEvent::BeforeShellExecution { payload } => {
            build_shell_permission_asked_event(payload)
        }
        CursorEvent::AfterShellExecution { payload } => build_shell_completed_event(payload),
        CursorEvent::BeforeMcpExecution { payload } => build_mcp_permission_asked_event(payload),
        CursorEvent::AfterMcpExecution { payload } => build_mcp_completed_event(payload),
        CursorEvent::AfterFileEdit { payload } => build_change_completed_event(payload),
        CursorEvent::Stop { payload } => build_turn_completed_event(payload),
        CursorEvent::SessionEnd { payload } => build_session_ended_event(payload),
    }
}

/// Build turn.started event from beforeSubmitPrompt payload
///
/// Note: Cursor's beforeSubmitPrompt fires on EVERY prompt submission.
/// Ideally we should track conversation_id changes to fire session.started only
/// on new conversations, but that requires stateful tracking across invocations.
/// For now, we fire turn.started on every call, which enables validation workflows.
///
/// Limitation: Cursor's beforeSubmitPrompt can only BLOCK prompts, not modify them.
/// The modifiedPrompt field is not supported - only blocking via user_message.
fn build_turn_started_event(payload: BeforeSubmitPromptPayload) -> AikiEvent {
    AikiEvent::TurnStarted(AikiTurnStartedPayload {
        session: create_session(&payload.conversation_id, &payload.cursor_version),
        cwd: get_cwd(&payload.workspace_roots),
        timestamp: chrono::Utc::now(),
        turn: crate::events::Turn::unknown(), // Set by handle_turn_started
        prompt: payload.prompt,
        injected_refs: vec![],
    })
}

/// Build shell.permission_asked event from beforeShellExecution payload
fn build_shell_permission_asked_event(payload: BeforeShellExecutionPayload) -> AikiEvent {
    AikiEvent::ShellPermissionAsked(AikiShellPermissionAskedPayload {
        session: create_session(&payload.conversation_id, &payload.cursor_version),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        command: payload.command,
    })
}

/// Build shell.completed event from afterShellExecution payload
fn build_shell_completed_event(payload: AfterShellExecutionPayload) -> AikiEvent {
    AikiEvent::ShellCompleted(AikiShellCompletedPayload {
        session: create_session(&payload.conversation_id, &payload.cursor_version),
        cwd: get_cwd(&payload.workspace_roots),
        timestamp: chrono::Utc::now(),
        command: payload.command,
        // Cursor doesn't provide exit code - assume success
        success: true,
        exit_code: None,
        // Cursor combines stdout/stderr in output field
        stdout: Some(payload.output),
        stderr: None,
    })
}

/// Build mcp.permission_asked event from beforeMCPExecution payload (non-file tools)
fn build_mcp_permission_asked_event(payload: BeforeMcpExecutionPayload) -> AikiEvent {
    // Parse tool_input as JSON if possible
    let parameters = serde_json::from_str(&payload.tool_input).unwrap_or(serde_json::Value::Null);
    let server = parse_mcp_server(&payload.tool_name);

    AikiEvent::McpPermissionAsked(AikiMcpPermissionAskedPayload {
        session: create_session(&payload.conversation_id, &payload.cursor_version),
        cwd: get_cwd(&payload.workspace_roots),
        timestamp: chrono::Utc::now(),
        server,
        tool_name: payload.tool_name,
        parameters,
    })
}

/// Build mcp.completed event from afterMCPExecution payload
fn build_mcp_completed_event(payload: AfterMcpExecutionPayload) -> AikiEvent {
    let server = parse_mcp_server(&payload.tool_name);

    AikiEvent::McpCompleted(AikiMcpCompletedPayload {
        session: create_session(&payload.conversation_id, &payload.cursor_version),
        cwd: get_cwd(&payload.workspace_roots),
        timestamp: chrono::Utc::now(),
        server,
        tool_name: payload.tool_name,
        success: true, // Cursor doesn't indicate failure in hook payload
        result: if payload.result_json.is_empty() {
            None
        } else {
            Some(payload.result_json)
        },
    })
}

/// Build change.completed event from afterFileEdit payload
fn build_change_completed_event(payload: AfterFileEditPayload) -> AikiEvent {
    // Create session first before moving any fields
    let session = create_session(&payload.conversation_id, &payload.cursor_version);
    let cwd = get_cwd(&payload.workspace_roots);
    let file_path = payload.file_path;

    // Extract edit details from Cursor's edits array for user edit detection
    let edit_details: Vec<crate::events::EditDetail> = payload
        .edits
        .iter()
        .map(|edit| {
            crate::events::EditDetail::new(
                file_path.clone(),
                edit.old_string.clone(),
                edit.new_string.clone(),
            )
        })
        .collect();

    if !edit_details.is_empty() {
        debug_log(|| format!("Cursor provided {} edits", edit_details.len()));
    }

    AikiEvent::ChangeCompleted(AikiChangeCompletedPayload {
        session,
        cwd,
        timestamp: chrono::Utc::now(),
        tool_name: "edit".to_string(), // Cursor doesn't distinguish Edit/Write
        success: true,                 // afterFileEdit implies success
        turn: crate::events::Turn::unknown(), // Cursor events don't have turn context
        operation: ChangeOperation::Write(WriteOperation {
            file_paths: vec![file_path],
            edit_details,
        }),
    })
}

/// Build turn.completed event from stop payload
///
/// # Token usage is unavailable (documented gap, defect B1)
///
/// `tokens` is deliberately `None`, not a missing TODO. Cursor exposes no token
/// usage on the surface aiki consumes:
/// - Its vendor hooks (this `stop` payload, `sessionEnd`, `afterAgentResponse`)
///   carry no usage fields — only model / status / duration.
/// - Its transcript file (`transcript_path`) records tool inputs, not token
///   counts.
/// - The Cursor CLI's `--output-format stream-json` *does* now emit a final
///   usage figure, but aiki has no Cursor execution runtime
///   (`harnesses/cursor` is `runtime: None`, "task execution not yet
///   supported") and never drives that CLI, so it is unreachable from here.
///
/// `None` is therefore the explicit "usage unavailable" state. The display
/// renders it as such (defect C3) rather than as a silent `0`. See
/// `ops/now/token-tracking-fixes.md` (B1) for the full rationale.
fn build_turn_completed_event(payload: StopPayload) -> AikiEvent {
    AikiEvent::TurnCompleted(AikiTurnCompletedPayload {
        session: create_session(&payload.conversation_id, &payload.cursor_version),
        cwd: get_cwd(&payload.workspace_roots),
        timestamp: chrono::Utc::now(),
        turn: crate::events::Turn::unknown(), // Set by handle_turn_completed
        response: String::new(),              // Cursor doesn't provide response text in stop hook
        modified_files: Vec::new(),           // Cursor doesn't track modified files in stop hook
        tasks: Default::default(),            // Populated by handle_turn_completed
        tokens: None,                         // usage unavailable — see fn doc (defect B1)
        model: None,
    })
}

/// Build session.ended event from sessionEnd payload
///
/// `tokens` is `None` for the same reason as the turn payload: Cursor reports no
/// usage on the hooks surface aiki consumes (see [`build_turn_completed_event`]).
/// Because every Cursor turn is also `None`, `aggregate_session_tokens` finds
/// nothing to sum and keeps the session total `None`, so the display shows
/// "usage unavailable" rather than a silent `0` (defects B1 / C3).
fn build_session_ended_event(payload: SessionEndPayload) -> AikiEvent {
    AikiEvent::SessionEnded(AikiSessionEndedPayload {
        session: create_session(&payload.conversation_id, &payload.cursor_version),
        cwd: get_cwd(&payload.workspace_roots),
        timestamp: chrono::Utc::now(),
        reason: payload.reason,
        tokens: None, // usage unavailable — see fn doc (defect B1)
    })
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Get working directory from workspace roots
/// Takes the first workspace root, or current directory as fallback
fn get_cwd(workspace_roots: &[String]) -> PathBuf {
    workspace_roots
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Cursor's `stop` hook carries no token usage (documented gap B1), so the
    /// turn.completed event it produces must surface that as `tokens: None` — the
    /// explicit "usage unavailable" state — never a fabricated `0`. Drives the
    /// real deserialize → dispatch path from a realistic hook payload.
    #[test]
    fn stop_hook_reports_token_usage_unavailable() {
        let json = r#"{
            "eventName": "stop",
            "conversationId": "conv-abc",
            "cursorVersion": "0.45.0",
            "workspaceRoots": ["/tmp/project"]
        }"#;

        let event: CursorEvent =
            serde_json::from_str(json).expect("stop hook payload deserializes");

        match cursor_event_to_aiki(event) {
            AikiEvent::TurnCompleted(payload) => assert!(
                payload.tokens.is_none(),
                "Cursor exposes no usage; turn.completed must be unavailable (None), not 0"
            ),
            other => panic!("expected TurnCompleted, got {other:?}"),
        }
    }

    /// Cursor's `sessionEnd` hook likewise carries no usage. With every turn also
    /// `None`, `aggregate_session_tokens` keeps the session total `None`, which the
    /// display renders as "usage unavailable" (B1 / C3).
    #[test]
    fn session_end_hook_reports_token_usage_unavailable() {
        let json = r#"{
            "eventName": "sessionEnd",
            "conversationId": "conv-abc",
            "cursorVersion": "0.45.0",
            "workspaceRoots": ["/tmp/project"],
            "reason": "completed"
        }"#;

        let event: CursorEvent =
            serde_json::from_str(json).expect("sessionEnd hook payload deserializes");

        match cursor_event_to_aiki(event) {
            AikiEvent::SessionEnded(payload) => {
                assert!(
                    payload.tokens.is_none(),
                    "Cursor exposes no usage; session.ended must be unavailable (None), not 0"
                );
                assert_eq!(payload.reason, "completed");
            }
            other => panic!("expected SessionEnded, got {other:?}"),
        }
    }
}
