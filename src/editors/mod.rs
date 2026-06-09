pub mod acp;
pub mod claude_code;
pub mod codex;
pub mod cursor;
pub mod npm;
pub mod transcript;
pub mod zed;

use crate::cache::debug_log;
use anyhow::Result;
use std::io::{self, Read};

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

/// Read and parse JSON from stdin
///
/// Shared utility for all editor handlers to read hook payload data.
pub fn read_stdin_json<T: serde::de::DeserializeOwned>() -> Result<T> {
    let mut stdin = io::stdin();
    let mut buffer = String::new();
    stdin.read_to_string(&mut buffer)?;

    // Debug: log raw JSON to see what we actually receive
    debug_log(|| format!("Raw hook payload JSON:\n{}", buffer));

    Ok(serde_json::from_str(&buffer)?)
}
