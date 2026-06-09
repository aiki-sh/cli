use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Re-export AgentType from canonical location
pub use crate::agents::AgentType;

/// Information about the AI agent that made a change
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub agent_type: AgentType,
    pub version: Option<String>,
    pub detected_at: DateTime<Utc>,
    pub confidence: AttributionConfidence,
    pub detection_method: DetectionMethod,
}

/// Confidence level of the attribution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AttributionConfidence {
    /// 100% - Hook-based detection
    High,
    /// 70-80% - lsof or directory check (Phase 3)
    Medium,
    /// 40-60% - Heuristic (Phase 3)
    Low,
    /// No detection succeeded
    Unknown,
}

/// Method used to detect the AI agent
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DetectionMethod {
    /// ACP (Agent Client Protocol) bidirectional proxy
    ACP,
    /// Claude Code PostToolUse hook
    Hook,
    /// Fallback (Phase 3)
    Unknown,
}

/// A complete provenance record for a change
///
/// This struct stores only metadata that JJ doesn't know about.
/// File paths, diffs, timestamps, and change IDs are retrieved from JJ when needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceRecord {
    /// Information about the agent that made the change
    pub agent: AgentInfo,
    /// Client (IDE) name that connected to the agent (e.g., "zed", "neovim")
    /// This is auto-detected from the ACP InitializeRequest
    pub client_name: Option<String>,
    /// Client (IDE) version (e.g., "0.213.3")
    /// This is auto-detected from the ACP InitializeRequest
    pub client_version: Option<String>,
    /// Agent version (e.g., "0.10.6")
    /// This is auto-detected from the ACP InitializeResponse
    pub agent_version: Option<String>,
    /// Session ID from the agent
    pub session_id: String,
    /// Tool name used (e.g., "Edit" or "Write")
    pub tool_name: String,
    /// Sequential turn number within session (0 if not tracked)
    #[serde(default)]
    pub turn: u32,
    /// Deterministic turn identifier (empty if not tracked)
    #[serde(default)]
    pub turn_id: String,
    /// Source of the current turn (user or autoreply)
    #[serde(default)]
    pub turn_source: String,
    /// Optional human coauthor (for overlapping user edits)
    pub coauthor: Option<String>,
    /// Task IDs that were in-progress when this change was made
    /// Ordered by start time (most recently started first)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<String>,
    /// JJ change_id of the prompt that triggered this turn
    /// Allows looking up the full prompt text via history::get_prompt_by_change_id()
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_change_id: Option<String>,
}

