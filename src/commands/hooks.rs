use crate::editors;
use crate::error::Result;
use crate::provenance;

// Re-export HookCommandOutput for backwards compatibility
#[allow(unused_imports)]
pub use crate::editors::HookCommandOutput;

pub fn run_stdin(agent: String, event: String, payload: Option<String>) -> Result<()> {
    let agent_type = parse_agent_type(&agent)?;
    handle_event(agent_type, &event, payload.as_deref())
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
fn handle_event(agent: provenance::AgentType, event: &str, payload: Option<&str>) -> Result<()> {
    use crate::error::AikiError;
    use provenance::AgentType;

    match agent {
        AgentType::ClaudeCode => Ok(editors::claude_code::handle(event)?),
        AgentType::Cursor => Ok(editors::cursor::handle(event)?),
        AgentType::Codex => Ok(editors::codex::handle(event, payload)?),
        _ => Err(AikiError::UnsupportedAgentType(format!("{:?}", agent))),
    }
}
