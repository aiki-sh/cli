use crate::cache::debug_log;
use crate::history::TurnSource;
use crate::jj::jj_cmd;
use std::path::Path;

/// Ephemeral turn state for a session
///
/// Tracks the current turn number by querying JJ history on load.
/// No longer persists to `.aiki/sessions/<uuid>.turn` files.
///
/// Turn IDs are deterministic: `turn_id = uuid_v5(session_uuid, turn.to_string())`
#[derive(Debug, Clone)]
pub struct TurnState {
    /// The session UUID (used as namespace for turn_id generation)
    session_uuid: String,
    /// Current turn number (starts at 0, incremented on each turn.started)
    pub current_turn: u32,
    /// Current turn ID (deterministic UUID v5)
    pub current_turn_id: String,
    /// Source of the current turn
    pub current_turn_source: TurnSource,
}

impl TurnState {
    /// Load turn state by querying JJ history
    ///
    /// Queries the `aiki/conversations` branch for the max turn number in this
    /// session's events. This makes turn state ephemeral (computed on load).
    ///
    /// Turn source defaults to User since we can't determine the source from
    /// JJ history at load time. The actual source is determined by checking
    /// for pending autoreply events in the conversation history.
    #[must_use]
    pub fn load(session_uuid: &str, repo_path: &Path) -> Self {
        // Always query JJ - no file persistence needed
        let current_turn = query_max_turn_from_jj(session_uuid, repo_path).unwrap_or(0);
        let current_turn_id = generate_turn_id(session_uuid, current_turn);

        Self {
            session_uuid: session_uuid.to_string(),
            current_turn,
            current_turn_id,
            // Default to User - actual source will be determined by history query
            current_turn_source: TurnSource::User,
        }
    }

    /// Start a new turn: increment counter, generate turn_id
    ///
    /// Returns the new turn number.
    /// Note: State is no longer persisted to disk - it's ephemeral.
    pub fn start_turn(&mut self, source: TurnSource) -> u32 {
        self.current_turn += 1;
        self.current_turn_id = generate_turn_id(&self.session_uuid, self.current_turn);
        self.current_turn_source = source;
        self.current_turn
    }
}

/// Query max turn number from JJ history for this session
///
/// Queries the aiki/conversations branch for the most recent event with
/// `session=<uuid>` in its description, and extracts the `turn=N` value.
///
/// Returns `None` if JJ is unavailable, the branch doesn't exist, or no turns are found.
fn query_max_turn_from_jj(session_uuid: &str, repo_path: &Path) -> Option<u32> {
    const CONVERSATIONS_BRANCH: &str = "aiki/conversations";

    let output = jj_cmd()
        .args([
            "log",
            "-r",
            &format!(
                "ancestors({}) & description(substring:'session={}') & description(substring:'turn=')",
                CONVERSATIONS_BRANCH, session_uuid
            ),
            "--template",
            "description ++ \"\\n\"",
            "--no-graph",
            "--limit",
            "1",
            "--ignore-working-copy",
        ])
        .current_dir(repo_path)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let turn = stdout.lines().find_map(|line| {
        let trimmed = line.trim();
        if trimmed.starts_with("turn=") {
            trimmed.strip_prefix("turn=")?.parse::<u32>().ok()
        } else {
            None
        }
    });

    if let Some(turn) = turn {
        debug_log(|| {
            format!(
                "Loaded turn counter from aiki/conversations branch: session={}, turn={}",
                session_uuid, turn
            )
        });
    }

    turn
}

