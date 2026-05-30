use serde::Deserialize;
use std::path::PathBuf;

use crate::cache::debug_log;
use crate::error::Result;
use crate::events::FileOperation;
use crate::events::{
    parse_mcp_server, AikiChangeCompletedPayload, AikiChangePermissionAskedPayload, AikiEvent,
    AikiMcpCompletedPayload, AikiMcpPermissionAskedPayload, AikiReadCompletedPayload,
    AikiReadPermissionAskedPayload, AikiSessionClearedPayload, AikiSessionCompactedPayload,
    AikiSessionEndedPayload, AikiSessionResumedPayload, AikiSessionStartPayload,
    AikiSessionWillCompactPayload, AikiShellCompletedPayload, AikiShellPermissionAskedPayload,
    AikiTurnCompletedPayload, AikiTurnStartedPayload, AikiWebCompletedPayload,
    AikiWebPermissionAskedPayload, ChangeOperation, DeleteOperation, MoveOperation,
    WriteOperation,
};
use crate::tools::ToolType;

use super::session::create_session;
use super::tools::{BashToolResponse, ClaudeTool};

// ============================================================================
// Hook Payload Structures (matches Claude Code API)
// See: https://code.claude.com/docs/en/hooks
// ============================================================================

/// Claude Code hook event - discriminated by hook_event_name
#[derive(Deserialize, Debug)]
#[serde(tag = "hook_event_name")]
enum ClaudeEvent {
    #[serde(rename = "SessionStart")]
    SessionStart {
        #[serde(flatten)]
        payload: SessionStartPayload,
    },
    #[serde(rename = "UserPromptSubmit")]
    UserPromptSubmit {
        #[serde(flatten)]
        payload: UserPromptSubmitPayload,
    },
    #[serde(rename = "PreToolUse")]
    PreToolUse {
        #[serde(flatten)]
        payload: PreToolUsePayload,
    },
    #[serde(rename = "PostToolUse")]
    PostToolUse {
        #[serde(flatten)]
        payload: PostToolUsePayload,
    },
    #[serde(rename = "Stop")]
    Stop {
        #[serde(flatten)]
        payload: StopPayload,
    },
    #[serde(rename = "PreCompact")]
    PreCompact {
        #[serde(flatten)]
        payload: PreCompactPayload,
    },
    #[serde(rename = "SessionEnd")]
    SessionEnd {
        #[serde(flatten)]
        payload: SessionEndPayload,
    },
}

/// SessionStart hook payload
///
/// Claude Code provides a `source` field indicating how the session started:
/// - "startup" - New session started
/// - "resume" - Session resumed (from --resume, --continue, or /resume)
/// - "clear" - Session after /clear command
/// - "compact" - Session after compaction
#[derive(Deserialize, Debug)]
struct SessionStartPayload {
    session_id: String,
    cwd: String,
    /// Source of the session start (startup, resume, clear, compact)
    #[serde(default = "default_session_source")]
    source: String,
}

fn default_session_source() -> String {
    "startup".to_string()
}

/// UserPromptSubmit hook payload
#[derive(Deserialize, Debug)]
struct UserPromptSubmitPayload {
    session_id: String,
    cwd: String,
    #[serde(default)]
    prompt: String,
}

/// PreToolUse hook payload
#[derive(Deserialize, Debug)]
pub struct PreToolUsePayload {
    pub session_id: String,
    pub cwd: String,
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Option<serde_json::Value>,
}

/// PostToolUse hook payload
#[derive(Deserialize, Debug)]
pub struct PostToolUsePayload {
    pub session_id: String,
    pub cwd: String,
    pub tool_name: String,
    #[serde(default)]
    pub tool_input: Option<serde_json::Value>,
    #[serde(default)]
    pub tool_response: Option<serde_json::Value>,
}

/// Stop hook payload
#[derive(Deserialize, Debug)]
struct StopPayload {
    session_id: String,
    cwd: String,
    /// Path to JSONL transcript file containing the full conversation
    #[serde(default)]
    transcript_path: Option<String>,
}

