//! Agent flag structs for CLI subcommands
//!
//! Provides three `clap::Args` structs for agent-based filtering:
//! - `SessionIdFlags` — requires a session ID value (for `session show`, `session transcript`)
//! - `AgentFilterFlags` — boolean flags, no value (for `session list`)
//! - `TaskAgentFlags` — optional value (for `task list`)

use crate::agents::{AgentType, Assignee};
use crate::error::{AikiError, Result};
use crate::session::AikiSession;

/// Common behaviour for structs that carry agent-shorthand flags.
pub trait AgentFlags {
    /// Which agent type (if any) was selected.
    fn agent_type(&self) -> Option<AgentType>;

    /// The external session ID supplied by the user (if any).
    fn external_session_id(&self) -> Option<&str>;

    /// Derive the deterministic Aiki session UUID from agent + external ID.
    fn session_uuid(&self) -> Option<String> {
        let agent = self.agent_type()?;
        let id = self.external_session_id()?;
        Some(AikiSession::generate_uuid(agent, id))
    }

    /// True when at least one agent flag was provided.
    fn is_set(&self) -> bool {
        self.agent_type().is_some()
    }
}

// ---------------------------------------------------------------------------
// SessionIdFlags — value required
// ---------------------------------------------------------------------------

/// Mutual-exclusion group where each flag takes a required `SESSION_ID`.
///
/// Used by `session show` and `session transcript`.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct SessionIdFlags {
    /// Filter by Claude Code session ID
    #[arg(long, value_name = "SESSION_ID", group = "agent_session")]
    pub claude: Option<String>,

    /// Filter by Codex session ID
    #[arg(long, value_name = "SESSION_ID", group = "agent_session")]
    pub codex: Option<String>,

    /// Filter by Cursor session ID
    #[arg(long, value_name = "SESSION_ID", group = "agent_session")]
    pub cursor: Option<String>,

    /// Filter by Gemini session ID
    #[arg(long, value_name = "SESSION_ID", group = "agent_session")]
    pub gemini: Option<String>,
}

impl AgentFlags for SessionIdFlags {
    fn agent_type(&self) -> Option<AgentType> {
        if self.claude.is_some() {
            Some(AgentType::ClaudeCode)
        } else if self.codex.is_some() {
            Some(AgentType::Codex)
        } else if self.cursor.is_some() {
            Some(AgentType::Cursor)
        } else if self.gemini.is_some() {
            Some(AgentType::Gemini)
        } else {
            None
        }
    }

    fn external_session_id(&self) -> Option<&str> {
        self.claude
            .as_deref()
            .or(self.codex.as_deref())
            .or(self.cursor.as_deref())
            .or(self.gemini.as_deref())
    }
}

// ---------------------------------------------------------------------------
// AgentFilterFlags — boolean, no value
// ---------------------------------------------------------------------------

/// Stackable boolean flags (no value).
///
/// Used by `session list` to filter by agent type.  Multiple flags can be
/// combined as an OR filter (e.g. `--claude --codex`).
#[derive(Debug, Clone, Default, clap::Args)]
pub struct AgentFilterFlags {
    /// Show only Claude Code sessions
    #[arg(long)]
    pub claude: bool,

    /// Show only Codex sessions
    #[arg(long)]
    pub codex: bool,

    /// Show only Cursor sessions
    #[arg(long)]
    pub cursor: bool,

    /// Show only Gemini sessions
    #[arg(long)]
    pub gemini: bool,

    /// Filter by agent name (e.g. "claude-code", "codex")
    #[arg(long, hide = true, conflicts_with_all = ["claude", "codex", "cursor", "gemini"])]
    pub agent: Option<String>,
}

impl AgentFilterFlags {
    /// Return an error if `--agent` was given with an unrecognized name.
    pub fn validate(&self) -> Result<()> {
        if let Some(name) = &self.agent {
            if AgentType::from_str(name).is_none() {
                return Err(AikiError::UnknownAgentType(name.clone()));
            }
        }
        Ok(())
    }

