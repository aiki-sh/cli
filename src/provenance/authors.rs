use crate::jj::jj_cmd;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::provenance::blame::BlameContext;
use crate::provenance::record::AgentType;

/// Author information
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Author {
    pub name: String,
    pub email: String,
    pub agent_type: AgentType,
    pub client_name: Option<String>,
}

/// Scope for author extraction
pub enum AuthorScope {
    /// Working copy change (@)
    WorkingCopy,
    /// Git staged changes
    GitStaged,
}

/// Output format for authors
pub enum OutputFormat {
    /// Plain format: one author per line (name <email>)
    Plain,
    /// Git format: Co-authored-by: lines
    Git,
    /// JSON format: array of author objects
    Json,
}

/// Extract authors from changes
pub struct AuthorsCommand {
    repo_path: PathBuf,
}

impl AuthorsCommand {
    #[must_use]
    pub fn new(repo_path: impl AsRef<Path>) -> Self {
        Self {
            repo_path: repo_path.as_ref().to_path_buf(),
        }
    }

    /// Get authors for the specified scope and format
    pub fn get_authors(&self, scope: AuthorScope, format: OutputFormat) -> Result<String> {
        let authors = match scope {
            AuthorScope::WorkingCopy => self.get_working_copy_authors()?,
            AuthorScope::GitStaged => self.get_git_staged_authors()?,
        };

        if authors.is_empty() {
            return Ok(String::new());
        }

        // Format the output
        match format {
            OutputFormat::Plain => Ok(self.format_plain(&authors)),
            OutputFormat::Git => Ok(self.format_git(&authors)),
            OutputFormat::Json => Ok(self.format_json(&authors)),
        }
    }

    /// Get authors from working copy change (@)
    fn get_working_copy_authors(&self) -> Result<Vec<Author>> {
        // Create blame context for working copy
        let blame_context = BlameContext::new(self.repo_path.clone())?;

        // For now, we'll get authors from all tracked files in working copy
        // TODO: In the future, optimize this to only look at modified files

        // Get list of files changed in working copy using jj status
        let changed_files = self.get_working_copy_changed_files()?;

        if changed_files.is_empty() {
            return Ok(Vec::new());
        }

        let mut authors_map: HashMap<String, Author> = HashMap::new();

        for file_path in changed_files {
            // Get blame for the file
            let attributions = match blame_context.blame_file(&file_path, None) {
                Ok(attr) => attr,
                Err(_) => {
                    // Skip files that can't be blamed
                    continue;
                }
            };

            // Collect AI agent attributions
            for attr in &attributions {
                // Skip unknown/human agents
                if matches!(attr.agent_type, AgentType::Unknown) {
                    continue;
                }

                let author = Author {
                    name: format_agent_name(&attr.agent_type),
                    email: attr.agent_type.email().to_string(),
                    agent_type: attr.agent_type.clone(),
                    client_name: attr.client_name.clone(),
                };

                authors_map.insert(author.email.clone(), author);
            }
        }

        let mut authors: Vec<Author> = authors_map.into_values().collect();
        authors.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(authors)
    }

    /// Get list of files changed in working copy using jj status
    fn get_working_copy_changed_files(&self) -> Result<Vec<PathBuf>> {
        let output = jj_cmd()
            .args(["status", "--no-pager"])
            .current_dir(&self.repo_path)
            .output()
            .context("Failed to run jj status")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("jj status failed: {}", stderr);
        }

        let status_output = String::from_utf8_lossy(&output.stdout);
        let mut files = Vec::new();

        // Parse jj status output to find modified/added files
        // Format: "M file.txt" or "A file.txt"
        for line in status_output.lines() {
            let line = line.trim();
            if line.starts_with("M ") || line.starts_with("A ") {
                if let Some(file_path) = line.split_whitespace().nth(1) {
                    files.push(PathBuf::from(file_path));
                }
            }
        }