/// SessionEnd hook payload
///
/// Claude Code fires this when the session terminates.
/// Reasons: "clear", "logout", "prompt_input_exit", "other"
#[derive(Deserialize, Debug)]
struct SessionEndPayload {
    session_id: String,
    cwd: String,
    /// Reason for session termination
    #[serde(default = "default_session_end_reason")]
    reason: String,
}

fn default_session_end_reason() -> String {
    "other".to_string()
}

/// PreCompact hook payload
///
/// Claude Code fires this before compaction. The trigger field indicates
/// whether compaction was manual (/compact) or automatic (context window full).
#[derive(Deserialize, Debug)]
struct PreCompactPayload {
    session_id: String,
    cwd: String,
    /// Trigger for compaction: "manual" or "auto"
    #[serde(default)]
    trigger: String,
}

// ============================================================================
// Event Building
// ============================================================================

/// Build AikiEvent from Claude Code event read from stdin
pub fn build_aiki_event_from_stdin() -> Result<AikiEvent> {
    // Parse event - serde discriminates by hook_event_name
    let event: ClaudeEvent = super::super::read_stdin_json()?;

    let aiki_event = match event {
        ClaudeEvent::SessionStart { payload } => build_session_started_event(payload),
        ClaudeEvent::UserPromptSubmit { payload } => build_turn_started_event(payload),
        ClaudeEvent::PreToolUse { payload } => build_permission_asked_event_for_tool_type(payload),
        ClaudeEvent::PostToolUse { payload } => build_completed_event_for_tool_type(payload),
        ClaudeEvent::PreCompact { payload } => build_session_will_compact_event(payload),
        ClaudeEvent::Stop { payload } => build_turn_completed_event(payload),
        ClaudeEvent::SessionEnd { payload } => build_session_ended_event(payload),
    };

    Ok(aiki_event)
}

/// Build appropriate pre-tool event based on tool type
fn build_permission_asked_event_for_tool_type(payload: PreToolUsePayload) -> AikiEvent {
    let tool = ClaudeTool::parse(&payload.tool_name, payload.tool_input.as_ref());

    match tool.tool_type() {
        ToolType::File => build_file_permission_asked_event(payload, tool),
        ToolType::Shell => build_shell_permission_asked_event(payload, tool),
        ToolType::Mcp => build_mcp_permission_asked_event(payload),
        ToolType::Web => build_web_permission_asked_event(payload, tool),
        ToolType::Internal => AikiEvent::Unsupported,
    }
}

/// Build appropriate post-tool event based on tool type
fn build_completed_event_for_tool_type(payload: PostToolUsePayload) -> AikiEvent {
    let tool = ClaudeTool::parse(&payload.tool_name, payload.tool_input.as_ref());

    match tool.tool_type() {
        ToolType::File => build_file_completed_event(payload, tool),
        ToolType::Shell => build_shell_completed_event(payload, tool),
        ToolType::Mcp => build_mcp_completed_event(payload),
        ToolType::Web => build_web_completed_event(payload, tool),
        ToolType::Internal => AikiEvent::Unsupported,
    }
}

/// Build session event based on SessionStart source field
///
/// Claude Code emits SessionStart for all session lifecycle events.
/// The `source` field distinguishes them:
/// - "startup" → session.started event
/// - "resume" → session.resumed event
/// - "compact" → session.compacted event
/// - "clear" → session.cleared event
fn build_session_started_event(payload: SessionStartPayload) -> AikiEvent {
    let session = create_session(&payload.session_id, &payload.cwd);
    let cwd = PathBuf::from(&payload.cwd);
    let timestamp = chrono::Utc::now();

    match payload.source.as_str() {
        "resume" => AikiEvent::SessionResumed(AikiSessionResumedPayload {
            session,
            cwd,
            timestamp,
        }),
        "compact" => AikiEvent::SessionCompacted(AikiSessionCompactedPayload {
            session,
            cwd,
            timestamp,
        }),
        "clear" => AikiEvent::SessionCleared(AikiSessionClearedPayload {
            session,
            cwd,
            timestamp,
        }),
        _ => AikiEvent::SessionStarted(AikiSessionStartPayload {
            session,
            cwd,
            timestamp,
        }),
    }
}