    /// Collect all selected agent types.
    ///
    /// When multiple shorthand flags are set (e.g. `--claude --codex`), all of
    /// them are returned so callers can use them as an OR filter.  Falls back to
    /// `--agent` when no shorthand flag is set.
    #[must_use]
    pub fn agent_types(&self) -> Vec<AgentType> {
        let mut types = Vec::new();
        if self.claude {
            types.push(AgentType::ClaudeCode);
        }
        if self.codex {
            types.push(AgentType::Codex);
        }
        if self.cursor {
            types.push(AgentType::Cursor);
        }
        if self.gemini {
            types.push(AgentType::Gemini);
        }
        if types.is_empty() {
            if let Some(at) = self.agent.as_deref().and_then(AgentType::from_str) {
                types.push(at);
            }
        }
        types
    }
}

impl AgentFlags for AgentFilterFlags {
    fn agent_type(&self) -> Option<AgentType> {
        if self.claude {
            Some(AgentType::ClaudeCode)
        } else if self.codex {
            Some(AgentType::Codex)
        } else if self.cursor {
            Some(AgentType::Cursor)
        } else if self.gemini {
            Some(AgentType::Gemini)
        } else {
            self.agent.as_deref().and_then(AgentType::from_str)
        }
    }

    fn external_session_id(&self) -> Option<&str> {
        None
    }
}

// ---------------------------------------------------------------------------
// TaskAgentFlags — optional value
// ---------------------------------------------------------------------------

/// Resolved filter for task queries.
#[derive(Debug)]
pub enum TaskAgentFilter {
    /// Filter by assignee (agent type, human, or unassigned).
    Assignee(Assignee),
    /// Filter by a specific external session ID (Aiki UUID).
    Session(String),
}

/// Mutual-exclusion group where each flag accepts an optional session ID.
///
/// Used by `task list`. Conflicts with `--assignee` and `--unassigned`.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct TaskAgentFlags {
    /// Filter tasks by Claude Code (optionally provide session ID)
    #[arg(
        long,
        value_name = "SESSION_ID",
        num_args = 0..=1,
        default_missing_value = "",
        group = "agent_task",
        conflicts_with_all = ["assignee", "unassigned"],
    )]
    pub claude: Option<String>,

    /// Filter tasks by Codex (optionally provide session ID)
    #[arg(
        long,
        value_name = "SESSION_ID",
        num_args = 0..=1,
        default_missing_value = "",
        group = "agent_task",
        conflicts_with_all = ["assignee", "unassigned"],
    )]
    pub codex: Option<String>,

    /// Filter tasks by Cursor (optionally provide session ID)
    #[arg(
        long,
        value_name = "SESSION_ID",
        num_args = 0..=1,
        default_missing_value = "",
        group = "agent_task",
        conflicts_with_all = ["assignee", "unassigned"],
    )]
    pub cursor: Option<String>,

    /// Filter tasks by Gemini (optionally provide session ID)
    #[arg(
        long,
        value_name = "SESSION_ID",
        num_args = 0..=1,
        default_missing_value = "",
        group = "agent_task",
        conflicts_with_all = ["assignee", "unassigned"],
    )]
    pub gemini: Option<String>,
}

impl AgentFlags for TaskAgentFlags {
    fn agent_type(&self) -> Option<AgentType> {
        if self.claude.is_some() {
            Some(AgentType::ClaudeCode)
        } else if self.codex.is_some() {
            Some(AgentType::Codex)
        } else if self.cursor.is_some() {
            Some(AgentType::Cursor)
        } else if self.gemini.is_some() {
            Some(AgentType::Gemini)
        } else {
            None
        }
    }

    fn external_session_id(&self) -> Option<&str> {
        let raw = self
            .claude
            .as_deref()
            .or(self.codex.as_deref())
            .or(self.cursor.as_deref())
            .or(self.gemini.as_deref())?;
        if raw.is_empty() {
            None
        } else {
            Some(raw)
        }
    }
}

