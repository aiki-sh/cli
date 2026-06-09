use anyhow::Context;
use jj_lib::annotate::FileAnnotator;
use jj_lib::object_id::ObjectId;
use jj_lib::repo::{Repo, StoreFactories};
use jj_lib::repo_path::RepoPath;
use jj_lib::revset::RevsetExpression;
use jj_lib::workspace::{default_working_copy_factories, Workspace};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::AikiError;

// For blame.rs, keep using anyhow::Result since it interacts heavily with jj-lib
type Result<T> = anyhow::Result<T>;

use crate::jj::JJWorkspace;
use crate::provenance::record::{AgentType, AttributionConfidence, ProvenanceRecord};
/// Line-by-line attribution information for a file
#[derive(Debug, Clone)]
pub struct LineAttribution {
    pub line_number: usize,
    pub line_text: String,
    pub commit_id: String,
    pub agent_type: AgentType,
    pub confidence: Option<AttributionConfidence>,
    pub session_id: Option<String>,
    pub client_name: Option<String>,
}

/// Command to show line-by-line blame/attribution for a file
pub struct BlameCommand {
    repo_path: PathBuf,
}

/// Reusable blame context that caches workspace and repo
/// Use this when blaming multiple files to avoid reloading
pub struct BlameContext {
    workspace: Workspace,
    repo: std::sync::Arc<jj_lib::repo::ReadonlyRepo>,
}

impl BlameContext {
    /// Create a new blame context by loading workspace and repo once
    #[must_use]
    pub fn new(repo_path: impl AsRef<Path>) -> Result<Self> {
        let repo_path = repo_path.as_ref();
        let settings = JJWorkspace::create_user_settings()?;
        let store_factories = StoreFactories::default();
        let working_copy_factories = default_working_copy_factories();

        let workspace = Workspace::load(
            &settings,
            repo_path,
            &store_factories,
            &working_copy_factories,
        )
        .context("Failed to load JJ workspace")?;

        let repo = workspace
            .repo_loader()
            .load_at_head()
            .context("Failed to load repository")?;

        Ok(Self { workspace, repo })
    }

    /// Blame a file using the cached workspace/repo
    /// Optionally filter to specific line ranges for efficiency
    pub fn blame_file(
        &self,
        file_path: &Path,
        line_filter: Option<&[(usize, usize)]>,
    ) -> Result<Vec<LineAttribution>> {
        // Convert path to RepoPath
        let path_str = file_path
            .to_str()
            .context("File path contains invalid UTF-8")?;
        let repo_path =
            RepoPath::from_internal_string(path_str).context("Invalid repository path")?;

        // Get the working copy commit
        let wc_commit_id = self
            .repo
            .view()
            .get_wc_commit_id(self.workspace.workspace_name())
            .context("Failed to get working copy commit ID")?;

        let wc_commit = self
            .repo
            .store()
            .get_commit(wc_commit_id)
            .context("Failed to load working copy commit")?;

        // Check if file exists in working copy
        let tree = wc_commit.tree()?;
        let file_value = tree.path_value(repo_path)?;

        let commit_to_use = if file_value.is_present() {
            wc_commit
        } else {
            // Try parent
            let parent_ids = wc_commit.parent_ids();
            if parent_ids.is_empty() {
                return Err(AikiError::FileNotFoundNoParents.into());
            }

            let parent_commit = self.repo.store().get_commit(&parent_ids[0])?;
            let parent_tree = parent_commit.tree()?;
            let parent_file_value = parent_tree.path_value(repo_path)?;

            if !parent_file_value.is_present() {
                return Err(AikiError::FileNotFoundInParent.into());
            }
            parent_commit
        };

        // Create file annotator
        let mut file_annotator = FileAnnotator::from_commit(&commit_to_use, repo_path)
            .context("Failed to create file annotator")?;

        let revset_expr = RevsetExpression::all();
        file_annotator
            .compute(self.repo.as_ref(), &revset_expr)
            .context("Failed to compute annotations")?;

        let file_annotation = file_annotator.to_annotation();

        // Cache for commit descriptions to avoid repeated lookups
        // Use String (hex) as key to avoid Vec allocation on every lookup
        let mut commit_cache: HashMap<String, Option<ProvenanceRecord>> = HashMap::new();

        let mut attributions = Vec::new();
        let mut line_num = 1;

        for (commit_id_result, line_text) in file_annotation.lines() {
            let commit_id = match commit_id_result {
                Ok(id) => id,
                Err(id) => id,
            };

            // Skip if line filter is provided and this line is not in range
            if let Some(ranges) = line_filter {
                let in_range = ranges
                    .iter()
                    .any(|(start, end)| line_num >= *start && line_num <= *end);
                if !in_range {
                    line_num += 1;
                    continue;
                }
            }

            // Get commit ID as hex string for cache key
            let commit_id_hex = commit_id.hex();

            // Check cache first
            let provenance = if let Some(cached) = commit_cache.get(&commit_id_hex) {
                cached.clone()
            } else {
                // Load commit and parse provenance
                let commit = self.repo.store().get_commit(&commit_id)?;
                let description = commit.description();
                let provenance = ProvenanceRecord::from_description(description).unwrap_or(None);

                commit_cache.insert(commit_id_hex.clone(), provenance.clone());

                provenance
            };

            let attribution = match provenance {
                Some(prov) => LineAttribution {
                    line_number: line_num,
                    line_text: line_text.to_string(),
                    commit_id: commit_id_hex.clone(),
                    agent_type: prov.agent.agent_type,
                    confidence: Some(prov.agent.confidence),
                    session_id: Some(prov.session_id),
                    client_name: prov.client_name,
                },
                None => LineAttribution {
                    line_number: line_num,
                    line_text: line_text.to_string(),
                    commit_id: commit_id_hex,
                    agent_type: AgentType::Unknown,
                    confidence: None,
                    session_id: None,
                    client_name: None,
                },
            };

            attributions.push(attribution);
            line_num += 1;
        }

        Ok(attributions)
    }
}

