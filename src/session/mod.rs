pub mod isolation;
pub mod turn_state;

use crate::cache::debug_log;
use crate::error::{AikiError, Result};
use crate::global;
use crate::provenance::record::{AgentType, DetectionMethod};
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use sysinfo::{Pid, ProcessesToUpdate, System};

/// Session mode - background vs interactive
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SessionMode {
    /// Background session created by `aiki task run`
    Background,
    /// Interactive session (user working directly in agent)
    Interactive,
}

impl SessionMode {
    /// Convert to string for metadata storage
    pub fn to_string(&self) -> &'static str {
        match self {
            SessionMode::Background => "background",
            SessionMode::Interactive => "interactive",
        }
    }

    /// Parse from metadata string
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "background" => Some(SessionMode::Background),
            "interactive" => Some(SessionMode::Interactive),
            _ => None,
        }
    }
}

/// Reason a session was cleaned up
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionCleanupReason {
    /// Parent process no longer alive
    PidDead,
}

/// Session file handle for atomic file operations
#[derive(Debug, Clone)]
pub struct AikiSessionFile {
    path: PathBuf,
    session: AikiSession,
}

impl AikiSessionFile {
    /// Create a new session file handle
    ///
    /// Session files are stored globally at `$AIKI_HOME/sessions/{uuid}`.
    #[must_use]
    pub fn new(session: &AikiSession) -> Self {
        let path = global::global_sessions_dir().join(session.uuid());
        Self {
            path,
            session: session.clone(),
        }
    }

    /// Atomically create the session file with metadata
    ///
    /// Uses `create_new()` (O_EXCL) for atomic file creation.
    /// Returns `Ok(true)` if created, `Ok(false)` if already exists.
    pub fn create(&self) -> Result<bool> {
        use std::fs::OpenOptions;
        use std::io::Write;

        // Build metadata from session in [aiki]...[/aiki] format
        let started_at = Utc::now();

        let mut metadata = format!(
            "[aiki]\nagent={}\nexternal_session_id={}\nsession_id={}\nstarted_at={}\n",
            self.session.agent_type().to_metadata_string(),
            self.session.external_id(),
            self.session.uuid(),
            started_at.to_rfc3339(),
        );

        // Add agent_version if available
        if let Some(version) = self.session.agent_version() {
            metadata.push_str(&format!("agent_version={}\n", version));
        }

        // Add mode (background vs interactive)
        metadata.push_str(&format!("mode={}\n", self.session.mode().to_string()));

        // Add parent_pid for PID-based session detection
        if let Some(pid) = self.session.parent_pid() {
            metadata.push_str(&format!("parent_pid={}\n", pid));
        }

        // Add task if this is a task-driven session (spawned by aiki plan or aiki task run --async)
        if let Some(task_id) = self.session.task() {
            metadata.push_str(&format!("task={}\n", task_id));
        }

        metadata.push_str("[/aiki]\n");

        // Create sessions directory if it doesn't exist
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                AikiError::Other(anyhow::anyhow!(
                    "Failed to create sessions directory: {}",
                    e
                ))
            })?;
        }

        // Try to create file atomically with O_EXCL flag
        match OpenOptions::new()
            .write(true)
            .create_new(true) // O_EXCL: fails if file exists (atomic check-and-create)
            .open(&self.path)
        {
            Ok(mut file) => {
                // File created successfully - write metadata
                file.write_all(metadata.as_bytes()).map_err(|e| {
                    AikiError::Other(anyhow::anyhow!("Failed to write session file: {}", e))
                })?;

                Ok(true) // File created
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // File already exists - this is expected for concurrent creates
                Ok(false) // File already exists
            }
            Err(e) => {
                // Other error - propagate
                Err(AikiError::Other(anyhow::anyhow!(
                    "Failed to create session file: {}",
                    e
                )))
            }
        }
    }

    /// Delete the session file
    pub fn delete(&self) -> Result<()> {
        // Try to remove the file; ignore if it doesn't exist
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AikiError::Other(anyhow::anyhow!(
                "Failed to delete session file: {}",
                e
            ))),
        }
    }

    /// Read agent_version from existing session file
    ///
    /// Returns None if file doesn't exist or agent_version field not found.
    /// This allows subsequent events to read cached version without re-detecting.
    #[allow(dead_code)] // Part of AikiSessionFile API
    pub fn read_agent_version(&self) -> Option<String> {
        fs::read_to_string(&self.path).ok().and_then(|content| {
            // Parse [aiki]...[/aiki] block for agent_version field
            content
                .lines()
                .find(|line| line.starts_with("agent_version="))
                .and_then(|line| line.strip_prefix("agent_version="))
                .map(|v| v.to_string())
        })
    }

    /// Update the session file with parent_pid if not already present.
    ///
    /// This is called when we discover the agent PID via `find_ancestor_by_name`
    /// after the session was created without a PID (e.g., Codex via OTEL).
    /// Subsequent lookups can then use fast PID-based matching.
    pub fn update_parent_pid(&self, pid: u32) -> Result<()> {
        use std::io::Write;

        let content = match fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(AikiError::Other(anyhow::anyhow!(
                    "Failed to read session file: {}",
                    e
                )))
            }
        };

        // Check if parent_pid already exists
        if content.lines().any(|line| line.starts_with("parent_pid=")) {
            return Ok(()); // Already has PID, no update needed
        }

        // Insert parent_pid before [/aiki] closing tag
        let new_content = content.replace("[/aiki]\n", &format!("parent_pid={}\n[/aiki]\n", pid));

        // Write atomically via temp file
        let tmp_path = self.path.with_extension("tmp");
        let mut file = fs::File::create(&tmp_path)
            .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to create temp file: {}", e)))?;
        file.write_all(new_content.as_bytes())
            .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to write temp file: {}", e)))?;
        file.sync_all()
            .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to sync temp file: {}", e)))?;

        fs::rename(&tmp_path, &self.path)
            .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to rename temp file: {}", e)))?;

        Ok(())
    }

    /// Read all repository IDs from the session file
    ///
    /// Returns a list of repo IDs (from `repo=` lines). Empty if file doesn't exist
    /// or has no repo fields.
    pub fn read_repos(&self) -> Vec<String> {
        match fs::read_to_string(&self.path) {
            Ok(content) => content
                .lines()
                .filter_map(|line| line.strip_prefix("repo="))
                .map(|s| s.to_string())
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Add a repository ID to the session file if not already present
    ///
    /// This tracks which repositories the session has touched.
    /// Repos are identified by stable IDs (root commit hash or local-*).
    pub fn add_repo(&self, repo_id: &str) -> Result<()> {
        use std::io::Write;

        let content = match fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => {
                return Err(AikiError::Other(anyhow::anyhow!(
                    "Failed to read session file: {}",
                    e
                )))
            }
        };

        // Check if this repo is already recorded
        let repo_line = format!("repo={}", repo_id);
        if content.lines().any(|line| line == repo_line) {
            return Ok(()); // Already recorded
        }

        // Insert repo before [/aiki] closing tag
        let new_content = content.replace("[/aiki]\n", &format!("{}\n[/aiki]\n", repo_line));

        // Write atomically via temp file
        let tmp_path = self.path.with_extension("tmp");
        let mut file = fs::File::create(&tmp_path)
            .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to create temp file: {}", e)))?;
        file.write_all(new_content.as_bytes())
            .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to write temp file: {}", e)))?;
        file.sync_all()
            .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to sync temp file: {}", e)))?;

        fs::rename(&tmp_path, &self.path)
            .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to rename temp file: {}", e)))?;

        Ok(())
    }

    /// Check if this session file exists
    pub fn exists(&self) -> bool {
        self.path.exists()
    }
}

/// Aiki Session tracking
///
/// Tracks an active AI agent session with deterministic ID generation.
/// Session IDs are deterministic hashes of (agent_type, external_session_id)
/// to ensure consistent session identification across multiple events.
///
/// Session metadata (client info, agent version, integration type) is stored here
/// to avoid duplication across events. Hook-based detection will have None for client info,
/// while ACP-based detection populates these fields from Initialize messages.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct AikiSession {
    /// The deterministic UUID v5 hash
    uuid: String,
    /// The agent type (claude, cursor, etc.)
    agent_type: AgentType,
    /// Agent version - e.g., "0.10.6" (from ACP InitializeResponse or hook detection)
    agent_version: Option<String>,
    /// The external session ID provided by the agent
    external_id: String,
    /// Client (IDE) name - e.g., "zed", "neovim" (from ACP InitializeRequest)
    client_name: Option<String>,
    /// Client (IDE) version - e.g., "0.213.3" (from ACP InitializeRequest)
    client_version: Option<String>,
    /// Integration type - how Aiki is integrated with the agent (Hook vs ACP)
    detection_method: DetectionMethod,
    /// Session mode - background (task runner) vs interactive (user-driven)
    mode: SessionMode,
    /// Parent process ID of the agent (for PID-based session detection)
    ///
    /// In hook mode, this is the parent of the hook process (the agent).
    /// In ACP mode, this is the `agent_pid` from the session/start message.
    /// Used to match bash commands back to their originating session.
    parent_pid: Option<u32>,
    /// Task ID driving this session (if any)
    ///
    /// Set from AIKI_TASK environment variable.
    /// Used for task-driven sessions spawned by `aiki plan` or `aiki task run --async`.
    /// When the driving task closes, interactive sessions auto-end.
    task: Option<String>,
}