impl TaskAgentFlags {
    /// Resolve the flags into a `TaskAgentFilter`.
    ///
    /// Returns `None` when no agent flag was provided.
    #[must_use]
    pub fn resolve(&self) -> Option<TaskAgentFilter> {
        let agent = self.agent_type()?;
        match self.external_session_id() {
            Some(id) => {
                let uuid = AikiSession::generate_uuid(agent, id);
                Some(TaskAgentFilter::Session(uuid))
            }
            None => Some(TaskAgentFilter::Assignee(Assignee::Agent(agent))),
        }
    }
}

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

/// Collapse per-agent bool flags and an `--agent <name>` option into a single
/// optional `AgentType`.
///
/// Parses the `--agent` string (if present) via `AgentType::from_str`, otherwise
/// maps the first `true` shorthand flag to its variant.  Returns `None` when
/// neither source provides a valid agent.
#[must_use]
pub fn resolve_agent_shorthand(
    agent: Option<String>,
    claude: bool,
    codex: bool,
    cursor: bool,
    gemini: bool,
) -> Option<AgentType> {
    agent
        .as_deref()
        .and_then(AgentType::from_str)
        .or_else(|| {
            if claude {
                Some(AgentType::ClaudeCode)
            } else if codex {
                Some(AgentType::Codex)
            } else if cursor {
                Some(AgentType::Cursor)
            } else if gemini {
                Some(AgentType::Gemini)
            } else {
                None
            }
        })
}

