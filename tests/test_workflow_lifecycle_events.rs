//! Consumer-path integration tests for the `workflow.*` / `step.*` lifecycle
//! events: dispatching the event must run the project's `.aiki/hooks.yml`
//! handler (declared under `after:`) with the documented template var
//! (`event.workflow.name` / `event.step.name`) resolved.
//!
//! Guards the full path that the herdr plugin (and any other integration)
//! relies on:
//!   `event_bus::dispatch` -> `handle_{workflow,step}_started` -> `execute_hook`
//!   -> compose project `.aiki/hooks.yml` -> composer event selector -> engine
//!   var registration -> handler runs.
//! The unit tests in `flows/engine.rs` cover var resolution in isolation; these
//! cover the dispatch-through-compose wiring those depend on.

use aiki::event_bus::dispatch;
use aiki::events::{AikiEvent, AikiStepStartedPayload, AikiWorkflowStartedPayload};
use chrono::Utc;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

/// Minimal aiki project whose `event_key` handler records the resolved `var`
/// to `marker` when that event fires.
fn write_project(root: &Path, event_key: &str, var: &str, marker: &Path) {
    fs::create_dir_all(root.join(".aiki")).unwrap();
    let hooks =
        "name: test\nversion: \"1\"\nafter:\n  EVENT_KEY:\n    - shell: 'printf \"%s\" \"{{VAR}}\" > \"MARKER\"'\n"
            .replace("EVENT_KEY", event_key)
            .replace("VAR", var)
            .replace("MARKER", &marker.display().to_string());
    fs::write(root.join(".aiki/hooks.yml"), hooks).unwrap();
}

#[test]
fn workflow_started_runs_project_handler_with_resolved_name() {
    let project = TempDir::new().unwrap();
    let marker = project.path().join("fired");
    write_project(project.path(), "workflow.started", "event.workflow.name", &marker);

    dispatch(AikiEvent::WorkflowStarted(AikiWorkflowStartedPayload {
        workflow: "build".to_string(),
        cwd: project.path().to_path_buf(),
        timestamp: Utc::now(),
    }))
    .expect("dispatch workflow.started");

    assert_eq!(
        fs::read_to_string(&marker)
            .expect("workflow.started handler did not run / write the marker"),
        "build",
    );
}

#[test]
fn step_started_runs_project_handler_with_resolved_name() {
    let project = TempDir::new().unwrap();
    let marker = project.path().join("fired");
    write_project(project.path(), "step.started", "event.step.name", &marker);

    dispatch(AikiEvent::StepStarted(AikiStepStartedPayload {
        step: "decompose".to_string(),
        cwd: project.path().to_path_buf(),
        timestamp: Utc::now(),
    }))
    .expect("dispatch step.started");

    assert_eq!(
        fs::read_to_string(&marker)
            .expect("step.started handler did not run / write the marker"),
        "decompose",
    );
}
