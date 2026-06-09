//! Common output formatting for review and fix commands
//!
//! Provides a `CommandOutput` struct and `format_command_output()` that both
//! review.rs and fix.rs use to produce consistent output.

use crate::reviews::{format_locations, parse_locations, ReviewScope};
use crate::tasks::TaskComment;

/// Structured output data for review/fix commands.
///
/// All output functions build one of these, then call `format_command_output()`.
pub struct CommandOutput<'a> {
    /// Heading: "Review Started", "Fix Completed", "Approved", etc.
    pub heading: &'a str,
    /// Task ID of the review or fix task
    pub task_id: &'a str,
    /// Review scope (provides Type + Scope lines)
    pub scope: Option<&'a ReviewScope>,
    /// Status description (e.g., "started", "completed", "started in background")
    pub status: &'a str,
    /// Issue list (when available, e.g., after review completion)
    pub issues: Option<&'a [TaskComment]>,
    /// Action hint: "Run `aiki fix ...` to remediate.", etc.
    pub hint: Option<String>,
}

/// Produces the canonical output block used by all review/fix output functions.
pub fn format_command_output(output: &CommandOutput) -> String {
    let mut content = format!("## {}\n- **Task:** {}\n", output.heading, output.task_id);

    if let Some(scope) = output.scope {
        content.push_str(&format!("- **Type:** {}\n", scope.kind.as_str()));
        content.push_str(&format!("- **Scope:** {}\n", scope.name()));
    }

    if let Some(issues) = output.issues {
        content.push_str(&format!("- **Issues found:** {}\n", issues.len()));
    }

    content.push_str(&format!("- {}\n", output.status));

    if let Some(issues) = output.issues {
        if !issues.is_empty() {
            // Sort by severity for display
            let mut sorted: Vec<&TaskComment> = issues.iter().collect();
            sorted.sort_by_key(|c| {
                let sev = c
                    .data
                    .get("severity")
                    .map(|s| s.as_str())
                    .unwrap_or("medium");
                match sev {
                    "high" => 0u8,
                    "medium" => 1,
                    "low" => 2,
                    _ => 1,
                }
            });
            content.push('\n');
            for (i, comment) in sorted.iter().enumerate() {
                let severity = comment
                    .data
                    .get("severity")
                    .map(|s| s.as_str())
                    .unwrap_or("medium");
                let locations = parse_locations(&comment.data);
                let loc_suffix = format_locations(&locations);
                let display_text = if comment.text.len() > 60 {
                    format!("{}...", &comment.text[..57])
                } else {
                    comment.text.clone()
                };
                if loc_suffix.is_empty() {
                    content.push_str(&format!("{}. [{}] {}\n", i + 1, severity, &display_text));
                } else {
                    content.push_str(&format!(
                        "{}. [{}] {} {}\n",
                        i + 1,
                        severity,
                        &display_text,
                        loc_suffix
                    ));
                }
            }
        }
    }

    if let Some(ref hint) = output.hint {
        content.push_str(&format!("\n---\n{}\n", hint));
    }

    content
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reviews::{ReviewScope, ReviewScopeKind};
    use std::collections::HashMap;

    #[test]
    fn test_format_minimal_output() {
        let output = CommandOutput {
            heading: "Approved",
            task_id: "abc123",
            scope: None,
            status: "No issues found.",
            issues: None,
            hint: None,
        };
        let result = format_command_output(&output);
        assert!(result.contains("## Approved"));
        assert!(result.contains("- **Task:** abc123"));
        assert!(result.contains("- No issues found."));
        assert!(!result.contains("**Type:**"));
        assert!(!result.contains("**Scope:**"));
    }

    #[test]
    fn test_format_with_scope() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Task,
            id: "xqrmnpst".to_string(),
            task_ids: vec![],
        };
        let output = CommandOutput {
            heading: "Review Started",
            task_id: "review123",
            scope: Some(&scope),
            status: "Review started.",
            issues: None,
            hint: None,
        };
        let result = format_command_output(&output);
        assert!(result.contains("- **Type:** task"));
        assert!(result.contains("- **Scope:** Task (xqrmnpst)"));
    }

    #[test]
    fn test_format_with_spec_scope() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Plan,
            id: "ops/now/feature.md".to_string(),
            task_ids: vec![],
        };
        let output = CommandOutput {
            heading: "Review Completed",
            task_id: "review456",
            scope: Some(&scope),
            status: "Review completed.",
            issues: None,
            hint: Some("Run `aiki fix review456` to remediate.".to_string()),
        };
        let result = format_command_output(&output);
        assert!(result.contains("- **Type:** plan"));
        assert!(result.contains("- **Scope:** Plan (feature.md)"));
        assert!(result.contains("---\nRun `aiki fix review456` to remediate."));
    }

    #[test]
    fn test_format_with_code_scope() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Code,
            id: "ops/now/feature.md".to_string(),
            task_ids: vec![],
        };
        let output = CommandOutput {
            heading: "Review Completed",
            task_id: "review789",
            scope: Some(&scope),
            status: "Review completed.",
            issues: None,
            hint: None,
        };
        let result = format_command_output(&output);
        assert!(result.contains("- **Type:** code"));
        assert!(result.contains("- **Scope:** Code (feature.md)"));
    }

    #[test]
    fn test_format_with_issues() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Task,
            id: "xqrmnpst".to_string(),
            task_ids: vec![],
        };
        let comments = vec![
            TaskComment {
                text: "Short issue".to_string(),
                id: None,
                timestamp: chrono::Utc::now(),
                data: HashMap::new(),
            },
            TaskComment {
                text: "A much longer issue description that definitely exceeds the sixty character truncation limit used for display".to_string(),
                id: None,
                timestamp: chrono::Utc::now(),
                data: HashMap::new(),
            },
        ];
        let output = CommandOutput {
            heading: "Fix Followup",
            task_id: "fix123",
            scope: Some(&scope),
            status: "Fix started.",
            issues: Some(&comments),
            hint: None,
        };
        let result = format_command_output(&output);
        assert!(result.contains("- **Issues found:** 2"));
        assert!(result.contains("1. [medium] Short issue"));
        assert!(result
            .contains("2. [medium] A much longer issue description that definitely exceeds t..."));
    }

    #[test]
    fn test_format_with_empty_issues() {
        let comments: Vec<TaskComment> = vec![];
        let output = CommandOutput {
            heading: "Fix Followup",
            task_id: "fix123",
            scope: None,
            status: "No issues.",
            issues: Some(&comments),
            hint: None,
        };
        let result = format_command_output(&output);
        assert!(result.contains("- **Issues found:** 0"));
        // Should not have numbered issue list
        assert!(!result.contains("1."));
    }
}