impl AikiSession {
    /// Namespace UUID for Aiki session IDs (fixed UUID v5 namespace)
    /// Generated once with: uuidgen -> 6ba7b810-9dad-11d1-80b4-00c04fd430c8
    const NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
        0x6b, 0xa7, 0xb8, 0x10, 0x9d, 0xad, 0x11, 0xd1, 0x80, 0xb4, 0x00, 0xc0, 0x4f, 0xd4, 0x30,
        0xc8,
    ]);

    /// Create a new Aiki session ID from agent type and external session ID
    ///
    /// Uses UUID v5 (SHA-1 hash) for deterministic ID generation, truncated to
    /// 8 hex characters:
    /// ```text
    /// session_id = UUIDv5(NAMESPACE, "{agent_type}:{external_session_id}")[..8]
    /// ```
    ///
    /// # Arguments
    /// * `agent_type` - The AI agent type (Claude, Cursor, etc.)
    /// * `external_id` - The session ID provided by the agent/IDE
    /// * `agent_version` - Optional agent version (e.g., "0.10.6")
    /// * `detection_method` - How Aiki is integrated (Hook, ACP, Unknown)
    ///
    /// # Examples
    /// ```
    /// use aiki::session::AikiSession;
    /// use aiki::provenance::{AgentType, DetectionMethod};
    ///
    /// // Hook-based (no agent version)
    /// let session = AikiSession::new(
    ///     AgentType::ClaudeCode,
    ///     "claude-session-abc123",
    ///     None::<&str>,
    ///     DetectionMethod::Hook
    /// );
    ///
    /// // ACP-based (with agent version)
    /// let session = AikiSession::new(
    ///     AgentType::ClaudeCode,
    ///     "claude-session-abc123",
    ///     Some("0.10.6"),
    ///     DetectionMethod::ACP
    /// );
    ///
    /// // Same inputs produce same UUID (deterministic)
    /// let session2 = AikiSession::new(
    ///     AgentType::ClaudeCode,
    ///     "claude-session-abc123",
    ///     None::<&str>,
    ///     DetectionMethod::Hook
    /// );
    /// assert_eq!(session.uuid(), session2.uuid());
    /// ```
    #[must_use]
    pub fn new(
        agent_type: AgentType,
        external_id: impl Into<String>,
        agent_version: Option<impl Into<String>>,
        detection_method: DetectionMethod,
        mode: SessionMode,
    ) -> Self {
        let external_id = external_id.into();
        let uuid = Self::generate_uuid(agent_type, &external_id);

        Self {
            uuid,
            agent_type,
            external_id,
            client_name: None,
            client_version: None,
            agent_version: agent_version.map(|v| v.into()),
            detection_method,
            mode,
            parent_pid: None,
            task: None,
        }
    }

    /// Reconstruct a session from a pre-computed UUID (e.g., from a session file)
    ///
    /// Used when we already have the final UUID and don't need to re-generate it.
    /// This is the case during TTL cleanup when reading session files.
    #[must_use]
    pub fn from_uuid(uuid: String, agent_type: AgentType, mode: SessionMode) -> Self {
        Self {
            uuid,
            agent_type,
            external_id: String::new(),
            client_name: None,
            client_version: None,
            agent_version: None,
            detection_method: DetectionMethod::Unknown,
            mode,
            parent_pid: None,
            task: None,
        }
    }

    /// Create a new session for hook-based detection
    ///
    /// Convenience constructor that automatically sets `DetectionMethod::Hook`
    /// and captures the parent process ID for PID-based session detection.
    /// Mode is determined by `AIKI_SESSION_MODE` env var:
    /// - "background" → Background mode (for `aiki task run --async`)
    /// - anything else → Interactive mode (default, for `aiki plan` and normal sessions)
    ///
    /// # Examples
    /// ```
    /// use aiki::session::AikiSession;
    /// use aiki::provenance::AgentType;
    ///
    /// let session = AikiSession::for_hook(
    ///     AgentType::ClaudeCode,
    ///     "claude-session-abc123",
    ///     Some("0.10.6")
    /// );
    /// assert!(session.parent_pid().is_some());
    /// ```
    #[must_use]
    pub fn for_hook(
        agent_type: AgentType,
        external_id: impl Into<String>,
        agent_version: Option<impl Into<String>>,
    ) -> Self {
        // Capture parent PID - the agent that spawned this hook process
        let parent_pid = get_parent_pid();

        // Determine mode from AIKI_SESSION_MODE env var
        // "background" → Background, anything else → Interactive
        let mode = match std::env::var("AIKI_SESSION_MODE").as_deref() {
            Ok("background") => SessionMode::Background,
            _ => SessionMode::Interactive,
        };

        Self::new(
            agent_type,
            external_id,
            agent_version,
            DetectionMethod::Hook,
            mode,
        )
        .with_parent_pid(parent_pid)
        .with_task_from_env()
    }

    /// Generate a deterministic short ID for a session
    ///
    /// Hashes "{agent_type}:{external_session_id}" with UUID v5, then takes the
    /// first 8 hex characters (32 bits). Collision-safe for <10K concurrent sessions.
    ///
    /// This is useful when you need to compute a session ID without creating
    /// a full AikiSession object (e.g., for cache lookups).
    #[must_use]
    pub fn generate_uuid(agent_type: AgentType, external_id: &str) -> String {
        // Create deterministic hash input: "agent_type:external_session_id"
        let hash_input = format!("{}:{}", agent_type.to_metadata_string(), external_id);

        // Generate UUID v5 (SHA-1 based, deterministic), take first 8 hex chars
        let uuid = uuid::Uuid::new_v5(&Self::NAMESPACE, hash_input.as_bytes());
        uuid.to_string()[..8].to_string()
    }

    /// Add client (IDE) information to the session
    ///
    /// This is typically called when using ACP-based detection, where the client
    /// provides its name and version in the InitializeRequest.
    ///
    /// # Example
    /// ```
    /// use aiki::session::AikiSession;
    /// use aiki::provenance::{AgentType, DetectionMethod};
    ///
    /// let session = AikiSession::new(
    ///     AgentType::ClaudeCode,
    ///     "session-123",
    ///     None::<&str>,
    ///     DetectionMethod::ACP
    /// )
    /// .with_client_info(Some("zed"), Some("0.213.3"));
    /// ```
    #[must_use]
    pub fn with_client_info(
        mut self,
        client_name: Option<impl Into<String>>,
        client_version: Option<impl Into<String>>,
    ) -> Self {
        self.client_name = client_name.map(|n| n.into());
        self.client_version = client_version.map(|v| v.into());
        self
    }

    /// Set the parent process ID for PID-based session detection
    ///
    /// In hook mode, this should be the parent of the hook process (the agent).
    /// In ACP mode, this should be the `agent_pid` from the session/start message.
    ///
    /// # Example
    /// ```
    /// use aiki::session::AikiSession;
    /// use aiki::provenance::{AgentType, DetectionMethod};
    ///
    /// let session = AikiSession::new(
    ///     AgentType::ClaudeCode,
    ///     "session-123",
    ///     None::<&str>,
    ///     DetectionMethod::Hook
    /// )
    /// .with_parent_pid(Some(1234));
    /// ```
    #[must_use]
    pub fn with_parent_pid(mut self, pid: Option<u32>) -> Self {
        self.parent_pid = pid;
        self
    }

    /// Set the task ID driving this session
    ///
    /// Used for sessions spawned by `aiki plan` or `aiki task run --async`.
    #[must_use]
    pub fn with_task(mut self, task_id: Option<String>) -> Self {
        self.task = task_id;
        self
    }

    /// Capture task ID from AIKI_TASK environment variable
    ///
    /// This should be called when creating sessions to check if this session
    /// was spawned by a workflow command (e.g., `aiki plan`, `aiki task run --async`).
    #[must_use]
    pub fn with_task_from_env(self) -> Self {
        let task_id = std::env::var("AIKI_TASK").ok();
        debug_log(|| {
            format!(
                "with_task_from_env: AIKI_TASK={:?}",
                task_id
            )
        });
        self.with_task(task_id)
    }

    /// Get the session UUID as a string
    #[must_use]
    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    /// Get the agent type
    #[must_use]
    pub fn agent_type(&self) -> AgentType {
        self.agent_type
    }

    /// Get the external session ID
    #[must_use]
    pub fn external_id(&self) -> &str {
        &self.external_id
    }

    /// Get the client (IDE) name
    #[must_use]
    pub fn client_name(&self) -> Option<&str> {
        self.client_name.as_deref()
    }

    /// Get the client (IDE) version
    #[must_use]
    pub fn client_version(&self) -> Option<&str> {
        self.client_version.as_deref()
    }

    /// Get the agent version
    #[must_use]
    pub fn agent_version(&self) -> Option<&str> {
        self.agent_version.as_deref()
    }

    /// Get the detection method (integration type)
    #[must_use]
    pub fn detection_method(&self) -> &DetectionMethod {
        &self.detection_method
    }

    /// Get the session mode (background vs interactive)
    #[must_use]
    pub fn mode(&self) -> SessionMode {
        self.mode
    }

    /// Get the parent process ID
    #[must_use]
    pub fn parent_pid(&self) -> Option<u32> {
        self.parent_pid
    }

    /// Get the task ID if this is a task-driven session
    #[must_use]
    pub fn task(&self) -> Option<&str> {
        self.task.as_deref()
    }

    /// Get a session file handle for this session
    ///
    /// Session files are stored globally at `$AIKI_HOME/sessions/{uuid}`.
    #[must_use]
    pub fn file(&self) -> AikiSessionFile {
        AikiSessionFile::new(self)
    }

    /// End this session and clean up its session file
    ///
    /// Deletes the session file from the global sessions directory.
    /// This is called automatically when a SessionEnd event is dispatched.
    pub fn end(&self) -> Result<()> {
        self.file().delete()
    }
}

