use anyhow::Result;

mod events;
mod output;
mod session;
mod tools;

use events::build_aiki_event_from_stdin;
use output::build_command_output;

/// Handle a Claude Code event
///
/// This is the vendor-specific handler for Claude Code hooks.
/// Parses the payload once and dispatches to event-specific handlers.
///
/// # Arguments
/// * `claude_event_name` - Vendor event name from CLI flag (used for output formatting)
pub fn handle(claude_event_name: &str) -> Result<()> {
    use crate::events::{AikiEvent, AikiSessionClearedPayload};

    // Build Aiki event from stdin JSON
    let aiki_event = build_aiki_event_from_stdin()?;

    // Claude Code's /clear fires SessionEnd without a subsequent SessionStart,
    // so we synthesize a session.cleared after the end to re-inject workspace
    // and task context for the new conversation.
    let clear_restart = if let AikiEvent::SessionEnded(ref e) = aiki_event {
        if e.reason == "clear" {
            Some(AikiSessionClearedPayload {
                session: e.session.clone(),
                cwd: e.cwd.clone(),
                timestamp: chrono::Utc::now(),
            })
        } else {
            None
        }
    } else {
        None
    };

    // Dispatch the primary event
    let aiki_response = crate::event_bus::dispatch(aiki_event)?;

    // If /clear, dispatch session.cleared to re-inject critical state.
    // Use the cleared response for output since SessionEnd has no additionalContext support.
    // Format as SessionStart output so Claude Code processes the additionalContext.
    let (final_response, output_event_name) = if let Some(cleared_payload) = clear_restart {
        match crate::event_bus::dispatch(AikiEvent::SessionCleared(cleared_payload)) {
            Ok(cleared_response) if cleared_response.has_context() => {
                (cleared_response, "SessionStart")
            }
            _ => (aiki_response, claude_event_name),
        }
    } else {
        (aiki_response, claude_event_name)
    };

    let hook_output = build_command_output(final_response, output_event_name);

    hook_output.print_and_exit();
}
