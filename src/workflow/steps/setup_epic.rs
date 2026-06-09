//! SetupEpic step: consolidates all pre-execution setup for build workflows.
//!
//! Handles both the plan path (find-or-create epic) and epic ID (lookup + validate)
//! entry points, replacing the logic previously split across `Step::Plan`,
//! `run_build_plan`, and `run_build_epic` in commands/build.rs.

use crate::error::AikiError;
use crate::plans::{parse_plan_metadata, PlanGraph};
use crate::tasks::{find_task, get_subtasks, materialize_graph, read_events, TaskStatus};

use crate::epic::{
    close_epic, close_epic_as_invalid, create_epic_task, restart_epic, undo_completed_subtasks,
};
use super::plan::{cleanup_stale_builds, resolve_plan_path, validate_plan_path};
use super::{StepResult, WorkflowChange, WorkflowContext};

/// SetupEpic step implementation.
///
/// When `ctx.task_id` is None (plan path entry):
/// - Validates plan path, checks draft status, cleans up stale builds
/// - Finds or creates epic with restart handling
/// - Checks epic blockers
/// - Sets `ctx.task_id`
///
/// When `ctx.task_id` is Some (epic ID entry):
/// - Looks up the epic task, extracts `data.plan` into `ctx.plan_path`
/// - Checks epic blockers
pub(crate) fn run(ctx: &mut WorkflowContext) -> anyhow::Result<StepResult> {
    let restart = ctx.opts.restart;
    if ctx.task_id.is_some() {
        ctx.status("looking up epic");
        run_from_epic_id(ctx)
    } else {
        run_from_plan_path(ctx, restart)
    }
}

/// Entry from plan path: validate, find-or-create epic, set ctx.task_id.
fn run_from_plan_path(ctx: &mut WorkflowContext, restart: bool) -> anyhow::Result<StepResult> {
    let plan_path = ctx
        .plan_path
        .as_ref()
        .ok_or_else(|| AikiError::InvalidArgument("No plan path in workflow context".to_string()))?
        .clone();

    ctx.status("validating plan");
    validate_plan_path(&ctx.cwd, &plan_path)?;

    let full_path = resolve_plan_path(&ctx.cwd, &plan_path);
    let metadata = parse_plan_metadata(&full_path);
    if metadata.draft {
        return Err(AikiError::InvalidArgument(
            "Cannot build draft plan. Remove `draft: true` from frontmatter first.".to_string(),
        )
        .into());
    }

    ctx.status("cleaning up stale builds");
    cleanup_stale_builds(&ctx.cwd, &plan_path)?;

    ctx.status("resolving epic");
    let events = read_events(&ctx.cwd)?;
    let graph = materialize_graph(&events);
    let plan_graph = PlanGraph::build(&graph);
    let existing_epic = plan_graph.resolve_epic_for_plan(&plan_path, &graph)?;

    let epic_id = if restart {
        if let Some(epic) = existing_epic {
            if epic.status != TaskStatus::Closed {
                undo_completed_subtasks(&ctx.cwd, &epic.id)?;
                close_epic(&ctx.cwd, &epic.id)?;
            }
        }
        None
    } else {
        match existing_epic {
            Some(epic) if epic.status != TaskStatus::Closed => {
                let subtasks = get_subtasks(&graph, &epic.id);
                if subtasks.is_empty() {
                    close_epic_as_invalid(&ctx.cwd, &epic.id)?;
                    None
                } else {
                    restart_epic(&ctx.cwd, &epic.id)?;
                    Some(epic.id.clone())
                }
            }
            _ => None,
        }
    };

    let epic_id = match epic_id {
        Some(id) => id,
        None => {
            ctx.status("creating epic");
            create_epic_task(&ctx.cwd, &plan_path, ctx.opts.agent)?
        }
    };

    ctx.status("checking blockers");
    let events = read_events(&ctx.cwd)?;
    let graph = materialize_graph(&events);
    if graph.is_blocked(&epic_id) {
        return Err(anyhow::anyhow!(
            "Epic {} is blocked by unresolved dependencies. Rerun with --restart to start over",
            &epic_id[..epic_id.len().min(8)]
        ));
    }

    ctx.task_id = Some(epic_id.clone());

    Ok(StepResult {
        change: WorkflowChange::None,
        message: "Epic ready".to_string(),
        task_id: Some(epic_id),
    })
}

/// Entry from epic ID: look up epic, extract plan_path, check blockers.
fn run_from_epic_id(ctx: &mut WorkflowContext) -> anyhow::Result<StepResult> {
    let epic_id_input = ctx.task_id.as_ref().unwrap().clone();

    let events = read_events(&ctx.cwd)?;
    let graph = materialize_graph(&events);
    let epic = find_task(&graph.tasks, &epic_id_input)?;
    let epic_id = epic.id.clone();

    // Extract plan_path from epic data
    let plan_path = epic.data.get("plan").cloned().ok_or_else(|| {
        AikiError::InvalidArgument(format!(
            "Epic task {} missing data.plan. Cannot run build without a plan path.",
            epic_id
        ))
    })?;

    ctx.status("checking blockers");
    if graph.is_blocked(&epic_id) {
        return Err(anyhow::anyhow!(
            "Epic {} is blocked by unresolved dependencies. Rerun with --restart to start over",
            &epic_id[..epic_id.len().min(8)]
        ));
    }

    // Update context
    ctx.task_id = Some(epic_id.clone());
    ctx.plan_path = Some(plan_path);

    Ok(StepResult {
        change: WorkflowChange::None,
        message: "Epic ready".to_string(),
        task_id: Some(epic_id),
    })
}
