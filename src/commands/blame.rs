use crate::error::{AikiError, Result};
use crate::provenance;
use crate::provenance::blame;
use anyhow::Context;
use std::env;
use std::path::PathBuf;

pub fn run(file: PathBuf, agent: Option<String>) -> Result<()> {
    // Get current directory
    let current_dir = env::current_dir().context("Failed to get current directory")?;

    // Find JJ workspace root (look for .jj directory)
    let jj_root = find_jj_workspace(&current_dir)
        .context("Not in a JJ workspace. Run this command from within a JJ repository.")?;

    // Check if file exists
    let file_path = if file.is_absolute() {
        file
    } else {
        current_dir.join(&file)
    };

    if !file_path.exists() {
        return Err(AikiError::FileNotFound(file_path));
    }

    // Convert to relative path from JJ workspace root
    let relative_path = file_path
        .strip_prefix(&jj_root)
        .context("File is not in repository")?;

    // Parse agent filter if provided
    let agent_filter = match agent {
        Some(agent_str) => Some(parse_agent_type(&agent_str)?),
        None => None,
    };

    // Create blame command
    let blame_cmd = blame::BlameCommand::new(jj_root);

    // Get attributions
    let attributions = blame_cmd
        .blame_file(relative_path)
        .context("Failed to generate blame information")?;

    // Format and print output
    let output = blame_cmd.format_blame(&attributions, agent_filter);
    print!("{}", output);

    Ok(())
}

/// Find the JJ workspace root by walking up the directory tree looking for .jj
fn find_jj_workspace(start_dir: &std::path::Path) -> Option<PathBuf> {
    let mut current = start_dir;
    loop {
        let jj_dir = current.join(".jj");
        if jj_dir.exists() && jj_dir.is_dir() {
            return Some(current.to_path_buf());
        }

        // Move up one directory
        match current.parent() {
            Some(parent) => current = parent,
            None => return None, // Reached filesystem root
        }
    }
}

/// Parse agent type from string
fn parse_agent_type(agent: &str) -> Result<provenance::AgentType> {
    match agent {
        "claude-code" => Ok(provenance::AgentType::ClaudeCode),
        "cursor" => Ok(provenance::AgentType::Cursor),
        _ => Err(AikiError::UnknownAgentType(agent.to_string())),
    }
}
