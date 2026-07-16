//! Generic CLI agent runtime.
//!
//! Spawns local-CLI agents using the per-harness `args` function. Replaces
//! the per-agent hand-written runtimes (Claude Code, Codex). Output parsing
//! lives in the editor module — this layer only does argv + spawn + lifecycle.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::{
    build_spawn_env, AgentRuntime, AgentSessionResult, AgentSpawnOptions, BackgroundHandle,
    MonitoredChild,
};
use super::AgentType;
use crate::error::{AikiError, Result};
use crate::harnesses::definition::HarnessDefinition;
use crate::harnesses::runtime::{CliArgs, RuntimeEnv, RuntimeKind};
use crate::utils::quarantine::{self, QuarantineStatus};
use crate::utils::sanitize::sanitize_field;

/// What the quarantine pre-flight should do before a spawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreflightAction {
    /// No warning, no error — build the command and exec.
    Proceed,
    /// Well-formed pending xattr: warn (the first launch may hang behind a
    /// Gatekeeper dialog) and proceed.
    WarnPending,
    /// Attribute present but uninterpretable: soft-warn and ALWAYS proceed —
    /// a failed detection must never hard-block a spawn.
    WarnUndetermined,
    /// Well-formed pending xattr with nobody to approve the dialog: fail
    /// before any exec instead of hanging to the discovery timeout.
    FailQuarantined,
}

/// The status × mode → warn/error/proceed decision, shared by all three spawn
/// methods. Pure so the full product is unit-testable.
///
/// `headless` comes from [`super::is_headless`] (stdout TTY-ness +
/// `SSH_CONNECTION`; stderr state is deliberately irrelevant). `skip_hard_fail`
/// is the `AIKI_SKIP_QUARANTINE_CHECK` escape hatch: it downgrades only the
/// headless fail-fast to a warning, never silences warnings.
fn preflight_action(
    status: &QuarantineStatus,
    headless: bool,
    skip_hard_fail: bool,
) -> PreflightAction {
    match status {
        QuarantineStatus::NotQuarantined | QuarantineStatus::Approved => PreflightAction::Proceed,
        QuarantineStatus::Pending { .. } if headless && !skip_hard_fail => {
            PreflightAction::FailQuarantined
        }
        QuarantineStatus::Pending { .. } => PreflightAction::WarnPending,
        QuarantineStatus::Undetermined { .. } => PreflightAction::WarnUndetermined,
        // A failure to detect must never block (or noise up) a spawn.
        QuarantineStatus::CheckFailed { .. } => PreflightAction::Proceed,
    }
}

fn pending_warning(agent: &str) -> String {
    format!(
        "[aiki] Warning: {agent} is quarantined by macOS Gatekeeper.\n\
         [aiki] The first launch may hang behind a system confirmation dialog.\n\
         [aiki] Approve the dialog if one appears, or run: aiki doctor --fix --quarantined --{agent}"
    )
}

fn undetermined_warning(agent: &str, raw: &str) -> String {
    format!(
        "[aiki] Warning: {agent} carries a macOS quarantine attribute aiki could not interpret ({}).\n\
         [aiki] If the first launch hangs behind a Gatekeeper dialog, approve it or run: \
         aiki doctor --fix --quarantined --{agent}",
        sanitize_field(raw)
    )
}

/// Route a warning through the caller's sink, falling back to stderr.
fn emit_warning(options: &AgentSpawnOptions, msg: &str) {
    match &options.warn {
        Some(sink) => sink.emit(msg),
        None => eprintln!("{msg}"), // stderr-ok: pre-spawn warning fallback
    }
}

pub struct CliAgentRuntime {
    binary: PathBuf,
    args_fn: fn(&AgentSpawnOptions) -> CliArgs,
    env_fn: Option<fn(&mut RuntimeEnv)>,
    agent_type: AgentType,
    label: &'static str,
}

impl CliAgentRuntime {
    pub fn from_harness(harness: &'static HarnessDefinition) -> Result<Self> {
        let runtime = harness.runtime.as_ref().ok_or_else(|| AikiError::HarnessNotCli {
            id: harness.identity.id.to_string(),
        })?;
        let RuntimeKind::Cli(cli) = &runtime.kind;
        let binary = which::which(cli.binary).map_err(|_| AikiError::CliBinaryNotFound {
            binary: cli.binary.to_string(),
        })?;
        Ok(Self {
            binary,
            args_fn: cli.args,
            env_fn: runtime.env,
            agent_type: harness.identity.agent_type,
            label: harness.identity.id,
        })
    }

