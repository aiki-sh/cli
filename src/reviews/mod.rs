//! Review management — scope types and review domain logic.

use std::collections::HashMap;

use std::path::Path;

use crate::agents::{get_available_agents, AgentType};
use crate::error::{AikiError, Result};
use crate::tasks::{find_task, materialize_graph_with_ids, read_events_with_ids, Task};

pub mod create;
pub mod detect;
pub mod history;
pub mod issues;
pub mod location;
pub mod output;
pub mod scope;

pub use create::{create_review, CreateReviewParams};
pub use detect::detect_target;
pub use history::{epic_review_history, ReviewIteration};
#[cfg(test)]
pub use history::{ReviewFix, ReviewIssue};
pub use issues::{get_issue_comments, has_actionable_issues, issue_count};
pub use location::{format_locations, parse_locations, Location};
pub use output::review_summary;
pub use scope::{ReviewScope, ReviewScopeKind};

/// Check if a task is a review task.
///
/// A task is considered a review task if:
/// 1. Its task_type is explicitly "review", OR
/// 2. It was created from a review template (template starts with "review" or legacy "aiki/review")
pub fn is_review_task(task: &Task) -> bool {
    if task.task_type.as_deref() == Some("review") {
        return true;
    }
    if let Some(ref template) = task.template {
        if template.starts_with("review") || template.starts_with("aiki/review") {
            return true;
        }
    }
    false
}

/// Resolve the plan template from CLI arg or review task data.
///
/// Priority: CLI arg > review_task.data["options.fix_template"] > None (caller default).
#[allow(dead_code)]
pub fn resolve_plan_template(
    cli_arg: Option<String>,
    review_data: &HashMap<String, String>,
) -> Option<String> {
    cli_arg.or_else(|| review_data.get("options.fix_template").cloned())
}

/// Resolves the final template name for fix-plan tasks.
/// Combines CLI arg / review-data resolution with the default fallback.
#[allow(dead_code)]
pub fn resolve_fix_template_name(
    cli_arg: Option<String>,
    review_data: &HashMap<String, String>,
) -> String {
    resolve_plan_template(cli_arg, review_data).unwrap_or_else(|| "fix".to_string())
}

/// Resolve scope and assignee from a review task.
///
/// Loads the task graph, extracts the review's scope, and determines the
/// appropriate followup assignee.
pub fn resolve_scope_and_assignee(
    cwd: &Path,
    review_id: &str,
    agent: Option<&str>,
) -> Result<(ReviewScope, Option<String>)> {
    let events_with_ids = read_events_with_ids(cwd)?;
    let tasks = materialize_graph_with_ids(&events_with_ids).tasks;
    let review_task = find_task(&tasks, review_id)?;
    let scope = ReviewScope::from_data(&review_task.data)?;
    let agent_type = agent.and_then(AgentType::from_str);
    let assignee =
        determine_followup_assignee(agent_type, None, review_task.assignee.as_deref(), None).ok();
    Ok((scope, assignee))
}

