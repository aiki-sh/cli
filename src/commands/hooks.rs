use std::io::Read;

use crate::editors;
use crate::error::Result;
use crate::provenance;

pub fn run_stdin(
    agent: String,
    event: String,
    continue_async: bool,
    payload: Option<String>,
) -> Result<()> {
    // When running behind the ACP proxy, the proxy handles all event dispatch.
    // Skip editor hooks to avoid duplicate sessions and events.
    if std::env::var("AIKI_ACP_PROXY").is_ok() {
        return Ok(());
    }

    let agent_type = parse_agent_type(&agent)?;

    // SessionEnd async flow: Claude Code kills the process before heavy cleanup
    // finishes, so we fork the work into a background process and exit immediately.
    //
    // - First call (no --_continue-async): read stdin, spawn background child
    //   with the same payload piped to its stdin, print empty success, exit.
    // - Second call (--_continue-async): this IS the background child — run the
    //   full session.ended flow synchronously.
    //
    // Exception: reason="clear" runs synchronously because /clear needs to
    // re-inject workspace/task context via a synthesized SessionCleared event
    // before the next turn begins. The clear path is fast (no workspace_absorb_all).
    if event == "SessionEnd" && !continue_async {
        return run_session_end_maybe_async(&agent);
    }

    handle_event(agent_type, &event, payload.as_deref())
}

/// Handle SessionEnd: run async for real exits, sync for /clear.
///
/// Reads the raw stdin payload and inspects the `reason` field:
/// - `"clear"`: run synchronously so the synthesized SessionCleared event can
///   re-inject context before the next turn.
/// - anything else: spawn a background child and exit immediately so Claude Code
///   doesn't kill us mid-cleanup.
fn run_session_end_maybe_async(agent: &str) -> Result<()> {
    // Read raw stdin bytes before anything else consumes them
    let mut stdin_payload = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut stdin_payload)
        .map_err(|e| anyhow::anyhow!("failed to read stdin for SessionEnd: {e}"))?;

    // /clear is fast and needs synchronous context re-injection
    if extract_reason_from_payload(&stdin_payload).as_deref() == Some("clear") {
        return run_session_end_sync_with_payload(agent, &stdin_payload);
    }

    // Determine cwd from the JSON payload (best-effort; fall back to current dir)
    let cwd = extract_cwd_from_payload(&stdin_payload)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    let agent_flag = match agent {
        "claude-code" => "--claude",
        "codex" => "--codex",
        "cursor" => "--cursor",
        "gemini" => "--gemini",
        _ => "--claude", // fallback; parse_agent_type will reject unknown agents earlier
    };
    let args = [
        "hooks", "stdin", agent_flag, "SessionEnd", "--_continue-async",
    ];

    match crate::workflow::async_run::spawn_with_stdin(&cwd, &args, &stdin_payload) {
        Ok(()) => {
            // Return empty success — Claude Code sees exit(0) immediately
            let output = editors::HookCommandOutput::new(None, 0);
            output.print_and_exit();
        }
        Err(e) => {
            // Fallback: run synchronously (better slow than lost work)
            eprintln!("warning: async SessionEnd spawn failed ({e}), falling back to sync");
            run_session_end_sync_with_payload(agent, &stdin_payload)
        }
    }
}

/// Fallback: run session.ended synchronously when the async spawn fails.
///
/// Since we already consumed stdin, we need to feed the payload to the handler
/// via the agent-specific path that accepts raw bytes.
fn run_session_end_sync_with_payload(agent: &str, payload: &[u8]) -> Result<()> {
    let agent_type = parse_agent_type(agent)?;
    match agent_type {
        provenance::AgentType::ClaudeCode => {
            Ok(editors::claude_code::handle_with_payload("SessionEnd", payload)?)
        }
        // Other agents don't have this async path yet; just warn
        _ => {
            eprintln!("warning: sync fallback not supported for agent {agent:?}");
            Ok(())
        }
    }
}

/// Extract lightweight fields from the hook JSON payload (best-effort).
#[derive(serde::Deserialize, Default)]
struct PayloadMeta {
    cwd: Option<String>,
    reason: Option<String>,
}

fn parse_payload_meta(payload: &[u8]) -> PayloadMeta {
    serde_json::from_slice::<PayloadMeta>(payload).unwrap_or_default()
}

fn extract_cwd_from_payload(payload: &[u8]) -> Option<std::path::PathBuf> {
    parse_payload_meta(payload).cwd.map(std::path::PathBuf::from)
}

fn extract_reason_from_payload(payload: &[u8]) -> Option<String> {
    parse_payload_meta(payload).reason
}

/// Parse agent type from string
fn parse_agent_type(agent: &str) -> Result<provenance::AgentType> {
    use crate::error::AikiError;

    match agent {
        "claude-code" => Ok(provenance::AgentType::ClaudeCode),
        "cursor" => Ok(provenance::AgentType::Cursor),
        "codex" => Ok(provenance::AgentType::Codex),
        _ => Err(AikiError::UnknownAgentType(agent.to_string())),
    }
}

/// Handle editor event (called by hooks)
fn handle_event(agent: provenance::AgentType, event: &str, _payload: Option<&str>) -> Result<()> {
    use crate::error::AikiError;
    use provenance::AgentType;

    match agent {
        AgentType::ClaudeCode => Ok(editors::claude_code::handle(event)?),
        AgentType::Cursor => Ok(editors::cursor::handle(event)?),
        AgentType::Codex => Ok(editors::codex::handle_stdin(event)?),
        _ => Err(AikiError::UnsupportedAgentType(format!("{:?}", agent))),
    }
}
