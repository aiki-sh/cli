use serde::Deserialize;

use crate::events::FileOperation;
use crate::tools::{ToolType, WebOperation};

// ============================================================================
// Tool Input Structures
// ============================================================================

/// Tool input for file operations (Edit, Write, NotebookEdit)
/// Unified struct that handles all file-modifying tools.
/// See: https://code.claude.com/docs/en/hooks#posttooluse-input
#[derive(Deserialize, Debug)]
pub struct FileToolInput {
    pub file_path: String,
    /// Old string to replace (Edit tool)
    #[serde(default)]
    pub old_string: String,
    /// New string to insert (Edit tool)
    #[serde(default)]
    pub new_string: String,
    /// File content (Write tool)
    #[serde(default)]
    pub content: String,
}

/// Tool input for MultiEdit tool (atomic multi-file edits)
#[derive(Deserialize, Debug)]
pub struct MultiEditToolInput {
    pub edits: Vec<MultiEditEntry>,
}

#[derive(Deserialize, Debug)]
pub struct MultiEditEntry {
    pub file_path: String,
    #[serde(default)]
    pub old_string: String,
    #[serde(default)]
    pub new_string: String,
}

/// Tool input for Bash tool
#[derive(Deserialize, Debug)]
pub struct BashToolInput {
    #[serde(default)]
    pub command: String,
}

// ============================================================================
// Read Tool Input Structures
// ============================================================================

/// Tool input for Read tool
#[derive(Deserialize, Debug)]
pub struct ReadToolInput {
    pub file_path: String,
}

/// Tool input for Glob tool
#[derive(Deserialize, Debug)]
pub struct GlobToolInput {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
}

/// Tool input for Grep tool
#[derive(Deserialize, Debug)]
pub struct GrepToolInput {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
}

/// Tool input for LS tool
#[derive(Deserialize, Debug)]
pub struct LsToolInput {
    #[serde(default)]
    pub path: Option<String>,
}

// ============================================================================
// Web Tool Input Structures
// ============================================================================

/// Tool input for WebFetch tool
#[derive(Deserialize, Debug)]
pub struct WebFetchToolInput {
    pub url: String,
}

/// Tool input for WebSearch tool
#[derive(Deserialize, Debug)]
pub struct WebSearchToolInput {
    pub query: String,
}

// ============================================================================
// Tool Response Structures (PostToolUse)
// ============================================================================

/// Response structure for Bash tool - includes exit code!
/// This is critical for flows that need to react to command failures.
/// See: https://code.claude.com/docs/en/hooks#posttooluse-input
#[derive(Deserialize, Debug)]
pub struct BashToolResponse {
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(rename = "exitCode", default)]
    pub exit_code: i32,
}

// ============================================================================
// Tool Classification (Claude Code specific)
// ============================================================================

/// Parsed Claude Code tool with its typed input
#[derive(Debug)]
pub enum ClaudeTool {
    // Write operations
    Edit(FileToolInput),
    Write(FileToolInput),
    NotebookEdit(FileToolInput),
    MultiEdit(MultiEditToolInput),

    // Read operations
    Read(ReadToolInput),
    Glob(GlobToolInput),
    Grep(GrepToolInput),
    LS(LsToolInput),

    // Shell operations
    Bash(BashToolInput),

    // Web operations
    WebFetch(WebFetchToolInput),
    WebSearch(WebSearchToolInput),

    // Other tools
    Internal, // Task, TodoRead, TodoWrite
    Mcp,      // MCP tools

    // Unknown or failed parse
    Unknown(String), // Store tool name for error reporting
}

