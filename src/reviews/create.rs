//! Review creation logic.

use std::path::Path;

use crate::agents::{determine_default_coder, determine_reviewer, is_agent_available, AgentType};
use crate::error::{AikiError, Result};
use crate::reviews::{ReviewScope, ReviewScopeKind};
use crate::session::find_own_session;
use crate::tasks::templates::create_review_task_from_template;
use crate::tasks::{find_task, materialize_graph, read_events, write_link_event_with_autorun};

/// Parameters for creating a review task.
#[derive(Debug, Clone)]
pub struct CreateReviewParams {
    /// Pre-resolved review scope (caller detects target type).
    pub scope: ReviewScope,
    /// Override the reviewer agent.
    pub agent_override: Option<String>,
    /// Template to use (default: scope-specific, e.g. review/task).
    pub template: Option<String>,
    /// Fix plan template (e.g., "fix"); Some means fix is enabled.
    pub fix_template: Option<String>,
    /// Enable autorun on the validates link (default: false, opt-in only).
    pub autorun: bool,
}

/// Result of creating a review task.
#[derive(Debug, Clone)]
pub struct CreateReviewResult {
    /// The created review task ID.
    pub review_task_id: String,
    /// The review scope (typed, replaces loose scope_name/scope_id).
    #[allow(dead_code)]
    pub scope: ReviewScope,
}

/// Core review creation logic. Used by both CLI and flow action.
///
/// This function creates the review task with subtasks but does NOT
/// start or run the task. The caller is responsible for the execution mode.
/// The scope must be pre-resolved by the caller (via `detect_target()` for CLI,
/// or directly constructed for flow actions).
pub fn create_review(cwd: &Path, params: CreateReviewParams) -> Result<CreateReviewResult> {
    let scope = params.scope;

    // Determine worker for reviewer assignment.
    // For task scope, use the task's assignee. For all other scopes (code, plan,
    // session), check if we're running inside an agent session (PID ancestry).
    // If not (user terminal), fall back to the default coder so that
    // determine_reviewer() picks the correct cross-reviewer.
    let worker = match scope.kind {
        ReviewScopeKind::Task => {
            let events = read_events(cwd)?;
            let tasks = materialize_graph(&events).tasks;
            let task = find_task(&tasks, &scope.id)?;
            task.assignee.as_deref().map(|s| s.to_string())
        }
        _ => find_own_session(cwd)
            .map(|s| s.agent_type.as_str().to_string())
            .or_else(|| determine_default_coder().ok().map(|a| a.as_str().to_string())),
    };

    // Determine assignee for review task
    let assignee = match params.agent_override {
        Some(a) => a,
        None => determine_reviewer(worker.as_deref())?,
    };

    // Validate the reviewer agent is actually installed
    if !is_agent_available(&assignee) {
        let agent_type = AgentType::from_str(&assignee);
        let hint = agent_type
            .map(|a| a.install_hint().to_string())
            .unwrap_or_else(|| format!("Unknown agent: {}", assignee));
        return Err(AikiError::AgentNotInstalled {
            agent: assignee,
            hint,
        });
    }

    let assignee = Some(assignee);

    // Create review task with subtasks from template
    let default_template = match scope.kind {
        ReviewScopeKind::Session => "review/task".to_string(),
        _ => format!("review/{}", scope.kind.as_str()),
    };
    let template = params.template.as_deref().unwrap_or(&default_template);
    let mut scope_data = scope.to_data();

    // Add options data
    if let Some(ref tmpl) = params.fix_template {
        scope_data.insert("options.fix".to_string(), "true".to_string());

        scope_data.insert("options.fix_template".to_string(), tmpl.clone());
    }

    // Build sources for lineage (not routing)
    let sources = match scope.kind {
        ReviewScopeKind::Task => vec![format!("task:{}", scope.id)],
        ReviewScopeKind::Plan | ReviewScopeKind::Code => {
            vec![format!("file:{}", scope.id)]
        }
        _ => vec![],
    };

    let review_id =
        create_review_task_from_template(cwd, &scope_data, &sources, &assignee, template)?;

    // Emit validates link for task-scoped reviews: review validates the original task
    // Autorun is opt-in only (--autorun flag); default is no autorun
    if scope.kind == ReviewScopeKind::Task {
        let events = read_events(cwd)?;
        let graph = materialize_graph(&events);
        let autorun = if params.autorun { Some(true) } else { None };
        write_link_event_with_autorun(cwd, &graph, "validates", &review_id, &scope.id, autorun)?;
    }

    Ok(CreateReviewResult {
        review_task_id: review_id,
        scope,
    })
}
