use std::collections::HashMap;
use std::path::Path;

use crate::agents::AgentType;
use crate::error::{AikiError, Result};
use crate::tasks::runner::TaskRunOptions;
use crate::tasks::{find_task, materialize_graph_with_ids, read_events_with_ids};
use crate::tasks::{
    generate_task_id, materialize_graph, read_events, write_event, write_link_event,
    write_link_event_with_autorun, TaskEvent, TaskPriority,
};

use super::run_task_with_show_tui;
#[cfg(test)]
use super::Step;
use super::StepResult;
use super::WorkflowChange;
use super::WorkflowContext;
use crate::reviews::{has_actionable_issues, ReviewScope, ReviewScopeKind};
// TODO: tech debt — create_from_template/TemplateTaskParams live in commands::task but
// should move to tasks::templates to eliminate workflow→commands coupling. The function
// is ~300 lines with private helpers (create_subtasks_from_entries, etc.) and 8+ callers
// across commands/, making the move non-trivial.
use super::fix_skip_to_regression_review;
use crate::commands::task::{create_from_template, TemplateTaskParams};

/// Resolve the agent type for a fix session.
///
/// When `--agent` is provided, it always wins. Otherwise, interactive sessions
/// (pair mode) default to `ClaudeCode`, and autonomous sessions use the
/// followup assignee from the review.
pub(crate) fn resolve_fix_agent(
    agent_override: Option<&str>,
    assignee: Option<&str>,
    interactive: bool,
) -> Result<AgentType> {
    // Tier 1: Explicit --agent override
    if let Some(agent_str) = agent_override {
        return AgentType::from_str(agent_str)
            .ok_or_else(|| AikiError::UnknownAgentType(agent_str.to_string()));
    }

    // Tier 2: Interactive sessions default to claude-code
    if interactive {
        return Ok(AgentType::ClaudeCode);
    }

    // Tier 3: Use the resolved assignee, fall back to claude-code
    match assignee.and_then(AgentType::from_str) {
        Some(agent) => Ok(agent),
        None => Ok(AgentType::ClaudeCode),
    }
}

/// Create the fix-parent task (container for fix subtasks, like an epic).
///
/// Emits `remediates` link to the review task and `fixes` links to the
/// reviewed targets.
pub(crate) fn create_fix_parent(
    cwd: &Path,
    review_id: &str,
    scope: &ReviewScope,
    assignee: &Option<String>,
    autorun: bool,
) -> Result<String> {
    let fix_parent_id = generate_task_id("fix-parent");
    let name = format!("Fix: {}", scope.name());
    let mut data = HashMap::new();
    data.insert("review".to_string(), review_id.to_string());

    // Add scope data
    for (k, v) in scope.to_data() {
        data.insert(k, v);
    }

    let event = TaskEvent::Created {
        task_id: fix_parent_id.clone(),
        name,
        slug: None,
        task_type: None,
        priority: TaskPriority::P2,
        assignee: assignee.clone(),
        sources: vec![format!("task:{}", review_id)],
        template: None,
        instructions: None,
        data,
        timestamp: chrono::Utc::now(),
    };
    write_event(cwd, &event)?;

    // Emit remediates link: fix-parent remediates the review task
    let events = read_events(cwd)?;
    let graph = materialize_graph(&events);
    let autorun_opt = if autorun { Some(true) } else { None };
    write_link_event_with_autorun(
        cwd,
        &graph,
        "remediates",
        &fix_parent_id,
        review_id,
        autorun_opt,
    )?;

    // Emit fixes link to the target(s) that were reviewed
    let reviewed_targets = graph.edges.targets(review_id, "validates");
    for target in reviewed_targets {
        write_link_event(cwd, &graph, "fixes", &fix_parent_id, target)?;
    }

    // Add fix-parent as subtask of the original task (epic) so that
    // `task diff <epic>` includes fix changes in the 2-stage review.
    if scope.kind == ReviewScopeKind::Task {
        let events = read_events(cwd)?;
        let graph = materialize_graph(&events);
        write_link_event(cwd, &graph, "subtask-of", &fix_parent_id, &scope.id)?;
    }

    Ok(fix_parent_id)
}