    /// The agent name used in quarantine messages and `aiki doctor` fix
    /// commands: the doctor scope flag (`claude`, `codex`) when the agent has
    /// one, else the harness label.
    fn quarantine_agent_name(&self) -> &str {
        self.agent_type.doctor_flag().unwrap_or(self.label)
    }

    /// Quarantine pre-flight shared by all three spawn methods, run before the
    /// command is built. Quarantine only affects the FIRST exec of a binary,
    /// so spawn time is the only place detection is needed (per-jj-command
    /// spawns and the stale-worker watchdog are deliberately not checked).
    ///
    /// Returns `Err(BinaryQuarantined)` only for a well-formed `Pending` xattr
    /// in headless mode — where the Gatekeeper dialog can never be approved,
    /// failing fast beats spawning a child guaranteed to hang to the discovery
    /// timeout. The error originates here rather than in `from_harness` so it
    /// propagates through the existing spawn error plumbing (and cannot be
    /// swallowed by `get_runtime`'s `Option` shape).
    fn quarantine_preflight(&self, options: &AgentSpawnOptions) -> Result<()> {
        let status = quarantine::check(&self.binary);
        let headless = super::is_headless();
        let skip_hard_fail = std::env::var("AIKI_SKIP_QUARANTINE_CHECK")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false);
        let agent = self.quarantine_agent_name();
        match preflight_action(&status, headless, skip_hard_fail) {
            PreflightAction::Proceed => Ok(()),
            PreflightAction::WarnPending => {
                emit_warning(options, &pending_warning(agent));
                Ok(())
            }
            PreflightAction::WarnUndetermined => {
                let raw = match &status {
                    QuarantineStatus::Undetermined { raw } => raw.as_str(),
                    _ => "",
                };
                emit_warning(options, &undetermined_warning(agent, raw));
                Ok(())
            }
            PreflightAction::FailQuarantined => Err(AikiError::BinaryQuarantined {
                agent: agent.to_string(),
                path: self.binary.clone(),
            }),
        }
    }

    fn build_command(&self, options: &AgentSpawnOptions, mode: &str) -> Command {
        let args = (self.args_fn)(options);
        let mut cmd = Command::new(&self.binary);
        cmd.current_dir(&options.cwd)
            .args(args.as_slice())
            .envs(build_spawn_env(options, mode));
        if let Some(env_fn) = self.env_fn {
            let mut env = RuntimeEnv::new();
            env_fn(&mut env);
            for key in env.removes() {
                cmd.env_remove(key);
            }
        }
        cmd
    }
}

impl AgentRuntime for CliAgentRuntime {
    fn spawn_blocking(&self, options: &AgentSpawnOptions) -> Result<AgentSessionResult> {
        self.quarantine_preflight(options)?;
        let mut cmd = self.build_command(options, "background");
        match cmd.output() {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                if output.status.success() {
                    let summary = extract_summary(&stdout);
                    Ok(AgentSessionResult::completed(summary))
                } else if stderr.contains("stopped") || stderr.contains("paused") {
                    Ok(AgentSessionResult::stopped(stderr))
                } else {
                    Ok(AgentSessionResult::failed(format!(
                        "Exit code: {:?}\nStderr: {}",
                        output.status.code(),
                        stderr
                    )))
                }
            }
            Err(e) => Ok(AgentSessionResult::failed(format!(
                "Failed to spawn {}: {}",
                self.label, e
            ))),
        }
    }

    fn spawn_background(&self, options: &AgentSpawnOptions) -> Result<BackgroundHandle> {
        self.quarantine_preflight(options)?;
        let mut cmd = self.build_command(options, "background");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match cmd.spawn() {
            Ok(child) => Ok(BackgroundHandle {
                thread: options.thread.clone(),
                session_id: None,
                agent_type: self.agent_type,
                // Capture the pid before the Child is dropped (detached). This is
                // the only handle to an agent that hangs before recording its
                // session, which the session-file-based kill path cannot reach.
                pid: Some(child.id()),
            }),
            Err(e) => Err(AikiError::AgentSpawnFailed(format!(
                "Failed to spawn {} in background: {}",
                self.label, e
            ))),
        }
    }

    fn spawn_monitored(&self, options: &AgentSpawnOptions) -> Result<MonitoredChild> {
        self.quarantine_preflight(options)?;
        let mut cmd = self.build_command(options, "monitored");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        match cmd.spawn() {
            Ok(child) => Ok(MonitoredChild::new(child)),
            Err(e) => Err(AikiError::AgentSpawnFailed(format!(
                "Failed to spawn {} for monitoring: {}",
                self.label, e
            ))),
        }
    }
}