/// Build session.will_compact event (maps from PreCompact hook)
fn build_session_will_compact_event(payload: PreCompactPayload) -> AikiEvent {
    debug_log(|| format!("PreCompact trigger: {}", payload.trigger));
    let session = create_session(&payload.session_id, &payload.cwd);
    AikiEvent::SessionWillCompact(AikiSessionWillCompactPayload {
        session,
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
    })
}

/// Build turn.started event (maps from UserPromptSubmit hook)
fn build_turn_started_event(payload: UserPromptSubmitPayload) -> AikiEvent {
    AikiEvent::TurnStarted(AikiTurnStartedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        turn: crate::events::Turn::unknown(), // Set by handle_turn_started
        prompt: payload.prompt,
        injected_refs: vec![],
    })
}

/// Build file.permission_asked event for all file tools
fn build_file_permission_asked_event(payload: PreToolUsePayload, tool: ClaudeTool) -> AikiEvent {
    // Extra safety check - should never happen due to tool_type() dispatch
    if !matches!(tool.tool_type(), ToolType::File) {
        eprintln!("[aiki] Error: build_file_permission_asked_event called on non-file tool");
        return AikiEvent::Unsupported;
    }

    let Some(operation) = tool.file_operation() else {
        eprintln!("[aiki] Error: Failed to get file operation");
        return AikiEvent::Unsupported;
    };

    match operation {
        FileOperation::Write => build_change_permission_asked_event_write(payload, tool),
        FileOperation::Read => build_read_permission_asked_event(payload, tool),
        FileOperation::Delete => build_change_permission_asked_event_delete(payload, tool),
        FileOperation::Move => build_change_permission_asked_event_move(payload, tool),
    }
}

/// Build change.permission_asked event for write operations (Edit, Write, NotebookEdit, MultiEdit)
fn build_change_permission_asked_event_write(
    payload: PreToolUsePayload,
    tool: ClaudeTool,
) -> AikiEvent {
    let file_paths = match tool {
        ClaudeTool::Edit(input) | ClaudeTool::Write(input) | ClaudeTool::NotebookEdit(input) => {
            vec![input.file_path]
        }
        ClaudeTool::MultiEdit(input) => {
            // MultiEdit affects multiple files
            input.edits.iter().map(|e| e.file_path.clone()).collect()
        }
        ClaudeTool::Unknown(name) => {
            eprintln!("[aiki] Warning: Failed to parse tool input for '{}'", name);
            Vec::new()
        }
        _ => {
            eprintln!(
                "[aiki] Warning: Unexpected tool type in change.permission_asked (write)"
            );
            Vec::new()
        }
    };

    AikiEvent::ChangePermissionAsked(AikiChangePermissionAskedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        tool_name: payload.tool_name,
        operation: ChangeOperation::Write(WriteOperation {
            file_paths,
            edit_details: vec![], // Edit details not available at permission time
        }),
    })
}

/// Build change.permission_asked event for delete operations
///
/// Claude Code doesn't currently have a dedicated delete file tool (deletes come
/// through shell commands like rm/rmdir), but we implement this handler properly
/// for future compatibility and to ensure the event pipeline doesn't drop operations.
fn build_change_permission_asked_event_delete(
    payload: PreToolUsePayload,
    tool: ClaudeTool,
) -> AikiEvent {
    // Extract file paths from tool - if no paths available, use empty list
    let file_paths = match tool {
        ClaudeTool::Edit(input) | ClaudeTool::Write(input) | ClaudeTool::NotebookEdit(input) => {
            vec![input.file_path]
        }
        ClaudeTool::Unknown(name) => {
            eprintln!(
                "[aiki] Warning: Delete permission with unknown tool '{}', no paths available",
                name
            );
            Vec::new()
        }
        _ => {
            // For other tool types, we can't extract paths
            debug_log(|| "[aiki] Delete permission with no extractable paths");
            Vec::new()
        }
    };

    AikiEvent::ChangePermissionAsked(AikiChangePermissionAskedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        tool_name: payload.tool_name,
        operation: ChangeOperation::Delete(DeleteOperation { file_paths }),
    })
}

