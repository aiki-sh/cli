use crate::cache::{debug_log, DEBUG_ENABLED};
use crate::error::Result;

use crate::events::result::HookResult;
use crate::events::{
    self, AikiChangeCompletedPayload, AikiChangePermissionAskedPayload, AikiEvent,
    ChangeOperation, DeleteOperation, MoveOperation,
};
use crate::tools::{parse_file_operation_from_shell_command, FileOperation};

/// Dispatch an event to the appropriate handler
///
/// This is the central routing point for all events in the system.
/// Events are routed based on their type, and handlers return generic
/// HookResult objects that can be translated to editor-specific formats.
pub fn dispatch(event: AikiEvent) -> Result<HookResult> {
    // Handle unsupported events immediately
    if matches!(event, AikiEvent::Unsupported) {
        return Ok(HookResult::success());
    }

    // Log event for debugging (uses cached debug flag)
    if *DEBUG_ENABLED {
        let event_type_name = match &event {
            // Session lifecycle
            AikiEvent::SessionStarted(_) => "session.started",
            AikiEvent::SessionResumed(_) => "session.resumed",
            AikiEvent::SessionWillCompact(_) => "session.will_compact",
            AikiEvent::SessionCompacted(_) => "session.compacted",
            AikiEvent::SessionCleared(_) => "session.cleared",
            AikiEvent::SessionEnded(_) => "session.ended",
            // Turn lifecycle
            AikiEvent::TurnStarted(_) => "turn.started",
            AikiEvent::TurnCompleted(_) => "turn.completed",
            // Read operations
            AikiEvent::ReadPermissionAsked(_) => "read.permission_asked",
            AikiEvent::ReadCompleted(_) => "read.completed",
            // Change operations (unified mutations: write, delete, move)
            AikiEvent::ChangePermissionAsked(_) => "change.permission_asked",
            AikiEvent::ChangeCompleted(_) => "change.completed",
            // Shell commands
            AikiEvent::ShellPermissionAsked(_) => "shell.permission_asked",
            AikiEvent::ShellCompleted(_) => "shell.completed",
            // Web access
            AikiEvent::WebPermissionAsked(_) => "web.permission_asked",
            AikiEvent::WebCompleted(_) => "web.completed",
            // MCP tools
            AikiEvent::McpPermissionAsked(_) => "mcp.permission_asked",
            AikiEvent::McpCompleted(_) => "mcp.completed",
            // Git integration
            AikiEvent::CommitMessageStarted(_) => "commit.message_started",
            // Repo transitions
            AikiEvent::RepoChanged(_) => "repo.changed",
            // Task lifecycle
            AikiEvent::TaskStarted(_) => "task.started",
            AikiEvent::TaskClosed(_) => "task.closed",
            // Fallback
            AikiEvent::Unsupported => "unsupported",
        };
        debug_log(|| {
            format!(
                "Dispatching event: {} from agent: {:?}",
                event_type_name,
                event.agent_type()
            )
        });
    }

    // Route to appropriate handler
    let result = match event {
        // Session lifecycle
        AikiEvent::SessionStarted(e) => events::handle_session_started(e),
        AikiEvent::SessionResumed(e) => events::handle_session_resumed(e),
        AikiEvent::SessionWillCompact(e) => events::handle_session_will_compact(e),
        AikiEvent::SessionCompacted(e) => events::handle_session_compacted(e),
        AikiEvent::SessionCleared(e) => events::handle_session_cleared(e),
        AikiEvent::SessionEnded(e) => events::handle_session_ended(e),

        // Turn lifecycle
        AikiEvent::TurnStarted(e) => events::handle_turn_started(e),
        AikiEvent::TurnCompleted(e) => events::handle_turn_completed(e),

        // Read operations
        AikiEvent::ReadPermissionAsked(e) => events::handle_read_permission_asked(e),
        AikiEvent::ReadCompleted(e) => events::handle_read_completed(e),

        // Change operations (unified mutations: write, delete, move)
        AikiEvent::ChangePermissionAsked(e) => events::handle_change_permission_asked(e),
        AikiEvent::ChangeCompleted(e) => events::handle_change_completed(e),

        // Shell commands - transform rm/rmdir to change.* events with Delete operation,
        // and mv to change.* events with Move operation
        AikiEvent::ShellPermissionAsked(e) => {
            let command = e.command.clone();
            let (file_op, paths) = parse_file_operation_from_shell_command(&command);
            match file_op {
                Some(FileOperation::Delete) => {
                    let change_event = transform_shell_delete_to_change_permission_asked(e, paths);
                    return events::handle_change_permission_asked(change_event);
                }
                Some(FileOperation::Move) => {
                    let change_event =
                        transform_shell_move_to_change_permission_asked(e, paths, &command);
                    return events::handle_change_permission_asked(change_event);
                }
                _ => events::handle_shell_permission_asked(e),
            }
        }
        AikiEvent::ShellCompleted(e) => {
            let command = e.command.clone();
            let (file_op, paths) = parse_file_operation_from_shell_command(&command);
            match file_op {
                Some(FileOperation::Delete) => {
                    let change_event = transform_shell_delete_to_change_completed(e, paths);
                    return events::handle_change_completed(change_event);
                }
                Some(FileOperation::Move) => {
                    let change_event = transform_shell_move_to_change_completed(e, paths, &command);
                    return events::handle_change_completed(change_event);
                }
                _ => events::handle_shell_completed(e),
            }
        }

        // Web access
        AikiEvent::WebPermissionAsked(e) => events::handle_web_permission_asked(e),
        AikiEvent::WebCompleted(e) => events::handle_web_completed(e),

        // MCP tools
        AikiEvent::McpPermissionAsked(e) => events::handle_mcp_permission_asked(e),
        AikiEvent::McpCompleted(e) => events::handle_mcp_completed(e),

        // Git integration
        AikiEvent::CommitMessageStarted(e) => events::handle_commit_message_started(e),

        // Repo transitions
        AikiEvent::RepoChanged(e) => events::handle_repo_changed(e),

        // Task lifecycle
        AikiEvent::TaskStarted(e) => events::handle_task_started(e),
        AikiEvent::TaskClosed(e) => events::handle_task_closed(e),

        // Fallback
        AikiEvent::Unsupported => return Ok(HookResult::success()),
    };

    // If handler fails, return a failure response instead of propagating error
    match result {
        Ok(response) => Ok(response),
        Err(e) => {
            eprintln!("Warning: Aiki event handler failed: {}", e);
            Ok(HookResult::failure(format!("Aiki handler failed: {}", e)))
        }
    }
}