/// Count active sessions globally
#[allow(dead_code)] // Part of session API
pub fn count_sessions() -> Result<usize> {
    let sessions_dir = global::global_sessions_dir();

    if !sessions_dir.exists() {
        return Ok(0);
    }

    let count = fs::read_dir(&sessions_dir)
        .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to read sessions directory: {}", e)))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .count();

    Ok(count)
}

/// Parsed session info for listing
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: String,
    pub agent: String,
    pub mode: SessionMode,
    pub started_at: String,
    pub parent_pid: Option<u32>,
    pub repos: Vec<String>,
}

/// List all sessions from the global sessions directory
pub fn list_all_sessions() -> Result<Vec<SessionInfo>> {
    let sessions_dir = global::global_sessions_dir();

    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }

    let entries = fs::read_dir(&sessions_dir).map_err(|e| {
        AikiError::Other(anyhow::anyhow!("Failed to read sessions directory: {}", e))
    })?;

    let mut sessions = Vec::new();

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() || path.extension().is_some() {
            continue; // Skip directories and .turn/.tmp files
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let mut agent = String::new();
        let mut session_id = String::new();
        let mut mode: Option<SessionMode> = None;
        let mut started_at = String::new();
        let mut parent_pid: Option<u32> = None;
        let mut repos = Vec::new();

        for line in content.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("agent=") {
                agent = val.to_string();
            } else if let Some(val) = line.strip_prefix("session_id=") {
                session_id = val.to_string();
            } else if let Some(val) = line.strip_prefix("aiki_session_id=") {
                if session_id.is_empty() {
                    session_id = val.to_string();
                }
            } else if let Some(val) = line.strip_prefix("mode=") {
                mode = SessionMode::from_str(val);
            } else if let Some(val) = line.strip_prefix("started_at=") {
                started_at = val.to_string();
            } else if let Some(val) = line.strip_prefix("parent_pid=") {
                parent_pid = val.parse().ok();
            } else if let Some(val) = line.strip_prefix("repo=") {
                repos.push(val.to_string());
            }
        }

        // Use filename as session_id fallback
        if session_id.is_empty() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                session_id = name.to_string();
            } else {
                continue;
            }
        }

        sessions.push(SessionInfo {
            session_id,
            agent,
            mode: mode.unwrap_or(SessionMode::Interactive), // Default to interactive for old sessions
            started_at,
            parent_pid,
            repos,
        });
    }

    // Sort by started_at descending (newest first)
    sessions.sort_by(|a, b| b.started_at.cmp(&a.started_at));

    Ok(sessions)
}

/// Result of detecting the current session context
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Part of session API
pub enum SessionContext {
    /// No active sessions - likely running from human terminal
    NoSession,
    /// Exactly one active session with this agent type
    SingleSession(AgentType),
    /// Multiple active sessions - ambiguous context
    MultipleSessions,
}

/// Detect the current session context
///
/// Returns detailed information about active sessions:
/// - NoSession: No session files found (human terminal)
/// - SingleSession(agent): Exactly one active session
/// - MultipleSessions: More than one session active (ambiguous)
#[allow(dead_code)] // Part of session API
pub fn get_session_context(repo_path: impl AsRef<Path>) -> SessionContext {
    let sessions_dir = repo_path.as_ref().join(".aiki/sessions");

    if !sessions_dir.exists() {
        return SessionContext::NoSession;
    }

    let session_files: Vec<_> = match fs::read_dir(&sessions_dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_file())
            .collect(),
        Err(_) => return SessionContext::NoSession,
    };

    match session_files.len() {
        0 => SessionContext::NoSession,
        1 => {
            // Try to read agent type from the session file
            if let Ok(content) = fs::read_to_string(session_files[0].path()) {
                for line in content.lines() {
                    let line = line.trim();
                    if let Some(agent_str) = line.strip_prefix("agent=") {
                        if let Some(agent_type) = AgentType::from_str(agent_str) {
                            return SessionContext::SingleSession(agent_type);
                        }
                    }
                }
            }
            // Couldn't parse agent type, treat as no session
            SessionContext::NoSession
        }
        _ => SessionContext::MultipleSessions,
    }
}

/// Get the agent type from the current active session, if any
///
/// Returns the agent type if there's exactly one active session.
/// Returns None if there are no sessions or multiple sessions.
#[allow(dead_code)] // Part of session API
pub fn get_current_agent_type(repo_path: impl AsRef<Path>) -> Option<AgentType> {
    match get_session_context(repo_path) {
        SessionContext::SingleSession(agent) => Some(agent),
        _ => None,
    }
}

/// Check if a specific session is active
///
/// Uses deterministic UUID generation to check if the session file exists.
/// This allows precise session lookup even when multiple sessions are active.
#[allow(dead_code)] // Part of session API
/// Check if a session is active by looking for its session file
pub fn has_active_session(agent_type: AgentType, external_session_id: &str) -> bool {
    let uuid = AikiSession::generate_uuid(agent_type, external_session_id);
    let session_file = global::global_sessions_dir().join(&uuid);
    session_file.exists()
}

/// End a session and clean up its session file
#[allow(dead_code)] // Part of session API
pub fn end_session(
    agent_type: AgentType,
    external_session_id: impl Into<String>,
    detection_method: DetectionMethod,
    mode: SessionMode,
) -> Result<()> {
    let session = AikiSession::new(
        agent_type,
        external_session_id,
        None::<&str>,
        detection_method,
        mode,
    );
    session.file().delete()?;
    Ok(())
}

// ============================================================================
// PID-based session detection
// ============================================================================

/// Get the parent process ID
///
/// Uses `libc::getppid()` directly — a syscall that always succeeds.
/// Previous implementation used sysinfo's process enumeration, which could
/// intermittently fail on macOS (returning None) when the system was under
/// load or due to timing issues with `sysctl`-based process listing.
#[must_use]
pub fn get_parent_pid() -> Option<u32> {
    // SAFETY: getppid() is always safe and always returns a valid PID
    let ppid = unsafe { libc::getppid() };
    if ppid > 0 {
        Some(ppid as u32)
    } else {
        None // Should never happen — even init has ppid=0
    }
}

/// Get all ancestor PIDs from the current process up to init
///
/// Returns a HashSet for O(1) lookup when matching against session files.
pub fn get_ancestor_pids() -> HashSet<u32> {
    let mut ancestors = HashSet::new();
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);

    let mut pid = Pid::from_u32(std::process::id());

    loop {
        let Some(process) = system.process(pid) else {
            break;
        };

        let Some(parent_pid) = process.parent() else {
            break;
        };

        // Prevent infinite loop
        if parent_pid == pid {
            break;
        }

        ancestors.insert(parent_pid.as_u32());
        pid = parent_pid;
    }

    ancestors
}

/// Find an ancestor process by name
///
/// Walks up the process tree from the current process, looking for a process
/// whose name contains the given substring (case-insensitive).
///
/// This is useful for detecting agent processes (like "codex") that don't
/// provide their PID via other means (e.g., OTEL attributes).
///
/// Returns the PID of the first matching ancestor, or None if not found.
#[must_use]
pub fn find_ancestor_by_name(name: &str) -> Option<u32> {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);

    let name_lower = name.to_lowercase();
    let mut pid = Pid::from_u32(std::process::id());

    loop {
        let Some(process) = system.process(pid) else {
            break;
        };

        let Some(parent_pid) = process.parent() else {
            break;
        };

        // Prevent infinite loop
        if parent_pid == pid {
            break;
        }

        // Check parent process name
        if let Some(parent_process) = system.process(parent_pid) {
            let process_name = parent_process.name().to_string_lossy().to_lowercase();
            if process_name.contains(&name_lower) {
                return Some(parent_pid.as_u32());
            }
        }

        pid = parent_pid;
    }

    None
}

/// Result of PID-based session lookup
#[derive(Debug, Clone)]
#[allow(dead_code)] // Fields are part of SessionMatch API
pub struct SessionMatch {
    /// The agent type from the session file
    pub agent_type: AgentType,
    /// The external session ID from the session file
    pub external_session_id: String,
    /// The Aiki session UUID (deterministic, stable identifier)
    pub session_id: String,
}

