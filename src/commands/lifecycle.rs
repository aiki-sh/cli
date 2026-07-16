//! Workflow-command lifecycle events.
//!
//! Emits neutral `workflow.started` / `workflow.completed` events bracketing an
//! aiki workflow command (`build`, `fix`, `loop`, `review`). Plugins (e.g. the
//! herdr integration) subscribe to these; aiki core stays integration-agnostic.
//! Granular within-run detail is emitted separately as `step.started` /
//! `step.completed` from the workflow step loop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::events::{AikiEvent, AikiWorkflowCompletedPayload, AikiWorkflowStartedPayload};

struct WorkflowCompletion {
    workflow: &'static str,
    cwd: PathBuf,
    success: AtomicBool,
    emitted: AtomicBool,
}

impl WorkflowCompletion {
    fn emit(&self) {
        if self
            .emitted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }

        let _ = crate::event_bus::dispatch(AikiEvent::WorkflowCompleted(
            AikiWorkflowCompletedPayload {
                workflow: self.workflow.to_string(),
                success: self.success.load(Ordering::Acquire),
                cwd: self.cwd.clone(),
                timestamp: chrono::Utc::now(),
            },
        ));
    }
}

/// Dispatch completion before an abrupt terminal signal terminates the process.
///
/// Rust does not unwind on SIGINT, SIGTERM, or SIGHUP, so the guard's normal
/// `Drop` path never runs. `signal-hook` receives the signal on a dedicated
/// thread, where it is safe to execute hooks, then restores the signal's
/// default termination behavior.
#[cfg(unix)]
struct SignalCleanup {
    handle: signal_hook::iterator::Handle,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(unix)]
impl SignalCleanup {
    fn install(completion: Arc<WorkflowCompletion>) -> Option<Self> {
        use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
        use signal_hook::iterator::Signals;

        let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP]).ok()?;
        let handle = signals.handle();
        let thread = std::thread::spawn(move || {
            if let Some(signal) = signals.forever().next() {
                completion.emit();
                let _ = signal_hook::low_level::emulate_default_handler(signal);
            }
        });

        Some(Self {
            handle,
            thread: Some(thread),
        })
    }
}

#[cfg(unix)]
impl Drop for SignalCleanup {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Guard that emits `workflow.started` on creation and `workflow.completed` on
/// drop or a terminating Unix signal — covering success, errors, unwinds, and
/// terminal interruption.
///
/// Call [`WorkflowGuard::succeeded`] just before returning `Ok` so the
/// completion event reports an accurate outcome; it defaults to
/// `success = false` (the command errored or panicked) otherwise.
pub struct WorkflowGuard {
    completion: Arc<WorkflowCompletion>,
    #[cfg(unix)]
    _signal_cleanup: Option<SignalCleanup>,
}

impl WorkflowGuard {
    /// Emit `workflow.started` for `workflow` (e.g. `"build"`) and return a
    /// guard that emits `workflow.completed` when dropped.
    pub fn start(workflow: &'static str) -> Self {
        let cwd = std::env::current_dir().unwrap_or_default();
        let completion = Arc::new(WorkflowCompletion {
            workflow,
            cwd: cwd.clone(),
            success: AtomicBool::new(false),
            emitted: AtomicBool::new(false),
        });
        #[cfg(unix)]
        let signal_cleanup = SignalCleanup::install(Arc::clone(&completion));

        let _ =
            crate::event_bus::dispatch(AikiEvent::WorkflowStarted(AikiWorkflowStartedPayload {
                workflow: workflow.to_string(),
                cwd,
                timestamp: chrono::Utc::now(),
            }));

        Self {
            #[cfg(unix)]
            _signal_cleanup: signal_cleanup,
            completion,
        }
    }

    /// Record that the workflow finished cleanly (reflected in the
    /// `success` field of the `workflow.completed` event).
    pub fn succeeded(&mut self) {
        self.completion.success.store(true, Ordering::Release);
    }
}

impl Drop for WorkflowGuard {
    fn drop(&mut self) {
        self.completion.emit();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;
    use std::time::Duration;

    const SIGNAL_PROBE_MARKER: &str = "AIKI_WORKFLOW_SIGNAL_PROBE_MARKER";

    #[test]
    fn signal_cleanup_thread_stops_cleanly() {
        let completion = Arc::new(WorkflowCompletion {
            workflow: "review",
            cwd: PathBuf::new(),
            success: AtomicBool::new(false),
            emitted: AtomicBool::new(false),
        });
        let cleanup = SignalCleanup::install(completion).expect("install signal cleanup");
        drop(cleanup);
    }

    #[test]
    fn workflow_guard_sigint_probe() {
        let Ok(marker) = std::env::var(SIGNAL_PROBE_MARKER) else {
            return;
        };

        fs::create_dir_all(".aiki").unwrap();
        let hooks = format!(
            "name: signal-probe\nversion: \"1\"\nafter:\n  workflow.completed:\n    - shell: 'printf \"%s:%s\" \"{{{{event.workflow.name}}}}\" \"{{{{event.workflow.success}}}}\" > \"{marker}\"'\n"
        );
        fs::write(".aiki/hooks.yml", hooks).unwrap();

        let _guard = WorkflowGuard::start("review");
        // Keep a failed signal implementation from hanging the parent test.
        unsafe { libc::alarm(10) };
        unsafe { libc::raise(libc::SIGINT) };
        std::thread::sleep(Duration::from_secs(20));
        panic!("SIGINT did not terminate the workflow process");
    }

    #[test]
    fn sigint_emits_workflow_completed_before_termination() {
        let project = tempfile::tempdir().unwrap();
        let marker = project.path().join("completed");
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("commands::lifecycle::tests::workflow_guard_sigint_probe")
            .arg("--exact")
            .arg("--nocapture")
            .current_dir(project.path())
            .env(SIGNAL_PROBE_MARKER, &marker)
            .status()
            .expect("run workflow signal probe");

        assert_eq!(status.signal(), Some(libc::SIGINT));
        assert_eq!(
            fs::read_to_string(marker).expect("workflow.completed marker"),
            "review:false"
        );
    }
}
