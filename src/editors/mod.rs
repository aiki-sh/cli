pub mod acp;
pub mod claude_code;
pub mod codex;
pub mod cursor;
pub mod npm;
pub mod transcript;
pub mod zed;

/// Response for editor hook commands (JSON output + exit code)
///
/// This is the editor protocol format, distinct from our internal `HookResult`.
/// - `HookResult`: Aiki's internal result (Decision, context, failures)
/// - `HookCommandOutput`: Editor protocol (JSON value, exit code)
pub struct HookCommandOutput {
    pub json_value: Option<serde_json::Value>,
    pub stdout_text: Option<String>,
    pub exit_code: i32,
}

impl HookCommandOutput {
    #[must_use]
    pub fn new(json_value: Option<serde_json::Value>, exit_code: i32) -> Self {
        Self {
            json_value,
            stdout_text: None,
            exit_code,
        }
    }

    #[must_use]
    #[allow(dead_code)]
    pub fn from_stdout(stdout_text: impl Into<String>, exit_code: i32) -> Self {
        Self {
            json_value: None,
            stdout_text: Some(stdout_text.into()),
            exit_code,
        }
    }

    pub fn print_and_exit(self) -> ! {
        if let Some(text) = &self.stdout_text {
            println!("{}", text);
        } else if let Some(value) = &self.json_value {
            if let Ok(json) = serde_json::to_string(value) {
                println!("{}", json);
            }
        }
        std::process::exit(self.exit_code);
    }
}

/// Why aiki is signalling "not active" on a SessionStart event.
///
/// Drives the agent-facing context text. The user-facing banner is the same
/// nudge in both cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotActiveReason {
    /// The repo declares aiki (`.aiki/` present) but this user has not opted in.
    NotEnabled,
    /// No `.aiki/` here at all (direct invocation in a non-aiki tree).
    NotInstalled,
}

impl NotActiveReason {
    /// Agent-visible context. Instructs the agent that aiki is dormant and that
    /// it must NOT auto-run `aiki init` — the user opts in explicitly.
    #[must_use]
    pub fn agent_context(self) -> &'static str {
        match self {
            NotActiveReason::NotEnabled => {
                "Aiki is installed but not active in this repo. The user can run `aiki init` to \
                 enable Aiki here. Do not invoke `aiki init` or other `aiki` commands unless the \
                 user explicitly requests it. You may suggest activation if relevant, but the user \
                 must opt in."
            }
            NotActiveReason::NotInstalled => {
                "Aiki is not active in this repo. The user can run `aiki init` to enable it. Do not \
                 invoke `aiki init` or other `aiki` commands unless the user explicitly requests it."
            }
        }
    }

    /// User-visible banner text (no leading glyph). Kept free of em-dashes.
    #[must_use]
    pub fn banner(self) -> &'static str {
        "aiki not active. Run `aiki init` to enable"
    }
}
