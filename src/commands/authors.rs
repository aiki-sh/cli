use crate::error::{AikiError, Result};
use crate::provenance::authors;
use anyhow::Context;
use std::env;
use std::path::PathBuf;

pub fn run(changes: Option<String>, format: String) -> Result<()> {
    // Get current directory
    let current_dir = env::current_dir().context("Failed to get current directory")?;

    // Find JJ workspace root (look for .jj directory)
    let jj_root = find_jj_workspace(&current_dir)
        .context("Not in a JJ workspace. Run this command from within a JJ repository.")?;

    // Parse scope
    let scope = match changes.as_deref() {
        Some("staged") => authors::AuthorScope::GitStaged,
        Some(other) => {
            return Err(AikiError::UnknownScope(other.to_string()));
        }
        None => authors::AuthorScope::WorkingCopy,
    };

    // Parse format
    let output_format = match format.as_str() {
        "plain" => authors::OutputFormat::Plain,
        "git" => authors::OutputFormat::Git,
        "json" => authors::OutputFormat::Json,
        other => {
            return Err(AikiError::UnknownFormat(other.to_string()));
        }
    };

    // Create authors command
    let authors_cmd = authors::AuthorsCommand::new(jj_root);

    // Get authors
    let output = authors_cmd
        .get_authors(scope, output_format)
        .context("Failed to get authors")?;

    // Print to stdout
    if !output.is_empty() {
        print!("{}", output);
    }

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
