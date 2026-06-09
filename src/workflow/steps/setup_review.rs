//! SetupReview step: detect target, validate constraints, create review task.

use super::{StepResult, WorkflowChange, WorkflowContext};
use crate::error::AikiError;
use crate::reviews::{
    create_review, detect_target, CreateReviewParams, ReviewScope, ReviewScopeKind,
};

/// Build the review scope for a build/fix workflow review step.
///
/// Uses `Task` scope so that downstream fix tasks become subtasks of the epic,
/// which triggers `reopen_if_closed` and keeps the epic in-progress during the
/// review/fix cycle.
pub(crate) fn build_review_scope(epic_id: &str) -> ReviewScope {
    ReviewScope {
        kind: ReviewScopeKind::Task,
        id: epic_id.to_string(),
        task_ids: vec![],
    }
}

fn review_agent_override(review_agent: Option<&str>) -> Result<Option<String>, AikiError> {
    if let Some(agent_str) = review_agent {
        let agent_type = crate::agents::AgentType::from_str(agent_str)
            .ok_or_else(|| AikiError::UnknownAgentType(agent_str.to_string()))?;
        Ok(Some(agent_type.as_str().to_string()))
    } else {
        Ok(None)
    }
}

/// Setup review step: detect target, validate constraints, create review task.
///
/// Sets `ctx.task_id` to the created review task ID. Does NOT run the review —
/// that's the job of the subsequent `Review` step.
///
/// Scope resolution:
/// - If `target` is None and `ctx.task_id` is set (build/fix workflow), derives
///   scope from the existing task via `build_review_scope`.
/// - Otherwise, detects target from CLI args via `detect_target`.
pub(crate) fn run(ctx: &mut WorkflowContext) -> anyhow::Result<StepResult> {
    let target = ctx.opts.target.clone();
    let code = ctx.opts.code;
    let template = ctx.opts.review_template.clone();
    let agent = ctx.opts.reviewer.clone();
    let fix = ctx.opts.fix;
    let fix_template = ctx.opts.fix_template.clone();
    let autorun = ctx.opts.autorun;

    ctx.status("resolving review scope");
    let (scope, _worker) = if target.is_none() && ctx.task_id.is_some() {
        let epic_id = ctx.task_id.as_ref().unwrap();
        (build_review_scope(epic_id), None)
    } else {
        detect_target(&ctx.cwd, target.as_deref(), code)?
    };

    // --fix is not supported for session reviews
    if fix && scope.kind == ReviewScopeKind::Session {
        return Err(AikiError::InvalidArgument(
            "--fix is not supported for session reviews".to_string(),
        )
        .into());
    }

    // Parse agent override
    let agent_override = review_agent_override(agent.as_deref())?;

    ctx.status("creating review task");
    let result = create_review(
        &ctx.cwd,
        CreateReviewParams {
            scope,
            agent_override,
            template,
            fix_template,
            autorun,
        },
    )?;

    ctx.task_id = Some(result.review_task_id.clone());

    Ok(StepResult {
        change: WorkflowChange::None,
        message: format!("Created review {}", &result.review_task_id[..7]),
        task_id: Some(result.review_task_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_agent_override_is_none_without_override() {
        assert_eq!(review_agent_override(None).unwrap(), None);
    }

    #[test]
    fn review_agent_override_normalizes_aliases() {
        assert_eq!(
            review_agent_override(Some("claude")).unwrap(),
            Some("claude-code".to_string())
        );
    }

    #[test]
    fn review_agent_override_rejects_unknown_agents() {
        assert!(review_agent_override(Some("not-an-agent")).is_err());
    }
}
