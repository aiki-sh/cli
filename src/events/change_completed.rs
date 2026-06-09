use super::prelude::*;
use super::Turn;
use crate::global;
use crate::history;
use crate::history::TurnSource;
use crate::session::turn_state::generate_turn_id;

// ============================================================================
// EditDetail Type (shared by write operations)
// ============================================================================

/// Details of an edit operation (old_string -> new_string replacement)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditDetail {
    /// The file path being edited
    pub file_path: String,
    /// The original text being replaced
    pub old_string: String,
    /// The new text that replaces old_string
    pub new_string: String,
}

impl EditDetail {
    /// Create a new edit detail
    #[must_use]
    pub fn new(
        file_path: impl Into<String>,
        old_string: impl Into<String>,
        new_string: impl Into<String>,
    ) -> Self {
        Self {
            file_path: file_path.into(),
            old_string: old_string.into(),
            new_string: new_string.into(),
        }
    }
}

// ============================================================================
// ChangeOperation Enum (Tagged Union)
// ============================================================================

/// The type of file mutation - each variant contains operation-specific data
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "lowercase")]
pub enum ChangeOperation {
    /// File content created or modified
    Write(WriteOperation),
    /// File removed
    Delete(DeleteOperation),
    /// File relocated (source deleted, destination created)
    Move(MoveOperation),
}

/// Write operation data - file content created or modified
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WriteOperation {
    /// Files that were created or modified
    pub file_paths: Vec<String>,
    /// Edit details (old_string -> new_string) for permission_asked and completed events
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edit_details: Vec<EditDetail>,
}

/// Delete operation data - file removed
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeleteOperation {
    /// Files that were removed
    pub file_paths: Vec<String>,
}

/// Move operation data - file relocated
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MoveOperation {
    /// Files affected by the move (same as destination_paths for consistency with Write/Delete)
    pub file_paths: Vec<String>,
    /// Original file paths before the move
    pub source_paths: Vec<String>,
    /// New file paths after the move (duplicates file_paths for move-specific access)
    pub destination_paths: Vec<String>,
}

impl MoveOperation {
    /// Create a MoveOperation from raw move command paths.
    ///
    /// Takes paths in the order they appear in a move command (sources followed by destination).
    /// When the destination is a directory target, expands each source to its final path
    /// within that directory.
    ///
    /// This is a convenience wrapper around `from_move_paths_with_hint` that uses
    /// syntactic-only detection. Use `from_move_paths_with_hint` when you have
    /// filesystem access to determine if the destination is a directory.
    ///
    /// # Examples
    ///
    /// Simple rename: `mv foo bar` → sources: ["foo"], destinations: ["bar"]
    /// Move to directory: `mv foo bar dir/` → sources: ["foo", "bar"], destinations: ["dir/foo", "dir/bar"]
    /// Multi-source: `mv foo bar dest` → sources: ["foo", "bar"], destinations: ["dest/foo", "dest/bar"]
    #[must_use]
    pub fn from_move_paths(paths: Vec<String>) -> Self {
        Self::from_move_paths_with_hint(paths, None)
    }

