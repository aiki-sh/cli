//! SetupFix step: validate review task, resolve scope/assignee/template, create fix-parent.

use crate::agents::AgentType;
use crate::error::AikiError;
use crate::reviews::determine_followup_assignee;
use crate::reviews::{has_actionable_issues, is_review_task};
use crate::reviews::{ReviewScope, ReviewScopeKind};
use crate::tasks::{find_task, materialize_graph_with_ids, read_events_with_ids};

use super::fix::create_fix_parent;
use super::{StepResult, WorkflowChange, WorkflowContext};

/// Setup fix step: validate review, resolve scope/assignee/template, create fix-parent.
///
/// Sets `ctx.task_id` to the created fix-parent task ID. Does NOT run any
/// fix agents — that's the job of subsequent steps.
///
/// Short-circuits with an early return if the review has no actionable issues.
pub(crate) fn run(ctx: &mut WorkflowContext) -> anyhow::Result<StepResult> {
    let cwd = ctx.cwd.clone();
    let agent = ctx.opts.coder.clone();
    let autorun = ctx.opts.autorun;
    let review_id = ctx
        .review_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Step::SetupFix requires ctx.review_id"))?
        .to_string();

    // Parse agent override
    let agent_type = if let Some(ref agent_str) = agent {
        Some(
            AgentType::from_str(agent_str)
                .ok_or_else(|| AikiError::UnknownAgentType(agent_str.clone()))?,
        )
    } else {
        None
    };

    ctx.status("loading review task");
    let events_with_ids = read_events_with_ids(&cwd)?;
    let tasks = materialize_graph_with_ids(&events_with_ids).tasks;

    // Find and validate review task
    let review_task = find_task(&tasks, &review_id)?;
    if !is_review_task(review_task) {
        return Err(AikiError::InvalidArgument(format!(
            "No review task found for ID: {}",
            review_id
        ))
        .into());
    }

    // Short-circuit if no actionable issues
    if !has_actionable_issues(review_task) {
        return Ok(StepResult {
            change: WorkflowChange::None,
            message: "approved — no actionable issues".to_string(),
            task_id: None,
        });
    }

    // Load scope from review data
    let scope = ReviewScope::from_data(&review_task.data)?;

    ctx.status("resolving assignee");
    let assignee = match scope.kind {
        ReviewScopeKind::Task => {
            let original_task = find_task(&tasks, &scope.id).ok();
            Some(determine_followup_assignee(
                agent_type,
                original_task,
                None,
                None,
            )?)
        }
        _ => Some(determine_followup_assignee(
            agent_type,
            None,
            review_task.assignee.as_deref(),
            None,
        )?),
    };

    ctx.status("creating fix-parent task");
    let fix_parent_id = create_fix_parent(&cwd, &review_id, &scope, &assignee, autorun)?;
    ctx.task_id = Some(fix_parent_id.clone());

    Ok(StepResult {
        change: WorkflowChange::None,
        message: format!("Created fix-parent {}", &fix_parent_id[..7]),
        task_id: Some(fix_parent_id),
    })
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

    #[test]
    fn approved_review_without_scope_short_circuits_before_scope_resolution() {
        let temp_dir = tempdir().unwrap();
        init_jj_repo(temp_dir.path());

        let review_id = "review-approved-no-scope";
        let mut data = std::collections::HashMap::new();
        data.insert("issue_count".to_string(), "0".to_string());
        write_event(
            temp_dir.path(),
            &TaskEvent::Created {
                task_id: review_id.to_string(),
                name: "Review: no-op".to_string(),
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
            task_id: None,
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

        assert_eq!(result.message, "approved — no actionable issues");
        assert!(result.task_id.is_none());
        assert!(ctx.task_id.is_none());
    }
}