/// Find an active session by matching parent_pid against the current process's ancestors
///
/// This is the core function for PID-based session detection:
/// 1. Get all ancestor PIDs of the current process
/// 2. Scan session files in global sessions directory
/// 3. Find sessions whose parent_pid matches one of our ancestors
/// 4. If multiple match, prefer the most recently *active* session
///    (queries JJ for latest event timestamp)
///
/// The `jj_cwd` parameter is needed for querying JJ to get latest event timestamps.
///
/// Returns None if no matching session found (human terminal mode).
pub fn find_session_by_ancestor_pid(jj_cwd: impl AsRef<Path>) -> Option<SessionMatch> {
    let jj_cwd = jj_cwd.as_ref();
    let sessions_dir = global::global_sessions_dir();

    if !sessions_dir.exists() {
        return None;
    }

    let ancestor_pids = get_ancestor_pids();
    if ancestor_pids.is_empty() {
        return None;
    }

    let entries = match fs::read_dir(&sessions_dir) {
        Ok(e) => e,
        Err(_) => return None,
    };

    // Track best match with its last-activity time
    let mut best_match: Option<(SessionMatch, std::time::SystemTime)> = None;

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        // Skip non-session files (e.g., .turn state files)
        if path.extension().is_some() {
            continue;
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Parse session file fields
        let mut parent_pid: Option<u32> = None;
        let mut agent_type: Option<AgentType> = None;
        let mut external_session_id: Option<String> = None;
        let mut aiki_session_id: Option<String> = None;

        for line in content.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("parent_pid=") {
                parent_pid = val.parse().ok();
            } else if let Some(val) = line.strip_prefix("agent=") {
                agent_type = AgentType::from_str(val);
            } else if let Some(val) = line.strip_prefix("external_session_id=") {
                external_session_id = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("session_id=") {
                aiki_session_id = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("aiki_session_id=") {
                if aiki_session_id.is_none() {
                    aiki_session_id = Some(val.to_string());
                }
            }
        }

        // Check if this session's parent_pid matches one of our ancestors
        if let Some(pid) = parent_pid {
            if ancestor_pids.contains(&pid) {
                if let (Some(agent), Some(ext_id), Some(aiki_id)) =
                    (agent_type, external_session_id, aiki_session_id.clone())
                {
                    let candidate = SessionMatch {
                        agent_type: agent,
                        external_session_id: ext_id,
                        session_id: aiki_id.clone(),
                    };

                    // Query JJ for latest event timestamp for this session
                    let last_activity = query_latest_event(jj_cwd, &aiki_id)
                        .ok()
                        .flatten()
                        .map(|dt| dt.into())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

                    let should_replace = match &best_match {
                        None => true,
                        Some((_, prev_time)) => last_activity > *prev_time,
                    };

                    if should_replace {
                        best_match = Some((candidate, last_activity));
                    }
                }
            }
        }
    }

    best_match.map(|(m, _)| m)
}

/// Info about a task-driven session (spawned by `aiki plan` or `aiki task run --async`)
#[derive(Debug, Clone)]
pub struct TaskSessionInfo {
    /// Session ID
    pub session_id: String,
    /// Task ID driving this session
    pub task_id: String,
    /// Process ID of the agent (for termination)
    pub pid: u32,
    /// Session mode (interactive or background)
    pub mode: SessionMode,
}

/// Find a task-driven session by task ID
///
/// Scans session files for one with `task=<task_id>` and returns
/// the session info including the PID for process termination.
///
/// Returns None if no matching session found.
pub fn find_task_session(task_id: &str) -> Option<TaskSessionInfo> {
    let sessions_dir = global::global_sessions_dir();

    debug_log(|| {
        format!(
            "find_task_session: looking for task={} in {:?}",
            task_id, sessions_dir
        )
    });

    if !sessions_dir.exists() {
        debug_log(|| "find_task_session: sessions dir does not exist".to_string());
        return None;
    }

    let entries = match fs::read_dir(&sessions_dir) {
        Ok(e) => e,
        Err(_) => return None,
    };

    let mut sessions_checked = 0;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        // Skip non-session files (e.g., .turn state files)
        if path.extension().is_some() {
            continue;
        }

        sessions_checked += 1;
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Parse session file fields
        let mut session_task: Option<String> = None;
        let mut parent_pid: Option<u32> = None;
        let mut session_id: Option<String> = None;
        let mut mode: Option<SessionMode> = None;

        for line in content.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("task=") {
                session_task = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("parent_pid=") {
                parent_pid = val.parse().ok();
            } else if let Some(val) = line.strip_prefix("session_id=") {
                session_id = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("mode=") {
                mode = SessionMode::from_str(val);
            }
        }

        // Check if this session matches our task
        if let (Some(st), Some(pid), Some(sid)) =
            (session_task.clone(), parent_pid, session_id.clone())
        {
            debug_log(|| {
                format!(
                    "find_task_session: session {} has task={}, looking for {}",
                    sid, st, task_id
                )
            });
            if st == task_id {
                debug_log(|| format!("find_task_session: FOUND match! pid={}", pid));
                return Some(TaskSessionInfo {
                    session_id: sid,
                    task_id: st,
                    pid,
                    mode: mode.unwrap_or(SessionMode::Interactive),
                });
            }
        }
    }

    debug_log(|| {
        format!(
            "find_task_session: checked {} sessions, no match found",
            sessions_checked
        )
    });
    None
}

/// Find an active session by agent type, used when PID matching fails but we
/// detect we're running under a specific agent (via `find_ancestor_by_name`).
///
/// This is a fallback for agents like Codex that don't provide their PID via OTEL.
/// Matches sessions by:
/// 1. Agent type (must match)
/// 2. Most recently active session (by JJ event timestamp)
///
/// The `jj_cwd` parameter is needed for querying JJ to get latest event timestamps.
///
/// Returns None if no matching session found.
pub fn find_session_by_agent_type(
    jj_cwd: impl AsRef<Path>,
    target_agent: AgentType,
) -> Option<SessionMatch> {
    let jj_cwd = jj_cwd.as_ref();
    let sessions_dir = global::global_sessions_dir();

    if !sessions_dir.exists() {
        return None;
    }

    let entries = match fs::read_dir(&sessions_dir) {
        Ok(e) => e,
        Err(_) => return None,
    };

    // Track best match with its last-activity time
    let mut best_match: Option<(SessionMatch, std::time::SystemTime)> = None;

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        // Skip non-session files (e.g., .turn state files)
        if path.extension().is_some() {
            continue;
        }

        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Parse session file fields
        let mut agent_type: Option<AgentType> = None;
        let mut external_session_id: Option<String> = None;
        let mut aiki_session_id: Option<String> = None;

        for line in content.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("agent=") {
                agent_type = AgentType::from_str(val);
            } else if let Some(val) = line.strip_prefix("external_session_id=") {
                external_session_id = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("session_id=") {
                aiki_session_id = Some(val.to_string());
            } else if let Some(val) = line.strip_prefix("aiki_session_id=") {
                if aiki_session_id.is_none() {
                    aiki_session_id = Some(val.to_string());
                }
            }
        }

        // Check if this session matches the target agent type
        if let Some(agent) = agent_type {
            if agent == target_agent {
                if let (Some(ext_id), Some(aiki_id)) = (external_session_id, aiki_session_id) {
                    let candidate = SessionMatch {
                        agent_type: agent,
                        external_session_id: ext_id,
                        session_id: aiki_id.clone(),
                    };

                    // Query JJ for latest event timestamp for this session
                    let last_activity = query_latest_event(jj_cwd, &aiki_id)
                        .ok()
                        .flatten()
                        .map(|dt| dt.into())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

                    let should_replace = match &best_match {
                        None => true,
                        Some((_, prev_time)) => last_activity > *prev_time,
                    };

                    if should_replace {
                        best_match = Some((candidate, last_activity));
                    }
                }
            }
        }
    }

    best_match.map(|(m, _)| m)
}

/// Find a session, trying PID-based matching first, then agent-type matching.
///
/// This is the main entry point for session detection:
/// 1. Try `find_session_by_ancestor_pid` (works for Claude Code, Cursor, etc.)
/// 2. If that fails, check `find_ancestor_by_name("codex")` for Codex
/// 3. If Codex detected, use `find_session_by_agent_type` as fallback
/// 4. If found via fallback, update session file with discovered PID for future lookups
/// 5. Final fallback: find most-recent session that includes the current repo ID
///
/// The `jj_cwd` parameter is needed for querying JJ to get latest event timestamps,
/// and is also used to derive the repo ID for fallback filtering.
///
/// Returns None if no matching session found (human terminal mode).
pub fn find_active_session(jj_cwd: impl AsRef<Path>) -> Option<SessionMatch> {
    let jj_cwd = jj_cwd.as_ref();

    // First try PID-based matching (works for most agents)
    if let Some(session) = find_session_by_ancestor_pid(jj_cwd) {
        return Some(session);
    }

    // Check if we're running under Codex (which doesn't provide PID via OTEL)
    if let Some(codex_pid) = find_ancestor_by_name("codex") {
        if let Some(session) = find_session_by_agent_type(jj_cwd, AgentType::Codex) {
            // Update session file with discovered PID for future fast lookups
            let session_file_path = global::global_sessions_dir().join(&session.session_id);
            let session_file = AikiSessionFile {
                path: session_file_path,
                session: AikiSession::new(
                    session.agent_type,
                    &session.external_session_id,
                    None::<&str>,
                    DetectionMethod::Hook,
                    SessionMode::Interactive,
                ),
            };
            if let Err(e) = session_file.update_parent_pid(codex_pid) {
                // Log but don't fail - session was still found
                crate::cache::debug_log(|| {
                    format!("Failed to update session file with PID: {}", e)
                });
            }
            return Some(session);
        }
    }

    // Final fallback: find most-recent session that includes the current repo ID
    // This handles cases where PID detection fails but the session is working in this repo
    if let Some(session) = find_session_by_repo(jj_cwd) {
        return Some(session);
    }

    // No session found
    None
}