impl ClaudeTool {
    /// Parse a Claude tool from tool_name and optional tool_input JSON
    pub fn parse(tool_name: &str, input_json: Option<&serde_json::Value>) -> Self {
        // Helper to deserialize and construct, or return None on failure
        fn try_parse<T, F>(
            constructor: F,
            input_json: Option<&serde_json::Value>,
        ) -> Option<ClaudeTool>
        where
            T: serde::de::DeserializeOwned,
            F: FnOnce(T) -> ClaudeTool,
        {
            input_json
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .map(constructor)
        }

        match tool_name {
            // File write tools
            "Edit" => try_parse(ClaudeTool::Edit, input_json),
            "Write" => try_parse(ClaudeTool::Write, input_json),
            "NotebookEdit" => try_parse(ClaudeTool::NotebookEdit, input_json),
            "MultiEdit" => try_parse(ClaudeTool::MultiEdit, input_json),

            // File read tools
            "Read" => try_parse(ClaudeTool::Read, input_json),
            "Glob" => try_parse(ClaudeTool::Glob, input_json),
            "Grep" => try_parse(ClaudeTool::Grep, input_json),
            "LS" => try_parse(ClaudeTool::LS, input_json),

            // Shell tool
            "Bash" => try_parse(ClaudeTool::Bash, input_json),

            // Web tools
            "WebFetch" => try_parse(ClaudeTool::WebFetch, input_json),
            "WebSearch" => try_parse(ClaudeTool::WebSearch, input_json),

            // Internal tools
            "Task" | "TodoRead" | "TodoWrite" | "EnterPlanMode" | "ExitPlanMode" => {
                Some(ClaudeTool::Internal)
            }

            // MCP tools follow naming convention: mcp__<server>__<tool>
            _ if tool_name.starts_with("mcp__") => Some(ClaudeTool::Mcp),

            // Unknown tool - warn and treat as internal to skip silently
            // This helps detect when Claude Code adds new native tools
            _ => {
                eprintln!(
                    "[aiki] Warning: Unknown Claude tool '{}'. \
                     This may be a new native tool - please report at \
                     https://github.com/anthropics/claude-code/issues",
                    tool_name
                );
                Some(ClaudeTool::Unknown(tool_name.to_string()))
            }
        }
        .unwrap_or_else(|| ClaudeTool::Unknown(tool_name.to_string()))
    }

    /// Get the ToolType for this Claude tool
    pub fn tool_type(&self) -> ToolType {
        match self {
            ClaudeTool::Edit(_)
            | ClaudeTool::Write(_)
            | ClaudeTool::NotebookEdit(_)
            | ClaudeTool::MultiEdit(_)
            | ClaudeTool::Read(_)
            | ClaudeTool::Glob(_)
            | ClaudeTool::Grep(_)
            | ClaudeTool::LS(_) => ToolType::File,
            ClaudeTool::Bash(_) => ToolType::Shell,
            ClaudeTool::WebFetch(_) | ClaudeTool::WebSearch(_) => ToolType::Web,
            ClaudeTool::Internal => ToolType::Internal,
            ClaudeTool::Mcp => ToolType::Mcp,
            ClaudeTool::Unknown(_) => ToolType::Internal, // Treat as internal to skip silently
        }
    }

    /// Get the FileOperation for file tools
    ///
    /// Returns None if called on non-file tools.
    pub fn file_operation(&self) -> Option<FileOperation> {
        match self {
            ClaudeTool::Edit(_)
            | ClaudeTool::Write(_)
            | ClaudeTool::NotebookEdit(_)
            | ClaudeTool::MultiEdit(_) => Some(FileOperation::Write),
            ClaudeTool::Read(_) | ClaudeTool::Glob(_) | ClaudeTool::Grep(_) | ClaudeTool::LS(_) => {
                Some(FileOperation::Read)
            }
            _ => {
                eprintln!(
                    "[aiki] Warning: file_operation() called on non-file tool: {:?}",
                    self
                );
                None
            }
        }
    }

    /// Get the WebOperation for web tools
    ///
    /// Returns None if called on non-web tools.
    pub fn web_operation(&self) -> Option<WebOperation> {
        match self {
            ClaudeTool::WebFetch(_) => Some(WebOperation::Fetch),
            ClaudeTool::WebSearch(_) => Some(WebOperation::Search),
            _ => {
                eprintln!(
                    "[aiki] Warning: web_operation() called on non-web tool: {:?}",
                    self
                );
                None
            }
        }
    }
}