    /// Create a MoveOperation from raw move command paths with a directory hint.
    ///
    /// Takes paths in the order they appear in a move command (sources followed by destination).
    /// When the destination is a directory target, expands each source to its final path
    /// within that directory.
    ///
    /// # Directory detection
    ///
    /// The `dest_is_directory` hint allows callers to provide filesystem-based detection:
    /// - `Some(true)`: Destination is known to be a directory (checked via filesystem)
    /// - `Some(false)`: Destination is known to NOT be a directory
    /// - `None`: Use syntactic-only detection (trailing slash or multiple sources)
    ///
    /// For `change.permission_asked` events (before the move), callers can safely check
    /// the filesystem. For `change.completed` events (after the move), syntactic-only
    /// detection should be used because the filesystem state has changed.
    ///
    /// Example problem with post-move filesystem checks:
    /// `mv docs docs-old` renames a directory. After the move, `docs-old` IS a directory,
    /// but we should NOT expand to `docs-old/docs`.
    ///
    /// # Syntactic indicators (when hint is None)
    ///
    /// 1. Trailing slash: `mv foo dir/` → destination is explicitly a directory
    /// 2. Multiple sources: `mv foo bar dest` → destination MUST be a directory
    ///
    /// # Examples
    ///
    /// ```
    /// use aiki::events::MoveOperation;
    ///
    /// // With filesystem hint: single source to existing directory
    /// let op = MoveOperation::from_move_paths_with_hint(
    ///     vec!["file.txt".into(), "existing_dir".into()],
    ///     Some(true),  // We checked: existing_dir is a directory
    /// );
    /// assert_eq!(op.destination_paths, vec!["existing_dir/file.txt"]);
    ///
    /// // Without hint (syntactic only): same command treated as rename
    /// let op = MoveOperation::from_move_paths_with_hint(
    ///     vec!["file.txt".into(), "existing_dir".into()],
    ///     None,  // No hint, syntactic only
    /// );
    /// assert_eq!(op.destination_paths, vec!["existing_dir"]);
    /// ```
    #[must_use]
    pub fn from_move_paths_with_hint(paths: Vec<String>, dest_is_directory: Option<bool>) -> Self {
        if paths.len() < 2 {
            // Not a valid move command, return as-is
            return Self {
                file_paths: vec![],
                source_paths: paths,
                destination_paths: vec![],
            };
        }

        let (sources, dest_slice) = paths.split_at(paths.len() - 1);
        let dest = &dest_slice[0];
        let source_paths: Vec<String> = sources.to_vec();

        // Determine if destination is a directory:
        // 1. Use the hint if provided (from filesystem check)
        // 2. Otherwise, use syntactic indicators:
        //    a. Trailing path separator indicates explicit directory target
        //    b. Multiple sources requires a directory destination
        let dest_is_dir = dest_is_directory.unwrap_or_else(|| {
            dest.ends_with('/') || dest.ends_with(std::path::MAIN_SEPARATOR) || sources.len() > 1
        });

        let destination_paths = if dest_is_dir {
            // Expand each source to its path within the destination directory
            source_paths
                .iter()
                .map(|src| {
                    // Extract the filename from the source path
                    let src_path = std::path::Path::new(src);
                    let filename = src_path.file_name().and_then(|f| f.to_str()).unwrap_or(src);

                    // Construct destination path
                    let dest_path = std::path::Path::new(dest);
                    dest_path.join(filename).to_string_lossy().to_string()
                })
                .collect()
        } else {
            // Simple rename: single source to single destination
            vec![dest.clone()]
        };

        // file_paths duplicates destination_paths for consistency with Write/Delete
        Self {
            file_paths: destination_paths.clone(),
            source_paths,
            destination_paths,
        }
    }

    /// Create a MoveOperation with filesystem-based directory detection.
    ///
    /// This method checks the filesystem to determine if the destination is a directory.
    /// Use this for `change.permission_asked` events where the filesystem state reflects
    /// the pre-move state.
    ///
    /// For `change.completed` events, use `from_move_paths()` with syntactic-only detection
    /// to avoid incorrect expansion (e.g., `mv docs docs-old` would incorrectly expand
    /// if we checked `docs-old.is_dir()` after the move).
    ///
    /// # Arguments
    ///
    /// * `paths` - All paths from the move command (sources + destination)
    /// * `cwd` - Current working directory for resolving relative paths
    #[must_use]
    pub fn from_move_paths_with_cwd(paths: Vec<String>, cwd: &std::path::Path) -> Self {
        if paths.len() < 2 {
            return Self::from_move_paths(paths);
        }

        // Get the destination (last path)
        let dest = &paths[paths.len() - 1];

        // Check if destination is a directory on the filesystem
        let dest_path = if std::path::Path::new(dest).is_absolute() {
            std::path::PathBuf::from(dest)
        } else {
            cwd.join(dest)
        };

        let dest_is_directory = dest_path.is_dir();

        Self::from_move_paths_with_hint(paths, Some(dest_is_directory))
    }
}

impl ChangeOperation {
    /// Get the operation name as a string
    #[must_use]
    pub fn operation_name(&self) -> &str {
        match self {
            Self::Write(_) => "write",
            Self::Delete(_) => "delete",
            Self::Move(_) => "move",
        }
    }

    /// Computed property: returns "true" if this is a Write operation, "" otherwise
    /// Enables truthiness check via `event.write` in flow conditions
    #[must_use]
    pub fn is_write(&self) -> &str {
        match self {
            Self::Write(_) => "true",
            _ => "",
        }
    }

