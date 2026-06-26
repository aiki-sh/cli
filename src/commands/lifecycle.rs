//! Workflow-command lifecycle events.
//!
//! Emits neutral `workflow.started` / `workflow.completed` events bracketing an
//! aiki workflow command (`build`, `fix`, `loop`, `review`). Plugins (e.g. the
//! herdr integration) subscribe to these; aiki core stays integration-agnostic.
//! Granular within-run detail is emitted separately as `step.started` /
//! `step.completed` from the workflow step loop.

use std::path::PathBuf;

use crate::events::{AikiEvent, AikiWorkflowCompletedPayload, AikiWorkflowStartedPayload};

/// RAII guard that emits `workflow.started` on creation and `workflow.completed`
/// on drop — covering success, `?`-propagated errors, and unwinds.
///
/// Call [`WorkflowGuard::succeeded`] just before returning `Ok` so the
/// completion event reports an accurate outcome; it defaults to
/// `success = false` (the command errored or panicked) otherwise.
pub struct WorkflowGuard {
    workflow: &'static str,
    cwd: PathBuf,
    success: bool,
}

impl WorkflowGuard {
    /// Emit `workflow.started` for `workflow` (e.g. `"build"`) and return a
    /// guard that emits `workflow.completed` when dropped.
    pub fn start(workflow: &'static str) -> Self {
        let cwd = std::env::current_dir().unwrap_or_default();
        let _ = crate::event_bus::dispatch(AikiEvent::WorkflowStarted(AikiWorkflowStartedPayload {
            workflow: workflow.to_string(),
            cwd: cwd.clone(),
            timestamp: chrono::Utc::now(),
        }));
        Self {
            workflow,
            cwd,
            success: false,
        }
    }

    /// Record that the workflow finished cleanly (reflected in the
    /// `success` field of the `workflow.completed` event).
    pub fn succeeded(&mut self) {
        self.success = true;
    }
}

impl Drop for WorkflowGuard {
    fn drop(&mut self) {
        let _ =
            crate::event_bus::dispatch(AikiEvent::WorkflowCompleted(AikiWorkflowCompletedPayload {
                workflow: self.workflow.to_string(),
                success: self.success,
                cwd: self.cwd.clone(),
                timestamp: chrono::Utc::now(),
            }));
    }
}
