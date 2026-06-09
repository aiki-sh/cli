//! Review scope types — what is being reviewed and how.

use std::collections::HashMap;
use std::path::Path;

use crate::error::{AikiError, Result};

/// What kind of review scope this is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewScopeKind {
    Task,
    Plan,
    Code,
    Session,
}

impl ReviewScopeKind {
    /// Convert to string representation for serialization.
    pub fn as_str(&self) -> &str {
        match self {
            ReviewScopeKind::Task => "task",
            ReviewScopeKind::Plan => "plan",
            ReviewScopeKind::Code => "code",
            ReviewScopeKind::Session => "session",
        }
    }

    /// Parse from string representation.
    pub fn from_str(s: &str) -> Result<Self> {
        match s {
            "task" => Ok(ReviewScopeKind::Task),
            "plan" => Ok(ReviewScopeKind::Plan),
            "code" => Ok(ReviewScopeKind::Code),
            "session" => Ok(ReviewScopeKind::Session),
            _ => Err(AikiError::UnknownReviewScope(s.to_string())),
        }
    }
}

/// What is being reviewed and how.
#[derive(Debug, Clone)]
pub struct ReviewScope {
    pub kind: ReviewScopeKind,
    /// Task ID or file path depending on kind.
    pub id: String,
    /// Task IDs for session reviews (empty otherwise).
    pub task_ids: Vec<String>,
}

impl ReviewScope {
    /// Get display name (computed from kind and id).
    pub fn name(&self) -> String {
        match self.kind {
            ReviewScopeKind::Task => format!("Task ({})", &self.id),
            ReviewScopeKind::Plan => {
                let filename = Path::new(&self.id)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&self.id);
                format!("Plan ({})", filename)
            }
            ReviewScopeKind::Code => {
                let filename = Path::new(&self.id)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&self.id);
                format!("Code ({})", filename)
            }
            ReviewScopeKind::Session => "Session".to_string(),
        }
    }

    /// Serialize to task data HashMap for persistence.
    pub fn to_data(&self) -> HashMap<String, String> {
        let mut data = HashMap::new();
        data.insert("scope.kind".into(), self.kind.as_str().into());
        data.insert("scope.id".into(), self.id.clone());
        data.insert("scope.name".into(), self.name());
        if !self.task_ids.is_empty() {
            data.insert("scope.task_ids".into(), self.task_ids.join(","));
        }
        data
    }

    /// Deserialize from task data HashMap.
    pub fn from_data(data: &HashMap<String, String>) -> Result<Self> {
        let kind_str = data.get("scope.kind").ok_or_else(|| {
            AikiError::InvalidArgument("Missing scope.kind in review task data".into())
        })?;
        let kind = ReviewScopeKind::from_str(kind_str)?;

        // scope.id is required for non-Session scopes (Task, Plan, Code)
        let id = match kind {
            ReviewScopeKind::Session => data.get("scope.id").cloned().unwrap_or_default(),
            _ => data
                .get("scope.id")
                .filter(|s| !s.is_empty())
                .cloned()
                .ok_or_else(|| {
                    AikiError::InvalidArgument(format!(
                        "Missing scope.id in review task data (required for {:?} scope kind)",
                        kind_str
                    ))
                })?,
        };

        Ok(Self {
            kind,
            id,
            task_ids: data
                .get("scope.task_ids")
                .map(|s| s.split(',').map(String::from).collect())
                .unwrap_or_default(),
        })
    }
}
