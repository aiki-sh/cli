pub use inventory::submit;

use super::runtime::{RuntimeConfig, RuntimeKind};
use crate::agents::runtime::AgentRuntime;
use crate::agents::AgentType;
use crate::events::result::{Decision, HookResult};
use anyhow::Result;

pub struct HarnessDefinition {
    /// Install metadata shown by `aiki doctor` and friends. Lives on the
    /// harness itself (not the runtime config) so harnesses without a runtime
    /// can still tell users how to install the underlying tool.
    pub install: Install,
    pub identity: Identity,
    pub hooks: HooksConfig,
    /// Spawn config. `None` means the harness is registered for identity
    /// and hooks but cannot be driven as an aiki agent (e.g. Cursor, Gemini).
    pub runtime: Option<RuntimeConfig>,
}

pub struct Install {
    /// Human-readable install instruction (rendered to the user as-is).
    pub hint: &'static str,
}

pub struct Identity {
    pub id: &'static str,
    pub aliases: &'static [&'static str],
    pub display_name: &'static str,
    pub agent_type: AgentType,
    pub email: &'static str,
    pub custom_metadata_name: Option<&'static str>,
}

pub struct HooksConfig {
    pub kind: HooksKind,
    pub supports_blocking: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HooksKind {
    Sdk,
    Vendor,
}

inventory::collect!(HarnessDefinition);

impl HarnessDefinition {
    pub fn is_available(&self) -> bool {
        let Some(runtime) = self.runtime.as_ref() else {
            return false;
        };
        runtime.is_available_by_default()
    }

    pub fn metadata_name(&self) -> &'static str {
        self.identity.custom_metadata_name.unwrap_or(self.identity.id)
    }

    pub fn runtime(&'static self) -> Option<Result<Box<dyn AgentRuntime>>> {
        let runtime = self.runtime.as_ref()?;
        match &runtime.kind {
            RuntimeKind::Cli(_) => Some(
                crate::agents::runtime::CliAgentRuntime::from_harness(self)
                    .map(|r| Box::new(r) as Box<dyn AgentRuntime>)
                    .map_err(Into::into),
            ),
        }
    }