/// Build change.permission_asked event for move operations
///
/// Claude Code doesn't currently have a dedicated move/rename tool (moves come
/// through shell commands like mv), but we implement this handler properly
/// for future compatibility and to ensure the event pipeline doesn't drop operations.
fn build_change_permission_asked_event_move(
    payload: PreToolUsePayload,
    tool: ClaudeTool,
) -> AikiEvent {
    // Extract source/destination paths from tool - if no paths available, use empty lists
    let (source_paths, destination_paths) = match tool {
        ClaudeTool::Edit(input) | ClaudeTool::Write(input) | ClaudeTool::NotebookEdit(input) => {
            // Single file tool can only represent source
            (vec![input.file_path], Vec::new())
        }
        ClaudeTool::Unknown(name) => {
            eprintln!(
                "[aiki] Warning: Move permission with unknown tool '{}', no paths available",
                name
            );
            (Vec::new(), Vec::new())
        }
        _ => {
            // For other tool types, we can't extract paths
            debug_log(|| "[aiki] Move permission with no extractable paths");
            (Vec::new(), Vec::new())
        }
    };

    AikiEvent::ChangePermissionAsked(AikiChangePermissionAskedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        tool_name: payload.tool_name,
        operation: ChangeOperation::Move(MoveOperation {
            file_paths: destination_paths.clone(),
            source_paths,
            destination_paths,
        }),
    })
}

/// Build read.permission_asked event for read operations (Read, LS, Glob, Grep)
fn build_read_permission_asked_event(payload: PreToolUsePayload, tool: ClaudeTool) -> AikiEvent {
    let (file_paths, pattern) = match tool {
        ClaudeTool::Read(input) => (vec![input.file_path], None),
        ClaudeTool::Glob(input) => {
            // Glob with no path means search from current directory
            let path = input.path.unwrap_or_else(|| payload.cwd.clone());
            (vec![path], Some(input.pattern))
        }
        ClaudeTool::Grep(input) => {
            // Grep with no path means search from current directory
            let path = input.path.unwrap_or_else(|| payload.cwd.clone());
            (vec![path], Some(input.pattern))
        }
        ClaudeTool::LS(input) => {
            // LS with no path means list current directory
            let path = input.path.unwrap_or_else(|| payload.cwd.clone());
            (vec![path], None)
        }
        ClaudeTool::Unknown(name) => {
            eprintln!("[aiki] Warning: Failed to parse tool input for '{}'", name);
            (Vec::new(), None)
        }
        _ => {
            eprintln!("[aiki] Warning: Unexpected tool type in read.permission_asked");
            (Vec::new(), None)
        }
    };

    AikiEvent::ReadPermissionAsked(AikiReadPermissionAskedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        tool_name: payload.tool_name,
        file_paths,
        pattern,
    })
}

/// Build file.completed event for all file tools
fn build_file_completed_event(payload: PostToolUsePayload, tool: ClaudeTool) -> AikiEvent {
    // Extra safety check - should never happen due to tool_type() dispatch
    if !matches!(tool.tool_type(), ToolType::File) {
        eprintln!("[aiki] Error: build_file_completed_event called on non-file tool");
        return AikiEvent::Unsupported;
    }

    let Some(operation) = tool.file_operation() else {
        eprintln!("[aiki] Error: Failed to get file operation");
        return AikiEvent::Unsupported;
    };

    match operation {
        FileOperation::Write => build_change_completed_event_write(payload, tool),
        FileOperation::Read => build_read_completed_event(payload, tool),
        FileOperation::Delete => build_change_completed_event_delete(payload, tool),
        FileOperation::Move => build_change_completed_event_move(payload, tool),
    }
}