        Ok(files)
    }

    /// Get authors from Git staged changes
    fn get_git_staged_authors(&self) -> Result<Vec<Author>> {
        // Get list of staged files with their line ranges
        let staged_changes = self.get_staged_changes()?;

        if staged_changes.is_empty() {
            return Ok(Vec::new());
        }

        // Create a single blame context for all files (optimization: reuses workspace/repo)
        let blame_context = BlameContext::new(self.repo_path.clone())?;

        let mut authors_map: HashMap<String, Author> = HashMap::new();

        for (file_path, line_ranges) in staged_changes {
            // Get blame for ONLY the changed line ranges (optimization: skip irrelevant lines)
            let attributions = match blame_context.blame_file(&file_path, Some(&line_ranges)) {
                Ok(attr) => attr,
                Err(_) => {
                    // Skip files that can't be blamed (new files, binary, etc.)
                    continue;
                }
            };

            // Check which lines are attributed to AI agents
            for attr in &attributions {
                // Skip unknown/human agents
                if matches!(attr.agent_type, AgentType::Unknown) {
                    continue;
                }

                let author = Author {
                    name: format_agent_name(&attr.agent_type),
                    email: attr.agent_type.email().to_string(),
                    agent_type: attr.agent_type.clone(),
                    client_name: attr.client_name.clone(),
                };

                authors_map.insert(author.email.clone(), author);
            }
        }

        let mut authors: Vec<Author> = authors_map.into_values().collect();
        authors.sort_by(|a, b| a.name.cmp(&b.name));

        Ok(authors)
    }

    /// Get staged changes from git diff
    /// Returns map of file paths to line ranges that were added/modified
    fn get_staged_changes(&self) -> Result<HashMap<PathBuf, Vec<(usize, usize)>>> {
        // Run git diff --cached with unified=0 to get minimal context
        // --no-color ensures output is parseable even with color.diff=always
        // --diff-filter=AM only shows added/modified files (not deleted)
        let output = Command::new("git")
            .args([
                "diff",
                "--cached",
                "--unified=0",
                "--no-color",
                "--diff-filter=AM",
            ])
            .current_dir(&self.repo_path)
            .output()
            .context("Failed to run git diff --cached")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git diff failed: {}", stderr);
        }

        let diff_output = String::from_utf8_lossy(&output.stdout);
        Ok(parse_diff(&diff_output))
    }

    /// Format authors in plain format (one per line: name <email>)
    fn format_plain(&self, authors: &[Author]) -> String {
        authors
            .iter()
            .map(|author| {
                if let Some(client) = &author.client_name {
                    format!("{} <{}> (via {})", author.name, author.email, client)
                } else {
                    format!("{} <{}>", author.name, author.email)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Format authors in Git trailer format (Co-Authored-By:)
    fn format_git(&self, authors: &[Author]) -> String {
        authors
            .iter()
            .map(|author| {
                if let Some(client) = &author.client_name {
                    format!(
                        "Co-Authored-By: {} <{}> (via {})",
                        author.name, author.email, client
                    )
                } else {
                    format!("Co-Authored-By: {} <{}>", author.name, author.email)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Format authors as JSON array
    fn format_json(&self, authors: &[Author]) -> String {
        let json_objects: Vec<String> = authors
            .iter()
            .map(|author| {
                if let Some(client) = &author.client_name {
                    format!(
                        r#"{{"name":"{}","email":"{}","agent_type":"{}","client_name":"{}"}}"#,
                        author.name,
                        author.email,
                        format!("{:?}", author.agent_type),
                        client
                    )
                } else {
                    format!(
                        r#"{{"name":"{}","email":"{}","agent_type":"{}"}}"#,
                        author.name,
                        author.email,
                        format!("{:?}", author.agent_type)
                    )
                }
            })
            .collect();

        format!("[{}]", json_objects.join(","))
    }
}

/// Parse git diff output to extract file paths and line ranges
/// Returns map of file paths to list of (start_line, end_line) tuples
fn parse_diff(diff_output: &str) -> HashMap<PathBuf, Vec<(usize, usize)>> {
    let mut result: HashMap<PathBuf, Vec<(usize, usize)>> = HashMap::new();
    let mut current_file: Option<PathBuf> = None;

    for line in diff_output.lines() {
        // Look for +++ b/filename lines to identify the file
        if let Some(path_str) = line.strip_prefix("+++ b/") {
            current_file = Some(PathBuf::from(path_str));
            continue;
        }

        // Look for @@ lines that indicate line ranges
        // Format: @@ -old_start,old_count +new_start,new_count @@
        if line.starts_with("@@") {
            if let Some(file_path) = &current_file {
                // Extract the new file range (+new_start,new_count)
                if let Some(range_info) = parse_hunk_header(line) {
                    result
                        .entry(file_path.clone())
                        .or_default()
                        .push(range_info);
                }
            }
        }
    }

    result
}

/// Parse a hunk header line to extract the new file's line range
/// Returns (start_line, end_line) for the changed lines
fn parse_hunk_header(line: &str) -> Option<(usize, usize)> {
    // Example: @@ -1,3 +1,4 @@
    // We want the +1,4 part (new file range)

    // Find the '+' that starts the new file range
    let plus_pos = line.find('+')?;
    let rest = &line[plus_pos + 1..];

    // Find the next space or @@
    let end_pos = rest.find(' ').or_else(|| rest.find("@@"))?;
    let range_str = &rest[..end_pos];

    // Parse "start" or "start,count"
    if let Some(comma_pos) = range_str.find(',') {
        let start: usize = range_str[..comma_pos].parse().ok()?;
        let count: usize = range_str[comma_pos + 1..].parse().ok()?;
        if count == 0 {
            // Deletion only, no new lines
            return None;
        }
        let end = start + count - 1;
        Some((start, end))
    } else {
        // Single line change
        let line_num: usize = range_str.parse().ok()?;
        Some((line_num, line_num))
    }
}

/// Format agent type as display name
fn format_agent_name(agent: &AgentType) -> String {
    match agent {
        AgentType::ClaudeCode => "Claude".to_string(),
        AgentType::Codex => "Codex".to_string(),
        AgentType::Cursor => "Cursor".to_string(),
        AgentType::Gemini => "Gemini".to_string(),
        AgentType::Unknown => "Unknown AI Agent".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hunk_header_single_line() {
        let header = "@@ -1 +1 @@";
        assert_eq!(parse_hunk_header(header), Some((1, 1)));
    }

    #[test]
    fn test_parse_hunk_header_multiple_lines() {
        let header = "@@ -1,3 +1,5 @@";
        assert_eq!(parse_hunk_header(header), Some((1, 5)));
    }

    #[test]
    fn test_parse_hunk_header_deletion() {
        let header = "@@ -1,3 +1,0 @@";
        assert_eq!(parse_hunk_header(header), None);
    }

    #[test]
    fn test_parse_hunk_header_addition() {
        let header = "@@ -0,0 +1,3 @@";
        assert_eq!(parse_hunk_header(header), Some((1, 3)));
    }

    #[test]
    fn test_parse_diff_single_file() {
        let diff = r#"diff --git a/file.txt b/file.txt
index abc123..def456 100644
--- a/file.txt
+++ b/file.txt
@@ -1,2 +1,3 @@
+new line
 existing line
 another line
"#;
        let result = parse_diff(diff);
        assert_eq!(result.len(), 1);
        let file_path = PathBuf::from("file.txt");
        assert!(result.contains_key(&file_path));
        assert_eq!(result[&file_path], vec![(1, 3)]);
    }

    #[test]
    fn test_format_agent_email() {
        assert_eq!(AgentType::ClaudeCode.email(), "noreply@anthropic.com");
        assert_eq!(AgentType::Cursor.email(), "noreply@cursor.com");
    }

    #[test]
    fn test_format_agent_name() {
        assert_eq!(format_agent_name(&AgentType::ClaudeCode), "Claude");
        assert_eq!(format_agent_name(&AgentType::Cursor), "Cursor");
    }

    #[test]
    fn test_author_equality() {
        let author1 = Author {
            name: "Claude".to_string(),
            email: "noreply@anthropic.com".to_string(),
            agent_type: AgentType::ClaudeCode,
            client_name: None,
        };
        let author2 = Author {
            name: "Claude".to_string(),
            email: "noreply@anthropic.com".to_string(),
            agent_type: AgentType::ClaudeCode,
            client_name: None,
        };
        assert_eq!(author1, author2);
    }
}