/// Create a plan-fix task from the `fix` template.
pub(crate) fn create_plan_fix_task(
    cwd: &Path,
    review_id: &str,
    fix_parent_id: &str,
    assignee: &Option<String>,
    template_override: Option<&str>,
) -> Result<String> {
    let mut data = HashMap::new();
    data.insert("review".to_string(), review_id.to_string());
    data.insert("target".to_string(), fix_parent_id.to_string());

    let params = TemplateTaskParams {
        template_name: template_override.unwrap_or("fix").to_string(),
        data,
        sources: vec![format!("task:{}", review_id)],
        assignee: assignee.clone(),
        parent_id: None,
        ..Default::default()
    };

    create_from_template(cwd, params)
}

/// Fix plan step: check actionable issues, create fix parent and plan-fix task.
///
/// Reads `ctx.review_id`, `ctx.scope`, and `ctx.assignee` from context.
pub(crate) fn run(ctx: &mut WorkflowContext) -> anyhow::Result<StepResult> {
    let cwd = ctx.cwd.clone();
    let template = ctx.opts.plan_template.clone();
    let autorun = ctx.opts.autorun;
    let review_id = ctx
        .review_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("Step::Fix requires ctx.review_id"))?
        .to_string();
    let scope = ctx
        .scope
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Step::Fix requires ctx.scope"))?
        .clone();
    let assignee = ctx.assignee.clone();

    let fix_parent_id = if let Some(id) = ctx.task_id() {
        id.to_string()
    } else {
        ctx.status("checking review issues");
        let events_with_ids = read_events_with_ids(&cwd)?;
        let tasks = materialize_graph_with_ids(&events_with_ids).tasks;
        let review_task = find_task(&tasks, &review_id)?;

        if !has_actionable_issues(review_task) {
            return Ok(StepResult {
                change: WorkflowChange::SkipSteps(fix_skip_to_regression_review()),
                message: "approved — no actionable issues".to_string(),
                task_id: None,
            });
        }

        ctx.status("creating fix-parent");
        create_fix_parent(&cwd, &review_id, &scope, &assignee, autorun)?
    };

    ctx.status("creating fix plan task");
    let template_name = template.as_deref().unwrap_or("fix");
    let plan_fix_id = create_plan_fix_task(
        &cwd,
        &review_id,
        &fix_parent_id,
        &assignee,
        Some(template_name),
    )?;
    ctx.status("running fix agent");
    let run_options = match ctx.opts.agent {
        Some(agent) => TaskRunOptions::new().with_agent(agent),
        None => TaskRunOptions::new(),
    };
    if ctx.notify_rx.is_some() {
        let output = ctx.output;
        let mut handler = super::SubtaskDrainHandler::new(
            &mut ctx.task_names,
            fix_parent_id.to_string(),
            output,
        );
        super::spawn_drain_finalize(
            &cwd,
            &plan_fix_id,
            &run_options,
            ctx.notify_rx.as_ref(),
            output,
            &mut handler,
        )?;
    } else {
        run_task_with_show_tui(&cwd, &plan_fix_id, run_options, false)?;
    }

    ctx.task_id = Some(fix_parent_id.clone());
    ctx.plan_path = Some(format!("/tmp/aiki/plans/{}.md", plan_fix_id));

    Ok(StepResult {
        change: WorkflowChange::None,
        message: "fix plan created".to_string(),
        task_id: Some(fix_parent_id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fix_skip_to_regression_review_preserves_regression_review() {
        let steps = fix_skip_to_regression_review();

        assert_eq!(steps.len(), 4);
        assert!(steps.contains(&Step::Decompose));
        assert!(steps.contains(&Step::Loop));
        assert!(steps.contains(&Step::SetupReview));
        assert!(steps.contains(&Step::Review));
        assert!(!steps.contains(&Step::RegressionReview));
    }

    #[test]
    fn resolve_fix_agent_override_wins() {
        let agent = resolve_fix_agent(Some("codex"), Some("claude-code"), true).unwrap();
        assert_eq!(agent, AgentType::Codex);
    }

    #[test]
    fn resolve_fix_agent_interactive_defaults_to_claude() {
        let agent = resolve_fix_agent(None, Some("codex"), true).unwrap();
        assert_eq!(agent, AgentType::ClaudeCode);
    }

    #[test]
    fn resolve_fix_agent_autonomous_uses_assignee() {
        let agent = resolve_fix_agent(None, Some("codex"), false).unwrap();
        assert_eq!(agent, AgentType::Codex);
    }

    #[test]
    fn resolve_fix_agent_autonomous_no_assignee_defaults_to_claude() {
        let agent = resolve_fix_agent(None, None, false).unwrap();
        assert_eq!(agent, AgentType::ClaudeCode);
    }
}
