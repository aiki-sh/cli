use super::definition::{self, HarnessDefinition, HooksConfig, HooksKind, Identity, Install};
use crate::agents::AgentType;

definition::submit! {
    HarnessDefinition {
        install: Install {
            hint: "Gemini task execution not yet supported",
        },
        identity: Identity {
            id: "gemini",
            aliases: &[],
            display_name: "Gemini",
            agent_type: AgentType::Gemini,
            email: "noreply@google.com",
            custom_metadata_name: None,
        },
        hooks: HooksConfig {
            kind: HooksKind::Vendor,
            supports_blocking: false,
        },
        runtime: None,
    }
}
