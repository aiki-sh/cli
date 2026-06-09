use crate::error::Result;
use anyhow::Context;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

/// Configure Zed editor to use aiki hooks acp proxy
pub fn configure_zed() -> Result<()> {
    let home = dirs::home_dir().context("Could not find home directory")?;
    let zed_settings = home.join(".config/zed/settings.json");

    // Create directory if it doesn't exist
    if let Some(parent) = zed_settings.parent() {
        fs::create_dir_all(parent).context("Failed to create Zed config directory")?;
    }

    // Read existing settings or create new
    let mut settings: Value = if zed_settings.exists() {
        let content =
            fs::read_to_string(&zed_settings).context("Failed to read Zed settings.json")?;
        // Strip // comments (Zed uses JSONC format)
        let stripped: String = content
            .lines()
            .filter(|line| !line.trim().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        serde_json::from_str(&stripped).context("Failed to parse Zed settings.json")?
    } else {
        json!({})
    };

    // Ensure agent_servers object exists
    if !settings.is_object() {
        settings = json!({});
    }

    let obj = settings.as_object_mut().unwrap();

    // Add or update agent_servers configuration
    obj.insert(
        "agent_servers".to_string(),
        json!({
            "claude": {
                "type": "custom",
                "command": "aiki",
                "args": ["hooks", "acp", "--agent", "claude-code"]
            },
            "codex": {
                "type": "custom",
                "command": "aiki",
                "args": ["hooks", "acp", "--agent", "codex"]
            },
            "gemini": {
                "type": "custom",
                "command": "aiki",
                "args": ["hooks", "acp", "--agent", "gemini"]
            }
        }),
    );

    // Write back to file
    let pretty_json =
        serde_json::to_string_pretty(&settings).context("Failed to serialize settings")?;
    fs::write(&zed_settings, pretty_json).context("Failed to write Zed settings.json")?;

    Ok(())
}

/// Check if Zed is configured to use aiki hooks acp proxy
pub fn is_zed_configured() -> Result<bool> {
    let home = dirs::home_dir().context("Could not find home directory")?;
    let zed_settings = home.join(".config/zed/settings.json");

    if !zed_settings.exists() {
        return Ok(false);
    }

    let content = fs::read_to_string(&zed_settings).context("Failed to read Zed settings.json")?;
    // Strip // comments (Zed uses JSONC format)
    let stripped: String = content
        .lines()
        .filter(|line| !line.trim().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let settings: Value =
        serde_json::from_str(&stripped).context("Failed to parse Zed settings.json")?;

    // Check if agent_servers.claude.command == "aiki"
    if let Some(agent_servers) = settings.get("agent_servers") {
        if let Some(claude) = agent_servers.get("claude") {
            if let Some(command) = claude.get("command") {
                return Ok(command.as_str() == Some("aiki"));
            }
        }
    }

    Ok(false)
}

/// Get the path to Zed settings file
pub fn zed_settings_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/zed/settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zed_settings_path() {
        let path = zed_settings_path();
        assert!(path.is_some());
        assert!(path.unwrap().to_string_lossy().contains("settings.json"));
    }
}