/// Transform shell.permission_asked to change.permission_asked for rm/rmdir commands
///
/// Called when shell command is detected as a delete operation.
fn transform_shell_delete_to_change_permission_asked(
    shell_event: crate::events::AikiShellPermissionAskedPayload,
    paths: Vec<String>,
) -> AikiChangePermissionAskedPayload {
    debug_log(|| {
        format!(
            "Transforming shell.permission_asked (rm/rmdir) to change.permission_asked: {:?}",
            paths
        )
    });

    AikiChangePermissionAskedPayload {
        session: shell_event.session,
        cwd: shell_event.cwd,
        timestamp: shell_event.timestamp,
        tool_name: "Bash".to_string(),
        operation: ChangeOperation::Delete(DeleteOperation { file_paths: paths }),
    }
}

/// Transform shell.completed to change.completed for rm/rmdir commands
///
/// First tries to use JJ for accurate path resolution. If JJ is unavailable
/// or returns no matching paths, falls back to syntactic detection from
/// command arguments (preserves existing behavior for Git-only repos).
fn transform_shell_delete_to_change_completed(
    shell_event: crate::events::AikiShellCompletedPayload,
    command_args: Vec<String>,
) -> AikiChangeCompletedPayload {
    // Try to get actual deleted paths from JJ, filtered by command arguments
    let deleted_paths =
        match crate::jj::diff::get_deleted_paths(&shell_event.cwd, &command_args) {
            Ok(paths) if !paths.is_empty() => {
                debug_log(|| format!("Using JJ-detected deleted paths: {:?}", paths));
                paths
            }
            Ok(_) => {
                // No deletions detected by JJ (or filtered out) - fall back to syntactic
                debug_log(|| {
                    "JJ detected no matching deletions, falling back to syntactic detection"
                });
                command_args
            }
            Err(e) => {
                // JJ not available or error - fall back to syntactic detection
                debug_log(|| {
                    format!(
                        "JJ error ({}), falling back to syntactic detection",
                        e
                    )
                });
                command_args
            }
        };

    debug_log(|| {
        format!(
            "Transforming shell.completed (rm/rmdir) to change.completed with {} paths",
            deleted_paths.len()
        )
    });

    AikiChangeCompletedPayload {
        session: shell_event.session,
        cwd: shell_event.cwd,
        timestamp: shell_event.timestamp,
        tool_name: "Bash".to_string(),
        success: shell_event.success,
        turn: crate::events::Turn::unknown(), // Shell commands don't have turn context
        operation: ChangeOperation::Delete(DeleteOperation {
            file_paths: deleted_paths,
        }),
    }
}