/// Find a session that includes the given repo in its repo list
///
/// Returns the most recent (by JJ activity) session that has the repo_id
/// from the given path in its `repo` field list.
///
/// **Safety:** Only matches sessions that do NOT have a `parent_pid` set.
/// Sessions with `parent_pid` belong to specific processes and should be
/// matched by `find_session_by_ancestor_pid` instead. If PID-based detection
/// didn't match them, we're not that process and must not claim their identity.
/// This prevents cross-session preemption when multiple agents share a repo.
fn find_session_by_repo(repo_path: impl AsRef<Path>) -> Option<SessionMatch> {
    use crate::repos;

    let repo_path = repo_path.as_ref();

    // Compute repo ID for the current directory
    let target_repo_id = match repos::compute_repo_id(repo_path) {
        Ok(id) => id,
        Err(_) => return None, // Can't determine repo ID
    };

    let sessions_dir = global::global_sessions_dir();
    let entries = fs::read_dir(&sessions_dir).ok()?;

    let mut matching_sessions: Vec<SessionMatch> = Vec::new();

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();

        // Skip directories
        if path.is_dir() {
            continue;
        }

        // Parse session info first (gives us parent_pid and other fields)
        let Some(info) = parse_session_file(&path) else {
            continue;
        };

        // Skip sessions that have a parent_pid — they belong to specific processes
        // and should only be matched via PID-based detection. Matching them here
        // would risk claiming another agent's identity and preempting their tasks.
        if info.parent_pid.is_some() {
            continue;
        }

        // Read session file and check if it has the target repo
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Check if this session has the target repo in its repo list
        let has_repo = content
            .lines()
            .any(|line| line.trim() == format!("repo={}", target_repo_id));

        if !has_repo {
            continue;
        }

        if let (Some(agent_type), Some(session_id)) = (info.agent_type, info.session_id.clone())
        {
            let external_id = info.external_session_id.unwrap_or_default();

            matching_sessions.push(SessionMatch {
                agent_type,
                external_session_id: external_id,
                session_id,
            });
        }
    }

    // Return the most recent session (by count, since we don't have activity timestamps here)
    // In practice, there should usually be at most one matching session per repo
    matching_sessions.pop()
}

/// Extract and parse the `timestamp=` field from a JJ event description.
///
/// Supports RFC 3339 format (e.g., "2026-01-23T12:00:00Z") and
/// JJ's native format (e.g., "2026-01-23 12:00:00.000 -08:00").
///
/// Returns:
/// - `Ok(Some(timestamp))` - timestamp found and parsed
/// - `Ok(None)` - empty description (no events)
/// - `Err(e)` - timestamp field missing or unparseable
fn parse_event_timestamp(description: &str) -> std::result::Result<Option<DateTime<Utc>>, String> {
    if description.trim().is_empty() {
        return Ok(None);
    }

    // Extract timestamp= field from the event description metadata
    let timestamp_line = description
        .lines()
        .find(|line| line.trim().starts_with("timestamp="));

    let timestamp_str = match timestamp_line {
        Some(line) => line.trim().strip_prefix("timestamp=").unwrap_or(""),
        None => return Err("No timestamp= field in event description".to_string()),
    };

    if timestamp_str.is_empty() {
        return Err("Empty timestamp= field in event description".to_string());
    }

    // Parse RFC 3339 timestamp (e.g., "2026-01-23T12:00:00Z")
    if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(timestamp_str) {
        return Ok(Some(ts.with_timezone(&Utc)));
    }

    // Try JJ format: "2026-01-23 12:00:00.000 -08:00"
    if let Ok(ts) = chrono::DateTime::parse_from_str(timestamp_str, "%Y-%m-%d %H:%M:%S%.3f %:z") {
        return Ok(Some(ts.with_timezone(&Utc)));
    }

    // Try without milliseconds
    if let Ok(ts) = chrono::DateTime::parse_from_str(timestamp_str, "%Y-%m-%d %H:%M:%S %:z") {
        return Ok(Some(ts.with_timezone(&Utc)));
    }

    Err(format!("Failed to parse timestamp: '{}'", timestamp_str))
}

/// Query the latest event timestamp for a session from JJ conversation history
///
/// Shells out to `jj log` to find the most recent event for a session.
/// Returns:
/// - `Ok(Some(timestamp))` - events found, latest timestamp returned
/// - `Ok(None)` - no events found (orphaned session)
/// - `Err(e)` - JJ query failed (repo lock, jj not in PATH, etc.)
fn query_latest_event(
    repo_path: &Path,
    session_id: &str,
) -> std::result::Result<Option<DateTime<Utc>>, String> {
    use crate::jj::jj_cmd;

    // Query JJ for latest event in this session
    // Use ::aiki/conversations (ancestors) to scan full conversation history
    // Extract the event metadata timestamp= field (not committer.timestamp() which
    // can skew if events are backfilled or timestamped differently)
    let output = jj_cmd()
        .args([
            "log",
            "-r",
            &format!(
                "::aiki/conversations & description(\"session={}\")",
                session_id
            ),
            "--limit",
            "1",
            "--no-graph",
            "--template",
            "description ++ \"\\n\"",
            "--ignore-working-copy",
        ])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to run jj: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("jj log failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        return Ok(None); // No events found
    }

    parse_event_timestamp(&stdout)
}

/// Parsed session file metadata used for cleanup and session detection
struct SessionFileInfo {
    path: PathBuf,
    agent_type: Option<AgentType>,
    session_id: Option<String>,
    external_session_id: Option<String>,
    mode: Option<SessionMode>,
    parent_pid: Option<u32>,
}

fn parse_session_file(path: &Path) -> Option<SessionFileInfo> {
    let content = fs::read_to_string(path).ok()?;
    let mut agent_type: Option<AgentType> = None;
    let mut session_id: Option<String> = None;
    let mut external_session_id: Option<String> = None;
    let mut mode: Option<SessionMode> = None;
    let mut parent_pid: Option<u32> = None;

    for line in content.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("parent_pid=") {
            parent_pid = val.parse().ok();
        } else if let Some(val) = line.strip_prefix("agent=") {
            agent_type = AgentType::from_str(val);
        } else if let Some(val) = line.strip_prefix("session_id=") {
            session_id = Some(val.to_string());
        } else if let Some(val) = line.strip_prefix("aiki_session_id=") {
            // Old field name
            if session_id.is_none() {
                session_id = Some(val.to_string());
            }
        } else if let Some(val) = line.strip_prefix("external_session_id=") {
            external_session_id = Some(val.to_string());
        } else if let Some(val) = line.strip_prefix("mode=") {
            mode = SessionMode::from_str(val);
        }
    }

    Some(SessionFileInfo {
        path: path.to_path_buf(),
        agent_type,
        session_id,
        external_session_id,
        mode,
        parent_pid,
    })
}

/// Emit a synthetic session.ended event through the event bus.
///
/// Used during PID cleanup when the agent process is dead.
/// Dispatches a full `session.ended` event so that history recording,
/// hook execution, and session file cleanup all happen through the
/// normal event handling path.
fn emit_synthetic_session_ended(session_info: &SessionFileInfo, reason: SessionCleanupReason) {
    use crate::cache::debug_log;
    use crate::event_bus;
    use crate::events::{AikiEvent, AikiSessionEndedPayload};

    let reason_str = match reason {
        SessionCleanupReason::PidDead => "pid_dead",
    };

    debug_log(|| {
        format!(
            "Synthetic session.ended: session={}, reason={}",
            session_info.session_id.as_deref().unwrap_or("unknown"),
            reason_str
        )
    });

    let (Some(session_id), Some(agent_type)) = (&session_info.session_id, session_info.agent_type)
    else {
        debug_log(|| {
            "Cannot emit synthetic session.ended: missing session_id or agent_type".to_string()
        });
        return;
    };

    let mode = session_info.mode.unwrap_or(SessionMode::Interactive);
    let session = AikiSession::from_uuid(session_id.clone(), agent_type, mode);
    let cwd = global::global_aiki_dir();

    let event = AikiEvent::SessionEnded(AikiSessionEndedPayload {
        session,
        cwd,
        timestamp: Utc::now(),
        reason: reason_str.to_string(),
    });

    if let Err(e) = event_bus::dispatch(event) {
        debug_log(|| format!("Failed to dispatch synthetic session.ended: {}", e));
    }
}

