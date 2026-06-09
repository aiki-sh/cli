//! Regression review step: quality-loop logic after fix.
//!
//! Checks the fix-parent review outcome, runs a regression review of the
//! original scope if needed, and injects the next quality-loop cycle via
//! `WorkflowChange::NextSteps`.

use std::path::Path;

use crate::error::AikiError;
use crate::reviews::{
    create_review, has_actionable_issues, CreateReviewParams, ReviewScope, ReviewScopeKind,
};
use crate::tasks::runner::TaskRunOptions;
use crate::tasks::types::TaskOutcome;
use crate::tasks::{find_task, materialize_graph_with_ids, read_events_with_ids};
use crate::tasks::{write_event, TaskEvent};
use crate::workflow::fix::{determine_review_outcome, ReviewOutcome, MAX_QUALITY_ITERATIONS};

use super::run_task_with_show_tui;
use super::Step;
use super::StepResult;
use super::WorkflowChange;
use super::WorkflowContext;

// ── Scope resolution ────────────────────────────────────────────────

/// How to obtain the review scope for a fix review.
#[derive(Clone, Copy)]
enum ReviewScopeSource {
    /// Scope is the fix-parent task itself.
    #[allow(dead_code)]
    FixParent,
    /// Scope is preserved in workflow context from the original review target.
    FromWorkflowScope,
}

/// Resolve the regression review scope from workflow context.
#[allow(dead_code)]
pub(crate) fn resolve_regression_review_scope(
    ctx: &WorkflowContext,
) -> anyhow::Result<Option<ReviewScope>> {
    resolve_review_scope(ctx, ReviewScopeSource::FromWorkflowScope)
}

fn resolve_review_scope(
    ctx: &WorkflowContext,
    scope_source: ReviewScopeSource,
) -> anyhow::Result<Option<ReviewScope>> {
    let cwd = ctx.cwd.clone();
    let task_id = match ctx.task_id() {
        Some(id) => id.to_string(),
        None => return Ok(None),
    };

    let scope = match scope_source {
        ReviewScopeSource::FixParent => ReviewScope {
            kind: ReviewScopeKind::Task,
            id: task_id,
            task_ids: vec![],
        },
        ReviewScopeSource::FromWorkflowScope => {
            if let Some(scope) = ctx.scope.clone() {
                scope
            } else {
                let events_with_ids = read_events_with_ids(&cwd)?;
                let tasks = materialize_graph_with_ids(&events_with_ids).tasks;
                let fix_parent = find_task(&tasks, &task_id)?;
                match ReviewScope::from_data(&fix_parent.data) {
                    Ok(scope) => scope,
                    Err(AikiError::InvalidArgument(message))
                        if message.starts_with("Missing scope.") =>
                    {
                        return Ok(None);
                    }
                    Err(err) => return Err(err.into()),
                }
            }
        }
    };

    Ok(Some(scope))
}

fn build_fix_review_params(
    scope: ReviewScope,
    template: Option<String>,
    review_agent: Option<String>,
) -> CreateReviewParams {
    CreateReviewParams {
        scope,
        agent_override: review_agent,
        template,
        fix_template: None,
        autorun: false,
    }
}

/// Run a review within the fix workflow (creates task + runs agent).
fn run_review_for_fix(
    ctx: &mut WorkflowContext,
    scope_source: ReviewScopeSource,
    template: Option<String>,
    agent: Option<String>,
) -> anyhow::Result<StepResult> {
    let cwd = ctx.cwd.clone();
    let Some(scope) = resolve_review_scope(ctx, scope_source)? else {
        return Ok(StepResult {
            change: WorkflowChange::None,
            message: "skipped".to_string(),
            task_id: None,
        });
    };

    ctx.status("creating review task");
    let review_result = create_review(&cwd, build_fix_review_params(scope, template, agent))?;

    ctx.status("running review agent");
    if ctx.notify_rx.is_some() {
        let output = ctx.output;
        let run_options = TaskRunOptions::new();
        let mut handler = super::ReviewDrainHandler::new(review_result.review_task_id.clone(), output);
        super::spawn_drain_finalize(
            &cwd,
            &review_result.review_task_id,
            &run_options,
            ctx.notify_rx.as_ref(),
            output,
            &mut handler,
        )?;
    } else {
        let run_options = TaskRunOptions::new();
        run_task_with_show_tui(&cwd, &review_result.review_task_id, run_options, false)?;
    }

    Ok(StepResult {
        change: WorkflowChange::None,
        message: match scope_source {
            ReviewScopeSource::FixParent => "review complete",
            ReviewScopeSource::FromWorkflowScope => "regression review complete",
        }
        .to_string(),
        task_id: Some(review_result.review_task_id),
    })
}