/// Transform shell.permission_asked to change.permission_asked for mv commands
///
/// Called when shell command is detected as a move operation.
/// The paths vector should contain source path(s) followed by destination path.
///
/// Uses filesystem-based directory detection to accurately expand paths like
/// `mv foo existing_dir` into `existing_dir/foo`. This is safe for permission_asked
/// because the filesystem reflects the pre-move state.
///
/// The completed event uses JJ for accurate post-move paths.
fn transform_shell_move_to_change_permission_asked(
    shell_event: crate::events::AikiShellPermissionAskedPayload,
    paths: Vec<String>,
    _command: &str, // No longer needed for cache key
) -> AikiChangePermissionAskedPayload {
    debug_log(|| {
        format!(
            "Transforming shell.permission_asked (mv) to change.permission_asked: {:?}",
            paths
        )
    });

    // Use filesystem-based detection for pre-event (destination still exists)
    // This correctly expands `mv foo existing_dir` to `existing_dir/foo`
    let move_op = MoveOperation::from_move_paths_with_cwd(paths, &shell_event.cwd);

    AikiChangePermissionAskedPayload {
        session: shell_event.session,
        cwd: shell_event.cwd,
        timestamp: shell_event.timestamp,
        tool_name: "Bash".to_string(),
        operation: ChangeOperation::Move(move_op),
    }
}

/// Transform shell.completed to change.completed for mv commands
///
/// First tries to use JJ for accurate path resolution. If JJ is unavailable
/// or returns no matching paths, falls back to syntactic detection from
/// command arguments (preserves existing behavior for Git-only repos).
fn transform_shell_move_to_change_completed(
    shell_event: crate::events::AikiShellCompletedPayload,
    command_args: Vec<String>,
    _command: &str, // No longer needed for cache key
) -> AikiChangeCompletedPayload {
    // Try to get actual move operations from JJ, filtered by command arguments
    let move_op = match crate::jj::diff::get_move_operations(&shell_event.cwd, &command_args) {
        Ok(ops) if !ops.is_empty() => {
            debug_log(|| format!("Using JJ-detected move operations: {:?}", ops));
            // Extract sources and destinations from JJ
            let sources: Vec<String> = ops.iter().map(|(s, _)| s.clone()).collect();
            let destinations: Vec<String> = ops.iter().map(|(_, d)| d.clone()).collect();

            MoveOperation {
                file_paths: destinations.clone(),
                source_paths: sources,
                destination_paths: destinations,
            }
        }
        Ok(_) => {
            // No moves detected by JJ (or filtered out) - fall back to syntactic
            debug_log(|| "JJ detected no matching moves, falling back to syntactic detection");
            // Use syntactic detection from command args (existing behavior)
            MoveOperation::from_move_paths(command_args)
        }
        Err(e) => {
            // JJ not available or error - fall back to syntactic detection
            debug_log(|| format!("JJ error ({}), falling back to syntactic detection", e));
            // Use syntactic detection from command args (existing behavior)
            MoveOperation::from_move_paths(command_args)
        }
    };

    debug_log(|| {
        format!(
            "Transforming shell.completed (mv) to change.completed with {} move operations",
            move_op.file_paths.len()
        )
    });

    AikiChangeCompletedPayload {
        session: shell_event.session,
        cwd: shell_event.cwd,
        timestamp: shell_event.timestamp,
        tool_name: "Bash".to_string(),
        success: shell_event.success,
        turn: crate::events::Turn::unknown(), // Shell commands don't have turn context
        operation: ChangeOperation::Move(move_op),
    }
}