    /// Computed property: returns "true" if this is a Delete operation, "" otherwise
    /// Enables truthiness check via `event.delete` in flow conditions
    #[must_use]
    pub fn is_delete(&self) -> &str {
        match self {
            Self::Delete(_) => "true",
            _ => "",
        }
    }

    /// Computed property: returns "true" if this is a Move operation, "" otherwise
    /// Enables truthiness check via `event.move` in flow conditions
    #[must_use]
    pub fn is_move(&self) -> &str {
        match self {
            Self::Move(_) => "true",
            _ => "",
        }
    }
}

// ============================================================================
// AikiChangeCompletedPayload
// ============================================================================

/// change.completed event payload
///
/// Fires after a file mutation operation completes (write, delete, or move).
/// This is the core provenance tracking event for all file mutations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AikiChangeCompletedPayload {
    pub session: AikiSession,
    pub cwd: PathBuf,
    pub timestamp: DateTime<Utc>,
    /// Tool that made the change (e.g., "Edit", "Write", "Delete", "Move", "Bash")
    pub tool_name: String,
    /// Whether the operation succeeded
    pub success: bool,
    /// Turn metadata (number, id, source) - defaults if not tracked
    #[serde(default)]
    pub turn: super::Turn,
    /// The specific operation that occurred (contains operation-specific fields)
    #[serde(flatten)]
    pub operation: ChangeOperation,
}

/// Resolve workspace CWD from file paths in the event.
///
/// When an agent runs in an isolated workspace (via context injection), its process
/// CWD is still the main repo. But the files it edits are inside the workspace dir.
/// This function detects that case and updates `payload.cwd` to the workspace root
/// so that subsequent `jj:` hook actions target the correct workspace.
fn resolve_workspace_cwd(payload: &mut AikiChangeCompletedPayload) {
    use crate::session::isolation::find_jj_root;

    // Get first file path from the operation
    let file_path = match &payload.operation {
        ChangeOperation::Write(op) => op.file_paths.first(),
        ChangeOperation::Delete(op) => op.file_paths.first(),
        ChangeOperation::Move(op) => op.destination_paths.first(),
    };

    let file_path = match file_path {
        Some(p) => p,
        None => return,
    };

    // Resolve to absolute path
    let abs_path = if std::path::Path::new(file_path).is_absolute() {
        PathBuf::from(file_path)
    } else {
        payload.cwd.join(file_path)
    };

    // Find the JJ root for this file path
    let file_jj_root = match find_jj_root(&abs_path) {
        Some(root) => root,
        None => return,
    };

    // If the file's JJ root differs from payload.cwd, update it.
    // This happens when the file is inside an isolated workspace.
    if file_jj_root != payload.cwd {
        debug_log(|| {
            format!(
                "Workspace CWD correction: {} -> {}",
                payload.cwd.display(),
                file_jj_root.display()
            )
        });
        payload.cwd = file_jj_root;
    }
}

/// Detect if the changed files belong to a different JJ repo than the session's
/// current repo root. If so, fire a `repo.changed` event before change.completed.
///
/// Uses the session file's `repo=<id>` entries to track which repo each session is
/// in across process invocations.
fn detect_repo_transition(payload: &AikiChangeCompletedPayload) {
    use crate::session::isolation::find_jj_root;
    use crate::session::AikiSessionFile;

    // Get first file path from the operation to determine the repo
    let file_path = match &payload.operation {
        ChangeOperation::Write(op) => op.file_paths.first(),
        ChangeOperation::Delete(op) => op.file_paths.first(),
        ChangeOperation::Move(op) => op.destination_paths.first(),
    };

    let file_path = match file_path {
        Some(p) => p,
        None => return,
    };

    // Resolve to absolute path if relative
    let abs_path = if std::path::Path::new(file_path).is_absolute() {
        PathBuf::from(file_path)
    } else {
        payload.cwd.join(file_path)
    };

    let new_root = match find_jj_root(&abs_path) {
        Some(root) => root,
        None => return,
    };

    // Read repo-id for the new root
    let new_repo_id = match crate::repos::ensure_repo_id(&new_root) {
        Ok(id) => id,
        _ => return,
    };

    let session_file = AikiSessionFile::new(&payload.session);
    let previous_repo_id = session_file.read_repo_id();

    // If the session is already tracked as being in the target repo, no transition.
    if previous_repo_id.as_deref() == Some(&new_repo_id) {
        return;
    }

    if let Err(e) = session_file.update_repo_id(&new_repo_id) {
        debug_log(|| format!("Failed to update session repo: {}", e));
        return;
    }

    // If no previous repo (first change in session): no transition to fire
    let previous_repo_id = match previous_repo_id {
        Some(id) => id,
        None => return,
    };

    debug_log(|| {
        format!(
            "Repo transition detected: {} -> {}",
            previous_repo_id, new_repo_id
        )
    });

    // Fire repo.changed event
    let root_name = new_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let repo_changed_payload = super::AikiRepoChangedPayload {
        session: payload.session.clone(),
        cwd: payload.cwd.clone(),
        timestamp: chrono::Utc::now(),
        repo: super::RepoRef::new(root_name, new_root, new_repo_id),
        previous_repo: None, // Previous root path not available from sidecar
    };

    if let Err(e) = super::handle_repo_changed(repo_changed_payload) {
        debug_log(|| format!("repo.changed handler error (non-fatal): {}", e));
    }
}