/// Collapse per-agent shorthand flags (each carrying an event value) and
/// explicit `--agent`/`--event` options into an `(AgentType, event)` pair.
///
/// Shorthand flags take precedence: `--claude SessionStart` resolves to
/// `(AgentType::ClaudeCode, "SessionStart")`.  Falls back to the explicit pair
/// when no shorthand is set.  Returns `None` if neither source provides a
/// complete pair (or if `--agent` is not a recognised agent name).
#[must_use]
pub fn resolve_agent_event_shorthand(
    agent: Option<String>,
    event: Option<String>,
    claude: Option<String>,
    codex: Option<String>,
    cursor: Option<String>,
    gemini: Option<String>,
) -> Option<(AgentType, String)> {
    if let Some(ev) = claude {
        Some((AgentType::ClaudeCode, ev))
    } else if let Some(ev) = codex {
        Some((AgentType::Codex, ev))
    } else if let Some(ev) = cursor {
        Some((AgentType::Cursor, ev))
    } else if let Some(ev) = gemini {
        Some((AgentType::Gemini, ev))
    } else {
        match (agent, event) {
            (Some(a), Some(e)) => AgentType::from_str(&a).map(|at| (at, e)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_flags_agent_type() {
        let flags = SessionIdFlags {
            claude: Some("abc".into()),
            ..Default::default()
        };
        assert_eq!(flags.agent_type(), Some(AgentType::ClaudeCode));
        assert_eq!(flags.external_session_id(), Some("abc"));
        assert!(flags.is_set());
    }

    #[test]
    fn session_id_flags_none() {
        let flags = SessionIdFlags::default();
        assert_eq!(flags.agent_type(), None);
        assert_eq!(flags.external_session_id(), None);
        assert!(!flags.is_set());
    }

    #[test]
    fn agent_filter_flags_bool() {
        let flags = AgentFilterFlags {
            codex: true,
            ..Default::default()
        };
        assert_eq!(flags.agent_type(), Some(AgentType::Codex));
        assert_eq!(flags.external_session_id(), None);
    }

    #[test]
    fn agent_filter_flags_agent_string() {
        let flags = AgentFilterFlags {
            agent: Some("cursor".into()),
            ..Default::default()
        };
        assert_eq!(flags.agent_type(), Some(AgentType::Cursor));
    }

    #[test]
    fn task_agent_flags_no_session() {
        let flags = TaskAgentFlags {
            claude: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(flags.agent_type(), Some(AgentType::ClaudeCode));
        assert_eq!(flags.external_session_id(), None);
        assert!(matches!(
            flags.resolve(),
            Some(TaskAgentFilter::Assignee(Assignee::Agent(AgentType::ClaudeCode)))
        ));
    }

    #[test]
    fn task_agent_flags_with_session() {
        let flags = TaskAgentFlags {
            gemini: Some("ext-123".into()),
            ..Default::default()
        };
        assert_eq!(flags.agent_type(), Some(AgentType::Gemini));
        assert_eq!(flags.external_session_id(), Some("ext-123"));
        assert!(matches!(flags.resolve(), Some(TaskAgentFilter::Session(_))));
    }

    #[test]
    fn task_agent_flags_none() {
        let flags = TaskAgentFlags::default();
        assert!(flags.resolve().is_none());
    }

    #[test]
    fn resolve_shorthand_agent_takes_precedence() {
        let result = resolve_agent_shorthand(Some("gemini".into()), true, false, false, false);
        assert_eq!(result, Some(AgentType::Gemini));
    }

    #[test]
    fn resolve_shorthand_bool_fallback() {
        assert_eq!(
            resolve_agent_shorthand(None, true, false, false, false),
            Some(AgentType::ClaudeCode)
        );
        assert_eq!(
            resolve_agent_shorthand(None, false, true, false, false),
            Some(AgentType::Codex)
        );
        assert_eq!(
            resolve_agent_shorthand(None, false, false, true, false),
            Some(AgentType::Cursor)
        );
        assert_eq!(
            resolve_agent_shorthand(None, false, false, false, true),
            Some(AgentType::Gemini)
        );
    }

    #[test]
    fn resolve_shorthand_none() {
        assert_eq!(
            resolve_agent_shorthand(None, false, false, false, false),
            None
        );
    }

    #[test]
    fn resolve_agent_event_claude() {
        assert_eq!(
            resolve_agent_event_shorthand(None, None, Some("SessionStart".into()), None, None, None),
            Some((AgentType::ClaudeCode, "SessionStart".into()))
        );
    }

    #[test]
    fn resolve_agent_event_codex() {
        assert_eq!(
            resolve_agent_event_shorthand(None, None, None, Some("stop".into()), None, None),
            Some((AgentType::Codex, "stop".into()))
        );
    }

    #[test]
    fn resolve_agent_event_explicit_pair() {
        assert_eq!(
            resolve_agent_event_shorthand(
                Some("cursor".into()),
                Some("beforeSubmitPrompt".into()),
                None, None, None, None,
            ),
            Some((AgentType::Cursor, "beforeSubmitPrompt".into()))
        );
    }

    #[test]
    fn resolve_agent_event_none() {
        assert_eq!(
            resolve_agent_event_shorthand(None, None, None, None, None, None),
            None,
        );
    }

    #[test]
    fn resolve_agent_event_agent_without_event() {
        assert_eq!(
            resolve_agent_event_shorthand(Some("claude-code".into()), None, None, None, None, None),
            None,
        );
    }

    // -----------------------------------------------------------------------
    // SessionIdFlags — all agent variants
    // -----------------------------------------------------------------------

    #[test]
    fn session_id_flags_codex() {
        let flags = SessionIdFlags {
            codex: Some("sess-codex".into()),
            ..Default::default()
        };
        assert_eq!(flags.agent_type(), Some(AgentType::Codex));
        assert_eq!(flags.external_session_id(), Some("sess-codex"));
        assert!(flags.is_set());
    }

    #[test]
    fn session_id_flags_cursor() {
        let flags = SessionIdFlags {
            cursor: Some("sess-cursor".into()),
            ..Default::default()
        };
        assert_eq!(flags.agent_type(), Some(AgentType::Cursor));
        assert_eq!(flags.external_session_id(), Some("sess-cursor"));
    }

    #[test]
    fn session_id_flags_gemini() {
        let flags = SessionIdFlags {
            gemini: Some("sess-gemini".into()),
            ..Default::default()
        };
        assert_eq!(flags.agent_type(), Some(AgentType::Gemini));
        assert_eq!(flags.external_session_id(), Some("sess-gemini"));
    }

    #[test]
    fn session_id_flags_session_uuid() {
        let flags = SessionIdFlags {
            claude: Some("ext-42".into()),
            ..Default::default()
        };
        let uuid = flags.session_uuid().expect("should produce uuid");
        let direct = AikiSession::generate_uuid(AgentType::ClaudeCode, "ext-42");
        assert_eq!(uuid, direct);
    }

    #[test]
    fn session_id_flags_session_uuid_none() {
        let flags = SessionIdFlags::default();
        assert!(flags.session_uuid().is_none());
    }

    // -----------------------------------------------------------------------
    // AgentFilterFlags — all agent variants
    // -----------------------------------------------------------------------

    #[test]
    fn agent_filter_flags_claude() {
        let flags = AgentFilterFlags {
            claude: true,
            ..Default::default()
        };
        assert_eq!(flags.agent_type(), Some(AgentType::ClaudeCode));
        assert_eq!(flags.external_session_id(), None);
        assert!(flags.is_set());
    }

    #[test]
    fn agent_filter_flags_cursor() {
        let flags = AgentFilterFlags {
            cursor: true,
            ..Default::default()
        };
        assert_eq!(flags.agent_type(), Some(AgentType::Cursor));
    }

    #[test]
    fn agent_filter_flags_gemini() {
        let flags = AgentFilterFlags {
            gemini: true,
            ..Default::default()
        };
        assert_eq!(flags.agent_type(), Some(AgentType::Gemini));
    }

    #[test]
    fn agent_filter_flags_none() {
        let flags = AgentFilterFlags::default();
        assert_eq!(flags.agent_type(), None);
        assert!(!flags.is_set());
    }

    #[test]
    fn agent_filter_flags_agent_unknown_string() {
        let flags = AgentFilterFlags {
            agent: Some("no-such-agent".into()),
            ..Default::default()
        };
        assert_eq!(flags.agent_type(), None);
        assert!(matches!(
            flags.validate(),
            Err(AikiError::UnknownAgentType(_))
        ));
    }

    #[test]
    fn agent_filter_flags_validate_known_agent() {
        let flags = AgentFilterFlags {
            agent: Some("cursor".into()),
            ..Default::default()
        };
        assert!(flags.validate().is_ok());
    }

    #[test]
    fn agent_filter_flags_validate_no_agent() {
        let flags = AgentFilterFlags::default();
        assert!(flags.validate().is_ok());
    }

    // -----------------------------------------------------------------------
    // AgentFilterFlags — agent_types() stackable OR filter
    // -----------------------------------------------------------------------

    #[test]
    fn agent_filter_types_single() {
        let flags = AgentFilterFlags {
            claude: true,
            ..Default::default()
        };
        assert_eq!(flags.agent_types(), vec![AgentType::ClaudeCode]);
    }

    #[test]
    fn agent_filter_types_multiple() {
        let flags = AgentFilterFlags {
            claude: true,
            codex: true,
            ..Default::default()
        };
        assert_eq!(
            flags.agent_types(),
            vec![AgentType::ClaudeCode, AgentType::Codex]
        );
    }

    #[test]
    fn agent_filter_types_all_four() {
        let flags = AgentFilterFlags {
            claude: true,
            codex: true,
            cursor: true,
            gemini: true,
            ..Default::default()
        };
        assert_eq!(
            flags.agent_types(),
            vec![
                AgentType::ClaudeCode,
                AgentType::Codex,
                AgentType::Cursor,
                AgentType::Gemini,
            ]
        );
    }

    #[test]
    fn agent_filter_types_none() {
        let flags = AgentFilterFlags::default();
        assert!(flags.agent_types().is_empty());
    }

    #[test]
    fn agent_filter_types_agent_string_fallback() {
        let flags = AgentFilterFlags {
            agent: Some("cursor".into()),
            ..Default::default()
        };
        assert_eq!(flags.agent_types(), vec![AgentType::Cursor]);
    }

    #[test]
    fn agent_filter_types_agent_unknown_string() {
        let flags = AgentFilterFlags {
            agent: Some("no-such-agent".into()),
            ..Default::default()
        };
        assert!(flags.agent_types().is_empty());
    }

    // -----------------------------------------------------------------------
    // TaskAgentFlags — resolve() dual-mode behaviour
    // -----------------------------------------------------------------------

    #[test]
    fn task_agent_flags_codex_no_session() {
        let flags = TaskAgentFlags {
            codex: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(flags.agent_type(), Some(AgentType::Codex));
        assert_eq!(flags.external_session_id(), None);
        assert!(matches!(
            flags.resolve(),
            Some(TaskAgentFilter::Assignee(Assignee::Agent(AgentType::Codex)))
        ));
    }

    #[test]
    fn task_agent_flags_cursor_with_session() {
        let flags = TaskAgentFlags {
            cursor: Some("ext-cursor-1".into()),
            ..Default::default()
        };
        assert_eq!(flags.agent_type(), Some(AgentType::Cursor));
        assert_eq!(flags.external_session_id(), Some("ext-cursor-1"));
        match flags.resolve() {
            Some(TaskAgentFilter::Session(uuid)) => {
                let expected = AikiSession::generate_uuid(AgentType::Cursor, "ext-cursor-1");
                assert_eq!(uuid, expected);
            }
            other => panic!("expected Session, got {:?}", other),
        }
    }

    #[test]
    fn task_agent_flags_session_uuid_roundtrip() {
        let flags = TaskAgentFlags {
            claude: Some("my-ext-id".into()),
            ..Default::default()
        };
        let uuid_via_flags = flags.session_uuid().expect("should produce uuid");
        let uuid_direct = AikiSession::generate_uuid(AgentType::ClaudeCode, "my-ext-id");
        assert_eq!(uuid_via_flags, uuid_direct);
    }

    // -----------------------------------------------------------------------
    // resolve_agent_shorthand — coverage for all flags
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_shorthand_cursor() {
        assert_eq!(
            resolve_agent_shorthand(None, false, false, true, false),
            Some(AgentType::Cursor)
        );
    }

    #[test]
    fn resolve_shorthand_gemini() {
        assert_eq!(
            resolve_agent_shorthand(None, false, false, false, true),
            Some(AgentType::Gemini)
        );
    }

    // -----------------------------------------------------------------------
    // resolve_agent_event_shorthand — remaining variants
    // -----------------------------------------------------------------------

    #[test]
    fn resolve_agent_event_cursor() {
        assert_eq!(
            resolve_agent_event_shorthand(None, None, None, None, Some("FileEdit".into()), None),
            Some((AgentType::Cursor, "FileEdit".into()))
        );
    }

    #[test]
    fn resolve_agent_event_gemini() {
        assert_eq!(
            resolve_agent_event_shorthand(None, None, None, None, None, Some("TurnEnd".into())),
            Some((AgentType::Gemini, "TurnEnd".into()))
        );
    }

    #[test]
    fn resolve_agent_event_shorthand_takes_precedence() {
        // Shorthand --claude with explicit --agent/--event should use shorthand
        assert_eq!(
            resolve_agent_event_shorthand(
                Some("cursor".into()),
                Some("ignored".into()),
                Some("SessionStart".into()),
                None,
                None,
                None,
            ),
            Some((AgentType::ClaudeCode, "SessionStart".into()))
        );
    }

    #[test]
    fn resolve_agent_event_event_without_agent() {
        assert_eq!(
            resolve_agent_event_shorthand(None, Some("SessionStart".into()), None, None, None, None),
            None,
        );
    }
}
