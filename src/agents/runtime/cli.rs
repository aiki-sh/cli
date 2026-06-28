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