/// Build change.completed event for write operations (Edit, Write, NotebookEdit, MultiEdit)
fn build_change_completed_event_write(payload: PostToolUsePayload, tool: ClaudeTool) -> AikiEvent {
    let (file_paths, edit_details) = match tool {
        ClaudeTool::Edit(input) | ClaudeTool::NotebookEdit(input) => {
            // Edit/NotebookEdit use old_string/new_string for replacements
            let details = if !input.old_string.is_empty() || !input.new_string.is_empty() {
                vec![crate::events::EditDetail::new(
                    input.file_path.clone(),
                    input.old_string.clone(),
                    input.new_string.clone(),
                )]
            } else {
                Vec::new()
            };
            (vec![input.file_path], details)
        }
        ClaudeTool::Write(input) => {
            // Write tool uses content field for full file writes
            let details = if !input.content.is_empty() {
                vec![crate::events::EditDetail::new(
                    input.file_path.clone(),
                    String::new(),
                    input.content.clone(),
                )]
            } else {
                Vec::new()
            };
            (vec![input.file_path], details)
        }
        ClaudeTool::MultiEdit(input) => {
            // MultiEdit performs atomic edits across multiple files
            let paths: Vec<String> = input.edits.iter().map(|e| e.file_path.clone()).collect();
            let details: Vec<crate::events::EditDetail> = input
                .edits
                .into_iter()
                .filter(|e| !e.old_string.is_empty() || !e.new_string.is_empty())
                .map(|e| crate::events::EditDetail::new(e.file_path, e.old_string, e.new_string))
                .collect();
            (paths, details)
        }
        ClaudeTool::Unknown(name) => {
            eprintln!("[aiki] Warning: Failed to parse tool input for '{}'", name);
            return AikiEvent::Unsupported;
        }
        _ => {
            eprintln!("[aiki] Warning: Unexpected tool type in change.completed (write)");
            return AikiEvent::Unsupported;
        }
    };

    AikiEvent::ChangeCompleted(AikiChangeCompletedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        tool_name: payload.tool_name,
        success: true,
        turn: crate::events::Turn::unknown(), // Turn info not available in PostToolUse hook
        operation: ChangeOperation::Write(WriteOperation {
            file_paths,
            edit_details,
        }),
    })
}

/// Build change.completed event for delete operations
///
/// Claude Code doesn't currently have a dedicated delete file tool (deletes come
/// through shell commands like rm/rmdir), but we implement this handler properly
/// for future compatibility and to ensure the event pipeline doesn't drop operations.
fn build_change_completed_event_delete(
    payload: PostToolUsePayload,
    tool: ClaudeTool,
) -> AikiEvent {
    // Extract file paths from tool - if no paths available, use empty list
    let file_paths = match tool {
        ClaudeTool::Edit(input) | ClaudeTool::Write(input) | ClaudeTool::NotebookEdit(input) => {
            vec![input.file_path]
        }
        ClaudeTool::Unknown(name) => {
            eprintln!(
                "[aiki] Warning: Delete operation with unknown tool '{}', no paths available",
                name
            );
            Vec::new()
        }
        _ => {
            // For other tool types, we can't extract paths
            debug_log(|| "[aiki] Delete operation with no extractable paths");
            Vec::new()
        }
    };

    AikiEvent::ChangeCompleted(AikiChangeCompletedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        tool_name: payload.tool_name,
        success: true,
        turn: crate::events::Turn::unknown(), // Turn info not available in PostToolUse hook
        operation: ChangeOperation::Delete(DeleteOperation { file_paths }),
    })
}

/// Build change.completed event for move operations
///
/// Claude Code doesn't currently have a dedicated move/rename tool (moves come
/// through shell commands like mv), but we implement this handler properly
/// for future compatibility and to ensure the event pipeline doesn't drop operations.
fn build_change_completed_event_move(
    payload: PostToolUsePayload,
    tool: ClaudeTool,
) -> AikiEvent {
    // Extract source/destination paths from tool - if no paths available, use empty lists
    let (source_paths, destination_paths) = match tool {
        ClaudeTool::Edit(input) | ClaudeTool::Write(input) | ClaudeTool::NotebookEdit(input) => {
            // Single file tool can only represent source
            (vec![input.file_path], Vec::new())
        }
        ClaudeTool::Unknown(name) => {
            eprintln!(
                "[aiki] Warning: Move operation with unknown tool '{}', no paths available",
                name
            );
            (Vec::new(), Vec::new())
        }
        _ => {
            // For other tool types, we can't extract paths
            debug_log(|| "[aiki] Move operation with no extractable paths");
            (Vec::new(), Vec::new())
        }
    };

    AikiEvent::ChangeCompleted(AikiChangeCompletedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        tool_name: payload.tool_name,
        success: true,
        turn: crate::events::Turn::unknown(), // Turn info not available in PostToolUse hook
        operation: ChangeOperation::Move(MoveOperation {
            file_paths: destination_paths.clone(),
            source_paths,
            destination_paths,
        }),
    })
}

