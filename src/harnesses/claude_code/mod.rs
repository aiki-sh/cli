mod runtime;

use super::definition::{self, HarnessDefinition, HooksConfig, HooksKind, Identity, Install};
use super::runtime::{CliRuntime, RuntimeConfig, RuntimeKind};
use crate::agents::AgentType;

definition::submit! {
    HarnessDefinition {
        install: Install {
            hint: "Install: brew install claude-code (or: npm install -g @anthropic-ai/claude-code)",
        },
        identity: Identity {
            id: "claude-code",
            aliases: &["claude"],
            display_name: "Claude",
            agent_type: AgentType::ClaudeCode,
            email: "noreply@anthropic.com",
            custom_metadata_name: Some("claude"),
        },
        hooks: HooksConfig {
            kind: HooksKind::Vendor,
            supports_blocking: true,
        },
        runtime: Some(RuntimeConfig {
            kind: RuntimeKind::Cli(CliRuntime {
                binary: "claude",
                args: runtime::args,
            }),
            env: Some(runtime::env),
        }),
    }
}