    /// Clamp a `HookResult` to what this harness's hook protocol can express.
    ///
    /// If the harness has `supports_blocking == false` and the result asks to
    /// block, the decision is downgraded to `Allow` and a stderr warning is
    /// emitted. The boolean return reports whether a downgrade happened so
    /// callers with their own warning channel (e.g. SDK output) can record it.
    pub fn enforce_blocking_support(&self, result: HookResult) -> (HookResult, bool) {
        if result.decision == Decision::Block && !self.hooks.supports_blocking {
            eprintln!(
                "Warning: harness '{}' does not support blocking; downgrading to allow",
                self.identity.id
            );
            return (
                HookResult {
                    decision: Decision::Allow,
                    ..result
                },
                true,
            );
        }
        (result, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harnesses::{
        lookup,
        runtime::{CliArgs, CliRuntime, RuntimeEnv},
    };
    use crate::agents::AgentSpawnOptions;

    fn dummy_args(_: &AgentSpawnOptions) -> CliArgs {
        CliArgs::new()
    }

    fn make_test_definition(runtime: Option<RuntimeConfig>) -> HarnessDefinition {
        HarnessDefinition {
            install: Install { hint: "test" },
            identity: Identity {
                id: "test-harness",
                aliases: &[],
                display_name: "Test",
                agent_type: AgentType::Unknown,
                email: "test@example.com",
                custom_metadata_name: None,
            },
            hooks: HooksConfig {
                kind: HooksKind::Sdk,
                supports_blocking: false,
            },
            runtime,
        }
    }

    // ------------------------------ is_available ------------------------------

    #[test]
    fn is_available_returns_false_when_runtime_is_none() {
        // Cursor and Gemini are registered without runtime configs.
        let cursor = lookup("cursor").expect("cursor harness is registered");
        assert!(cursor.runtime.is_none());
        assert!(!cursor.is_available());

        let gemini = lookup("gemini").expect("gemini harness is registered");
        assert!(gemini.runtime.is_none());
        assert!(!gemini.is_available());
    }

    #[test]
    fn is_available_delegates_to_runtime_when_some() {
        // If the binary is on PATH, claude-code should report available.
        // If the binary is missing, it should not. Either way, the answer must
        // match what `RuntimeConfig::is_available_by_default()` would say.
        let claude = lookup("claude-code").expect("claude-code harness is registered");
        let runtime = claude.runtime.as_ref().expect("claude has runtime");
        assert_eq!(claude.is_available(), runtime.is_available_by_default());
    }

    // ------------------------------ metadata_name ------------------------------

    #[test]
    fn metadata_name_uses_custom_when_set() {
        // claude-code overrides metadata_name to "claude" for backward compat.
        let claude = lookup("claude-code").expect("claude-code harness is registered");
        assert_eq!(claude.identity.id, "claude-code");
        assert_eq!(claude.metadata_name(), "claude");
    }

    #[test]
    fn metadata_name_falls_back_to_id() {
        // codex has no custom_metadata_name override.
        let codex = lookup("codex").expect("codex harness is registered");
        assert!(codex.identity.custom_metadata_name.is_none());
        assert_eq!(codex.metadata_name(), "codex");

        let cursor = lookup("cursor").expect("cursor harness is registered");
        assert_eq!(cursor.metadata_name(), cursor.identity.id);

        let gemini = lookup("gemini").expect("gemini harness is registered");
        assert_eq!(gemini.metadata_name(), gemini.identity.id);
    }

    // -------------------------------- runtime() --------------------------------

    #[test]
    fn runtime_returns_none_when_runtime_is_none() {
        let cursor = lookup("cursor").expect("cursor harness is registered");
        assert!(cursor.runtime().is_none());

        let gemini = lookup("gemini").expect("gemini harness is registered");
        assert!(gemini.runtime().is_none());
    }

    #[test]
    fn runtime_dispatches_to_cli_path() {
        // For wired CLI harnesses, runtime() should return Some(...). It's
        // either Ok (binary found) or Err::CliBinaryNotFound (missing binary).
        let codex = lookup("codex").expect("codex harness is registered");
        let result = codex.runtime();
        assert!(result.is_some(), "CLI harness must return Some(...)");
    }

    #[test]
    fn runtime_cli_returns_binary_not_found_for_missing_binary() {
        // Build a CLI harness whose binary is guaranteed missing.
        let def: &'static HarnessDefinition =
            Box::leak(Box::new(make_test_definition(Some(RuntimeConfig {
                kind: RuntimeKind::Cli(CliRuntime {
                    binary: "aiki-test-binary-that-should-never-exist-xyz123",
                    args: dummy_args,
                }),
                env: None,
            }))));

        let result = def
            .runtime()
            .expect("CLI harness must return Some(...)");
        let err = match result {
            Ok(_) => panic!("missing binary should produce an error"),
            Err(e) => e,
        };
        // The error chain must surface the missing-binary cause.
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("not found"),
            "expected 'not found' in error chain, got: {msg}"
        );
    }

    #[test]
    fn is_available_false_when_required_env_missing() {
        fn env(env: &mut RuntimeEnv) {
            env.require("AIKI_TEST_HARNESS_REQUIRED_ENV_DEFINITELY_UNSET");
        }
        // Even with a binary that exists, a missing required env var must
        // make the harness unavailable.
        if cfg!(windows) || which::which("sh").is_err() {
            return;
        }
        let def = make_test_definition(Some(RuntimeConfig {
            kind: RuntimeKind::Cli(CliRuntime {
                binary: "sh",
                args: dummy_args,
            }),
            env: Some(env),
        }));
        assert!(!def.is_available());
    }
}