/// Generate a deterministic turn ID using UUID v5
///
/// `turn_id = uuid_v5(session_uuid_as_namespace, turn.to_string())`
///
/// This ensures the same session + turn number always produces the same turn_id.
pub fn generate_turn_id(session_uuid: &str, turn: u32) -> String {
    // Parse the session UUID to use as namespace
    let namespace = uuid::Uuid::parse_str(session_uuid).unwrap_or(uuid::Uuid::nil());
    let turn_id = uuid::Uuid::new_v5(&namespace, turn.to_string().as_bytes());
    turn_id.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_fresh_state_defaults_to_zero() {
        // Without JJ history, turn defaults to 0
        let tmp = TempDir::new().unwrap();
        let state = TurnState::load("test-session-uuid", tmp.path());
        assert_eq!(state.current_turn, 0);
        assert_eq!(state.current_turn_source, TurnSource::User);
    }

    #[test]
    fn test_start_turn_increments() {
        let tmp = TempDir::new().unwrap();

        let mut state = TurnState::load("test-session-uuid", tmp.path());
        assert_eq!(state.current_turn, 0);

        let turn = state.start_turn(TurnSource::User);
        assert_eq!(turn, 1);
        assert_eq!(state.current_turn, 1);

        let turn = state.start_turn(TurnSource::User);
        assert_eq!(turn, 2);
        assert_eq!(state.current_turn, 2);
    }

    #[test]
    fn test_state_is_ephemeral_no_file_persistence() {
        // TurnState is ephemeral - state is NOT persisted to files
        // Each load starts fresh from JJ history (which defaults to 0 without a JJ repo)
        let tmp = TempDir::new().unwrap();

        // Start some turns
        {
            let mut state = TurnState::load("test-session-uuid", tmp.path());
            state.start_turn(TurnSource::User);
            state.start_turn(TurnSource::User);
            state.start_turn(TurnSource::Autoreply);
            assert_eq!(state.current_turn, 3);
            assert_eq!(state.current_turn_source, TurnSource::Autoreply);
        }

        // Load again - starts fresh (no JJ repo means turn=0)
        let state = TurnState::load("test-session-uuid", tmp.path());
        assert_eq!(state.current_turn, 0);
        assert_eq!(state.current_turn_source, TurnSource::User);
    }

    #[test]
    fn test_turn_id_is_deterministic() {
        let id1 = generate_turn_id("550e8400-e29b-41d4-a716-446655440000", 1);
        let id2 = generate_turn_id("550e8400-e29b-41d4-a716-446655440000", 1);
        assert_eq!(id1, id2);

        // Different turn numbers produce different IDs
        let id3 = generate_turn_id("550e8400-e29b-41d4-a716-446655440000", 2);
        assert_ne!(id1, id3);

        // Different sessions produce different IDs
        let id4 = generate_turn_id("660e8400-e29b-41d4-a716-446655440000", 1);
        assert_ne!(id1, id4);
    }

    #[test]
    fn test_turn_id_generated_on_start_turn() {
        let tmp = TempDir::new().unwrap();

        let mut state = TurnState::load("550e8400-e29b-41d4-a716-446655440000", tmp.path());

        // Before start_turn, turn_id is for turn 0
        let initial_turn_id = state.current_turn_id.clone();

        // After start_turn, turn_id changes
        state.start_turn(TurnSource::User);
        assert_ne!(state.current_turn_id, initial_turn_id);

        // Turn ID should be deterministic for turn 1
        let expected_id = generate_turn_id("550e8400-e29b-41d4-a716-446655440000", 1);
        assert_eq!(state.current_turn_id, expected_id);
    }

    #[test]
    fn test_turn_source_tracking() {
        let tmp = TempDir::new().unwrap();

        let mut state = TurnState::load("test-session-uuid", tmp.path());

        // Default source is User
        assert_eq!(state.current_turn_source, TurnSource::User);

        // start_turn with User keeps it User
        state.start_turn(TurnSource::User);
        assert_eq!(state.current_turn_source, TurnSource::User);

        // start_turn with Autoreply changes to Autoreply
        state.start_turn(TurnSource::Autoreply);
        assert_eq!(state.current_turn_source, TurnSource::Autoreply);

        // start_turn with User changes back to User
        state.start_turn(TurnSource::User);
        assert_eq!(state.current_turn_source, TurnSource::User);
    }

    #[test]
    fn test_turn_id_with_invalid_uuid_uses_nil_namespace() {
        // Invalid UUID strings should fall back to nil namespace
        let id1 = generate_turn_id("not-a-uuid", 1);
        let id2 = generate_turn_id("also-not-a-uuid", 1);

        // Different invalid UUIDs that both fall back to nil namespace
        // produce the same turn_id for the same turn number
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_turn_id_format_is_valid_uuid() {
        let id = generate_turn_id("550e8400-e29b-41d4-a716-446655440000", 1);

        // Should be a valid UUID string
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }
}