// ── Fix-parent helpers ──────────────────────────────────────────────

/// Close the fix-parent task when looping back to a new iteration.
fn close_fix_parent(cwd: &Path, fix_parent_id: &str, iteration: usize) -> anyhow::Result<()> {
    let summary = format!("Fix iteration {} complete, looping back", iteration);
    write_event(
        cwd,
        &TaskEvent::Closed {
            task_ids: vec![fix_parent_id.to_string()],
            outcome: TaskOutcome::Done,
            confidence: None,
            summary: Some(summary),
            session_id: None,
            turn_id: None,
            timestamp: chrono::Utc::now(),
        },
    )?;
    Ok(())
}

/// Extract the fix-parent ID from the review task's scope data.
fn fix_parent_id_from_review(review_task: &crate::tasks::types::Task) -> Option<String> {
    ReviewScope::from_data(&review_task.data)
        .ok()
        .map(|scope| scope.id)
}

// ── Step handler ────────────────────────────────────────────────────

/// Regression review step: checks the fix-parent review outcome, runs a
/// regression review of the original scope if needed, and injects the next
/// quality-loop cycle via `WorkflowChange::NextSteps`.
pub(crate) fn run(ctx: &mut WorkflowContext) -> anyhow::Result<StepResult> {
    let cwd = ctx.cwd.clone();

    // Get the fix-parent review (set by SetupReview → Review steps).
    // When task_id is None, Fix short-circuited because the review had no
    // actionable issues. The unified skip policy now always leaves
    // RegressionReview in the queue, so we handle this as "approved".
    let fix_parent_review_id = match ctx.task_id.as_ref() {
        Some(id) => id.clone(),
        None => {
            return Ok(StepResult {
                change: WorkflowChange::None,
                message: "approved — no actionable issues".to_string(),
                task_id: None,
            });
        }
    };

    ctx.status("checking fix-parent review");
    let events_with_ids = read_events_with_ids(&cwd)?;
    let tasks = materialize_graph_with_ids(&events_with_ids).tasks;
    let fix_parent_review = find_task(&tasks, &fix_parent_review_id)?;
    let fix_parent_has_issues = has_actionable_issues(fix_parent_review);

    // Extract fix-parent ID from the review's scope so we can close it on loop-back.
    let fix_parent_id = fix_parent_id_from_review(fix_parent_review);

    // First decision: does the fix-parent review itself have issues?
    let outcome =
        determine_review_outcome(fix_parent_has_issues, &fix_parent_review_id, None, None);

    match outcome {
        ReviewOutcome::LoopBack(id) => {
            if let Some(ref fp_id) = fix_parent_id {
                close_fix_parent(&cwd, fp_id, ctx.iteration)?;
            }
            return Ok(capped_loop_back(ctx, id, fix_parent_review_id));
        }
        ReviewOutcome::ReReviewOriginalScope => {
            ctx.status("running regression review");
            let template = ctx.opts.review_template.clone();
            let agent = ctx.opts.reviewer.clone();
            let regression_result =
                run_review_for_fix(ctx, ReviewScopeSource::FromWorkflowScope, template, agent)?;

            let Some(ref regression_review_id) = regression_result.task_id else {
                ctx.warn(
                    "skipping regression review because the original scope could not be resolved",
                );
                return Ok(StepResult {
                    change: WorkflowChange::None,
                    message: "skipped — original scope unavailable".to_string(),
                    task_id: Some(fix_parent_review_id),
                });
            };

            ctx.status("checking regression results");
            let events_with_ids = read_events_with_ids(&cwd)?;
            let tasks = materialize_graph_with_ids(&events_with_ids).tasks;
            let regression_review = find_task(&tasks, regression_review_id)?;

            let orig_outcome = determine_review_outcome(
                false, // fix-parent already passed
                &fix_parent_review_id,
                Some(has_actionable_issues(regression_review)),
                Some(regression_review_id),
            );

            match orig_outcome {
                ReviewOutcome::Approved(id) => Ok(result_approved(id)),
                ReviewOutcome::LoopBack(id) => {
                    if let Some(ref fp_id) = fix_parent_id {
                        close_fix_parent(&cwd, fp_id, ctx.iteration)?;
                    }
                    Ok(capped_loop_back(ctx, id, fix_parent_review_id))
                }
                ReviewOutcome::ReReviewOriginalScope => Ok(result_re_review(fix_parent_review_id)),
            }
        }
        // Can't happen: no original data was passed.
        ReviewOutcome::Approved(_) => unreachable!(),
    }
}

// ── Review outcome helpers ──────────────────────────────────────────

fn result_approved(review_id: String) -> StepResult {
    StepResult {
        message: "approved".to_string(),
        task_id: Some(review_id),
        change: WorkflowChange::None,
    }
}