/// Build read.completed event for read operations (Read, LS, Glob, Grep)
fn build_read_completed_event(payload: PostToolUsePayload, tool: ClaudeTool) -> AikiEvent {
    let file_paths = match tool {
        ClaudeTool::Read(input) => vec![input.file_path],
        ClaudeTool::Glob(input) => {
            // Glob with no path means search from current directory
            vec![input.path.unwrap_or_else(|| payload.cwd.clone())]
        }
        ClaudeTool::Grep(input) => {
            // Grep with no path means search from current directory
            vec![input.path.unwrap_or_else(|| payload.cwd.clone())]
        }
        ClaudeTool::LS(input) => {
            // LS with no path means list current directory
            vec![input.path.unwrap_or_else(|| payload.cwd.clone())]
        }
        ClaudeTool::Unknown(name) => {
            eprintln!("[aiki] Warning: Failed to parse tool input for '{}'", name);
            return AikiEvent::Unsupported;
        }
        _ => {
            eprintln!("[aiki] Warning: Unexpected tool type in read.completed");
            return AikiEvent::Unsupported;
        }
    };

    AikiEvent::ReadCompleted(AikiReadCompletedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        tool_name: payload.tool_name,
        file_paths,
        success: true,
    })
}

/// Build shell.permission_asked event (Bash tool)
fn build_shell_permission_asked_event(payload: PreToolUsePayload, tool: ClaudeTool) -> AikiEvent {
    let command = match tool {
        ClaudeTool::Bash(input) => input.command,
        ClaudeTool::Unknown(_) => {
            eprintln!("[aiki] Warning: Failed to parse Bash tool input");
            String::new()
        }
        _ => {
            eprintln!("[aiki] Warning: Unexpected tool type in shell.permission_asked");
            String::new()
        }
    };

    AikiEvent::ShellPermissionAsked(AikiShellPermissionAskedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        command,
    })
}

/// Build shell.completed event (Bash tool)
fn build_shell_completed_event(payload: PostToolUsePayload, tool: ClaudeTool) -> AikiEvent {
    let command = match tool {
        ClaudeTool::Bash(input) => input.command,
        ClaudeTool::Unknown(_) => {
            eprintln!("[aiki] Warning: Failed to parse Bash tool input");
            String::new()
        }
        _ => {
            eprintln!("[aiki] Warning: Unexpected tool type in shell.completed");
            String::new()
        }
    };

    // Claude Code provides exit_code, stdout, stderr in tool_response
    let (success, exit_code, stdout, stderr) = payload
        .tool_response
        .as_ref()
        .and_then(|v| serde_json::from_value::<BashToolResponse>(v.clone()).ok())
        .map(|resp| {
            (
                resp.exit_code == 0,
                Some(resp.exit_code),
                Some(resp.stdout),
                Some(resp.stderr),
            )
        })
        .unwrap_or_else(|| {
            debug_log(|| "Warning: PostToolUse Bash missing tool_response, assuming success");
            (true, None, None, None)
        });

    AikiEvent::ShellCompleted(AikiShellCompletedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        command,
        success,
        exit_code,
        stdout,
        stderr,
    })
}