/// Extract a summary from the agent's output.
///
/// Takes the last few non-empty lines as a summary, capped at ~500 chars.
fn extract_summary(output: &str) -> String {
    let lines: Vec<&str> = output.lines().filter(|l| !l.trim().is_empty()).collect();

    if lines.is_empty() {
        return "Task completed".to_string();
    }

    let mut summary = String::new();
    for line in lines.iter().rev().take(10) {
        let prepend = format!("{}\n", line);
        if summary.len() + prepend.len() > 500 {
            break;
        }
        summary = prepend + summary.as_str();
    }

    if summary.is_empty() {
        "Task completed".to_string()
    } else {
        summary.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harnesses::lookup;

    #[test]
    fn test_extract_summary_empty() {
        assert_eq!(extract_summary(""), "Task completed");
        assert_eq!(extract_summary("   \n  \n  "), "Task completed");
    }

    #[test]
    fn test_extract_summary_short() {
        let output = "Fixed the bug.\nTests pass.";
        let summary = extract_summary(output);
        assert!(summary.contains("Fixed the bug"));
        assert!(summary.contains("Tests pass"));
    }

    #[test]
    fn test_extract_summary_long() {
        let long_output = (0..100)
            .map(|i| format!("Line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let summary = extract_summary(&long_output);
        assert!(summary.len() <= 600);
    }

    #[test]
    fn from_harness_rejects_runtime_less_harness() {
        let harness = lookup("gemini").expect("gemini harness is registered");
        match CliAgentRuntime::from_harness(harness) {
            Err(AikiError::HarnessNotCli { id }) => assert_eq!(id, "gemini"),
            Err(other) => panic!("expected HarnessNotCli, got {other:?}"),
            Ok(_) => panic!("expected HarnessNotCli, got Ok"),
        }
    }

    #[test]
    fn from_harness_returns_cli_binary_not_found_for_missing_binary() {
        // Codex is wired (has runtime), so we can exercise the binary-resolution
        // path. Skip when the binary happens to be installed on this machine.
        if which::which("codex").is_ok() {
            return;
        }
        let harness = lookup("codex").expect("codex harness is registered");
        match CliAgentRuntime::from_harness(harness) {
            Err(AikiError::CliBinaryNotFound { binary }) => assert_eq!(binary, "codex"),
            Err(other) => panic!("expected CliBinaryNotFound, got {other:?}"),
            Ok(_) => panic!("expected CliBinaryNotFound, got Ok"),
        }
    }

    #[test]
    fn from_harness_succeeds_for_resolvable_binary() {
        use crate::harnesses::definition::{
            HarnessDefinition, HooksConfig, HooksKind, Identity, Install,
        };
        use crate::harnesses::runtime::{CliArgs, CliRuntime, RuntimeConfig};

        if cfg!(windows) || which::which("sh").is_err() {
            return;
        }
        fn dummy_args(_: &AgentSpawnOptions) -> CliArgs {
            CliArgs::new()
        }
        let def: &'static HarnessDefinition = Box::leak(Box::new(HarnessDefinition {
            install: Install { hint: "test" },
            identity: Identity {
                id: "test-sh",
                aliases: &[],
                display_name: "Test Sh",
                agent_type: AgentType::Unknown,
                email: "test@example.com",
                custom_metadata_name: None,
            },
            hooks: HooksConfig {
                kind: HooksKind::Sdk,
                supports_blocking: false,
            },
            runtime: Some(RuntimeConfig {
                kind: RuntimeKind::Cli(CliRuntime {
                    binary: "sh",
                    args: dummy_args,
                }),
                env: None,
            }),
        }));
        // Construction succeeding is the assertion: the binary resolved
        // through `which` and the harness wiring is complete.
        CliAgentRuntime::from_harness(def).expect("sh should resolve via which");
    }

    // ── Quarantine pre-flight decision table (test matrix row (a)) ──
    //
    // The full status × headless × skip product for the ONE pure decision
    // function all three spawn methods share. Run-mode inputs arrive as a
    // plain bool here; consumer-path tests set the mode at the real boundary
    // (piped stdout). Stderr state plays no part by construction — the
    // function never sees it.

    #[test]
    fn preflight_decision_table_full_product() {
        use PreflightAction::*;
        let pending = QuarantineStatus::Pending {
            raw: "0083;0;t;u".into(),
        };
        let undetermined = QuarantineStatus::Undetermined { raw: "??".into() };
        let check_failed = QuarantineStatus::CheckFailed { errno: 13 };

        // (status, headless, skip_hard_fail) → expected
        let table = [
            (&QuarantineStatus::NotQuarantined, false, false, Proceed),
            (&QuarantineStatus::NotQuarantined, true, false, Proceed),
            (&QuarantineStatus::NotQuarantined, false, true, Proceed),
            (&QuarantineStatus::NotQuarantined, true, true, Proceed),
            (&QuarantineStatus::Approved, false, false, Proceed),
            (&QuarantineStatus::Approved, true, false, Proceed),
            (&QuarantineStatus::Approved, false, true, Proceed),
            (&QuarantineStatus::Approved, true, true, Proceed),
            (&pending, false, false, WarnPending),
            (&pending, true, false, FailQuarantined),
            // Escape hatch downgrades ONLY the headless hard failure.
            (&pending, true, true, WarnPending),
            (&pending, false, true, WarnPending),
            // Undetermined never hard-fails, in either mode.
            (&undetermined, false, false, WarnUndetermined),
            (&undetermined, true, false, WarnUndetermined),
            (&undetermined, false, true, WarnUndetermined),
            (&undetermined, true, true, WarnUndetermined),
            // CheckFailed proceeds silently in both modes.
            (&check_failed, false, false, Proceed),
            (&check_failed, true, false, Proceed),
            (&check_failed, false, true, Proceed),
            (&check_failed, true, true, Proceed),
        ];
        for (status, headless, skip, expected) in table {
            assert_eq!(
                preflight_action(status, headless, skip),
                expected,
                "status={status:?} headless={headless} skip={skip}"
            );
        }
    }

    #[test]
    fn pending_warning_names_agent_and_fix_command() {
        let msg = pending_warning("claude");
        assert!(msg.contains("claude is quarantined by macOS Gatekeeper"));
        assert!(msg.contains("aiki doctor --fix --quarantined --claude"));
    }

    #[test]
    fn undetermined_warning_sanitizes_untrusted_raw_value() {
        let msg = undetermined_warning("codex", "evil\x1b[2J\nvalue");
        assert!(msg.contains("\\x1b[2J\\nvalue"), "raw must render escaped: {msg}");
        assert!(!msg.contains('\x1b'), "no raw ESC may reach the terminal");
        assert!(msg.contains("aiki doctor --fix --quarantined --codex"));
    }

    // ── Spawn lifecycle tests against fake binaries (B4/B5) ──

    #[cfg(unix)]
    fn write_fake_binary(dir: &std::path::Path, name: &str, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, script).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    /// Build a runtime directly around an absolute fake-binary path,
    /// bypassing the registry: these tests exercise spawn lifecycle, not
    /// harness resolution.
    #[cfg(unix)]
    fn fake_runtime(
        binary: PathBuf,
        args_fn: fn(&AgentSpawnOptions) -> CliArgs,
    ) -> CliAgentRuntime {
        CliAgentRuntime {
            binary,
            args_fn,
            env_fn: None,
            agent_type: AgentType::Unknown,
            label: "fake",
        }
    }

    #[cfg(unix)]
    fn fake_options(dir: &std::path::Path) -> AgentSpawnOptions {
        use crate::tasks::lanes::ThreadId;
        AgentSpawnOptions::new(dir, ThreadId::single("task123".to_string()))
    }

    /// One test (not several) because the env seams are process-global and
    /// tests run concurrently; the forced status is path-scoped to this
    /// test's unique fake binary so concurrent spawn tests are unaffected.
    /// Interactive mode cannot be forced in-process (stdout's TTY-ness is
    /// real); the pure decision table covers it.
    #[test]
    #[cfg(unix)]
    fn spawn_methods_honor_forced_quarantine_status() {
        use crate::agents::runtime::WarnSink;
        use std::sync::{Arc, Mutex};

        let tmp = tempfile::tempdir().unwrap();
        let bin = write_fake_binary(tmp.path(), "fake-agent", "#!/bin/sh\nexit 0\n");
        fn args(_: &AgentSpawnOptions) -> CliArgs {
            CliArgs::new()
        }
        let runtime = fake_runtime(bin.clone(), args);
        let options = fake_options(tmp.path());

        // Force headless deterministically: SSH_CONNECTION classifies headless
        // regardless of whether this test process's stdout is a TTY. Left set:
        // every other preflight in this process resolves NotQuarantined for
        // its own path and proceeds in either mode.
        std::env::set_var("SSH_CONNECTION", "aiki-test");

        // Pending + headless: all three spawn methods fail fast with the
        // typed error, before any exec.
        std::env::set_var(
            "AIKI_TEST_QUARANTINE_STATUS",
            format!("pending:{}", bin.display()),
        );
        match runtime.spawn_blocking(&options) {
            Err(AikiError::BinaryQuarantined { agent, path }) => {
                assert_eq!(path, bin);
                // Unknown agent type has no doctor flag; falls back to label.
                assert_eq!(agent, "fake");
            }
            other => panic!("spawn_blocking: expected BinaryQuarantined, got {other:?}"),
        }
        assert!(
            matches!(
                runtime.spawn_background(&options),
                Err(AikiError::BinaryQuarantined { .. })
            ),
            "spawn_background must fail fast on pending + headless"
        );
        assert!(
            matches!(
                runtime.spawn_monitored(&options).map(|_| ()),
                Err(AikiError::BinaryQuarantined { .. })
            ),
            "spawn_monitored must fail fast on pending + headless"
        );

        // Undetermined: warn through the wired sink and PROCEED.
        std::env::set_var(
            "AIKI_TEST_QUARANTINE_STATUS",
            format!("undetermined:{}", bin.display()),
        );
        let warnings: Arc<Mutex<Vec<String>>> = Arc::default();
        let sink_warnings = Arc::clone(&warnings);
        let with_sink = options.clone().with_warn_sink(WarnSink::new(move |msg| {
            sink_warnings.lock().unwrap().push(msg.to_string());
        }));
        runtime
            .spawn_blocking(&with_sink)
            .expect("undetermined must never hard-fail");
        let seen = warnings.lock().unwrap().join("\n");
        assert!(
            seen.contains("could not interpret"),
            "undetermined warning must flow through the warn sink, got: {seen}"
        );

        // CheckFailed: silent proceed — no warning, no error.
        std::env::set_var(
            "AIKI_TEST_QUARANTINE_STATUS",
            format!("checkfailed:{}", bin.display()),
        );
        warnings.lock().unwrap().clear();
        let sink_warnings = Arc::clone(&warnings);
        let with_sink = options.clone().with_warn_sink(WarnSink::new(move |msg| {
            sink_warnings.lock().unwrap().push(msg.to_string());
        }));
        runtime
            .spawn_blocking(&with_sink)
            .expect("checkfailed must never block a spawn");
        assert!(
            warnings.lock().unwrap().is_empty(),
            "checkfailed must proceed silently"
        );

        std::env::remove_var("AIKI_TEST_QUARANTINE_STATUS");
    }

    #[test]
    #[cfg(unix)]
    fn spawn_blocking_passes_args_and_returns_completed() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = write_fake_binary(
            tmp.path(),
            "fake-agent",
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$(dirname \"$0\")/argv.txt\"\nprintenv AIKI_THREAD > \"$(dirname \"$0\")/thread.txt\"\necho \"did the work\"\n",
        );
        fn args(_: &AgentSpawnOptions) -> CliArgs {
            let mut a = CliArgs::new();
            a.push("--alpha");
            a.push("--beta");
            a
        }
        let runtime = fake_runtime(bin, args);
        let result = runtime
            .spawn_blocking(&fake_options(tmp.path()))
            .expect("spawn_blocking should not error");

        match result {
            AgentSessionResult::Completed { summary } => {
                assert!(
                    summary.contains("did the work"),
                    "summary should carry stdout, got: {summary}"
                );
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        let argv = std::fs::read_to_string(tmp.path().join("argv.txt")).unwrap();
        assert_eq!(argv, "--alpha\n--beta\n", "argv order must match args fn");
        let thread = std::fs::read_to_string(tmp.path().join("thread.txt")).unwrap();
        assert_eq!(thread.trim(), "task123", "AIKI_THREAD must reach the child");
    }

    #[test]
    #[cfg(unix)]
    fn spawn_blocking_maps_failure_exit_to_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = write_fake_binary(
            tmp.path(),
            "fake-agent",
            "#!/bin/sh\necho \"boom\" >&2\nexit 3\n",
        );
        fn args(_: &AgentSpawnOptions) -> CliArgs {
            CliArgs::new()
        }
        let runtime = fake_runtime(bin, args);
        let result = runtime
            .spawn_blocking(&fake_options(tmp.path()))
            .expect("spawn_blocking should not error");
        match result {
            AgentSessionResult::Failed { error } => {
                assert!(error.contains("boom"), "stderr should surface, got: {error}");
                assert!(error.contains("3"), "exit code should surface, got: {error}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn spawn_background_detaches_and_runs_the_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = write_fake_binary(
            tmp.path(),
            "fake-agent",
            "#!/bin/sh\ntouch \"$(dirname \"$0\")/ran.marker\"\n",
        );
        fn args(_: &AgentSpawnOptions) -> CliArgs {
            CliArgs::new()
        }
        let runtime = fake_runtime(bin, args);
        let handle = runtime
            .spawn_background(&fake_options(tmp.path()))
            .expect("spawn_background should succeed");
        assert_eq!(handle.thread.head, "task123");
        assert_eq!(handle.agent_type, AgentType::Unknown);
        assert!(handle.session_id.is_none());
        // The pid is captured so an orphaned spawn can be reaped even if its
        // session never registers.
        assert!(handle.pid.is_some(), "spawn should report the child pid");

        // The child is detached (no Child handle), so prove execution via a
        // marker file with a generous timeout.
        let marker = tmp.path().join("ran.marker");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !marker.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "detached child never ran (no marker file)"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    #[cfg(unix)]
    fn spawn_monitored_try_wait_then_kill_then_wait() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = write_fake_binary(tmp.path(), "fake-agent", "#!/bin/sh\nsleep 30\n");
        fn args(_: &AgentSpawnOptions) -> CliArgs {
            CliArgs::new()
        }
        let runtime = fake_runtime(bin, args);
        let mut child = runtime
            .spawn_monitored(&fake_options(tmp.path()))
            .expect("spawn_monitored should succeed");

        // Still running: try_wait must report None without blocking.
        assert!(
            child.try_wait().expect("try_wait should not error").is_none(),
            "child should still be running"
        );

        // Explicit kill-based cleanup, then wait() must reap it.
        child.kill().expect("kill should succeed");
        let status = child.wait().expect("wait should reap the child");
        assert!(!status.success(), "killed child must not report success");
    }

    #[test]
    #[cfg(unix)]
    fn spawn_monitored_wait_reaps_completed_child() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = write_fake_binary(tmp.path(), "fake-agent", "#!/bin/sh\nexit 0\n");
        fn args(_: &AgentSpawnOptions) -> CliArgs {
            CliArgs::new()
        }
        let runtime = fake_runtime(bin, args);
        let mut child = runtime
            .spawn_monitored(&fake_options(tmp.path()))
            .expect("spawn_monitored should succeed");
        let status = child.wait().expect("wait should reap the child");
        assert!(status.success(), "clean exit should report success");
        // After reaping, try_wait keeps returning the status without error.
        let again = child.try_wait().expect("try_wait after reap should not error");
        assert!(again.is_some(), "reaped child should keep reporting an exit status");
    }
}