fn result_loop_back(ctx: &mut WorkflowContext, new_review_id: String) -> StepResult {
    ctx.review_id = Some(new_review_id.clone());
    ctx.iteration += 1;
    // Reset task_id so that Fix creates a new fix-parent.
    ctx.task_id = None;

    StepResult {
        message: format!("issues found — iteration {}", ctx.iteration),
        task_id: Some(new_review_id),
        change: WorkflowChange::NextSteps(vec![
            Step::Fix,
            Step::Decompose,
            Step::Loop,
            Step::SetupReview,
            Step::Review,
            Step::RegressionReview,
        ]),
    }
}

fn capped_loop_back(
    ctx: &mut WorkflowContext,
    new_review_id: String,
    terminal_review_id: String,
) -> StepResult {
    if loop_back_allowed(ctx.iteration) {
        result_loop_back(ctx, new_review_id)
    } else {
        result_max_iterations(ctx, terminal_review_id)
    }
}

fn loop_back_allowed(current_iteration: usize) -> bool {
    current_iteration
        .checked_add(1)
        .is_some_and(|next_iteration| next_iteration <= MAX_QUALITY_ITERATIONS)
}

fn result_max_iterations(ctx: &WorkflowContext, review_id: String) -> StepResult {
    ctx.warn(&format!(
        "quality loop reached maximum iterations ({}) without full approval. Review {} may still have unresolved issues.",
        MAX_QUALITY_ITERATIONS,
        review_id
    ));
    StepResult {
        message: format!("max iterations ({}) reached", MAX_QUALITY_ITERATIONS),
        task_id: Some(review_id),
        change: WorkflowChange::None,
    }
}