/// Build mcp.permission_asked event (MCP tools)
fn build_mcp_permission_asked_event(payload: PreToolUsePayload) -> AikiEvent {
    let parameters = payload.tool_input.unwrap_or(serde_json::Value::Null);
    let server = parse_mcp_server(&payload.tool_name);

    AikiEvent::McpPermissionAsked(AikiMcpPermissionAskedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        server,
        tool_name: payload.tool_name,
        parameters,
    })
}

/// Build mcp.completed event (MCP tools)
fn build_mcp_completed_event(payload: PostToolUsePayload) -> AikiEvent {
    let result = payload
        .tool_response
        .as_ref()
        .map(|v| serde_json::to_string(v).unwrap_or_default())
        .filter(|s| !s.is_empty() && s != "null");
    let server = parse_mcp_server(&payload.tool_name);

    AikiEvent::McpCompleted(AikiMcpCompletedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        server,
        tool_name: payload.tool_name,
        success: true,
        result,
    })
}

/// Build web.permission_asked event (WebFetch, WebSearch)
fn build_web_permission_asked_event(payload: PreToolUsePayload, tool: ClaudeTool) -> AikiEvent {
    let Some(operation) = tool.web_operation() else {
        eprintln!("[aiki] Error: Failed to get web operation");
        return AikiEvent::Unsupported;
    };

    let (url, query) = match tool {
        ClaudeTool::WebFetch(input) => (Some(input.url), None),
        ClaudeTool::WebSearch(input) => (None, Some(input.query)),
        ClaudeTool::Unknown(name) => {
            eprintln!(
                "[aiki] Warning: Failed to parse web tool input for '{}'",
                name
            );
            (None, None)
        }
        _ => {
            eprintln!("[aiki] Warning: Unexpected tool type in web.permission_asked");
            (None, None)
        }
    };

    AikiEvent::WebPermissionAsked(AikiWebPermissionAskedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        operation,
        url,
        query,
    })
}

/// Build web.completed event (WebFetch, WebSearch)
fn build_web_completed_event(payload: PostToolUsePayload, tool: ClaudeTool) -> AikiEvent {
    let Some(operation) = tool.web_operation() else {
        eprintln!("[aiki] Error: Failed to get web operation");
        return AikiEvent::Unsupported;
    };

    let (url, query) = match tool {
        ClaudeTool::WebFetch(input) => (Some(input.url), None),
        ClaudeTool::WebSearch(input) => (None, Some(input.query)),
        ClaudeTool::Unknown(name) => {
            eprintln!(
                "[aiki] Warning: Failed to parse web tool input for '{}'",
                name
            );
            (None, None)
        }
        _ => {
            eprintln!("[aiki] Warning: Unexpected tool type in web.completed");
            (None, None)
        }
    };

    // Web operations are always considered successful if we reach PostToolUse
    AikiEvent::WebCompleted(AikiWebCompletedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        operation,
        url,
        query,
        success: true,
    })
}

/// Build turn.completed event (maps from Stop hook)
fn build_turn_completed_event(payload: StopPayload) -> AikiEvent {
    let response = payload
        .transcript_path
        .as_deref()
        .and_then(|path| extract_last_assistant_response(path))
        .unwrap_or_default();

    AikiEvent::TurnCompleted(AikiTurnCompletedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        turn: crate::events::Turn::unknown(), // Set by handle_turn_completed
        response,
        modified_files: vec![],
        tasks: Default::default(), // Populated by handle_turn_completed
    })
}

/// Extract the last assistant response text from a Claude Code JSONL transcript file.
///
/// The transcript is a JSONL file where each line is a JSON object. Assistant entries
/// have `"type": "assistant"` with a `message.content` array containing blocks like
/// `{"type": "text", "text": "..."}`. We find the last assistant entry and concatenate
/// all text blocks.
fn extract_last_assistant_response(path: &str) -> Option<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            debug_log(|| format!("Failed to read transcript file '{}': {}", path, e));
            return None;
        }
    };

    // Walk lines in reverse to find the last assistant entry
    for line in content.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        if entry.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }

        // Extract text from message.content array
        let Some(content_arr) = entry
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };

        let text: String = content_arr
            .iter()
            .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
            // Skip streaming placeholder entries that Claude Code writes before the real response
            .filter(|t| *t != "(no content)")
            .collect::<Vec<_>>()
            .join("\n");

        if !text.is_empty() {
            return Some(text);
        }
    }

    debug_log(|| format!("No assistant response found in transcript '{}'", path));
    None
}

