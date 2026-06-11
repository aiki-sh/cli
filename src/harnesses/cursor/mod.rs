use super::definition::{self, HarnessDefinition, HooksConfig, HooksKind, Identity, Install};
use crate::agents::AgentType;

definition::submit! {
    HarnessDefinition {
        install: Install {
            hint: "Install Cursor from https://cursor.com (task execution not yet supported)",
        },
        identity: Identity {
            id: "cursor",
            aliases: &[],
            display_name: "Cursor",
            agent_type: AgentType::Cursor,
            email: "noreply@cursor.com",
            custom_metadata_name: None,
        },
        hooks: HooksConfig {
            kind: HooksKind::Vendor,
            supports_blocking: false,
        },
        runtime: None,
    }
}