fn result_re_review(fix_parent_review_id: String) -> StepResult {
    StepResult {
        message: "fix passed — checking original scope".to_string(),
        task_id: Some(fix_parent_review_id),
        change: WorkflowChange::NextSteps(vec![Step::RegressionReview]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{write_event, TaskEvent, TaskPriority};
    use crate::workflow::{OutputKind, WorkflowOpts, WorkflowOutput};
    use chrono::Utc;
    use tempfile::tempdir;

    fn init_jj_repo(path: &std::path::Path) {
        let git = std::process::Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .expect("initialize git repo");
        assert!(
            git.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&git.stderr)
        );

        let jj = std::process::Command::new("jj")
            .args(["git", "init", "--colocate"])
            .current_dir(path)
            .output()
            .expect("initialize jj repo");
        assert!(
            jj.status.success(),
            "jj git init failed: {}",
            String::from_utf8_lossy(&jj.stderr)
        );
    }

    fn test_ctx(task_id: Option<&str>, scope: Option<ReviewScope>) -> WorkflowContext {
        WorkflowContext {
            task_id: task_id.map(str::to_string),
            plan_path: None,
            cwd: std::env::current_dir().unwrap(),
            output: WorkflowOutput::new(OutputKind::Quiet),
            opts: WorkflowOpts::default(),
            review_id: None,
            scope,
            assignee: None,
            iteration: 0,
            notify_rx: None,
            task_names: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn regression_review_prefers_preserved_workflow_scope() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Task,
            id: "fix-parent-original".to_string(),
            task_ids: vec![],
        };
        let ctx = test_ctx(Some("review-task-overwrite"), Some(scope.clone()));

        let resolved = resolve_review_scope(&ctx, ReviewScopeSource::FromWorkflowScope)
            .unwrap()
            .unwrap();

        assert_eq!(resolved.kind, scope.kind);
        assert_eq!(resolved.id, scope.id);
        assert_eq!(resolved.task_ids, scope.task_ids);
    }

    #[test]
    fn fix_review_uses_current_task_as_review_target() {
        let ctx = test_ctx(Some("fix-parent-123"), None);

        let resolved = resolve_review_scope(&ctx, ReviewScopeSource::FixParent)
            .unwrap()
            .unwrap();

        assert_eq!(resolved.kind, ReviewScopeKind::Task);
        assert_eq!(resolved.id, "fix-parent-123");
        assert!(resolved.task_ids.is_empty());
    }

    #[test]
    fn missing_task_id_skips_scope_resolution() {
        let ctx = test_ctx(None, None);

        let resolved = resolve_review_scope(&ctx, ReviewScopeSource::FromWorkflowScope).unwrap();

        assert!(resolved.is_none());
    }

    #[test]
    fn build_fix_review_params_preserves_explicit_reviewer_override() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Task,
            id: "fix-parent-123".to_string(),
            task_ids: vec![],
        };

        let params = build_fix_review_params(
            scope.clone(),
            Some("review/task".to_string()),
            Some("claude-code".to_string()),
        );

        assert_eq!(params.scope.id, scope.id);
        assert_eq!(params.template, Some("review/task".to_string()));
        assert_eq!(params.agent_override, Some("claude-code".to_string()));
        assert_eq!(params.fix_template, None);
        assert!(!params.autorun);
    }

    #[test]
    fn build_fix_review_params_leaves_reviewer_unset_without_override() {
        let scope = ReviewScope {
            kind: ReviewScopeKind::Task,
            id: "fix-parent-123".to_string(),
            task_ids: vec![],
        };

        let params = build_fix_review_params(scope, None, None);

        assert_eq!(params.agent_override, None);
    }

    #[test]
    fn build_fix_review_params_does_not_reuse_coder_as_reviewer() {
        let mut ctx = test_ctx(
            Some("fix-parent-review"),
            Some(ReviewScope {
                kind: ReviewScopeKind::Task,
                id: "original-task".to_string(),
                task_ids: vec![],
            }),
        );
        ctx.assignee = Some("codex".to_string());
        ctx.opts.coder = Some("codex".to_string());
        ctx.opts.reviewer = None;

        let scope = resolve_review_scope(&ctx, ReviewScopeSource::FromWorkflowScope)
            .unwrap()
            .unwrap();
        let params = build_fix_review_params(
            scope,
            ctx.opts.review_template.clone(),
            ctx.opts.reviewer.clone(),
        );

        assert_eq!(ctx.assignee.as_deref(), Some("codex"));
        assert_eq!(ctx.opts.coder.as_deref(), Some("codex"));
        assert_eq!(params.agent_override, None);
    }

    #[test]
    fn loop_back_cap_allows_final_legal_iteration() {
        assert!(loop_back_allowed(MAX_QUALITY_ITERATIONS - 1));
    }

    #[test]
    fn loop_back_cap_stops_once_max_iteration_is_reached() {
        assert!(!loop_back_allowed(MAX_QUALITY_ITERATIONS));
    }

    #[test]
    fn capped_loop_back_increments_iteration_when_within_cap() {
        let mut ctx = test_ctx(None, None);
        ctx.iteration = MAX_QUALITY_ITERATIONS - 1;

        let result = capped_loop_back(
            &mut ctx,
            "review-final-pass".to_string(),
            "review-terminal".to_string(),
        );

        assert_eq!(ctx.iteration, MAX_QUALITY_ITERATIONS);
        assert_eq!(ctx.review_id.as_deref(), Some("review-final-pass"));
        assert!(matches!(result.change, WorkflowChange::NextSteps(_)));
    }

    #[test]
    fn capped_loop_back_returns_terminal_result_at_iteration_cap() {
        let mut ctx = test_ctx(None, None);
        ctx.iteration = MAX_QUALITY_ITERATIONS;

        let result = capped_loop_back(
            &mut ctx,
            "review-extra-pass".to_string(),
            "review-terminal".to_string(),
        );

        assert_eq!(ctx.iteration, MAX_QUALITY_ITERATIONS);
        assert!(matches!(result.change, WorkflowChange::None));
        assert_eq!(result.task_id.as_deref(), Some("review-terminal"));
        assert_eq!(
            result.message,
            format!("max iterations ({}) reached", MAX_QUALITY_ITERATIONS)
        );
    }

    #[test]
    fn regression_review_missing_scope_exits_cleanly() {
        let temp_dir = tempdir().unwrap();
        init_jj_repo(temp_dir.path());

        let review_id = "fix-parent-review";
        let mut data = std::collections::HashMap::new();
        data.insert("issue_count".to_string(), "0".to_string());
        write_event(
            temp_dir.path(),
            &TaskEvent::Created {
                task_id: review_id.to_string(),
                name: "Review: fix parent".to_string(),
                slug: None,
                task_type: Some("review".to_string()),
                priority: TaskPriority::P2,
                assignee: Some("codex".to_string()),
                sources: vec![],
                template: Some("review/task".to_string()),
                instructions: None,
                data,
                timestamp: Utc::now(),
            },
        )
        .unwrap();

        let mut ctx = WorkflowContext {
            task_id: Some(review_id.to_string()),
            plan_path: None,
            cwd: temp_dir.path().to_path_buf(),
            output: WorkflowOutput::new(OutputKind::Quiet),
            opts: WorkflowOpts::default(),
            review_id: Some(review_id.to_string()),
            scope: None,
            assignee: None,
            iteration: 0,
            notify_rx: None,
            task_names: std::collections::HashMap::new(),
        };

        let result = run(&mut ctx).unwrap();

        assert!(matches!(result.change, WorkflowChange::None));
        assert_eq!(result.message, "skipped — original scope unavailable");
        assert_eq!(result.task_id.as_deref(), Some(review_id));
    }
}