/// Build session.ended event (maps from SessionEnd hook)
fn build_session_ended_event(payload: SessionEndPayload) -> AikiEvent {
    AikiEvent::SessionEnded(AikiSessionEndedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        reason: payload.reason,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session_start(source: &str) -> SessionStartPayload {
        SessionStartPayload {
            session_id: "test-session-123".to_string(),
            cwd: "/tmp/test".to_string(),
            source: source.to_string(),
        }
    }

    #[test]
    fn test_session_start_startup_maps_to_session_started() {
        let event = build_session_started_event(make_session_start("startup"));
        assert!(
            matches!(event, AikiEvent::SessionStarted(_)),
            "SessionStart(source=startup) should map to SessionStarted"
        );
    }

    #[test]
    fn test_session_start_resume_maps_to_session_resumed() {
        let event = build_session_started_event(make_session_start("resume"));
        assert!(
            matches!(event, AikiEvent::SessionResumed(_)),
            "SessionStart(source=resume) should map to SessionResumed"
        );
    }

    #[test]
    fn test_session_start_compact_maps_to_session_compacted() {
        let event = build_session_started_event(make_session_start("compact"));
        assert!(
            matches!(event, AikiEvent::SessionCompacted(_)),
            "SessionStart(source=compact) should map to SessionCompacted"
        );
    }

    #[test]
    fn test_session_start_clear_maps_to_session_cleared() {
        let event = build_session_started_event(make_session_start("clear"));
        assert!(
            matches!(event, AikiEvent::SessionCleared(_)),
            "SessionStart(source=clear) should map to SessionCleared"
        );
    }

    #[test]
    fn test_session_start_unknown_source_maps_to_session_started() {
        let event = build_session_started_event(make_session_start("unknown"));
        assert!(
            matches!(event, AikiEvent::SessionStarted(_)),
            "SessionStart with unknown source should fall back to SessionStarted"
        );
    }

    #[test]
    fn test_precompact_maps_to_session_will_compact() {
        let payload = PreCompactPayload {
            session_id: "test-session-123".to_string(),
            cwd: "/tmp/test".to_string(),
            trigger: "auto".to_string(),
        };
        let event = build_session_will_compact_event(payload);
        assert!(
            matches!(event, AikiEvent::SessionWillCompact(_)),
            "PreCompact should map to SessionWillCompact"
        );
    }

    #[test]
    fn test_session_start_deserialization_with_source() {
        // Verify that serde correctly deserializes SessionStart with various sources
        let json = r#"{"hook_event_name":"SessionStart","session_id":"abc","cwd":"/tmp","source":"compact"}"#;
        let event: ClaudeEvent = serde_json::from_str(json).unwrap();
        match event {
            ClaudeEvent::SessionStart { payload } => {
                assert_eq!(payload.source, "compact");
            }
            _ => panic!("Expected SessionStart variant"),
        }
    }

    #[test]
    fn test_session_start_deserialization_defaults_to_startup() {
        // When source field is missing, it should default to "startup"
        let json =
            r#"{"hook_event_name":"SessionStart","session_id":"abc","cwd":"/tmp"}"#;
        let event: ClaudeEvent = serde_json::from_str(json).unwrap();
        match event {
            ClaudeEvent::SessionStart { payload } => {
                assert_eq!(payload.source, "startup");
            }
            _ => panic!("Expected SessionStart variant"),
        }
    }

    #[test]
    fn test_precompact_deserialization() {
        let json = r#"{"hook_event_name":"PreCompact","session_id":"abc","cwd":"/tmp","trigger":"manual"}"#;
        let event: ClaudeEvent = serde_json::from_str(json).unwrap();
        match event {
            ClaudeEvent::PreCompact { payload } => {
                assert_eq!(payload.trigger, "manual");
            }
            _ => panic!("Expected PreCompact variant"),
        }
    }
}