/// Determine assignee for followup task.
///
/// The followup should be assigned to whoever did the original work (the reviewed task's assignee),
/// not the opposite of the reviewer. The person who wrote the code should fix issues in their code.
/// When no assignee is known and multiple agents are available, falls back to the default coder
/// (first available agent, typically claude-code).
///
/// `exclude` names an agent to avoid (typically the reviewer). When picking from
/// the agent registry, the first agent that isn't the excluded one wins.
pub fn determine_followup_assignee(
    agent_override: Option<AgentType>,
    reviewed_task: Option<&Task>,
    exclude: Option<&str>,
    available_agents: Option<&[AgentType]>,
) -> Result<String> {
    // Tier 1: Explicit agent override
    if let Some(agent) = agent_override {
        return Ok(agent.as_str().to_string());
    }

    // Tier 2: Original task assignee
    if let Some(task) = reviewed_task {
        if let Some(ref assignee) = task.assignee {
            return Ok(assignee.clone());
        }
    }

    // Tier 3: Use agent registry, preferring an agent that isn't `exclude`
    let available = match available_agents {
        Some(agents) => agents.to_vec(),
        None => get_available_agents(),
    };
    if available.is_empty() {
        return Err(AikiError::Other(anyhow::anyhow!(
            "No agent CLIs found on PATH. Install claude or codex to use task delegation."
        )));
    }
    // Pick first agent that isn't the excluded one; fall back to first available
    let pick = exclude
        .and_then(|ex| available.iter().find(|a| a.as_str() != ex))
        .unwrap_or(&available[0]);
    Ok(pick.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{TaskPriority, TaskStatus};

    fn make_test_task(id: &str) -> Task {
        Task {
            id: id.to_string(),
            name: format!("Task {}", id),
            slug: None,
            task_type: None,
            status: TaskStatus::Open,
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            instructions: None,
            data: HashMap::new(),
            created_at: chrono::Utc::now(),
            started_at: None,
            claimed_by_session: None,
            last_session_id: None,
            stopped_reason: None,
            closed_outcome: None,
            confidence: None,
            summary: None,
            turn_started: None,
            closed_at: None,
            turn_closed: None,
            turn_stopped: None,
            comments: Vec::new(),
        }
    }

    #[test]
    fn test_determine_followup_assignee_override() {
        let mut task = make_test_task("test");
        task.assignee = Some("codex".to_string());

        // Override should take precedence (Tier 1)
        let result = determine_followup_assignee(Some(AgentType::Codex), Some(&task), None, None);
        assert_eq!(result.unwrap(), "codex");
    }

    #[test]
    fn test_determine_followup_assignee_from_reviewed_task() {
        let mut task = make_test_task("reviewed");
        task.name = "Original Work".to_string();
        task.status = TaskStatus::Closed;
        task.assignee = Some("claude-code".to_string());

        let result = determine_followup_assignee(None, Some(&task), None, None);
        assert_eq!(result.unwrap(), "claude-code");
    }

    #[test]
    fn test_determine_followup_assignee_no_reviewed_task_no_agents() {
        let result = determine_followup_assignee(None, None, None, Some(&[]));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No agent CLIs found"));
    }

    #[test]
    fn test_determine_followup_assignee_no_reviewed_task_single_agent() {
        let agents = [AgentType::ClaudeCode];
        let result = determine_followup_assignee(None, None, None, Some(&agents));
        assert_eq!(result.unwrap(), "claude-code");
    }

    #[test]
    fn test_determine_followup_assignee_no_reviewed_task_multiple_agents() {
        let agents = [AgentType::ClaudeCode, AgentType::Codex];
        let result = determine_followup_assignee(None, None, None, Some(&agents));
        assert_eq!(result.unwrap(), "claude-code");
    }

    #[test]
    fn test_determine_followup_assignee_reviewed_task_no_assignee_no_agents() {
        let task = make_test_task("no-assignee");
        let result = determine_followup_assignee(None, Some(&task), None, Some(&[]));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("No agent CLIs found"));
    }

    #[test]
    fn test_determine_followup_assignee_reviewed_task_no_assignee_single_agent() {
        let task = make_test_task("no-assignee");
        let agents = [AgentType::Codex];
        let result = determine_followup_assignee(None, Some(&task), None, Some(&agents));
        assert_eq!(result.unwrap(), "codex");
    }

    #[test]
    fn test_determine_followup_assignee_reviewed_task_no_assignee_multiple_agents() {
        let task = make_test_task("no-assignee");
        let agents = [AgentType::ClaudeCode, AgentType::Codex];
        let result = determine_followup_assignee(None, Some(&task), None, Some(&agents));
        assert_eq!(result.unwrap(), "claude-code");
    }

    #[test]
    fn test_followup_assignee_tier1_explicit_override() {
        let agents = [AgentType::Codex];
        let result =
            determine_followup_assignee(Some(AgentType::ClaudeCode), None, None, Some(&agents));
        assert_eq!(result.unwrap(), "claude-code");
    }

    #[test]
    fn test_followup_assignee_tier2_original_task_assignee() {
        let mut task = make_test_task("original");
        task.assignee = Some("codex".to_string());
        let result = determine_followup_assignee(None, Some(&task), None, None);
        assert_eq!(result.unwrap(), "codex");
    }

    #[test]
    fn test_followup_assignee_tier3_single_available_agent() {
        let agents = [AgentType::Codex];
        let result = determine_followup_assignee(None, None, None, Some(&agents));
        assert_eq!(result.unwrap(), "codex");
    }

    #[test]
    fn test_followup_assignee_tier4_default_coder() {
        let agents = [AgentType::ClaudeCode, AgentType::Codex];
        let result = determine_followup_assignee(None, None, None, Some(&agents));
        assert_eq!(result.unwrap(), "claude-code");
    }

    #[test]
    fn test_followup_assignee_no_agents_errors() {
        let result = determine_followup_assignee(None, None, None, Some(&[]));
        assert!(result.is_err());
    }

    #[test]
    fn test_followup_assignee_exclude_reviewer() {
        let agents = [AgentType::ClaudeCode, AgentType::Codex];
        let result = determine_followup_assignee(None, None, Some("codex"), Some(&agents));
        assert_eq!(result.unwrap(), "claude-code");

        let result = determine_followup_assignee(None, None, Some("claude-code"), Some(&agents));
        assert_eq!(result.unwrap(), "codex");
    }

    #[test]
    fn test_followup_assignee_exclude_only_agent_falls_back() {
        let agents = [AgentType::Codex];
        let result = determine_followup_assignee(None, None, Some("codex"), Some(&agents));
        assert_eq!(result.unwrap(), "codex");
    }

    /// Validates the review→fix agent assignment chain for plan reviews.
    ///
    /// When a user runs `aiki review <plan> -f` from their terminal (no active
    /// agent session), the worker should default to the default coder (claude-code).
    /// The reviewer is then the cross-review agent (codex). The fix followup
    /// excludes the reviewer and picks claude-code — the coder does the fix.
    ///
    /// Previously, `find_active_session` could match a stale repo session
    /// (e.g. codex), making the system think codex was the worker. This flipped
    /// the reviewer to claude-code and the fix agent to codex — the opposite
    /// of the intended behavior.
    #[test]
    fn plan_review_fix_chain_terminal_user_assigns_coder_to_fix() {
        use crate::agents::determine_reviewer_with;

        let agents = [AgentType::ClaudeCode, AgentType::Codex];

        // Simulate: user runs from terminal, find_own_session returns None,
        // falls back to determine_default_coder → "claude-code"
        let worker = Some("claude-code");

        // Step 1: determine_reviewer picks cross-review agent for the review task
        let reviewer = determine_reviewer_with(worker, &agents).unwrap();
        assert_eq!(reviewer, "codex", "reviewer should be cross-review of worker");

        // Step 2: fix followup excludes the reviewer, picks the coder
        let fix_agent =
            determine_followup_assignee(None, None, Some(&reviewer), Some(&agents)).unwrap();
        assert_eq!(
            fix_agent, "claude-code",
            "fix agent should be the coder, not the reviewer"
        );
    }

    /// Validates the chain when an agent (e.g. codex) runs `aiki review` on
    /// its own work — find_own_session detects it, reviewer is cross-review,
    /// fix goes back to the original agent.
    #[test]
    fn plan_review_fix_chain_agent_session_assigns_coder_to_fix() {
        use crate::agents::determine_reviewer_with;

        let agents = [AgentType::ClaudeCode, AgentType::Codex];

        // Simulate: codex is running and reviews its own plan
        let worker = Some("codex");

        // Step 1: reviewer is cross-review
        let reviewer = determine_reviewer_with(worker, &agents).unwrap();
        assert_eq!(reviewer, "claude-code");

        // Step 2: fix excludes reviewer, picks codex (original worker)
        let fix_agent =
            determine_followup_assignee(None, None, Some(&reviewer), Some(&agents)).unwrap();
        assert_eq!(
            fix_agent, "codex",
            "fix should go back to the original worker"
        );
    }

    /// The bug scenario: if worker is incorrectly detected as codex (stale
    /// repo session), reviewer becomes claude-code, and fix goes to codex.
    /// This test documents the wrong behavior to prevent regression.
    #[test]
    fn plan_review_fix_chain_wrong_worker_causes_wrong_fix_agent() {
        use crate::agents::determine_reviewer_with;

        let agents = [AgentType::ClaudeCode, AgentType::Codex];

        // BUG: find_active_session matched a stale codex session
        let wrong_worker = Some("codex");

        let reviewer = determine_reviewer_with(wrong_worker, &agents).unwrap();
        assert_eq!(reviewer, "claude-code", "reviewer flipped to claude-code");

        let fix_agent =
            determine_followup_assignee(None, None, Some(&reviewer), Some(&agents)).unwrap();
        // This is the WRONG result — codex does the fix instead of claude-code
        assert_eq!(
            fix_agent, "codex",
            "demonstrates the bug: fix goes to codex instead of claude-code"
        );
    }
}
