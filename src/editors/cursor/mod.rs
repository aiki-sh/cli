use anyhow::Result;

mod events;
mod output;
mod session;
mod tools;

use events::{build_aiki_event_from_json, build_aiki_event_from_stdin};
use output::build_command_output;

/// Handle a Cursor event
///
/// This is the vendor-specific handler for Cursor hooks.
/// Parses the payload once and dispatches to event-specific handlers.
///
/// # Arguments
/// * `cursor_event_name` - Vendor event name from CLI flag (used for output formatting)
pub fn handle(cursor_event_name: &str) -> Result<()> {
    let aiki_event = build_aiki_event_from_stdin()?;
    dispatch_and_output(cursor_event_name, aiki_event)
}

/// Handle a Cursor event from a pre-read payload buffer (the stdin-once path).
pub fn handle_with_payload(cursor_event_name: &str, payload: &[u8]) -> Result<()> {
    let aiki_event = build_aiki_event_from_json(payload)?;
    dispatch_and_output(cursor_event_name, aiki_event)
}

/// Shared dispatch + output for both stdin and pre-read-payload paths.
fn dispatch_and_output(cursor_event_name: &str, aiki_event: crate::events::AikiEvent) -> Result<()> {
    let aiki_response = crate::event_bus::dispatch(aiki_event)?;
    let hook_output = build_command_output(aiki_response, cursor_event_name);
    hook_output.print_and_exit();
}