/// Remove session files whose parent PID is dead.
///
/// Dispatches full session.ended events through the event bus for each dead
/// session. The event handler records to history, executes flows, and cleans
/// up the session file.
///
/// Does not query JJ for TTL — only checks process liveness.
pub fn prune_dead_pid_sessions() {
    let sessions_dir = global::global_sessions_dir();

    if !sessions_dir.exists() {
        return;
    }

    let entries = match fs::read_dir(&sessions_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);

    let session_files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|entry| {
            let path = entry.path();
            path.is_file() && path.extension().is_none()
        })
        .filter_map(|entry| parse_session_file(&entry.path()))
        .collect();

    for session_info in &session_files {
        let pid_alive = match session_info.parent_pid {
            Some(pid) => system.process(Pid::from_u32(pid)).is_some(),
            None => true, // No PID = can't determine, treat as alive
        };

        if !pid_alive {
            // Recover any orphaned workspaces from the dead session
            if let Some(ref session_uuid) = session_info.session_id {
                match isolation::recover_orphaned_workspaces(session_uuid) {
                    Ok(0) => {}
                    Ok(n) => {
                        eprintln!(
                            "[aiki] Recovered {} workspace(s) from crashed session",
                            n
                        );
                    }
                    Err(e) => {
                        debug_log(|| {
                            format!(
                                "Warning: failed to recover workspaces for dead session {}: {}",
                                session_uuid, e
                            )
                        });
                    }
                }
            }

            // Dispatches through event bus → handle_session_ended which
            // records history, runs flows, and deletes the session file.
            emit_synthetic_session_ended(session_info, SessionCleanupReason::PidDead);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, Timelike};
    use std::env;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // Mutex to serialize tests that modify AIKI_HOME env var
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Guard that restores AIKI_HOME on drop
    struct EnvGuard {
        original: Option<String>,
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(v) => env::set_var(global::AIKI_HOME_ENV, v),
                None => env::remove_var(global::AIKI_HOME_ENV),
            }
        }
    }

    /// Set up a test repo AND configure AIKI_HOME for isolation.
    /// CALLER MUST HOLD ENV_MUTEX LOCK.
    /// Returns (repo TempDir, AIKI home TempDir, guard for cleanup).
    fn setup_test_repo_with_global_inner() -> (TempDir, TempDir, EnvGuard) {
        // Create repo temp dir
        let repo_dir = TempDir::new().unwrap();
        fs::create_dir_all(repo_dir.path().join(".aiki")).unwrap();

        // Create global AIKI_HOME temp dir
        let aiki_home = TempDir::new().unwrap();
        let aiki_home_path = aiki_home.path().to_path_buf();
        fs::create_dir_all(aiki_home_path.join("sessions")).unwrap();

        // Save original AIKI_HOME and set new value
        let original = env::var(global::AIKI_HOME_ENV).ok();
        env::set_var(global::AIKI_HOME_ENV, &aiki_home_path);

        (repo_dir, aiki_home, EnvGuard { original })
    }

    /// Set up isolated AIKI_HOME only (for tests that don't need repo path).
    /// CALLER MUST HOLD ENV_MUTEX LOCK.
    /// Returns (AIKI home TempDir, guard for cleanup).
    fn setup_global_aiki_home_inner() -> (TempDir, EnvGuard) {
        // Create global AIKI_HOME temp dir
        let aiki_home = TempDir::new().unwrap();
        let aiki_home_path = aiki_home.path().to_path_buf();
        fs::create_dir_all(aiki_home_path.join("sessions")).unwrap();

        // Save original AIKI_HOME and set new value
        let original = env::var(global::AIKI_HOME_ENV).ok();
        env::set_var(global::AIKI_HOME_ENV, &aiki_home_path);

        (aiki_home, EnvGuard { original })
    }

    /// Simple test repo setup (for tests that only need a local temp directory)
    fn setup_test_repo() -> TempDir {
        let temp_dir = TempDir::new().unwrap();
        fs::create_dir_all(temp_dir.path().join(".aiki")).unwrap();
        temp_dir
    }

    #[test]
    fn test_create_and_query_session() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();

        // Create a session and write its file
        let session =
            AikiSession::for_hook(AgentType::ClaudeCode, "claude-session-abc123", None::<&str>);
        session.file().create().unwrap();

        // Verify session file was created using the global API
        let session_file_path = global::global_sessions_dir().join(session.uuid());
        assert!(session_file_path.exists());

        // Verify session file format uses [aiki]...[/aiki] blocks
        let content = fs::read_to_string(&session_file_path).unwrap();
        assert!(content.starts_with("[aiki]\n"));
        assert!(content.contains("agent=claude"));
        assert!(content.contains("external_session_id=claude-session-abc123"));
        assert!(content.contains(&format!("session_id={}", session.uuid())));
        assert!(content.contains("started_at="));
        assert!(
            !content.contains("cwd="),
            "cwd field should not be in session file"
        );
        assert!(content.ends_with("[/aiki]\n"));

        // Verify session count
        assert_eq!(count_sessions().unwrap(), 1);
    }

    #[test]
    fn test_multiple_creates_same_session() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();

        // Create session file twice (idempotent via O_EXCL)
        let session1 =
            AikiSession::for_hook(AgentType::ClaudeCode, "claude-session-abc123", None::<&str>);
        let created1 = session1.file().create().unwrap();
        assert!(created1); // First create succeeds

        let session2 =
            AikiSession::for_hook(AgentType::ClaudeCode, "claude-session-abc123", None::<&str>);
        let created2 = session2.file().create().unwrap();
        assert!(!created2); // Second create returns false (already exists)

        // Should produce same session UUID
        assert_eq!(session1.uuid(), session2.uuid());

        // Should only have one file
        assert_eq!(count_sessions().unwrap(), 1);
    }

    #[test]
    fn test_update_parent_pid_adds_pid_to_session_file() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();

        // Create session without PID (using new() instead of for_hook() to avoid auto-capture)
        let session = AikiSession::new(
            AgentType::Codex,
            "codex-session-123",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        // Don't call with_parent_pid - leave it as None
        let session_file = session.file();
        session_file.create().unwrap();

        // Verify no parent_pid initially
        let session_file_path = global::global_sessions_dir().join(session.uuid());
        let content = fs::read_to_string(&session_file_path).unwrap();
        assert!(
            !content.contains("parent_pid="),
            "Session should not have parent_pid initially"
        );

        // Update with PID
        session_file.update_parent_pid(12345).unwrap();

        // Verify parent_pid was added
        let content = fs::read_to_string(&session_file_path).unwrap();
        assert!(
            content.contains("parent_pid=12345"),
            "Session should have parent_pid after update"
        );
        assert!(
            content.contains("[/aiki]"),
            "Session file should still have closing tag"
        );
    }

    #[test]
    fn test_update_parent_pid_idempotent() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();

        // Create session without PID
        let session = AikiSession::new(
            AgentType::Codex,
            "codex-session-456",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        let session_file = session.file();
        session_file.create().unwrap();

        // Update with PID twice
        session_file.update_parent_pid(11111).unwrap();
        session_file.update_parent_pid(22222).unwrap(); // Should not change anything

        // Verify only first PID is present
        let session_file_path = global::global_sessions_dir().join(session.uuid());
        let content = fs::read_to_string(&session_file_path).unwrap();
        assert!(content.contains("parent_pid=11111"));
        assert!(!content.contains("parent_pid=22222"));
    }

    #[test]
    fn test_multiple_different_sessions() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();

        // Create multiple different sessions
        let session1 =
            AikiSession::for_hook(AgentType::ClaudeCode, "claude-session-1", None::<&str>);
        session1.file().create().unwrap();

        let session2 = AikiSession::new(
            AgentType::Cursor,
            "cursor-session-2",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        session2.file().create().unwrap();

        // Both should exist
        assert!(global::global_sessions_dir().join(session1.uuid()).exists());
        assert!(global::global_sessions_dir().join(session2.uuid()).exists());

        // Should have 2 sessions
        assert_eq!(count_sessions().unwrap(), 2);
    }

    #[test]
    fn test_deterministic_session_ids() {
        // Same inputs should produce same session UUIDs
        let session1 = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        let session2 = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );

        assert_eq!(session1.uuid(), session2.uuid());
        assert_eq!(session1.uuid(), session2.uuid());

        // Different inputs should produce different UUIDs
        let session3 = AikiSession::new(
            AgentType::Cursor,
            "test-session",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        assert_ne!(session1.uuid(), session3.uuid());
    }

    #[test]
    fn test_session_end() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();

        // Start a session
        let session = AikiSession::for_hook(
            AgentType::ClaudeCode,
            "claude-session-end-test",
            None::<&str>,
        );
        session.file().create().unwrap();

        // Verify it exists
        assert!(global::global_sessions_dir().join(session.uuid()).exists());
        assert_eq!(count_sessions().unwrap(), 1);

        // End the session
        end_session(
            AgentType::ClaudeCode,
            "claude-session-end-test",
            DetectionMethod::Hook,
            SessionMode::Interactive,
        )
        .unwrap();

        // Verify it's gone
        assert!(!global::global_sessions_dir().join(session.uuid()).exists());
        assert_eq!(count_sessions().unwrap(), 0);
    }

    #[test]
    fn test_session_lifecycle() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();

        // Start
        let session = AikiSession::for_hook(AgentType::ClaudeCode, "lifecycle-test", None::<&str>);
        session.file().create().unwrap();

        // Verify session file exists
        let session_file = global::global_sessions_dir().join(session.uuid());
        assert!(session_file.exists());

        // End
        session.end().unwrap();

        // Verify session file is deleted
        assert!(!session_file.exists());
    }

    #[test]
    fn test_idempotent_file_creation() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();

        // Create same session file 5 times (idempotent via O_EXCL)
        for i in 0..5 {
            let session =
                AikiSession::for_hook(AgentType::ClaudeCode, "idempotent-test", None::<&str>);
            let created = session.file().create().unwrap();
            // Only first create should return true
            assert_eq!(created, i == 0);
        }

        // Should only have 1 session file
        assert_eq!(count_sessions().unwrap(), 1);
    }

    #[test]
    fn test_session_file_stores_agent_version() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();

        // Create a session with agent version
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session-with-version",
            Some("2.0.61"),
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );

        // Write session file
        let session_file = session.file();
        session_file.create().unwrap();

        // Verify agent_version is stored in the file
        let content = fs::read_to_string(&session_file.path).unwrap();
        assert!(
            content.contains("agent_version=2.0.61"),
            "Session file should contain agent_version"
        );

        // Verify we can read it back
        let cached_version = session_file.read_agent_version();
        assert_eq!(cached_version, Some("2.0.61".to_string()));
    }

    #[test]
    fn test_session_file_without_agent_version() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();

        // Create a session without agent version
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session-no-version",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );

        // Write session file
        let session_file = session.file();
        session_file.create().unwrap();

        // Verify agent_version is NOT in the file
        let content = fs::read_to_string(&session_file.path).unwrap();
        assert!(
            !content.contains("agent_version="),
            "Session file should not contain agent_version field"
        );

        // Verify read returns None
        let cached_version = session_file.read_agent_version();
        assert_eq!(cached_version, None);
    }

    // ========================================================================
    // PID-based session detection tests
    // ========================================================================

    #[test]
    fn test_session_file_stores_parent_pid() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();

        // Create a session with parent_pid
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session-with-pid",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        )
        .with_parent_pid(Some(12345));

        // Write session file
        let session_file = session.file();
        session_file.create().unwrap();

        // Verify parent_pid is stored in the file
        let content = fs::read_to_string(&session_file.path).unwrap();
        assert!(
            content.contains("parent_pid=12345"),
            "Session file should contain parent_pid"
        );
    }

    #[test]
    fn test_session_file_without_parent_pid() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();

        // Create a session without parent_pid (ACP mode without agent_pid)
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session-no-pid",
            None::<&str>,
            DetectionMethod::ACP,
            SessionMode::Interactive,
        );

        // Write session file
        let session_file = session.file();
        session_file.create().unwrap();

        // Verify parent_pid is NOT in the file
        let content = fs::read_to_string(&session_file.path).unwrap();
        assert!(
            !content.contains("parent_pid="),
            "Session file should not contain parent_pid field"
        );
    }

    #[test]
    fn test_for_hook_captures_parent_pid() {
        // for_hook should capture the current parent PID
        let session = AikiSession::for_hook(AgentType::ClaudeCode, "test-session", None::<&str>);

        // Should have a parent_pid (unless we're init which has no parent)
        // Just verify the method doesn't panic and returns a session
        assert_eq!(session.agent_type(), AgentType::ClaudeCode);
        // Parent PID may or may not be set depending on the process tree
    }

    #[test]
    fn test_with_parent_pid_builder() {
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session",
            None::<&str>,
            DetectionMethod::ACP,
            SessionMode::Interactive,
        )
        .with_parent_pid(Some(99999));

        assert_eq!(session.parent_pid(), Some(99999));
    }

    #[test]
    fn test_get_parent_pid_returns_value() {
        // get_parent_pid should return Some value for any normal process
        let pid = get_parent_pid();
        // In a normal test environment, we should have a parent process
        // This could be None only for init process (PID 1)
        assert!(std::process::id() != 1 || pid.is_none());
    }

    #[test]
    fn test_find_session_by_ancestor_pid_no_sessions() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (repo_dir, _aiki_home, _guard) = setup_test_repo_with_global_inner();
        let repo_path = repo_dir.path();

        // No sessions exist
        let result = find_session_by_ancestor_pid(repo_path);
        assert!(result.is_none());
    }

    #[test]
    fn test_find_session_by_ancestor_pid_with_matching_session() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (repo_dir, _aiki_home, _guard) = setup_test_repo_with_global_inner();
        let repo_path = repo_dir.path();

        // Create a session with our parent PID
        let our_parent_pid = get_parent_pid();

        if let Some(pid) = our_parent_pid {
            let session = AikiSession::new(
                AgentType::ClaudeCode,
                "matching-session",
                None::<&str>,
                DetectionMethod::Hook,
                SessionMode::Interactive,
            )
            .with_parent_pid(Some(pid));

            session.file().create().unwrap();

            // Should find the session
            let result = find_session_by_ancestor_pid(repo_path);
            assert!(result.is_some());

            let matched = result.unwrap();
            assert_eq!(matched.agent_type, AgentType::ClaudeCode);
            assert_eq!(matched.external_session_id, "matching-session");
        }
    }

    #[test]
    fn test_find_session_by_ancestor_pid_non_matching_pid() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (repo_dir, _aiki_home, _guard) = setup_test_repo_with_global_inner();
        let repo_path = repo_dir.path();

        // Create a session with a PID that's not in our ancestor chain
        // Use a very high PID that's unlikely to be a real process
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "non-matching-session",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        )
        .with_parent_pid(Some(999999));

        session.file().create().unwrap();

        // Should not find the session (PID doesn't match our ancestors)
        let result = find_session_by_ancestor_pid(repo_path);
        assert!(result.is_none());
    }

    #[test]
    fn test_prune_dead_pid_sessions_removes_dead_pid() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_repo_dir, _aiki_home, _guard) = setup_test_repo_with_global_inner();

        // Create a session with a PID that definitely doesn't exist
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "stale-session",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        )
        .with_parent_pid(Some(999999));

        session.file().create().unwrap();

        // Verify session file exists
        let session_file = global::global_sessions_dir().join(session.uuid());
        assert!(session_file.exists());

        // Cleanup should remove it
        prune_dead_pid_sessions();

        // Session file should be gone
        assert!(!session_file.exists());
    }

    #[test]
    fn test_prune_dead_pid_sessions_keeps_live_pid() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_repo_dir, _aiki_home, _guard) = setup_test_repo_with_global_inner();

        // Create a session with our own PID (which is alive)
        let our_pid = std::process::id();
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "live-session",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        )
        .with_parent_pid(Some(our_pid));

        session.file().create().unwrap();

        // Verify session file exists
        let session_file = global::global_sessions_dir().join(session.uuid());
        assert!(session_file.exists());

        // Cleanup should NOT remove it (process is alive)
        prune_dead_pid_sessions();

        // Session file should still exist
        assert!(session_file.exists());
    }

    #[test]
    fn test_for_hook_session_file_has_parent_pid() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();

        // Create session using for_hook (which captures parent PID)
        let session = AikiSession::for_hook(AgentType::ClaudeCode, "hook-session", None::<&str>);
        session.file().create().unwrap();

        // Verify parent_pid is in the session file
        let session_file = global::global_sessions_dir().join(session.uuid());
        let content = fs::read_to_string(&session_file).unwrap();

        // Should have parent_pid for hook mode
        assert!(
            content.contains("parent_pid="),
            "Hook mode session should have parent_pid"
        );
    }

    #[test]
    fn test_from_uuid_preserves_uuid() {
        let uuid = "12345678-1234-5678-1234-567812345678".to_string();
        let session = AikiSession::from_uuid(
            uuid.clone(),
            AgentType::ClaudeCode,
            SessionMode::Interactive,
        );
        assert_eq!(session.uuid(), &uuid);
        assert_eq!(session.agent_type(), AgentType::ClaudeCode);
    }

    #[test]
    fn test_from_uuid_does_not_rehash() {
        // from_uuid should preserve the UUID, not regenerate it
        let original = AikiSession::new(
            AgentType::ClaudeCode,
            "test-ext-id",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        let original_uuid = original.uuid().to_string();

        // from_uuid with the generated UUID should preserve it
        let reconstructed = AikiSession::from_uuid(
            original_uuid.clone(),
            AgentType::ClaudeCode,
            SessionMode::Interactive,
        );
        assert_eq!(reconstructed.uuid(), &original_uuid);

        // But new() with the UUID as external_id would generate a different UUID
        let rehashed = AikiSession::new(
            AgentType::ClaudeCode,
            &original_uuid,
            None::<&str>,
            DetectionMethod::Unknown,
            SessionMode::Interactive,
        );
        assert_ne!(
            rehashed.uuid(),
            &original_uuid,
            "new() should rehash the UUID"
        );
    }

    #[test]
    fn test_parse_session_file_new_format() {
        let temp_dir = setup_test_repo();
        let sessions_dir = temp_dir.path().join(".aiki/sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        let file_path = sessions_dir.join("test-session-id");
        fs::write(&file_path, "[aiki]\nagent=claude\nexternal_session_id=ext-123\nsession_id=uuid-456\nstarted_at=2026-01-24T12:00:00Z\nparent_pid=99999\n[/aiki]\n").unwrap();

        let info = parse_session_file(&file_path).unwrap();
        assert_eq!(info.agent_type, Some(AgentType::ClaudeCode));
        assert_eq!(info.session_id, Some("uuid-456".to_string()));
        assert_eq!(info.parent_pid, Some(99999));
    }

    #[test]
    fn test_parse_session_file_old_format() {
        let temp_dir = setup_test_repo();
        let sessions_dir = temp_dir.path().join(".aiki/sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        let file_path = sessions_dir.join("test-session-id");
        fs::write(&file_path, "[aiki]\nagent=cursor\nexternal_session_id=ext-789\naiki_session_id=old-uuid-123\nstarted_at=2026-01-24T12:00:00Z\nparent_pid=88888\n[/aiki]\n").unwrap();

        let info = parse_session_file(&file_path).unwrap();
        assert_eq!(info.agent_type, Some(AgentType::Cursor));
        assert_eq!(info.session_id, Some("old-uuid-123".to_string()));
        assert_eq!(info.parent_pid, Some(88888));
    }

    #[test]
    fn test_find_session_by_ancestor_pid_with_multiple_matching_sessions() {
        // This test verifies that when multiple sessions match our PID ancestry,
        // we find at least one session. Activity-based preference now uses JJ queries,
        // so in a non-JJ test environment all sessions get UNIX_EPOCH timestamps.
        // The important behavior is that we return *a* matching session.

        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (repo_dir, _aiki_home, _guard) = setup_test_repo_with_global_inner();
        let repo_path = repo_dir.path();
        let sessions_dir = global::global_sessions_dir();

        // Use parent PID (which is in our ancestor chain)
        let parent_pid = match get_parent_pid() {
            Some(pid) => pid,
            None => return, // Skip test if parent PID unavailable
        };

        // Create session A
        let a_content = format!(
            "[aiki]\nagent=claude\nexternal_session_id=session-a\nsession_id=uuid-a\nstarted_at=2026-01-24T12:00:00Z\nparent_pid={}\n[/aiki]\n",
            parent_pid
        );
        fs::write(sessions_dir.join("uuid-a"), &a_content).unwrap();

        // Create session B
        let b_content = format!(
            "[aiki]\nagent=claude\nexternal_session_id=session-b\nsession_id=uuid-b\nstarted_at=2026-01-20T12:00:00Z\nparent_pid={}\n[/aiki]\n",
            parent_pid
        );
        fs::write(sessions_dir.join("uuid-b"), &b_content).unwrap();

        let result = find_session_by_ancestor_pid(repo_path);
        assert!(
            result.is_some(),
            "Should find at least one matching session"
        );
        let session = result.unwrap();
        // Without JJ, both sessions have UNIX_EPOCH timestamps, so either may be returned
        assert!(
            session.session_id == "uuid-a" || session.session_id == "uuid-b",
            "Should return one of the matching sessions"
        );
    }

    #[test]
    fn test_session_file_uses_new_format() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();

        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        session.file().create().unwrap();

        let session_file = global::global_sessions_dir().join(session.uuid());
        let content = fs::read_to_string(&session_file).unwrap();

        // Should use new field name
        assert!(
            content.contains("session_id="),
            "Should use session_id field"
        );
        assert!(
            !content.contains("aiki_session_id="),
            "Should not use old field name"
        );
        assert!(!content.contains("cwd="), "Should not include cwd field");
    }

    // ========================================================================
    // parse_event_timestamp tests (covers TTL cleanup timestamp parsing)
    // ========================================================================

    #[test]
    fn test_parse_event_timestamp_rfc3339() {
        let description =
            "[aiki]\nevent=prompt\nsession=sess123\ntimestamp=2026-01-23T12:00:00Z\n[/aiki]\n";
        let result = parse_event_timestamp(description);
        assert!(result.is_ok());
        let ts = result.unwrap().unwrap();
        assert_eq!(ts.year(), 2026);
        assert_eq!(ts.month(), 1);
        assert_eq!(ts.day(), 23);
        assert_eq!(ts.hour(), 12);
    }

    #[test]
    fn test_parse_event_timestamp_rfc3339_with_offset() {
        let description = "event=response\ntimestamp=2026-01-23T04:00:00-08:00\n";
        let result = parse_event_timestamp(description);
        assert!(result.is_ok());
        let ts = result.unwrap().unwrap();
        // 04:00 -08:00 = 12:00 UTC
        assert_eq!(ts.hour(), 12);
    }

    #[test]
    fn test_parse_event_timestamp_jj_format_with_millis() {
        let description = "event=prompt\ntimestamp=2026-01-23 12:00:00.000 +00:00\n";
        let result = parse_event_timestamp(description);
        assert!(result.is_ok());
        let ts = result.unwrap().unwrap();
        assert_eq!(ts.hour(), 12);
    }

    #[test]
    fn test_parse_event_timestamp_jj_format_no_millis() {
        let description = "event=prompt\ntimestamp=2026-01-23 12:00:00 +00:00\n";
        let result = parse_event_timestamp(description);
        assert!(result.is_ok());
        let ts = result.unwrap().unwrap();
        assert_eq!(ts.hour(), 12);
    }

    #[test]
    fn test_parse_event_timestamp_empty_description() {
        let result = parse_event_timestamp("");
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn test_parse_event_timestamp_whitespace_only() {
        let result = parse_event_timestamp("  \n  \n  ");
        assert_eq!(result, Ok(None));
    }

    #[test]
    fn test_parse_event_timestamp_no_timestamp_field() {
        let description = "event=prompt\nsession=sess123\nagent_type=claude-code\n";
        let result = parse_event_timestamp(description);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No timestamp="));
    }

    #[test]
    fn test_parse_event_timestamp_empty_value() {
        let description = "event=prompt\ntimestamp=\n";
        let result = parse_event_timestamp(description);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Empty timestamp="));
    }

    #[test]
    fn test_parse_event_timestamp_unparseable() {
        let description = "event=prompt\ntimestamp=not-a-timestamp\n";
        let result = parse_event_timestamp(description);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Failed to parse"));
    }

    #[test]
    fn test_parse_event_timestamp_indented_field() {
        // Handles whitespace before timestamp= field
        let description = "event=prompt\n  timestamp=2026-01-23T12:00:00Z\n";
        let result = parse_event_timestamp(description);
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
    }

    #[test]
    fn test_find_ancestor_by_name_finds_shell() {
        // Should find zsh or bash in our process ancestry (test runner runs in a shell)
        let zsh = find_ancestor_by_name("zsh");
        let bash = find_ancestor_by_name("bash");
        let sh = find_ancestor_by_name("sh");

        // At least one shell should be in our ancestry
        assert!(
            zsh.is_some() || bash.is_some() || sh.is_some(),
            "Should find a shell in process ancestry"
        );
    }

    #[test]
    fn test_find_ancestor_by_name_not_found() {
        // Should not find a process with an unlikely name
        let result = find_ancestor_by_name("definitely_not_a_real_process_xyz123");
        assert!(result.is_none());
    }

    #[test]
    fn test_find_ancestor_by_name_case_insensitive() {
        // Test case insensitivity - ZSH should match zsh
        let lower = find_ancestor_by_name("zsh");
        let upper = find_ancestor_by_name("ZSH");

        // If zsh exists, both should find it
        if lower.is_some() {
            assert_eq!(lower, upper, "Search should be case-insensitive");
        }
    }

    // ========================================================================
    // Session file repo tracking tests
    // ========================================================================

    #[test]
    fn test_read_repos_no_file() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        let session_file = session.file();

        // File doesn't exist - should return empty vec
        let repos = session_file.read_repos();
        assert!(repos.is_empty());
    }

    #[test]
    fn test_read_repos_no_repo_fields() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        let session_file = session.file();
        session_file.create().unwrap();

        // File exists but has no repo fields
        let repos = session_file.read_repos();
        assert!(repos.is_empty());
    }

    #[test]
    fn test_add_repo_to_session_file() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        let session_file = session.file();
        session_file.create().unwrap();

        // Add a repo
        session_file.add_repo("abc123def456").unwrap();

        // Verify it was added
        let repos = session_file.read_repos();
        assert_eq!(repos, vec!["abc123def456"]);

        // Verify file content
        let content = fs::read_to_string(&session_file.path).unwrap();
        assert!(content.contains("repo=abc123def456"));
        assert!(content.contains("[/aiki]"));
    }

    #[test]
    fn test_add_multiple_repos() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        let session_file = session.file();
        session_file.create().unwrap();

        // Add multiple repos
        session_file.add_repo("abc123").unwrap();
        session_file.add_repo("def456").unwrap();
        session_file.add_repo("ghi789").unwrap();

        // Verify all were added
        let repos = session_file.read_repos();
        assert_eq!(repos.len(), 3);
        assert!(repos.contains(&"abc123".to_string()));
        assert!(repos.contains(&"def456".to_string()));
        assert!(repos.contains(&"ghi789".to_string()));
    }

    #[test]
    fn test_add_repo_idempotent() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        let session_file = session.file();
        session_file.create().unwrap();

        // Add same repo twice
        session_file.add_repo("abc123").unwrap();
        session_file.add_repo("abc123").unwrap();

        // Should only appear once
        let repos = session_file.read_repos();
        assert_eq!(repos, vec!["abc123"]);
    }

    #[test]
    fn test_add_repo_to_nonexistent_file() {
        let _lock = ENV_MUTEX.lock().unwrap_or_else(|e| e.into_inner());
        let (_aiki_home, _guard) = setup_global_aiki_home_inner();
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session",
            None::<&str>,
            DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        let session_file = session.file();

        // Don't create the file - add_repo should handle gracefully
        let result = session_file.add_repo("abc123");
        assert!(result.is_ok());
    }
}