/// Handle change.completed event
///
/// This is the core provenance tracking event for file mutations.
/// Records metadata about the file changes in the JJ change description.
pub fn handle_change_completed(mut payload: AikiChangeCompletedPayload) -> Result<HookResult> {
    use super::prelude::execute_hook;

    // Look up current turn info from conversation history
    // This populates turn metadata (number, id, source) for provenance tracking
    // Uses global JJ repo at ~/.aiki/.jj/ for cross-repo conversation history
    // Defensive fallback: if history lookup fails, use defaults (turn=0, source=User)
    if !payload.turn.is_known() {
        let (turn_number, source) = match history::get_current_turn_info(
            &global::global_aiki_dir(),
            payload.session.uuid(),
        ) {
            Ok(result) => result,
            Err(e) => {
                debug_log(|| {
                    format!(
                        "History lookup failed for session {}, using defaults (turn=0): {}",
                        payload.session.uuid(),
                        e
                    )
                });
                (0, TurnSource::User)
            }
        };
        if turn_number > 0 {
            payload.turn = Turn::new(
                turn_number,
                generate_turn_id(payload.session.uuid(), turn_number),
                source.to_string(),
            );
        }
    }

    debug_log(|| {
        format!(
            "change.completed ({}) event from {:?}, session: {}, tool: {}, turn: {}",
            payload.operation.operation_name(),
            payload.session.agent_type(),
            payload.session.external_id(),
            payload.tool_name,
            payload.turn.number
        )
    });

    // Repo transition detection: fire repo.changed if file belongs to a different repo
    detect_repo_transition(&payload);

    // Workspace CWD correction: if the changed file is inside an isolated workspace,
    // update payload.cwd to the workspace root so that jj: actions in hooks
    // (e.g. jj metaedit for [aiki] metadata) target the workspace, not default@.
    // This handles the case where the agent process CWD is the main repo but the
    // agent was instructed to work in an isolated workspace via context injection.
    resolve_workspace_cwd(&mut payload);

    // Load core hook for fallback
    let core_hook = crate::flows::load_core_hook();

    // Build execution state from payload
    let mut state = AikiState::new(payload);

    // Execute hook via HookComposer (with fallback to bundled core hook)
    let _flow_result = execute_hook(
        EventType::ChangeCompleted,
        &mut state,
        &core_hook.handlers.change_completed,
    )?;

    // Extract failures from state
    let failures = state.take_failures();

    // change.completed never blocks - always allow (operation already completed)
    Ok(HookResult {
        context: None,
        decision: Decision::Allow,
        failures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_change_operation_write_serialization() {
        let op = ChangeOperation::Write(WriteOperation {
            file_paths: vec!["src/main.rs".to_string()],
            edit_details: vec![EditDetail::new("src/main.rs", "foo", "bar")],
        });

        let json = serde_json::to_string(&op).unwrap();
        assert!(json.contains(r#""operation":"write""#));
        assert!(json.contains(r#""file_paths":["src/main.rs"]"#));
    }

    #[test]
    fn test_change_operation_delete_serialization() {
        let op = ChangeOperation::Delete(DeleteOperation {
            file_paths: vec!["old_file.txt".to_string()],
        });

        let json = serde_json::to_string(&op).unwrap();
        assert!(json.contains(r#""operation":"delete""#));
        assert!(json.contains(r#""file_paths":["old_file.txt"]"#));
    }

    #[test]
    fn test_change_operation_move_serialization() {
        let op = ChangeOperation::Move(MoveOperation {
            file_paths: vec!["new/file.txt".to_string()],
            source_paths: vec!["old/file.txt".to_string()],
            destination_paths: vec!["new/file.txt".to_string()],
        });

        let json = serde_json::to_string(&op).unwrap();
        assert!(json.contains(r#""operation":"move""#));
        assert!(json.contains(r#""file_paths":["new/file.txt"]"#));
        assert!(json.contains(r#""source_paths":["old/file.txt"]"#));
        assert!(json.contains(r#""destination_paths":["new/file.txt"]"#));
    }

    #[test]
    fn test_change_operation_computed_properties() {
        let write_op = ChangeOperation::Write(WriteOperation {
            file_paths: vec![],
            edit_details: vec![],
        });
        let delete_op = ChangeOperation::Delete(DeleteOperation { file_paths: vec![] });
        let move_op = ChangeOperation::Move(MoveOperation {
            file_paths: vec![],
            source_paths: vec![],
            destination_paths: vec![],
        });

        // is_write
        assert_eq!(write_op.is_write(), "true");
        assert_eq!(delete_op.is_write(), "");
        assert_eq!(move_op.is_write(), "");

        // is_delete
        assert_eq!(write_op.is_delete(), "");
        assert_eq!(delete_op.is_delete(), "true");
        assert_eq!(move_op.is_delete(), "");

        // is_move
        assert_eq!(write_op.is_move(), "");
        assert_eq!(delete_op.is_move(), "");
        assert_eq!(move_op.is_move(), "true");
    }

    #[test]
    fn test_move_operation_simple_rename() {
        // mv foo bar -> simple rename
        let paths = vec!["foo".to_string(), "bar".to_string()];
        let op = MoveOperation::from_move_paths(paths);

        assert_eq!(op.file_paths, vec!["bar"]);
        assert_eq!(op.source_paths, vec!["foo"]);
        assert_eq!(op.destination_paths, vec!["bar"]);
    }

    #[test]
    fn test_move_operation_directory_rename() {
        // mv docs docs-old -> directory rename (single source, no trailing slash)
        // This should NOT expand to docs-old/docs
        let paths = vec!["docs".to_string(), "docs-old".to_string()];
        let op = MoveOperation::from_move_paths(paths);

        assert_eq!(op.file_paths, vec!["docs-old"]);
        assert_eq!(op.source_paths, vec!["docs"]);
        assert_eq!(op.destination_paths, vec!["docs-old"]);
    }

    #[test]
    fn test_move_operation_multi_file_to_directory_trailing_slash() {
        // mv foo bar dir/ -> multiple files into directory (trailing slash)
        let paths = vec![
            "foo".to_string(),
            "bar".to_string(),
            "target_dir/".to_string(),
        ];
        let op = MoveOperation::from_move_paths(paths);

        assert_eq!(op.file_paths, vec!["target_dir/foo", "target_dir/bar"]);
        assert_eq!(op.source_paths, vec!["foo", "bar"]);
        assert_eq!(
            op.destination_paths,
            vec!["target_dir/foo", "target_dir/bar"]
        );
    }

    #[test]
    fn test_move_operation_multi_file_to_directory_no_slash() {
        // mv foo bar dir -> multiple sources implies directory destination
        let paths = vec![
            "foo".to_string(),
            "bar".to_string(),
            "target_dir".to_string(),
        ];
        let op = MoveOperation::from_move_paths(paths);

        assert_eq!(op.file_paths, vec!["target_dir/foo", "target_dir/bar"]);
        assert_eq!(op.source_paths, vec!["foo", "bar"]);
        assert_eq!(
            op.destination_paths,
            vec!["target_dir/foo", "target_dir/bar"]
        );
    }

    #[test]
    fn test_move_operation_preserves_nested_paths() {
        // mv src/foo.rs lib/bar.rs dest/ -> should preserve filenames
        let paths = vec![
            "src/foo.rs".to_string(),
            "lib/bar.rs".to_string(),
            "dest/".to_string(),
        ];
        let op = MoveOperation::from_move_paths(paths);

        assert_eq!(op.file_paths, vec!["dest/foo.rs", "dest/bar.rs"]);
        assert_eq!(op.source_paths, vec!["src/foo.rs", "lib/bar.rs"]);
        assert_eq!(op.destination_paths, vec!["dest/foo.rs", "dest/bar.rs"]);
    }

    #[test]
    fn test_move_operation_single_path_invalid() {
        // Single path is not a valid move command
        let paths = vec!["foo".to_string()];
        let op = MoveOperation::from_move_paths(paths);

        assert_eq!(op.file_paths, Vec::<String>::new());
        assert_eq!(op.source_paths, vec!["foo"]);
        assert_eq!(op.destination_paths, Vec::<String>::new());
    }

    #[test]
    fn test_move_operation_empty_paths() {
        // Empty paths
        let paths: Vec<String> = vec![];
        let op = MoveOperation::from_move_paths(paths);

        assert!(op.file_paths.is_empty());
        assert!(op.source_paths.is_empty());
        assert!(op.destination_paths.is_empty());
    }

    // =========================================================================
    // Directory hint tests
    // =========================================================================

    #[test]
    fn test_move_operation_with_directory_hint_true() {
        // With hint: destination IS a directory
        // mv file.txt existing_dir -> should expand to existing_dir/file.txt
        let paths = vec!["file.txt".to_string(), "existing_dir".to_string()];
        let op = MoveOperation::from_move_paths_with_hint(paths, Some(true));

        assert_eq!(op.file_paths, vec!["existing_dir/file.txt"]);
        assert_eq!(op.source_paths, vec!["file.txt"]);
        assert_eq!(op.destination_paths, vec!["existing_dir/file.txt"]);
    }

    #[test]
    fn test_move_operation_with_directory_hint_false() {
        // With hint: destination is NOT a directory
        // mv docs docs-old -> simple rename, no expansion
        let paths = vec!["docs".to_string(), "docs-old".to_string()];
        let op = MoveOperation::from_move_paths_with_hint(paths, Some(false));

        assert_eq!(op.file_paths, vec!["docs-old"]);
        assert_eq!(op.source_paths, vec!["docs"]);
        assert_eq!(op.destination_paths, vec!["docs-old"]);
    }

    #[test]
    fn test_move_operation_hint_overrides_trailing_slash() {
        // Hint should override trailing slash indicator
        // Even with trailing slash, if hint says "not a dir", don't expand
        let paths = vec!["file.txt".to_string(), "dest/".to_string()];
        let op = MoveOperation::from_move_paths_with_hint(paths, Some(false));

        // With hint=false, should NOT expand despite trailing slash
        assert_eq!(op.file_paths, vec!["dest/"]);
        assert_eq!(op.destination_paths, vec!["dest/"]);
    }

    #[test]
    fn test_move_operation_hint_overrides_multi_source() {
        // With explicit hint=false, don't expand even with multiple sources
        // This is an unusual case but tests that hint takes precedence
        let paths = vec![
            "file1.txt".to_string(),
            "file2.txt".to_string(),
            "dest".to_string(),
        ];
        let op = MoveOperation::from_move_paths_with_hint(paths, Some(false));

        // Even with multiple sources, hint overrides
        assert_eq!(op.file_paths, vec!["dest"]);
        assert_eq!(op.destination_paths, vec!["dest"]);
    }

    #[test]
    fn test_move_operation_with_cwd_directory_exists() {
        use tempfile::TempDir;

        // Create a temp directory with a subdirectory
        let temp_dir = TempDir::new().unwrap();
        let existing_dir = temp_dir.path().join("existing_dir");
        std::fs::create_dir(&existing_dir).unwrap();

        // mv file.txt existing_dir -> should detect existing_dir is a directory
        let paths = vec!["file.txt".to_string(), "existing_dir".to_string()];
        let op = MoveOperation::from_move_paths_with_cwd(paths, temp_dir.path());

        assert_eq!(op.file_paths, vec!["existing_dir/file.txt"]);
        assert_eq!(op.destination_paths, vec!["existing_dir/file.txt"]);
    }

    #[test]
    fn test_move_operation_with_cwd_destination_not_directory() {
        use tempfile::TempDir;

        // Create a temp directory with no subdirectory named "new_name"
        let temp_dir = TempDir::new().unwrap();

        // mv file.txt new_name -> new_name doesn't exist, so it's a rename
        let paths = vec!["file.txt".to_string(), "new_name".to_string()];
        let op = MoveOperation::from_move_paths_with_cwd(paths, temp_dir.path());

        assert_eq!(op.file_paths, vec!["new_name"]);
        assert_eq!(op.destination_paths, vec!["new_name"]);
    }
}
