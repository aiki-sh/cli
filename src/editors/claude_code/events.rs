use serde::Deserialize;
use std::path::PathBuf;

use crate::cache::debug_log;
use crate::error::Result;
use crate::events::FileOperation;
use crate::events::TokenUsage;
use crate::events::{
    parse_mcp_server, AikiChangeCompletedPayload, AikiChangePermissionAskedPayload, AikiEvent,
    AikiMcpCompletedPayload, AikiMcpPermissionAskedPayload, AikiReadCompletedPayload,
    AikiReadPermissionAskedPayload, AikiSessionClearedPayload, AikiSessionCompactedPayload,
    AikiSessionEndedPayload, AikiSessionResumedPayload, AikiSessionStartPayload,
    AikiSessionWillCompactPayload, AikiShellCompletedPayload, AikiShellPermissionAskedPayload,
    AikiTurnCompletedPayload, AikiTurnStartedPayload, AikiWebCompletedPayload,
    AikiWebPermissionAskedPayload, ChangeOperation, DeleteOperation, MoveOperation, WriteOperation,
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
    /// Path to the JSONL transcript file for this session. Recorded into
    /// conversation history so the stale-worker watchdog (and `aiki session
    /// transcript`) can resolve the transcript without the legacy
    /// session-file external-id fallback.
    #[serde(default)]
    transcript_path: Option<String>,
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

/// Build AikiEvent from a pre-read JSON payload buffer.
pub fn build_aiki_event_from_json(payload: &[u8]) -> Result<AikiEvent> {
    let event: ClaudeEvent =
        serde_json::from_slice(payload).map_err(|e| anyhow::anyhow!(e))?;
    Ok(claude_event_to_aiki(event))
}

/// Convert a parsed ClaudeEvent into an AikiEvent.
fn claude_event_to_aiki(event: ClaudeEvent) -> AikiEvent {
    match event {
        ClaudeEvent::SessionStart { payload } => build_session_started_event(payload),
        ClaudeEvent::UserPromptSubmit { payload } => build_turn_started_event(payload),
        ClaudeEvent::PreToolUse { payload } => build_permission_asked_event_for_tool_type(payload),
        ClaudeEvent::PostToolUse { payload } => build_completed_event_for_tool_type(payload),
        ClaudeEvent::PreCompact { payload } => build_session_will_compact_event(payload),
        ClaudeEvent::Stop { payload } => build_turn_completed_event(payload),
        ClaudeEvent::SessionEnd { payload } => build_session_ended_event(payload),
    }
}

/// Build appropriate pre-tool event based on tool type
fn build_permission_asked_event_for_tool_type(payload: PreToolUsePayload) -> AikiEvent {
    let tool = ClaudeTool::parse(&payload.tool_name, payload.tool_input.as_ref());

    match tool.tool_type() {
        ToolType::File => build_file_permission_asked_event(payload, tool),
        ToolType::Shell => build_shell_permission_asked_event(payload, tool),
        ToolType::Mcp => build_mcp_permission_asked_event(payload),
        ToolType::Web => build_web_permission_asked_event(payload, tool),
        ToolType::Internal => {
            // Special handling for ExitPlanMode: absorb workspace before showing approval prompt
            if payload.tool_name == "ExitPlanMode" {
                let session = create_session(&payload.session_id, &payload.cwd);
                let _ = crate::flows::core::workspace_absorb_all(&session);
            }
            AikiEvent::Unsupported
        }
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
            transcript_path: payload.transcript_path,
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
            eprintln!("[aiki] Warning: Unexpected tool type in change.permission_asked (write)");
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
fn build_change_completed_event_delete(payload: PostToolUsePayload, tool: ClaudeTool) -> AikiEvent {
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
fn build_change_completed_event_move(payload: PostToolUsePayload, tool: ClaudeTool) -> AikiEvent {
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

use crate::editors::transcript::{TranscriptEntry, TurnTranscript};

/// Build turn.completed event (maps from Stop hook)
fn build_turn_completed_event(payload: StopPayload) -> AikiEvent {
    let transcript = payload
        .transcript_path
        .as_deref()
        .map(|p| TurnTranscript::parse(p, parse_transcript_lines))
        .unwrap_or_default();

    AikiEvent::TurnCompleted(AikiTurnCompletedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        turn: crate::events::Turn::unknown(), // Set by handle_turn_completed
        response: transcript.response,
        modified_files: vec![],
        tasks: Default::default(), // Populated by handle_turn_completed
        tokens: transcript.tokens,
        model: transcript.model,
    })
}

/// Whether a `type: "user"` transcript line is a genuine user prompt (a turn
/// boundary) rather than mid-turn bookkeeping. Claude Code writes several
/// user-typed line shapes that do NOT start a turn:
///
/// - **Tool results**: after every tool call, the result comes back as a user
///   line whose content blocks carry `tool_result`.
/// - **Meta lines** (`isMeta: true`): injected context, command echoes.
/// - **Sidechain lines** (`isSidechain: true`): a subagent's conversation
///   interleaved into the file; its prompts are not this turn's boundary.
fn is_turn_boundary_user_entry(entry: &serde_json::Value) -> bool {
    if entry.get("isMeta").and_then(|v| v.as_bool()) == Some(true)
        || entry.get("isSidechain").and_then(|v| v.as_bool()) == Some(true)
    {
        return false;
    }
    match entry.get("message").and_then(|m| m.get("content")) {
        Some(serde_json::Value::String(_)) => true,
        Some(serde_json::Value::Array(blocks)) => !blocks
            .iter()
            .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result")),
        _ => false,
    }
}

/// Parse Claude Code JSONL content into transcript entries.
///
/// Resets on genuine user prompts so only the current (last) turn's entries are
/// returned. Tool results and meta/sidechain lines also arrive as `type: "user"`
/// mid-turn and must not reset — see [`is_turn_boundary_user_entry`].
fn parse_transcript_lines(content: &str) -> Vec<TranscriptEntry> {
    let mut entries: Vec<TranscriptEntry> = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        let entry_type = entry.get("type").and_then(|t| t.as_str());

        // Reset only on a genuine user prompt. Tool results are also
        // `type: "user"` lines; clearing on those would drop every API call
        // before the turn's last tool result, under-counting a tool-use turn
        // to just its final call.
        if entry_type == Some("user") {
            if is_turn_boundary_user_entry(&entry) {
                entries.clear();
            }
            continue;
        }

        if entry_type != Some("assistant") {
            continue;
        }

        let Some(message) = entry.get("message") else {
            continue;
        };

        let mut transcript_entry = TranscriptEntry::default();

        // Extract the message id. Claude Code emits a streaming-snapshot entry
        // and then a finalized entry for the SAME id, each carrying that call's
        // usage; the id lets sum_tokens dedup the pair so the call counts once.
        if let Some(id) = message.get("id").and_then(|v| v.as_str()) {
            transcript_entry.id = Some(id.to_string());
        }

        // Extract text from message.content array
        if let Some(content_arr) = message.get("content").and_then(|c| c.as_array()) {
            let text: String = content_arr
                .iter()
                .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"))
                .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                // Skip streaming placeholder entries that Claude Code writes before the real response
                .filter(|t| *t != "(no content)")
                .collect::<Vec<_>>()
                .join("\n");

            if !text.is_empty() {
                transcript_entry.response = Some(text);
            }
        }

        // Extract model string
        if let Some(model) = message.get("model").and_then(|m| m.as_str()) {
            transcript_entry.model = Some(model.to_string());
        }

        // Extract token usage from message.usage
        if let Some(usage) = message.get("usage") {
            let input = usage.get("input_tokens").and_then(|v| v.as_u64());
            let output = usage.get("output_tokens").and_then(|v| v.as_u64());
            if let (Some(input), Some(output)) = (input, output) {
                let cache_read = usage
                    .get("cache_read_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let cache_created = usage
                    .get("cache_creation_input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                transcript_entry.tokens = Some(TokenUsage {
                    input,
                    output,
                    cache_read,
                    cache_created,
                });
            }
        }

        entries.push(transcript_entry);
    }

    entries
}

/// Build session.ended event (maps from SessionEnd hook)
fn build_session_ended_event(payload: SessionEndPayload) -> AikiEvent {
    AikiEvent::SessionEnded(AikiSessionEndedPayload {
        session: create_session(&payload.session_id, &payload.cwd),
        cwd: PathBuf::from(&payload.cwd),
        timestamp: chrono::Utc::now(),
        reason: payload.reason,
        tokens: None,
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
            transcript_path: None,
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
        let json = r#"{"hook_event_name":"SessionStart","session_id":"abc","cwd":"/tmp"}"#;
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

    #[test]
    fn test_parse_transcript_with_usage_and_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let content = r#"{"type":"user","message":{"content":"hello"}}
{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"Hi there!"}],"usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":30,"cache_creation_input_tokens":10}}}"#;
        std::fs::write(&path, content).unwrap();

        let extract = TurnTranscript::parse(path.to_str().unwrap(), parse_transcript_lines);
        assert_eq!(extract.response, "Hi there!");
        assert_eq!(extract.model.as_deref(), Some("claude-sonnet-4-20250514"));
        let tokens = extract.tokens.unwrap();
        assert_eq!(tokens.input, 100);
        assert_eq!(tokens.output, 50);
        assert_eq!(tokens.cache_read, 30);
        assert_eq!(tokens.cache_created, 10);
    }

    #[test]
    fn test_parse_transcript_without_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let content = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Response without usage"}]}}"#;
        std::fs::write(&path, content).unwrap();

        let extract = TurnTranscript::parse(path.to_str().unwrap(), parse_transcript_lines);
        assert_eq!(extract.response, "Response without usage");
        assert!(extract.tokens.is_none());
        assert!(extract.model.is_none());
    }

    #[test]
    fn test_parse_transcript_partial_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        // Has usage but missing cache fields
        let content = r#"{"type":"assistant","message":{"model":"claude-opus-4-20250514","content":[{"type":"text","text":"Hello"}],"usage":{"input_tokens":200,"output_tokens":80}}}"#;
        std::fs::write(&path, content).unwrap();

        let extract = TurnTranscript::parse(path.to_str().unwrap(), parse_transcript_lines);
        assert_eq!(extract.response, "Hello");
        assert_eq!(extract.model.as_deref(), Some("claude-opus-4-20250514"));
        let tokens = extract.tokens.unwrap();
        assert_eq!(tokens.input, 200);
        assert_eq!(tokens.output, 80);
        assert_eq!(tokens.cache_read, 0);
        assert_eq!(tokens.cache_created, 0);
    }

    #[test]
    fn test_parse_transcript_no_file() {
        let t = TurnTranscript::parse("/nonexistent/path.jsonl", parse_transcript_lines);
        assert_eq!(t.response, "");
        assert!(t.tokens.is_none());
    }

    #[test]
    fn test_parse_transcript_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        std::fs::write(&path, "").unwrap();

        let t = TurnTranscript::parse(path.to_str().unwrap(), parse_transcript_lines);
        assert_eq!(t.response, "");
        assert!(t.tokens.is_none());
    }

    #[test]
    fn test_build_turn_completed_populates_tokens_and_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let content = r#"{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"Done!"}],"usage":{"input_tokens":500,"output_tokens":100,"cache_read_input_tokens":50,"cache_creation_input_tokens":0}}}"#;
        std::fs::write(&path, content).unwrap();

        let payload = StopPayload {
            session_id: "test-session".to_string(),
            cwd: "/tmp/test".to_string(),
            transcript_path: Some(path.to_str().unwrap().to_string()),
        };
        let event = build_turn_completed_event(payload);
        match event {
            AikiEvent::TurnCompleted(p) => {
                assert_eq!(p.response, "Done!");
                assert_eq!(p.model.as_deref(), Some("claude-sonnet-4-20250514"));
                let tokens = p.tokens.unwrap();
                assert_eq!(tokens.input, 500);
                assert_eq!(tokens.output, 100);
                assert_eq!(tokens.cache_read, 50);
                assert_eq!(tokens.cache_created, 0);
            }
            _ => panic!("Expected TurnCompleted"),
        }
    }

    #[test]
    fn test_extract_sums_tokens_across_multiple_assistant_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        // Simulate a turn with three assistant entries (tool-use rounds)
        let content = [
            r#"{"type":"user","message":{"content":"Do something complex"}}"#,
            r#"{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"Let me check..."}],"usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":10,"cache_creation_input_tokens":5}}}"#,
            r#"{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"(no content)"}],"usage":{"input_tokens":150,"output_tokens":30,"cache_read_input_tokens":20,"cache_creation_input_tokens":0}}}"#,
            r#"{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"Here is the result."}],"usage":{"input_tokens":200,"output_tokens":50,"cache_read_input_tokens":30,"cache_creation_input_tokens":10}}}"#,
        ]
        .join("\n");
        std::fs::write(&path, content).unwrap();

        let extract = TurnTranscript::parse(path.to_str().unwrap(), parse_transcript_lines);
        // Response text should be from the last entry with non-empty/non-placeholder text
        assert_eq!(extract.response, "Here is the result.");
        assert_eq!(extract.model.as_deref(), Some("claude-sonnet-4-20250514"));
        // Tokens should be summed across all three assistant entries
        let tokens = extract.tokens.unwrap();
        assert_eq!(tokens.input, 450);    // 100 + 150 + 200
        assert_eq!(tokens.output, 100);   // 20 + 30 + 50
        assert_eq!(tokens.cache_read, 60);     // 10 + 20 + 30
        assert_eq!(tokens.cache_created, 15);  // 5 + 0 + 10
    }

    #[test]
    fn test_extract_resets_accumulators_on_user_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        // Two turns: first turn has large tokens, second turn has small tokens.
        // Only the second turn's tokens should be returned.
        let content = [
            r#"{"type":"user","message":{"content":"First question"}}"#,
            r#"{"type":"assistant","message":{"model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"First answer"}],"usage":{"input_tokens":1000,"output_tokens":500,"cache_read_input_tokens":200,"cache_creation_input_tokens":100}}}"#,
            r#"{"type":"user","message":{"content":"Second question"}}"#,
            r#"{"type":"assistant","message":{"model":"claude-opus-4-20250514","content":[{"type":"text","text":"Second answer"}],"usage":{"input_tokens":50,"output_tokens":25,"cache_read_input_tokens":10,"cache_creation_input_tokens":5}}}"#,
        ]
        .join("\n");
        std::fs::write(&path, content).unwrap();

        let extract = TurnTranscript::parse(path.to_str().unwrap(), parse_transcript_lines);
        assert_eq!(extract.response, "Second answer");
        assert_eq!(extract.model.as_deref(), Some("claude-opus-4-20250514"));
        // Only second turn's tokens, not accumulated from the first turn
        let tokens = extract.tokens.unwrap();
        assert_eq!(tokens.input, 50);
        assert_eq!(tokens.output, 25);
        assert_eq!(tokens.cache_read, 10);
        assert_eq!(tokens.cache_created, 5);
    }

    #[test]
    fn test_extract_tool_use_only_turn_returns_tokens() {
        // Tool-use turn: stop_reason=tool_use, no text blocks, only tool_use content.
        // Should still return tokens even though there's no response text.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let content = [
            r#"{"type":"user","message":{"content":"Fix the bug"}}"#,
            r#"{"type":"assistant","message":{"model":"claude-opus-4-6","stop_reason":"tool_use","content":[{"type":"tool_use","id":"toolu_01","name":"Edit","input":{"file":"src/main.rs"}}],"usage":{"input_tokens":5000,"output_tokens":150,"cache_read_input_tokens":4800,"cache_creation_input_tokens":0}}}"#,
        ]
        .join("\n");
        std::fs::write(&path, content).unwrap();

        let extract = TurnTranscript::parse(path.to_str().unwrap(), parse_transcript_lines);
        assert_eq!(extract.response, "");
        assert_eq!(extract.model.as_deref(), Some("claude-opus-4-6"));
        let tokens = extract.tokens.unwrap();
        assert_eq!(tokens.input, 5000);
        assert_eq!(tokens.output, 150);
        assert_eq!(tokens.cache_read, 4800);
        assert_eq!(tokens.cache_created, 0);
    }

    #[test]
    fn test_extract_streaming_plus_tool_use_pair() {
        // Real pattern: Claude Code writes an intermediate streaming-snapshot
        // entry (stop_reason=null, partial output) and then the finalized entry
        // for the SAME message id, both carrying that call's usage (identical
        // cache/input, differing output). The pair is ONE API call: dedup by
        // message id keeps the finalized usage, it must NOT sum to double values.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let content = [
            r#"{"type":"user","message":{"content":"Do something"}}"#,
            r#"{"type":"assistant","message":{"id":"msg_pair1","model":"claude-opus-4-6","stop_reason":null,"content":[],"usage":{"input_tokens":3,"output_tokens":23,"cache_read_input_tokens":8693,"cache_creation_input_tokens":16911}}}"#,
            r#"{"type":"assistant","message":{"id":"msg_pair1","model":"claude-opus-4-6","stop_reason":"tool_use","content":[{"type":"tool_use","id":"toolu_01","name":"Read","input":{}}],"usage":{"input_tokens":3,"output_tokens":196,"cache_read_input_tokens":8693,"cache_creation_input_tokens":16911}}}"#,
        ]
        .join("\n");
        std::fs::write(&path, content).unwrap();

        let extract = TurnTranscript::parse(path.to_str().unwrap(), parse_transcript_lines);
        assert_eq!(extract.response, "");
        let tokens = extract.tokens.unwrap();
        // Deduped to the single finalized call, not snapshot + finalized.
        assert_eq!(tokens.input, 3);
        assert_eq!(tokens.output, 196);
        assert_eq!(tokens.cache_read, 8693);
        assert_eq!(tokens.cache_created, 16911);
    }

    #[test]
    fn test_extract_streaming_plus_end_turn_pair() {
        // Streaming snapshot followed by the finalized end_turn entry for the
        // SAME message id. The pair is one API call: dedup keeps the finalized
        // usage and text, not the doubled sum.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let content = [
            r#"{"type":"user","message":{"content":"Explain this"}}"#,
            r#"{"type":"assistant","message":{"id":"msg_pair2","model":"claude-opus-4-6","stop_reason":null,"content":[],"usage":{"input_tokens":3,"output_tokens":28,"cache_read_input_tokens":39261,"cache_creation_input_tokens":412}}}"#,
            r#"{"type":"assistant","message":{"id":"msg_pair2","model":"claude-opus-4-6","stop_reason":"end_turn","content":[{"type":"text","text":"Here is the explanation."}],"usage":{"input_tokens":3,"output_tokens":288,"cache_read_input_tokens":39261,"cache_creation_input_tokens":412}}}"#,
        ]
        .join("\n");
        std::fs::write(&path, content).unwrap();

        let extract = TurnTranscript::parse(path.to_str().unwrap(), parse_transcript_lines);
        assert_eq!(extract.response, "Here is the explanation.");
        let tokens = extract.tokens.unwrap();
        // Deduped to the single finalized call.
        assert_eq!(tokens.input, 3);
        assert_eq!(tokens.output, 288);
        assert_eq!(tokens.cache_read, 39261);
        assert_eq!(tokens.cache_created, 412);
    }

    #[test]
    fn test_extract_multiple_tool_use_rounds() {
        // Multiple tool calls in one turn — several assistant entries between
        // user messages, each a DISTINCT API call (different message ids, input
        // genuinely grows 100/200/300/400). Distinct ids must still sum; only a
        // snapshot+finalized pair sharing one id is deduped.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let content = [
            r#"{"type":"user","message":{"content":"Fix everything"}}"#,
            r#"{"type":"assistant","message":{"id":"msg_r1","model":"claude-opus-4-6","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}],"usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
            r#"{"type":"assistant","message":{"id":"msg_r2","model":"claude-opus-4-6","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t2","name":"Edit","input":{}}],"usage":{"input_tokens":200,"output_tokens":80,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
            r#"{"type":"assistant","message":{"id":"msg_r3","model":"claude-opus-4-6","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t3","name":"Bash","input":{}}],"usage":{"input_tokens":300,"output_tokens":60,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
            r#"{"type":"assistant","message":{"id":"msg_r4","model":"claude-opus-4-6","stop_reason":"end_turn","content":[{"type":"text","text":"All fixed."}],"usage":{"input_tokens":400,"output_tokens":120,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
        ]
        .join("\n");
        std::fs::write(&path, content).unwrap();

        let extract = TurnTranscript::parse(path.to_str().unwrap(), parse_transcript_lines);
        assert_eq!(extract.response, "All fixed.");
        let tokens = extract.tokens.unwrap();
        assert_eq!(tokens.input, 1000);
        assert_eq!(tokens.output, 310);
    }

    #[test]
    fn test_extract_no_content_placeholder_skipped() {
        // Claude Code writes "(no content)" as the streaming-snapshot placeholder
        // for the SAME message id as the finalized entry. The placeholder text is
        // filtered out AND its usage is deduped away — the pair is one API call.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let content = [
            r#"{"type":"user","message":{"content":"Hello"}}"#,
            r#"{"type":"assistant","message":{"id":"msg_pair3","model":"claude-opus-4-6","stop_reason":null,"content":[{"type":"text","text":"(no content)"}],"usage":{"input_tokens":1,"output_tokens":4,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
            r#"{"type":"assistant","message":{"id":"msg_pair3","model":"claude-opus-4-6","stop_reason":"end_turn","content":[{"type":"text","text":"Hi there!"}],"usage":{"input_tokens":1,"output_tokens":10,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
        ]
        .join("\n");
        std::fs::write(&path, content).unwrap();

        let extract = TurnTranscript::parse(path.to_str().unwrap(), parse_transcript_lines);
        assert_eq!(extract.response, "Hi there!");
        let tokens = extract.tokens.unwrap();
        // Deduped to the single finalized call (input 1, output 10), not summed.
        assert_eq!(tokens.input, 1);
        assert_eq!(tokens.output, 10);
    }

    #[test]
    fn test_golden_streaming_and_tool_use_fixture_parses() {
        // Drive the committed golden transcript through the real harness parser.
        // The fixture is a realistic single turn: streaming snapshot + finalized
        // pair (msg_01A, deduped by id), then two more distinct calls, with the
        // turn's tool_result USER lines interleaved between calls exactly as
        // Claude Code writes them. Exact sums are load-bearing: the parser once
        // cleared its accumulator on every user-typed line (tool_results
        // included), which silently dropped all but the final call.
        let content =
            include_str!("../../../tests/fixtures/tokens/claude_streaming_and_tool_use.jsonl");
        let extract = TurnTranscript::from_entries(parse_transcript_lines(content));

        // Response text and model come from the finalized end_turn entry.
        assert_eq!(
            extract.response,
            "Done. Refactored the parser and the tests pass."
        );
        assert_eq!(extract.model.as_deref(), Some("claude-opus-4-8"));

        // Sum over msg_01A (finalized) + msg_02B + msg_03C.
        let tokens = extract.tokens.expect("fixture carries usage");
        assert_eq!(tokens.input, 4 + 12000 + 12500);
        assert_eq!(tokens.output, 178 + 95 + 210);
        assert_eq!(tokens.cache_read, 8693 + 16915 + 17000);
        assert_eq!(tokens.cache_created, 16911);
        assert_eq!(tokens.total(), 84_506);
    }

    #[test]
    fn test_extract_tool_result_user_entries_do_not_reset() {
        // Real turn shape: prompt → assistant tool_use → USER tool_result →
        // assistant tool_use → USER tool_result → assistant end_turn. The
        // tool_result carriers are `type: "user"` but are NOT turn boundaries;
        // all three API calls must sum. Regression guard: the parser used to
        // clear on every user line, keeping only the final call (a 78-96%
        // under-count measured on real tool-heavy sessions).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let content = [
            r#"{"type":"user","message":{"content":"Fix the bug"}}"#,
            r#"{"type":"assistant","message":{"id":"msg_a","model":"claude-opus-4-6","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}],"usage":{"input_tokens":100,"output_tokens":10,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
            r#"{"type":"user","message":{"content":[{"tool_use_id":"t1","type":"tool_result","content":"file contents"}]}}"#,
            r#"{"type":"assistant","message":{"id":"msg_b","model":"claude-opus-4-6","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t2","name":"Edit","input":{}}],"usage":{"input_tokens":200,"output_tokens":20,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
            r#"{"type":"user","message":{"content":[{"tool_use_id":"t2","type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"assistant","message":{"id":"msg_c","model":"claude-opus-4-6","stop_reason":"end_turn","content":[{"type":"text","text":"Fixed."}],"usage":{"input_tokens":300,"output_tokens":30,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
        ]
        .join("\n");
        std::fs::write(&path, content).unwrap();

        let extract = TurnTranscript::parse(path.to_str().unwrap(), parse_transcript_lines);
        assert_eq!(extract.response, "Fixed.");
        let tokens = extract.tokens.unwrap();
        assert_eq!(tokens.input, 600);
        assert_eq!(tokens.output, 60);
    }

    #[test]
    fn test_extract_meta_and_sidechain_user_entries_do_not_reset() {
        // `isMeta` user lines (injected context) and `isSidechain` user lines
        // (subagent prompts) are not turn boundaries even when their content is
        // a plain string.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let content = [
            r#"{"type":"user","message":{"content":"Do the thing"}}"#,
            r#"{"type":"assistant","message":{"id":"msg_a","model":"claude-opus-4-6","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}],"usage":{"input_tokens":100,"output_tokens":10,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"<system-reminder>injected context</system-reminder>"}}"#,
            r#"{"type":"user","isSidechain":true,"message":{"content":"You are a subagent, search for X"}}"#,
            r#"{"type":"user","message":{"content":[{"tool_use_id":"t1","type":"tool_result","content":"done"}]}}"#,
            r#"{"type":"assistant","message":{"id":"msg_b","model":"claude-opus-4-6","stop_reason":"end_turn","content":[{"type":"text","text":"Done."}],"usage":{"input_tokens":200,"output_tokens":20,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
        ]
        .join("\n");
        std::fs::write(&path, content).unwrap();

        let extract = TurnTranscript::parse(path.to_str().unwrap(), parse_transcript_lines);
        assert_eq!(extract.response, "Done.");
        let tokens = extract.tokens.unwrap();
        assert_eq!(tokens.input, 300);
        assert_eq!(tokens.output, 30);
    }

    #[test]
    fn test_extract_genuine_prompt_still_resets_after_tool_results() {
        // A genuine next prompt (string content, no meta flags) after a
        // tool-use turn must still reset: only the last turn's calls count.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let content = [
            r#"{"type":"user","message":{"content":"First request"}}"#,
            r#"{"type":"assistant","message":{"id":"msg_a","model":"claude-opus-4-6","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}],"usage":{"input_tokens":1000,"output_tokens":100,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
            r#"{"type":"user","message":{"content":[{"tool_use_id":"t1","type":"tool_result","content":"data"}]}}"#,
            r#"{"type":"assistant","message":{"id":"msg_b","model":"claude-opus-4-6","stop_reason":"end_turn","content":[{"type":"text","text":"First done."}],"usage":{"input_tokens":2000,"output_tokens":200,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
            r#"{"type":"user","message":{"content":"Second request"}}"#,
            r#"{"type":"assistant","message":{"id":"msg_c","model":"claude-opus-4-6","stop_reason":"end_turn","content":[{"type":"text","text":"Second done."}],"usage":{"input_tokens":50,"output_tokens":5,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}}"#,
        ]
        .join("\n");
        std::fs::write(&path, content).unwrap();

        let extract = TurnTranscript::parse(path.to_str().unwrap(), parse_transcript_lines);
        assert_eq!(extract.response, "Second done.");
        let tokens = extract.tokens.unwrap();
        assert_eq!(tokens.input, 50);
        assert_eq!(tokens.output, 5);
    }
}