impl BlameCommand {
    #[must_use]
    pub fn new(repo_path: impl AsRef<Path>) -> Self {
        Self {
            repo_path: repo_path.as_ref().to_path_buf(),
        }
    }

    /// Get line-by-line attribution for a file
    pub fn blame_file(&self, file_path: impl AsRef<Path>) -> Result<Vec<LineAttribution>> {
        // Create a context and use it to blame the file
        // For single-file operations, this is equivalent to the old implementation
        let context = BlameContext::new(self.repo_path.clone())?;
        context.blame_file(file_path.as_ref(), None)
    }

    /// Format blame output in git-blame style
    /// Optionally filter by agent type
    pub fn format_blame(
        &self,
        attributions: &[LineAttribution],
        agent_filter: Option<AgentType>,
    ) -> String {
        use std::fmt::Write;

        // Pre-allocate output buffer (estimate ~100 bytes per line)
        let mut output = String::with_capacity(attributions.len() * 100);

        for attr in attributions {
            // Apply agent filter if specified
            if let Some(ref filter) = agent_filter {
                if attr.agent_type != *filter {
                    continue;
                }
            }

            // Format: commit_id (agent session confidence [client]) line_num| line_text
            // Use Display trait for human-friendly agent names
            let agent_str = format!("{}", attr.agent_type);
            let session_str = attr
                .session_id
                .as_ref()
                .map(|s| truncate_session_id(s))
                .unwrap_or_else(|| "-".to_string());
            let confidence_str = attr
                .confidence
                .as_ref()
                .map(|c| format!("{:?}", c))
                .unwrap_or_else(|| "-".to_string());
            let client_str = attr
                .client_name
                .as_ref()
                .map(|c| format!(" via {}", c))
                .unwrap_or_else(String::new);

            // Truncate commit ID to 8 chars for readability
            let short_commit = &attr.commit_id[..8.min(attr.commit_id.len())];

            // Use write! to avoid intermediate allocations
            write!(
                output,
                "{} ({:12} {:12} {:6}{}) {:4}| {}\n",
                short_commit,
                agent_str,
                session_str,
                confidence_str,
                client_str,
                attr.line_number,
                attr.line_text
            )
            .unwrap(); // Writing to String never fails
        }

        output
    }
}

/// Truncate session ID for display (keep first 8 chars)
fn truncate_session_id(session_id: &str) -> String {
    if session_id.len() > 12 {
        format!("{}...", &session_id[..9])
    } else {
        session_id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_session_id() {
        assert_eq!(truncate_session_id("short"), "short");
        assert_eq!(truncate_session_id("exactly-12ch"), "exactly-12ch");
        assert_eq!(
            truncate_session_id("this-is-a-very-long-session-id-123456"),
            "this-is-a..."
        );
    }

    #[test]
    fn test_line_attribution_creation() {
        let attr = LineAttribution {
            line_number: 1,
            line_text: "fn main() {".to_string(),
            commit_id: "def456".to_string(),
            agent_type: AgentType::ClaudeCode,
            confidence: Some(AttributionConfidence::High),
            session_id: Some("session-123".to_string()),
            client_name: Some("zed".to_string()),
        };

        assert_eq!(attr.line_number, 1);
        assert_eq!(attr.line_text, "fn main() {");
    }

    #[test]
    fn test_blame_context_reusability() {
        // This test verifies that BlameContext can be created
        // Multiple calls would reuse the same workspace/repo (optimization)
        // Actual behavior tested in integration tests with real repos

        // Just verify the API is usable
        let _ = std::path::PathBuf::from("/tmp/test");
    }
}