impl ProvenanceRecord {
    /// Create a ProvenanceRecord from a change.completed event
    ///
    /// This constructor extracts all necessary fields from the unified change event
    /// and creates a provenance record. Works with Write, Delete, and Move operations.
    ///
    /// Turn info (turn, turn_id, turn_source) is taken directly from the event payload
    /// rather than querying TurnState. If these fields are not set in the payload
    /// (e.g., for shell commands without turn context), they default to 0/"".
    ///
    /// Note: The `tasks` field is empty by default. Use `with_tasks()` to add
    /// task IDs that were in-progress when the change was made.
    pub fn from_change_completed_event(event: &crate::events::AikiChangeCompletedPayload) -> Self {
        // Use turn info from payload (populated by the turn context if available)
        // This avoids the JJ query overhead of loading TurnState for each change event

        Self {
            agent: AgentInfo {
                agent_type: event.session.agent_type(),
                version: event.session.agent_version().map(|s| s.to_string()),
                detected_at: event.timestamp,
                confidence: AttributionConfidence::High,
                detection_method: event.session.detection_method().clone(),
            },
            client_name: event.session.client_name().map(|s| s.to_string()),
            client_version: event.session.client_version().map(|s| s.to_string()),
            agent_version: event.session.agent_version().map(|s| s.to_string()),
            session_id: event.session.uuid().to_string(),
            tool_name: event.tool_name.clone(),
            turn: event.turn.number,
            turn_id: event.turn.id.clone(),
            turn_source: event.turn.source.clone(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        }
    }

    /// Set the tasks that were in-progress when this change was made
    #[must_use]
    pub fn with_tasks(mut self, tasks: Vec<String>) -> Self {
        self.tasks = tasks;
        self
    }

    /// Set the prompt change_id for this change
    #[must_use]
    pub fn with_prompt_change_id(mut self, prompt_change_id: Option<String>) -> Self {
        self.prompt_change_id = prompt_change_id;
        self
    }

    /// Serialize provenance metadata to change description format
    ///
    /// Note: In jj, every working copy state is a "change" with a stable change_id.
    /// The metadata is stored in the change's description field.
    ///
    /// Format:
    /// ```text
    /// [aiki]
    /// author=claude-code
    /// agent_version=0.10.6
    /// client=zed
    /// client_version=0.213.3
    /// session=claude-session-abc123xyz
    /// tool=Edit
    /// confidence=High
    /// method=Hook
    /// [/aiki]
    /// ```
    ///
    /// # Example
    /// ```
    /// use aiki::provenance::record::*;
    /// use chrono::Utc;
    ///
    /// let record = ProvenanceRecord {
    ///     agent: AgentInfo {
    ///         agent_type: AgentType::ClaudeCode,
    ///         version: None,
    ///         detected_at: Utc::now(),
    ///         confidence: AttributionConfidence::High,
    ///         detection_method: DetectionMethod::Hook,
    ///     },
    ///     client_name: Some("zed".to_string()),
    ///     client_version: None,
    ///     agent_version: None,
    ///     session_id: "test-session".to_string(),
    ///     tool_name: "Edit".to_string(),
    ///     turn: 0,
    ///     turn_id: String::new(),
    ///     turn_source: String::new(),
    ///     coauthor: None,
    ///     tasks: Vec::new(),
    ///     prompt_change_id: None,
    /// };
    ///
    /// let description = record.to_description();
    /// assert!(description.contains("[aiki]"));
    /// assert!(description.contains("author=claude"));
    /// ```
    pub fn to_description(&self) -> String {
        let agent_type = match self.agent.agent_type {
            AgentType::ClaudeCode => "claude",
            AgentType::Codex => "codex",
            AgentType::Cursor => "cursor",
            AgentType::Gemini => "gemini",
            AgentType::Unknown => "unknown",
        };

        let confidence = match self.agent.confidence {
            AttributionConfidence::High => "High",
            AttributionConfidence::Medium => "Medium",
            AttributionConfidence::Low => "Low",
            AttributionConfidence::Unknown => "Unknown",
        };

        let method = match self.agent.detection_method {
            DetectionMethod::ACP => "ACP",
            DetectionMethod::Hook => "Hook",
            DetectionMethod::Unknown => "Unknown",
        };

        let mut lines = vec![
            "[aiki]".to_string(),
            format!("author={}", agent_type),
            "author_type=agent".to_string(),
        ];

        if let Some(ref agent_ver) = self.agent_version {
            lines.push(format!("agent_version={}", agent_ver));
        }

        if let Some(ref client) = self.client_name {
            lines.push(format!("client={}", client));
        }

        if let Some(ref client_ver) = self.client_version {
            lines.push(format!("client_version={}", client_ver));
        }

        lines.extend(vec![
            format!("session={}", self.session_id),
            format!("tool={}", self.tool_name),
            format!("confidence={}", confidence),
            format!("method={}", method),
        ]);

        // Add turn tracking metadata (only if turn has been started)
        if self.turn > 0 {
            lines.push(format!("turn={}", self.turn));
            lines.push(format!("turn_id={}", self.turn_id));
            lines.push(format!("turn_source={}", self.turn_source));
        }

        // Add prompt change_id (allows looking up the prompt that triggered this turn)
        if let Some(ref prompt_id) = self.prompt_change_id {
            lines.push(format!("prompt={}", prompt_id));
        }

        if let Some(ref coauthor) = self.coauthor {
            lines.push(format!("coauthor={}", coauthor));
        }

        // Add task IDs (one per line, most recently started first)
        for task_id in &self.tasks {
            lines.push(format!("task={}", task_id));
        }

        lines.push("[/aiki]".to_string());

        lines.join("\n")
    }

    /// Parse provenance metadata from change description
    ///
    /// Returns None if no [aiki] metadata found (human change or pre-aiki change)
    /// Returns Some(record) if valid aiki metadata is found
    pub fn from_description(description: &str) -> Result<Option<Self>> {
        // Look for [aiki]...[/aiki] block
        let start_marker = "[aiki]";
        let end_marker = "[/aiki]";

        let start = match description.find(start_marker) {
            Some(pos) => pos,
            None => return Ok(None), // No aiki metadata - human change
        };

        let end = description
            .find(end_marker)
            .context("Found [aiki] start marker but no [/aiki] end marker")?;

        let aiki_block = &description[start + start_marker.len()..end];

        // Parse key=value pairs
        // Use HashMap for single-value fields, Vec for multi-value fields (task=)
        let mut metadata = HashMap::new();
        let mut tasks = Vec::new();

        for line in aiki_block.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            if let Some((key, value)) = line.split_once('=') {
                let key = key.trim();
                let value = value.trim().to_string();

                // task= can appear multiple times
                if key == "task" {
                    tasks.push(value);
                } else {
                    metadata.insert(key.to_string(), value);
                }
            }
        }

        // Extract and parse required fields
        let agent_type = match metadata.get("author").map(|s| s.as_str()) {
            Some("claude") => AgentType::ClaudeCode,
            Some("codex") => AgentType::Codex,
            Some("cursor") => AgentType::Cursor,
            Some("gemini") => AgentType::Gemini,
            Some("unknown") => AgentType::Unknown,
            _ => return Err(anyhow::anyhow!("Missing or invalid 'author' field")),
        };

        let session_id = metadata
            .get("session")
            .context("Missing 'session' field")?
            .clone();

        let tool_name = metadata
            .get("tool")
            .context("Missing 'tool' field")?
            .clone();

        let confidence = match metadata.get("confidence").map(|s| s.as_str()) {
            Some("High") => AttributionConfidence::High,
            Some("Medium") => AttributionConfidence::Medium,
            Some("Low") => AttributionConfidence::Low,
            Some("Unknown") => AttributionConfidence::Unknown,
            _ => return Err(anyhow::anyhow!("Missing or invalid 'confidence' field")),
        };

        let method = match metadata.get("method").map(|s| s.as_str()) {
            Some("ACP") => DetectionMethod::ACP,
            Some("Hook") => DetectionMethod::Hook,
            Some("Unknown") => DetectionMethod::Unknown,
            _ => return Err(anyhow::anyhow!("Missing or invalid 'method' field")),
        };

        let client_name = metadata.get("client").cloned();
        let client_version = metadata.get("client_version").cloned();
        let agent_version = metadata.get("agent_version").cloned();
        let coauthor = metadata.get("coauthor").cloned();
        let prompt_change_id = metadata.get("prompt").cloned();

        // Parse turn tracking fields (optional, default to 0/"")
        let turn = metadata
            .get("turn")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let turn_id = metadata.get("turn_id").cloned().unwrap_or_default();
        let turn_source = metadata.get("turn_source").cloned().unwrap_or_default();

        Ok(Some(ProvenanceRecord {
            agent: AgentInfo {
                agent_type,
                version: agent_version.clone(),
                detected_at: Utc::now(), // Timestamp comes from jj change, not stored here
                confidence,
                detection_method: method,
            },
            client_name,
            client_version,
            agent_version,
            session_id,
            tool_name,
            turn,
            turn_id,
            turn_source,
            coauthor,
            tasks,
            prompt_change_id,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_description() {
        let record = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::ClaudeCode,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "test-session-123".to_string(),
            tool_name: "Edit".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        };

        let description = record.to_description();

        // Check that all required fields are present
        assert!(description.contains("[aiki]"));
        assert!(description.contains("[/aiki]"));
        assert!(description.contains("author=claude"));
        assert!(description.contains("session=test-session-123"));
        assert!(description.contains("tool=Edit"));
        assert!(description.contains("confidence=High"));
        assert!(description.contains("method=Hook"));
        // Note: timestamp not stored, comes from jj commit
    }

    #[test]
    fn test_to_description_with_special_characters_in_session_id() {
        // Test that special characters in session ID don't break the format
        let record = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::ClaudeCode,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "session-with-dashes_underscores.dots".to_string(),
            tool_name: "Edit".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        };

        let description = record.to_description();

        assert!(description.contains("session=session-with-dashes_underscores.dots"));
        assert!(description.contains("[aiki]"));
        assert!(description.contains("[/aiki]"));
    }

    #[test]
    fn test_to_description_with_very_long_session_id() {
        // Test with a very long session ID (e.g., UUID + timestamp)
        let long_session_id = format!("claude-session-{}", "a".repeat(200));
        let record = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::ClaudeCode,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: long_session_id.clone(),
            tool_name: "Edit".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        };

        let description = record.to_description();

        assert!(description.contains(&format!("session={}", long_session_id)));
        assert!(description.contains("[aiki]"));
        assert!(description.contains("[/aiki]"));
    }

    #[test]
    fn test_to_description_with_special_tool_names() {
        // Test various tool names that might be used
        let tool_names = vec!["Edit", "Write", "Bash", "Read", "mcp__acp__Edit"];

        for tool_name in tool_names {
            let record = ProvenanceRecord {
                agent: AgentInfo {
                    agent_type: AgentType::ClaudeCode,
                    version: None,
                    detected_at: Utc::now(),
                    confidence: AttributionConfidence::High,
                    detection_method: DetectionMethod::Hook,
                },
                client_name: None,
                client_version: None,
                agent_version: None,
                session_id: "test-session".to_string(),
                tool_name: tool_name.to_string(),
                turn: 0,
                turn_id: String::new(),
                turn_source: String::new(),
                coauthor: None,
                tasks: Vec::new(),
                prompt_change_id: None,
            };

            let description = record.to_description();
            assert!(description.contains(&format!("tool={}", tool_name)));
        }
    }

    #[test]
    fn test_to_description_all_agent_types() {
        // Test serialization for all agent types
        let agent_types = vec![
            (AgentType::ClaudeCode, "claude"),
            (AgentType::Unknown, "unknown"),
        ];

        for (agent_type, expected_str) in agent_types {
            let record = ProvenanceRecord {
                agent: AgentInfo {
                    agent_type,
                    version: None,
                    detected_at: Utc::now(),
                    confidence: AttributionConfidence::High,
                    detection_method: DetectionMethod::Hook,
                },
                client_name: None,
                client_version: None,
                agent_version: None,
                session_id: "test".to_string(),
                tool_name: "Edit".to_string(),
                turn: 0,
                turn_id: String::new(),
                turn_source: String::new(),
                coauthor: None,
                tasks: Vec::new(),
                prompt_change_id: None,
            };

            let description = record.to_description();
            assert!(description.contains(&format!("author={}", expected_str)));
        }
    }

    #[test]
    fn test_to_description_all_confidence_levels() {
        // Test serialization for all confidence levels
        let confidence_levels = vec![
            (AttributionConfidence::High, "High"),
            (AttributionConfidence::Medium, "Medium"),
            (AttributionConfidence::Low, "Low"),
            (AttributionConfidence::Unknown, "Unknown"),
        ];

        for (confidence, expected_str) in confidence_levels {
            let record = ProvenanceRecord {
                agent: AgentInfo {
                    agent_type: AgentType::ClaudeCode,
                    version: None,
                    detected_at: Utc::now(),
                    confidence,
                    detection_method: DetectionMethod::Hook,
                },
                client_name: None,
                client_version: None,
                agent_version: None,
                session_id: "test".to_string(),
                tool_name: "Edit".to_string(),
                turn: 0,
                turn_id: String::new(),
                turn_source: String::new(),
                coauthor: None,
                tasks: Vec::new(),
                prompt_change_id: None,
            };

            let description = record.to_description();
            assert!(description.contains(&format!("confidence={}", expected_str)));
        }
    }

    #[test]
    fn test_to_description_all_detection_methods() {
        // Test serialization for all detection methods
        let methods = vec![
            (DetectionMethod::Hook, "Hook"),
            (DetectionMethod::Unknown, "Unknown"),
        ];

        for (method, expected_str) in methods {
            let record = ProvenanceRecord {
                agent: AgentInfo {
                    agent_type: AgentType::ClaudeCode,
                    version: None,
                    detected_at: Utc::now(),
                    confidence: AttributionConfidence::High,
                    detection_method: method,
                },
                client_name: None,
                client_version: None,
                agent_version: None,
                session_id: "test".to_string(),
                tool_name: "Edit".to_string(),
                turn: 0,
                turn_id: String::new(),
                turn_source: String::new(),
                coauthor: None,
                tasks: Vec::new(),
                prompt_change_id: None,
            };

            let description = record.to_description();
            assert!(description.contains(&format!("method={}", expected_str)));
        }
    }

    #[test]
    fn test_to_description_format_structure() {
        // Test that the format has correct structure
        let record = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::ClaudeCode,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "test".to_string(),
            tool_name: "Edit".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        };

        let description = record.to_description();

        // Should start with [aiki] and end with [/aiki]
        assert!(description.starts_with("[aiki]\n"));
        assert!(description.ends_with("[/aiki]"));

        // Count newlines - should have one per field plus markers
        let line_count = description.lines().count();
        assert_eq!(line_count, 8); // [aiki], author, author_type, session, tool, confidence, method, [/aiki]
    }

