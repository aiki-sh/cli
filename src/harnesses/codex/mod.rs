mod runtime;

use super::definition::{self, HarnessDefinition, HooksConfig, HooksKind, Identity, Install};
use super::runtime::{CliRuntime, RuntimeConfig, RuntimeKind};
use crate::agents::AgentType;

definition::submit! {
    HarnessDefinition {
        install: Install {
            hint: "Install: npm install -g @openai/codex",
        },
        identity: Identity {
            id: "codex",
            aliases: &[],
            display_name: "Codex",
            agent_type: AgentType::Codex,
            email: "noreply@openai.com",
            custom_metadata_name: None,
        },
        hooks: HooksConfig {
            kind: HooksKind::Vendor,
            supports_blocking: true,
        },
        runtime: Some(RuntimeConfig {
            kind: RuntimeKind::Cli(CliRuntime {
                binary: "codex",
                args: runtime::args,
            }),
            env: None,
        }),
    }
}
