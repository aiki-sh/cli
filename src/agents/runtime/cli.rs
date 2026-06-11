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
            Ok(_child) => Ok(BackgroundHandle {
                thread: options.thread.clone(),
                session_id: None,
                agent_type: self.agent_type,
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
}