    #[test]
    fn test_to_description_no_equals_in_markers() {
        // Verify that the markers themselves don't contain '='
        let record = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::ClaudeCode,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "test".to_string(),
            tool_name: "Edit".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        };

        let description = record.to_description();

        // The first and last lines should be just markers
        let lines: Vec<&str> = description.lines().collect();
        assert_eq!(lines[0], "[aiki]");
        assert_eq!(lines[lines.len() - 1], "[/aiki]");

        // All middle lines should contain '='
        for line in &lines[1..lines.len() - 1] {
            assert!(line.contains('='), "Line '{}' should contain '='", line);
        }
    }

    #[test]
    fn test_empty_session_id() {
        // Test edge case with empty session ID
        let record = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::ClaudeCode,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "".to_string(),
            tool_name: "Edit".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        };

        let description = record.to_description();

        // Should still be valid format even with empty session
        assert!(description.contains("session="));
        assert!(description.contains("[aiki]"));
        assert!(description.contains("[/aiki]"));
    }

    #[test]
    fn test_serialization_deserialization_roundtrip() {
        // Test that ProvenanceRecord can be serialized and deserialized
        let record = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::ClaudeCode,
                version: Some("1.0.0".to_string()),
                detected_at: Utc::now(),
                confidence: AttributionConfidence::Medium,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "roundtrip-test".to_string(),
            tool_name: "Write".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        };

        // Test JSON serialization
        let json = serde_json::to_string(&record).unwrap();
        let deserialized: ProvenanceRecord = serde_json::from_str(&json).unwrap();

        // Verify key fields match
        assert_eq!(record.session_id, deserialized.session_id);
        assert_eq!(record.tool_name, deserialized.tool_name);
    }

    #[test]
    fn test_from_description_with_aiki_metadata() {
        let description = "[aiki]\n\
            author=claude\n\
            author_type=agent\n\
            session=test-session-456\n\
            tool=Write\n\
            confidence=High\n\
            method=Hook\n\
            [/aiki]";

        let result = ProvenanceRecord::from_description(description).unwrap();
        assert!(result.is_some());

        let record = result.unwrap();
        assert!(matches!(record.agent.agent_type, AgentType::ClaudeCode));
        assert_eq!(record.session_id, "test-session-456");
        assert_eq!(record.tool_name, "Write");
        assert!(matches!(
            record.agent.confidence,
            AttributionConfidence::High
        ));
        assert!(matches!(
            record.agent.detection_method,
            DetectionMethod::Hook
        ));
    }

    #[test]
    fn test_from_description_without_aiki_metadata() {
        // Human commit - no aiki metadata
        let description = "Fix bug in parser\n\nThis commit fixes issue #123";

        let result = ProvenanceRecord::from_description(description).unwrap();
        assert!(result.is_none(), "Expected None for human commit");
    }

    #[test]
    fn test_from_description_with_extra_content() {
        // Commit with both aiki metadata and regular description
        let description = "Add new feature\n\n\
            This is a longer description.\n\n\
            [aiki]\n\
            author=claude\n\
            author_type=agent\n\
            session=abc123\n\
            tool=Edit\n\
            confidence=High\n\
            method=Hook\n\
            [/aiki]\n\n\
            Additional notes here.";

        let result = ProvenanceRecord::from_description(description).unwrap();
        assert!(result.is_some());

        let record = result.unwrap();
        assert_eq!(record.session_id, "abc123");
    }

    #[test]
    fn test_from_description_round_trip() {
        let original = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::ClaudeCode,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "round-trip".to_string(),
            tool_name: "Edit".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        };

        let description = original.to_description();
        let parsed = ProvenanceRecord::from_description(&description)
            .unwrap()
            .unwrap();

        assert!(matches!(parsed.agent.agent_type, AgentType::ClaudeCode));
        assert_eq!(parsed.session_id, original.session_id);
        assert_eq!(parsed.tool_name, original.tool_name);
        assert!(matches!(
            parsed.agent.confidence,
            AttributionConfidence::High
        ));
    }

    #[test]
    fn test_from_description_missing_field() {
        let description = "[aiki]\n\
            author=claude-code\n\
            author_type=agent\n\
            tool=Edit\n\
            [/aiki]";

        let result = ProvenanceRecord::from_description(description);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Missing"));
    }

    #[test]
    fn test_from_description_invalid_agent() {
        let description = "[aiki]\n\
            author=invalid-agent\n\
            author_type=agent\n\
            session=test\n\
            tool=Edit\n\
            confidence=High\n\
            method=Hook\n\
            [/aiki]";

        let result = ProvenanceRecord::from_description(description);
        assert!(result.is_err());
    }

    #[test]
    fn test_cursor_agent_type_serialization() {
        let record = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::Cursor,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "cursor-session-123".to_string(),
            tool_name: "Edit".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        };

        let description = record.to_description();
        assert!(description.contains("author=cursor"));
        assert!(description.contains("session=cursor-session-123"));
    }

    #[test]
    fn test_cursor_agent_type_deserialization() {
        let description = "[aiki]\n\
            author=cursor\n\
            author_type=agent\n\
            session=cursor-test-session\n\
            tool=Edit\n\
            confidence=High\n\
            method=Hook\n\
            [/aiki]";

        let result = ProvenanceRecord::from_description(description).unwrap();
        assert!(result.is_some());
        let record = result.unwrap();
        assert!(matches!(record.agent.agent_type, AgentType::Cursor));
        assert_eq!(record.session_id, "cursor-test-session");
    }

    #[test]
    fn test_cursor_agent_type_round_trip() {
        let original = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::Cursor,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "cursor-roundtrip".to_string(),
            tool_name: "Write".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        };

        let description = original.to_description();
        let parsed = ProvenanceRecord::from_description(&description)
            .unwrap()
            .unwrap();

        assert!(matches!(parsed.agent.agent_type, AgentType::Cursor));
        assert_eq!(parsed.session_id, original.session_id);
        assert_eq!(parsed.tool_name, original.tool_name);
    }

    #[test]
    fn test_codex_agent_type_serialization() {
        let record = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::Codex,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "codex-session-123".to_string(),
            tool_name: "Edit".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        };

        let description = record.to_description();
        assert!(description.contains("author=codex"));
        assert!(description.contains("session=codex-session-123"));
    }

    #[test]
    fn test_codex_agent_type_deserialization() {
        let description = "[aiki]\n\
            author=codex\n\
            author_type=agent\n\
            session=codex-test-session\n\
            tool=Edit\n\
            confidence=High\n\
            method=Hook\n\
            [/aiki]";

        let result = ProvenanceRecord::from_description(description).unwrap();
        assert!(result.is_some());
        let record = result.unwrap();
        assert!(matches!(record.agent.agent_type, AgentType::Codex));
        assert_eq!(record.session_id, "codex-test-session");
    }

    #[test]
    fn test_codex_agent_type_round_trip() {
        let original = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::Codex,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "codex-roundtrip".to_string(),
            tool_name: "Write".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        };

        let description = original.to_description();
        let parsed = ProvenanceRecord::from_description(&description)
            .unwrap()
            .unwrap();

        assert!(matches!(parsed.agent.agent_type, AgentType::Codex));
        assert_eq!(parsed.session_id, original.session_id);
        assert_eq!(parsed.tool_name, original.tool_name);
    }

    // =========================================================================
    // Task field tests
    // =========================================================================

    #[test]
    fn test_to_description_with_single_task() {
        let record = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::ClaudeCode,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "test-session".to_string(),
            tool_name: "Edit".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: vec!["abc123".to_string()],
            prompt_change_id: None,
        };

        let description = record.to_description();
        assert!(description.contains("task=abc123"));
    }

    #[test]
    fn test_to_description_with_multiple_tasks() {
        let record = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::ClaudeCode,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "test-session".to_string(),
            tool_name: "Edit".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: vec![
                "task1".to_string(),
                "task2".to_string(),
                "task3".to_string(),
            ],
            prompt_change_id: None,
        };

        let description = record.to_description();
        assert!(description.contains("task=task1"));
        assert!(description.contains("task=task2"));
        assert!(description.contains("task=task3"));

        // Tasks should appear in order (most recently started first)
        let task1_pos = description.find("task=task1").unwrap();
        let task2_pos = description.find("task=task2").unwrap();
        let task3_pos = description.find("task=task3").unwrap();
        assert!(task1_pos < task2_pos);
        assert!(task2_pos < task3_pos);
    }

    #[test]
    fn test_to_description_with_no_tasks() {
        let record = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::ClaudeCode,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "test-session".to_string(),
            tool_name: "Edit".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        };

        let description = record.to_description();
        // No task= lines should be present
        assert!(!description.contains("task="));
    }

    #[test]
    fn test_from_description_with_single_task() {
        let description = "[aiki]\n\
            author=claude\n\
            session=test-session\n\
            tool=Edit\n\
            confidence=High\n\
            method=Hook\n\
            task=abc123\n\
            [/aiki]";

        let result = ProvenanceRecord::from_description(description).unwrap();
        assert!(result.is_some());
        let record = result.unwrap();
        assert_eq!(record.tasks, vec!["abc123"]);
    }

    #[test]
    fn test_from_description_with_multiple_tasks() {
        let description = "[aiki]\n\
            author=claude\n\
            session=test-session\n\
            tool=Edit\n\
            confidence=High\n\
            method=Hook\n\
            task=task1\n\
            task=task2\n\
            task=task3\n\
            [/aiki]";

        let result = ProvenanceRecord::from_description(description).unwrap();
        assert!(result.is_some());
        let record = result.unwrap();
        assert_eq!(record.tasks, vec!["task1", "task2", "task3"]);
    }

    #[test]
    fn test_from_description_with_no_tasks() {
        let description = "[aiki]\n\
            author=claude\n\
            session=test-session\n\
            tool=Edit\n\
            confidence=High\n\
            method=Hook\n\
            [/aiki]";

        let result = ProvenanceRecord::from_description(description).unwrap();
        assert!(result.is_some());
        let record = result.unwrap();
        assert!(record.tasks.is_empty());
    }

    #[test]
    fn test_tasks_round_trip() {
        let original = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::ClaudeCode,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "task-roundtrip".to_string(),
            tool_name: "Edit".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: vec!["task-alpha".to_string(), "task-beta".to_string()],
            prompt_change_id: None,
        };

        let description = original.to_description();
        let parsed = ProvenanceRecord::from_description(&description)
            .unwrap()
            .unwrap();

        assert_eq!(parsed.tasks, original.tasks);
    }

    #[test]
    fn test_with_tasks_builder() {
        // Create a mock payload with necessary fields
        // Note: This is a simplified test - full integration would require a real payload
        let record = ProvenanceRecord {
            agent: AgentInfo {
                agent_type: AgentType::ClaudeCode,
                version: None,
                detected_at: Utc::now(),
                confidence: AttributionConfidence::High,
                detection_method: DetectionMethod::Hook,
            },
            client_name: None,
            client_version: None,
            agent_version: None,
            session_id: "test".to_string(),
            tool_name: "Edit".to_string(),
            turn: 0,
            turn_id: String::new(),
            turn_source: String::new(),
            coauthor: None,
            tasks: Vec::new(),
            prompt_change_id: None,
        };

        // Use with_tasks to add task IDs
        let record_with_tasks = record.with_tasks(vec!["task1".to_string(), "task2".to_string()]);

        assert_eq!(record_with_tasks.tasks, vec!["task1", "task2"]);
    }
}
