//! Built-in functions for the aiki/core hook namespace
//!
//! This module contains native Rust implementations of functions that can be called
//! from flow definitions using the function call syntax.
//!
//! # Functions
//!
//! ## Metadata Generation (for change.* events)
//! - [`build_write_metadata`] - Build metadata for write operations
//! - [`build_delete_metadata`] - Build metadata for delete operations
//! - [`build_move_metadata`] - Build metadata for move operations
//! - [`build_human_metadata_change_pre`] - Build metadata for human changes (pre-permission)
//! - [`build_human_metadata_change_post`] - Build metadata for human changes (post-completion)
//!
//! ## Edit Analysis
//! - [`classify_edits_change`] - Classify edits to detect user modifications
//!
//! ## Edit Separation
//! - [`prepare_separation_change`] - Prepare files for separation by reconstructing AI-only content
//! - [`write_ai_files_change`] - Write AI-only content to working copy
//! - [`restore_original_files_change`] - Restore original content after jj split
//!
//! ## Git Integration
//! - [`generate_coauthors`] - Generate co-authors for Git commit from staged changes
//!
//! ## Utility Functions
//! - [`get_git_user`] - Get the git user (name + email) from git config

use crate::cache::debug_log;
use crate::error::{AikiError, Result};
use crate::events::{
    AikiChangeCompletedPayload, AikiChangePermissionAskedPayload, AikiCommitMessageStartedPayload,
    ChangeOperation,
};
use crate::flows::state::ActionResult;
use crate::provenance::authors::{AuthorScope, AuthorsCommand, OutputFormat};
use crate::provenance::record::ProvenanceRecord;
use crate::tasks::{manager, storage};
use anyhow::Context;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
// =============================================================================
// Utility Functions
// =============================================================================

/// Get the git user (name + email) from git config
///
/// Returns the user in "Name <email>" format, or None if git config is not set.
pub fn get_git_user() -> Option<String> {
    let name = Command::new("git")
        .args(["config", "user.name"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())?;

    let email = Command::new("git")
        .args(["config", "user.email"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_string())?;

    Some(format!("{} <{}>", name, email))
}

/// Get in-progress task IDs for a session
///
/// Queries the task storage to find tasks that are currently in-progress
/// and claimed by the given session. Returns task IDs ordered by most
/// recently started first.
///
/// Returns an empty Vec if tasks cannot be read (graceful degradation).
fn get_in_progress_tasks_for_session(cwd: &Path, session_id: &str) -> Vec<String> {
    // Read task events from storage (gracefully handle errors)
    let events = match storage::read_events(cwd) {
        Ok(events) => events,
        Err(e) => {
            debug_log(|| format!("[flows/core] Failed to read task events: {}", e));
            return Vec::new();
        }
    };

    // Materialize graph and get in-progress ones for this session
    let graph = crate::tasks::graph::materialize_graph(&events);
    let mut task_ids = manager::get_in_progress_task_ids_for_session(&graph.tasks, session_id);

    // Expand with ancestor chain for each in-progress task
    let mut ancestors = Vec::new();
    for id in &task_ids {
        ancestors.extend(graph.ancestor_chain(id));
    }

    // Deduplicate preserving order (leaf tasks first, then ancestors)
    let mut seen = std::collections::HashSet::new();
    task_ids.extend(ancestors);
    task_ids.retain(|id| seen.insert(id.clone()));

    task_ids
}

/// Get the prompt change_id for a session
///
/// Queries the conversation history to find the latest prompt's JJ change_id.
/// Returns None if lookup fails (graceful degradation).
fn get_prompt_change_id_for_session(session_id: &str) -> Option<String> {
    use crate::global;
    use crate::history;

    match history::get_latest_prompt_change_id(&global::global_aiki_dir(), session_id) {
        Ok(change_id) => change_id,
        Err(e) => {
            debug_log(|| {
                format!(
                    "[flows/core] Failed to get prompt change_id for session {}: {}",
                    session_id, e
                )
            });
            None
        }
    }
}

// =============================================================================
// Metadata Generation Functions
// =============================================================================

/// Build metadata for Write operations (change.completed with operation=write)
///
/// This function validates that the event contains a Write operation and generates
/// provenance metadata with write-specific fields (edit_details, file_paths).
///
/// # Returns
/// JSON with author and message fields:
/// ```json
/// {
///   "author": "Claude <noreply@anthropic.com>",
///   "message": "[aiki]\nauthor=claude\n...[/aiki]"
/// }
/// ```
pub fn build_write_metadata(
    event: &AikiChangeCompletedPayload,
    context: Option<&crate::flows::state::AikiState>,
) -> Result<ActionResult> {
    // Validate operation is Write
    let _write_op = match &event.operation {
        ChangeOperation::Write(op) => op,
        _ => {
            return Err(AikiError::Other(anyhow::anyhow!(
                "build_write_metadata called on non-Write operation: {:?}",
                event.operation.operation_name()
            )))
        }
    };

    // Get in-progress tasks for this session
    // Note: Tasks are stored with the session UUID (not external_id), so we must use uuid() here
    let task_ids = get_in_progress_tasks_for_session(&event.cwd, event.session.uuid());

    // Get prompt change_id for this session (if available)
    let prompt_change_id = get_prompt_change_id_for_session(event.session.uuid());

    // Create provenance record from change event with task IDs and prompt
    let mut provenance = ProvenanceRecord::from_change_completed_event(event)
        .with_tasks(task_ids)
        .with_prompt_change_id(prompt_change_id);

    // Check if we have overlapping user edits and should add coauthor
    if let Some(ctx) = context {
        if let Some(detection) = ctx.get_variable("detection") {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(detection) {
                if let Some(classification_type) =
                    json.get("classification_type").and_then(|v| v.as_str())
                {
                    if classification_type == "OverlappingUserEdits" {
                        provenance.coauthor = get_git_user();
                    }
                }
            }
        }
    }

    let message = provenance.to_description();
    let author = event.session.agent_type().git_author();

    debug_log(|| {
        format!(
            "[flows/core] Generated write metadata - author: {}, message length: {}",
            author,
            message.len()
        )
    });

    let json = serde_json::json!({
        "author": author,
        "message": message,
    });

    Ok(ActionResult {
        success: true,
        exit_code: Some(0),
        stdout: json.to_string(),
        stderr: String::new(),
    })
}

/// Build metadata for Delete operations (change.completed with operation=delete)
///
/// This function validates that the event contains a Delete operation and generates
/// provenance metadata with delete-specific fields (file_paths only, no edits).
///
/// # Returns
/// JSON with author and message fields
pub fn build_delete_metadata(
    event: &AikiChangeCompletedPayload,
    _context: Option<&crate::flows::state::AikiState>,
) -> Result<ActionResult> {
    // Validate operation is Delete
    let _delete_op = match &event.operation {
        ChangeOperation::Delete(op) => op,
        _ => {
            return Err(AikiError::Other(anyhow::anyhow!(
                "build_delete_metadata called on non-Delete operation: {:?}",
                event.operation.operation_name()
            )))
        }
    };

    // Get in-progress tasks for this session
    // Note: Tasks are stored with the session UUID (not external_id), so we must use uuid() here
    let task_ids = get_in_progress_tasks_for_session(&event.cwd, event.session.uuid());

    // Get prompt change_id for this session (if available)
    let prompt_change_id = get_prompt_change_id_for_session(event.session.uuid());

    let provenance = ProvenanceRecord::from_change_completed_event(event)
        .with_tasks(task_ids)
        .with_prompt_change_id(prompt_change_id);
    let message = provenance.to_description();
    let author = event.session.agent_type().git_author();

    debug_log(|| {
        format!(
            "[flows/core] Generated delete metadata - author: {}, message length: {}",
            author,
            message.len()
        )
    });

    let json = serde_json::json!({
        "author": author,
        "message": message,
    });

    Ok(ActionResult {
        success: true,
        exit_code: Some(0),
        stdout: json.to_string(),
        stderr: String::new(),
    })
}

/// Build metadata for Move operations (change.completed with operation=move)
///
/// This function validates that the event contains a Move operation and generates
/// provenance metadata with move-specific fields (source_paths, destination_paths).
///
/// # Returns
/// JSON with author and message fields
pub fn build_move_metadata(
    event: &AikiChangeCompletedPayload,
    _context: Option<&crate::flows::state::AikiState>,
) -> Result<ActionResult> {
    // Validate operation is Move
    let _move_op = match &event.operation {
        ChangeOperation::Move(op) => op,
        _ => {
            return Err(AikiError::Other(anyhow::anyhow!(
                "build_move_metadata called on non-Move operation: {:?}",
                event.operation.operation_name()
            )))
        }
    };

    // Get in-progress tasks for this session
    // Note: Tasks are stored with the session UUID (not external_id), so we must use uuid() here
    let task_ids = get_in_progress_tasks_for_session(&event.cwd, event.session.uuid());

    // Get prompt change_id for this session (if available)
    let prompt_change_id = get_prompt_change_id_for_session(event.session.uuid());

    let provenance = ProvenanceRecord::from_change_completed_event(event)
        .with_tasks(task_ids)
        .with_prompt_change_id(prompt_change_id);
    let message = provenance.to_description();
    let author = event.session.agent_type().git_author();

    debug_log(|| {
        format!(
            "[flows/core] Generated move metadata - author: {}, message length: {}",
            author,
            message.len()
        )
    });

    let json = serde_json::json!({
        "author": author,
        "message": message,
    });

    Ok(ActionResult {
        success: true,
        exit_code: Some(0),
        stdout: json.to_string(),
        stderr: String::new(),
    })
}

/// Build metadata for human changes during an AI edit session
///
/// Creates an `[aiki]` block where the author is the git user (human)
/// in "Name <email>" format. This is used when we detect user modifications
/// during an AI editing session (AdditiveUserEdits case).
///
/// # Required Event Variables
/// - `event.session.uuid` - Session UUID
///
/// # Returns
/// An ActionResult with JSON output: `{"author": "...", "message": "..."}`
///
/// # Example Flow Usage
/// ```yaml
/// change.done:
///   - if: $detection.classification_type == "AdditiveUserEdits"
///     then:
///       - let: user_metadata = self.build_user_metadata
///       - jj: metaedit --message "$user_metadata.message" --author "$user_metadata.author"
/// ```
/// Build metadata for human changes (works with both change.permission_asked and change.done)
///
/// Creates an [aiki] metadata block with the git user as author and author_type=human.
/// This is used for any changes made by the human user (before or during AI edits).
///
/// # Returns
/// JSON with author and message fields:
/// ```json
/// {
///   "author": "Name <email>",
///   "message": "[aiki]\nauthor=Name <email>\nauthor_type=human\nsession=session-id\n[/aiki]"
/// }
/// ```
fn build_human_metadata_impl(session: &crate::session::AikiSession) -> Result<ActionResult> {
    let git_user = get_git_user().ok_or_else(|| {
        crate::error::AikiError::Other(anyhow::anyhow!(
            "Git user not configured. Run 'git config user.name' and 'git config user.email'"
        ))
    })?;

    // Create a metadata block with the git user as author (no coauthor)
    let lines = vec![
        "[aiki]".to_string(),
        format!("author={}", git_user),
        "author_type=human".to_string(),
        format!("session={}", session.uuid()),
    ];

    let message = format!("{}\n[/aiki]", lines.join("\n"));

    debug_log(|| {
        format!(
            "[flows/core] Generated human metadata - author: {}",
            git_user
        )
    });

    let json = serde_json::json!({
        "author": git_user,
        "message": message,
    });

    Ok(ActionResult {
        success: true,
        exit_code: Some(0),
        stdout: json.to_string(),
        stderr: String::new(),
    })
}

/// Build human metadata - works with change.permission_asked events
pub fn build_human_metadata_change_pre(
    event: &AikiChangePermissionAskedPayload,
    _context: Option<&crate::flows::state::AikiState>,
) -> Result<ActionResult> {
    build_human_metadata_impl(&event.session)
}

/// Build human metadata - works with change.completed events (new unified event)
pub fn build_human_metadata_change_post(
    event: &AikiChangeCompletedPayload,
    _context: Option<&crate::flows::state::AikiState>,
) -> Result<ActionResult> {
    build_human_metadata_impl(&event.session)
}

// =============================================================================
// Edit Analysis Functions
// =============================================================================

/// Classification result for edit detection
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditClassification {
    /// All AI edits found exactly in file (no user modifications)
    ExactMatch,
    /// User added more changes to same file (AI edits present + extra changes)
    AdditiveUserEdits,
    /// User modified AI's changes (AI edits not fully present or conflicting)
    OverlappingUserEdits,
}

/// Classify edits from a change.completed event (Write operation only)
///
/// This variant accepts the unified change event type. It validates that the
/// operation is Write and extracts the edit details for classification.
pub fn classify_edits_change(event: &AikiChangeCompletedPayload) -> Result<ActionResult> {
    // Validate operation is Write
    let write_op = match &event.operation {
        ChangeOperation::Write(op) => op,
        _ => {
            return Err(AikiError::Other(anyhow::anyhow!(
                "classify_edits_change called on non-Write operation: {:?}",
                event.operation.operation_name()
            )))
        }
    };

    // If no edit details, we can't classify - treat as exact match (AI-only)
    if write_op.edit_details.is_empty() {
        debug_log(|| "[flows/core] No edit details available - assuming AI-only changes");
        return Ok(ActionResult {
            success: true,
            exit_code: Some(0),
            stdout: "ExactMatch".to_string(),
            stderr: String::new(),
        });
    }

    // Group edit details by file
    let mut files_with_edits: HashSet<String> = HashSet::new();
    let mut details = serde_json::Map::new();

    for edit_detail in &write_op.edit_details {
        files_with_edits.insert(edit_detail.file_path.clone());
    }

    // Classify each file
    let mut all_exact = true;
    let mut has_additive = false;
    let mut has_overlapping = false;

    for file_path in &files_with_edits {
        let classification = classify_file_change(file_path, event, write_op)?;

        let class_str = match classification {
            EditClassification::ExactMatch => "ExactMatch",
            EditClassification::AdditiveUserEdits => {
                has_additive = true;
                all_exact = false;
                "AdditiveUserEdits"
            }
            EditClassification::OverlappingUserEdits => {
                has_overlapping = true;
                all_exact = false;
                "OverlappingUserEdits"
            }
        };

        details.insert(file_path.clone(), serde_json::json!(class_str));
    }

    // Check for extra files (in file_paths but not in edit_details)
    let extra_files: Vec<String> = write_op
        .file_paths
        .iter()
        .filter(|p| !files_with_edits.contains(*p))
        .cloned()
        .collect();

    if !extra_files.is_empty() {
        all_exact = false;
    }

    // Determine overall classification type
    let classification_type = if has_overlapping {
        "OverlappingUserEdits"
    } else if has_additive || !extra_files.is_empty() {
        "AdditiveUserEdits"
    } else if all_exact {
        "ExactMatch"
    } else {
        "Unknown"
    };

    debug_log(|| {
        format!(
            "[flows/core] Classification: type={}, extra_files={}",
            classification_type,
            extra_files.len()
        )
    });

    Ok(ActionResult {
        success: true,
        exit_code: Some(0),
        stdout: classification_type.to_string(),
        stderr: String::new(),
    })
}

/// Classify edits for a single file (change event variant)
fn classify_file_change(
    file_path: &str,
    event: &AikiChangeCompletedPayload,
    write_op: &crate::events::WriteOperation,
) -> Result<EditClassification> {
    // Read current file content
    let full_path = event.cwd.join(file_path);
    let current_content = read_file_safe(&full_path)?;

    // Get all edits for this file
    let file_edits: Vec<_> = write_op
        .edit_details
        .iter()
        .filter(|e| e.file_path == file_path)
        .collect();

    if file_edits.is_empty() {
        return Ok(EditClassification::ExactMatch);
    }

    // Check each edit
    let mut all_new_present = true;
    let mut any_old_present = false;

    for edit in &file_edits {
        if !edit.new_string.is_empty() && !current_content.contains(&edit.new_string) {
            all_new_present = false;
        }

        if !edit.old_string.is_empty() {
            let old_present = current_content.contains(&edit.old_string);
            let new_present = current_content.contains(&edit.new_string);

            if old_present && new_present {
                if !edit.new_string.contains(&edit.old_string) {
                    any_old_present = true;
                }
            } else if old_present {
                any_old_present = true;
            }
        }
    }

    if all_new_present && !any_old_present {
        Ok(EditClassification::ExactMatch)
    } else if any_old_present {
        Ok(EditClassification::OverlappingUserEdits)
    } else {
        Ok(EditClassification::AdditiveUserEdits)
    }
}

/// Read file content safely, handling missing files
fn read_file_safe(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| {
        AikiError::Other(anyhow::anyhow!(
            "Failed to read file '{}': {}",
            path.display(),
            e
        ))
    })
}

// =============================================================================
// Edit Separation Functions
// =============================================================================

/// Prepare files for separation (change.completed event variant)
///
/// This variant accepts the unified change event type. It validates that the
/// operation is Write and extracts the necessary data for separation.
pub fn prepare_separation_change(event: &AikiChangeCompletedPayload) -> Result<ActionResult> {
    // Validate operation is Write
    let write_op = match &event.operation {
        ChangeOperation::Write(op) => op,
        _ => {
            return Err(AikiError::Other(anyhow::anyhow!(
                "prepare_separation_change called on non-Write operation: {:?}",
                event.operation.operation_name()
            )))
        }
    };

    if write_op.edit_details.is_empty() {
        debug_log(|| "[flows/core] No edit details available, skipping preparation");
        return Ok(ActionResult {
            success: true,
            exit_code: Some(0),
            stdout: serde_json::json!({
                "skipped": true,
                "reason": "no_edit_details"
            })
            .to_string(),
            stderr: String::new(),
        });
    }

    // Get in-progress tasks for this session
    // Note: Tasks are stored with the session UUID (not external_id), so we must use uuid() here
    let task_ids = get_in_progress_tasks_for_session(&event.cwd, event.session.uuid());

    // Get prompt change_id for this session (if available)
    let prompt_change_id = get_prompt_change_id_for_session(event.session.uuid());

    let provenance = ProvenanceRecord::from_change_completed_event(event)
        .with_tasks(task_ids)
        .with_prompt_change_id(prompt_change_id);
    let ai_message = provenance.to_description();
    let ai_author = event.session.agent_type().git_author();

    let files_with_edits: HashSet<String> = write_op
        .edit_details
        .iter()
        .map(|e| e.file_path.clone())
        .collect();

    let ai_only_files: Vec<String> = write_op
        .file_paths
        .iter()
        .filter(|p| !files_with_edits.contains(*p))
        .cloned()
        .collect();

    debug_log(|| {
        format!(
            "[flows/core] Preparing separation for {} files with edits, {} AI-only files",
            files_with_edits.len(),
            ai_only_files.len()
        )
    });

    let mut files_data = serde_json::Map::new();

    for file_path in &files_with_edits {
        let full_path = event.cwd.join(file_path);

        let current_content = fs::read_to_string(&full_path).map_err(|e| {
            AikiError::Other(anyhow::anyhow!(
                "Failed to read file '{}': {}",
                full_path.display(),
                e
            ))
        })?;

        let ai_only_content =
            reconstruct_ai_only_content_change(&current_content, file_path, write_op)?;

        files_data.insert(
            file_path.to_string(),
            serde_json::json!({
                "ai_only_content": ai_only_content,
                "original_content": current_content,
            }),
        );
    }

    let mut all_ai_files: Vec<String> = files_with_edits
        .iter()
        .map(|p| normalize_path_for_jj(p, &event.cwd))
        .collect();
    all_ai_files.extend(
        ai_only_files
            .iter()
            .map(|p| normalize_path_for_jj(p, &event.cwd)),
    );
    let file_list = all_ai_files.join(" ");

    let json = serde_json::json!({
        "ai_message": ai_message,
        "ai_author": ai_author,
        "file_list": file_list,
        "files": files_data,
    });

    Ok(ActionResult {
        success: true,
        exit_code: Some(0),
        stdout: json.to_string(),
        stderr: String::new(),
    })
}

/// Reconstruct AI-only content (change event variant)
fn reconstruct_ai_only_content_change(
    current_content: &str,
    file_path: &str,
    write_op: &crate::events::WriteOperation,
) -> Result<String> {
    let mut ai_content = current_content.to_string();

    let file_edits: Vec<_> = write_op
        .edit_details
        .iter()
        .filter(|e| e.file_path == file_path)
        .collect();

    for edit in &file_edits {
        if !edit.new_string.is_empty() && ai_content.contains(&edit.new_string) {
            continue;
        } else if !edit.old_string.is_empty() && ai_content.contains(&edit.old_string) {
            ai_content = ai_content.replace(&edit.old_string, &edit.new_string);
        }
    }

    Ok(ai_content)
}

/// Write AI-only content to files (change.completed event variant)
pub fn write_ai_files_change(
    event: &AikiChangeCompletedPayload,
    context: Option<&crate::flows::state::AikiState>,
) -> Result<ActionResult> {
    let ctx = context.ok_or_else(|| {
        AikiError::Other(anyhow::anyhow!(
            "write_ai_files_change requires context with prep variable"
        ))
    })?;

    let prep = ctx.get_variable("prep").ok_or_else(|| {
        AikiError::Other(anyhow::anyhow!(
            "write_ai_files_change requires prep variable from prepare_separation"
        ))
    })?;

    let prep_json: serde_json::Value = serde_json::from_str(prep)
        .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to parse prep variable: {}", e)))?;

    let files = prep_json
        .get("files")
        .and_then(|f| f.as_object())
        .ok_or_else(|| AikiError::Other(anyhow::anyhow!("Missing 'files' in prep")))?;

    for (file_path, file_data) in files {
        let ai_only_content = file_data
            .get("ai_only_content")
            .and_then(|c| c.as_str())
            .ok_or_else(|| {
                AikiError::Other(anyhow::anyhow!(
                    "Missing 'ai_only_content' for file '{}'",
                    file_path
                ))
            })?;

        let full_path = event.cwd.join(file_path);
        fs::write(&full_path, ai_only_content).map_err(|e| {
            AikiError::Other(anyhow::anyhow!(
                "Failed to write AI-only content to '{}': {}",
                full_path.display(),
                e
            ))
        })?;

        debug_log(|| {
            format!(
                "[flows/core] Wrote AI-only content to '{}'",
                full_path.display()
            )
        });
    }

    Ok(ActionResult {
        success: true,
        exit_code: Some(0),
        stdout: "AI-only content written".to_string(),
        stderr: String::new(),
    })
}

/// Restore original content after jj split (change.completed event variant)
pub fn restore_original_files_change(
    event: &AikiChangeCompletedPayload,
    context: Option<&crate::flows::state::AikiState>,
) -> Result<ActionResult> {
    let ctx = context.ok_or_else(|| {
        AikiError::Other(anyhow::anyhow!(
            "restore_original_files_change requires context with prep variable"
        ))
    })?;

    let prep = ctx.get_variable("prep").ok_or_else(|| {
        AikiError::Other(anyhow::anyhow!(
            "restore_original_files_change requires prep variable from prepare_separation"
        ))
    })?;

    let prep_json: serde_json::Value = serde_json::from_str(prep)
        .map_err(|e| AikiError::Other(anyhow::anyhow!("Failed to parse prep variable: {}", e)))?;

    let files = prep_json
        .get("files")
        .and_then(|f| f.as_object())
        .ok_or_else(|| AikiError::Other(anyhow::anyhow!("Missing 'files' in prep")))?;

    for (file_path, file_data) in files {
        let original_content = file_data
            .get("original_content")
            .and_then(|c| c.as_str())
            .ok_or_else(|| {
                AikiError::Other(anyhow::anyhow!(
                    "Missing 'original_content' for file '{}'",
                    file_path
                ))
            })?;

        let full_path = event.cwd.join(file_path);
        fs::write(&full_path, original_content).map_err(|e| {
            AikiError::Other(anyhow::anyhow!(
                "Failed to restore content to '{}': {}",
                full_path.display(),
                e
            ))
        })?;

        debug_log(|| {
            format!(
                "[flows/core] Restored original content to '{}'",
                full_path.display()
            )
        });
    }

    Ok(ActionResult {
        success: true,
        exit_code: Some(0),
        stdout: "Original content restored".to_string(),
        stderr: String::new(),
    })
}

/// Normalize a file path for use with jj commands
///
/// JJ commands expect relative paths from the repo root. This function:
/// 1. If path is absolute and starts with cwd, make it relative to cwd
/// 2. If path is already relative, return as-is
/// 3. If path is absolute but not under cwd, return as-is (will likely fail, but let jj handle the error)
fn normalize_path_for_jj(file_path: &str, cwd: &Path) -> String {
    let path = Path::new(file_path);

    // If already relative, return as-is
    if path.is_relative() {
        return file_path.to_string();
    }

    // If absolute, try to make it relative to cwd
    if let Ok(relative) = path.strip_prefix(cwd) {
        relative.to_string_lossy().to_string()
    } else {
        // Path is absolute but not under cwd - return as-is
        file_path.to_string()
    }
}

// =============================================================================
// Git Integration Functions
// =============================================================================

/// Generate co-authors for Git commit from staged changes
///
/// This function is called during commit.message_started events to generate Git trailer
/// lines (Co-authored-by:) for AI agents that contributed to the staged changes.
pub fn generate_coauthors(event: &AikiCommitMessageStartedPayload) -> Result<ActionResult> {
    // Create authors command using the event's working directory
    let authors_cmd = AuthorsCommand::new(&event.cwd);

    // Get authors from Git staged changes in Git trailer format
    let coauthors = authors_cmd
        .get_authors(AuthorScope::GitStaged, OutputFormat::Git)
        .context("Failed to get co-authors from staged changes")?;

    Ok(ActionResult {
        success: true,
        exit_code: Some(0),
        stdout: coauthors,
        stderr: String::new(),
    })
}

// =============================================================================
// Task System Functions
// =============================================================================

/// Get the size of the ready task queue for a specific agent
///
/// Returns the number of open, unblocked tasks visible to the given agent,
/// filtered by the current scope (same as `aiki task list`).
/// This is used for context injection in flows.
pub fn task_list_size_for_agent(
    cwd: &Path,
    agent: &crate::agents::AgentType,
) -> Result<ActionResult> {
    let events = crate::tasks::storage::read_events(cwd)?;
    let graph = crate::tasks::graph::materialize_graph(&events);
    let scope_set = crate::tasks::manager::get_current_scope_set(&graph);
    let ready = crate::tasks::manager::get_ready_queue_for_agent_scoped(&graph, &scope_set, agent);

    Ok(ActionResult {
        success: true,
        exit_code: Some(0),
        stdout: ready.len().to_string(),
        stderr: String::new(),
    })
}

/// Get the IDs of all tasks currently in progress
///
/// Returns a JSON array of task IDs that are currently being worked on.
/// This is used for context injection in flows.
pub fn task_in_progress(cwd: &Path) -> Result<ActionResult> {
    let events = crate::tasks::storage::read_events(cwd)?;
    let tasks = crate::tasks::graph::materialize_graph(&events).tasks;
    let in_progress = crate::tasks::manager::get_in_progress(&tasks);

    // Return as JSON array of IDs
    let ids: Vec<&str> = in_progress.iter().map(|t| t.id.as_str()).collect();
    let json = serde_json::to_string(&ids).unwrap_or_else(|_| "[]".to_string());

    Ok(ActionResult {
        success: true,
        exit_code: Some(0),
        stdout: json,
        stderr: String::new(),
    })
}

// =============================================================================
// Workspace Isolation Functions
// =============================================================================

pub fn workspace_ensure_isolated(
    session: &crate::session::AikiSession,
    cwd: &Path,
) -> Result<ActionResult> {
    use crate::session::isolation;

    let repo_root = match isolation::find_jj_root(cwd) {
        Some(root) => root,
        None => {
            debug_log(|| "[workspace] Not in a JJ repo, skipping workspace creation");
            return Ok(ActionResult::success());
        }
    };

    match isolation::create_isolated_workspace(&repo_root, session.uuid()) {
        Ok(ws) => {
            debug_log(|| {
                format!(
                    "[workspace] Workspace '{}' ready at {}",
                    ws.name,
                    ws.path.display()
                )
            });
            Ok(ActionResult {
                success: true,
                exit_code: Some(0),
                stdout: ws.path.to_string_lossy().to_string(),
                stderr: String::new(),
            })
        }
        Err(e) => {
            eprintln!(
                "[aiki] Warning: workspace creation failed, continuing in main workspace: {}",
                e
            );
            Ok(ActionResult {
                success: true,
                exit_code: Some(0),
                stdout: String::new(),
                stderr: format!("fallback: {}", e),
            })
        }
    }
}

/// Absorb all workspaces for the current session back into parent/main.
///
/// Called from `session.ended` hook.
/// Iterates `/tmp/aiki/*/<session-uuid>/`, absorbs and cleans up each.
/// No-op if no workspaces exist (solo session that never needed isolation).
///
/// # Returns
/// ActionResult with stdout being the count of absorbed workspaces
pub fn workspace_absorb_all(session: &crate::session::AikiSession) -> Result<ActionResult> {
    use crate::session::isolation;

    let session_uuid = session.uuid();
    let workspaces_dir = crate::session::isolation::workspaces_dir();

    if !workspaces_dir.exists() {
        return Ok(ActionResult {
            success: true,
            exit_code: Some(0),
            stdout: "0".to_string(),
            stderr: String::new(),
        });
    }

    let parent_session_uuid: Option<String> = std::env::var("AIKI_PARENT_SESSION_UUID").ok();
    let mut absorbed = 0u32;
    let mut has_conflicts = false;
    let mut last_conflicted_files = String::new();
    let mut seen_repo_roots: Vec<PathBuf> = Vec::new();

    // Scan all repo-id directories for workspaces belonging to this session
    let entries = match std::fs::read_dir(&workspaces_dir) {
        Ok(e) => e,
        Err(_) => {
            return Ok(ActionResult {
                success: true,
                exit_code: Some(0),
                stdout: "0".to_string(),
                stderr: String::new(),
            });
        }
    };

    for entry in entries.flatten() {
        let repo_id_dir = entry.path();
        if !repo_id_dir.is_dir() {
            continue;
        }

        let session_ws_dir = repo_id_dir.join(session_uuid);
        if !session_ws_dir.exists() {
            continue;
        }

        let workspace_name = format!("aiki-{}", session_uuid);

        // Find the real repo root from the workspace's .jj/repo pointer.
        // We must NOT use find_jj_root() here because it walks up looking for
        // .jj/ — and the workspace itself has a .jj/ directory, so it would
        // return the workspace path instead of the actual repo root.
        let repo_root = match isolation::find_repo_root_from_workspace(&session_ws_dir) {
            Some(root) => root,
            None => {
                debug_log(|| {
                    format!(
                        "[workspace] Could not find repo root for workspace at {}",
                        session_ws_dir.display()
                    )
                });
                // Clean up directory even if we can't absorb
                let _ = std::fs::remove_dir_all(&session_ws_dir);
                continue;
            }
        };

        if !seen_repo_roots.contains(&repo_root) {
            seen_repo_roots.push(repo_root.clone());
        }

        let workspace = isolation::IsolatedWorkspace {
            name: workspace_name,
            path: session_ws_dir,
        };

        match isolation::absorb_workspace(&repo_root, &workspace, parent_session_uuid.as_deref()) {
            Ok(isolation::AbsorbResult::Absorbed) => {
                absorbed += 1;

                // Post-absorption: check if target's @ is conflicted.
                // Compute target_dir same as absorb_workspace: parent workspace if
                // it exists, otherwise repo root.
                let target_dir = if let Some(ref parent_uuid) = parent_session_uuid {
                    let repo_id = crate::repos::ensure_repo_id(&repo_root).unwrap_or_default();
                    let parent_ws_path =
                        isolation::workspaces_dir().join(&repo_id).join(parent_uuid);
                    if parent_ws_path.exists() {
                        parent_ws_path
                    } else {
                        repo_root.clone()
                    }
                } else {
                    repo_root.clone()
                };

                // Uses `conflicts() & @` — if @ is clean, all ancestor conflicts
                // have been resolved, so no false positives from historical conflicts.
                let conflict_check = crate::jj::jj_cmd()
                    .current_dir(&target_dir)
                    .args([
                        "log",
                        "-r",
                        "conflicts() & @",
                        "--no-graph",
                        "-T",
                        r#"change_id ++ "\n""#,
                        "--ignore-working-copy",
                    ])
                    .output();
                match conflict_check {
                    Ok(output) if output.status.success() => {
                        let conflicted = String::from_utf8_lossy(&output.stdout);
                        if !conflicted.trim().is_empty() {
                            has_conflicts = true;
                            // Get conflicted file list for the autoreply.
                            // Capture both stdout and stderr — output stream varies
                            // across JJ versions.
                            let files_output = crate::jj::jj_cmd()
                                .current_dir(&target_dir)
                                .args(["resolve", "--list", "-r", "@"])
                                .output();
                            last_conflicted_files = match files_output {
                                Ok(fo) => {
                                    let stdout = String::from_utf8_lossy(&fo.stdout);
                                    let stderr = String::from_utf8_lossy(&fo.stderr);
                                    let files = if !stdout.trim().is_empty() { stdout } else { stderr };
                                    let trimmed = files.trim().to_string();
                                    if trimmed.is_empty() {
                                        "(conflict detected but file list unavailable — run `jj resolve --list` to inspect)".to_string()
                                    } else {
                                        trimmed
                                    }
                                }
                                Err(_) => "(conflict detected but `jj resolve --list` failed — run it manually to inspect)".to_string(),
                            };
                        }
                    }
                    Ok(output) => {
                        // jj log exited with non-zero — conflict state unknown, treat as conflicted
                        // to force the agent to investigate.
                        eprintln!(
                            "[aiki] Warning: conflict check `jj log -r 'conflicts() & @'` failed (exit {}): {}",
                            output.status.code().unwrap_or(-1),
                            String::from_utf8_lossy(&output.stderr).trim()
                        );
                        has_conflicts = true;
                        last_conflicted_files =
                            "(conflict check failed — run `jj log -r 'conflicts() & @'` to verify)"
                                .to_string();
                    }
                    Err(e) => {
                        // jj command failed to spawn — conflict state unknown, treat as conflicted.
                        eprintln!("[aiki] Warning: conflict check failed to run: {}", e);
                        has_conflicts = true;
                        last_conflicted_files = "(conflict check failed to run — run `jj log -r 'conflicts() & @'` to verify)".to_string();
                    }
                }
            }
            Ok(isolation::AbsorbResult::Skipped) => {
                let _ = isolation::cleanup_workspace(&repo_root, &workspace);
            }
            Err(e) => {
                eprintln!(
                    "[aiki] Warning: failed to absorb workspace '{}': {}",
                    workspace.name, e
                );
                let _ = isolation::cleanup_workspace(&repo_root, &workspace);
            }
        }
    }

    // Opportunistically clean up orphaned JJ workspaces for repositories that had
    // absorbed workspaces.
    // This prevents the jj workspace list from growing unbounded with dead sessions.
    for repo_root in &seen_repo_roots {
        if let Err(e) = isolation::cleanup_orphaned_workspaces(repo_root) {
            debug_log(|| {
                format!(
                    "[workspace] Orphaned workspace cleanup failed for {}: {}",
                    repo_root.display(),
                    e
                )
            });
        }
    }

    debug_log(|| format!("[workspace] Absorbed {} workspace(s)", absorbed));

    // Emit Absorbed event for tasks closed by this session.
    // This signals `run_wait` that the session's work is complete,
    // even when no file changes were absorbed.
    let session_uuid_str = session_uuid.to_string();
    for repo_root in &seen_repo_roots {
        if let Ok(events) = crate::tasks::storage::read_events(repo_root) {
            let graph = crate::tasks::materialize_graph(&events);
            let terminal_task_ids: Vec<String> = graph
                .tasks
                .iter()
                .filter(|(_, task)| {
                    matches!(
                        task.status,
                        crate::tasks::TaskStatus::Closed | crate::tasks::TaskStatus::Stopped
                    ) && task.last_session_id.as_deref() == Some(&session_uuid_str)
                })
                .map(|(id, _)| id.clone())
                .collect();

            if !terminal_task_ids.is_empty() {
                let turn_id = crate::tasks::current_turn_id(Some(&session_uuid_str));
                let absorbed_event = crate::tasks::TaskEvent::Absorbed {
                    task_ids: terminal_task_ids,
                    session_id: session_uuid_str.clone(),
                    turn_id,
                    timestamp: chrono::Utc::now(),
                };
                let _ = crate::tasks::storage::write_event(repo_root, &absorbed_event);
            }
        }
    }

    let stdout = if has_conflicts {
        last_conflicted_files
    } else if absorbed > 0 {
        "ok".to_string()
    } else {
        "0".to_string()
    };

    Ok(ActionResult {
        success: true,
        exit_code: Some(0),
        stdout,
        stderr: String::new(),
    })
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{ChangeOperation, EditDetail, WriteOperation};
    use crate::provenance::record::AgentType;
    use crate::session::{AikiSession, SessionMode};
    use tempfile::TempDir;

    // =========================================================================
    // Helper Functions
    // =========================================================================

    fn create_write_change_event(
        cwd: &Path,
        file_paths: Vec<String>,
        edit_details: Vec<EditDetail>,
    ) -> AikiChangeCompletedPayload {
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test".to_string(),
            None::<&str>,
            crate::provenance::DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        AikiChangeCompletedPayload {
            session,
            cwd: cwd.to_path_buf(),
            timestamp: chrono::Utc::now(),
            tool_name: "Edit".to_string(),
            success: true,
            turn: crate::events::Turn::unknown(),
            operation: ChangeOperation::Write(WriteOperation {
                file_paths,
                edit_details,
            }),
        }
    }

    // =========================================================================
    // Metadata Generation Tests
    // =========================================================================

    #[test]
    fn test_build_write_metadata_with_claude() {
        let session = AikiSession::new(
            AgentType::ClaudeCode,
            "test-session-123".to_string(),
            None::<&str>,
            crate::provenance::DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        let event = AikiChangeCompletedPayload {
            session,
            cwd: std::path::PathBuf::from("/tmp"),
            timestamp: chrono::Utc::now(),
            tool_name: "Edit".to_string(),
            success: true,
            turn: crate::events::Turn::unknown(),
            operation: ChangeOperation::Write(WriteOperation {
                file_paths: vec!["/tmp/file.rs".to_string()],
                edit_details: vec![],
            }),
        };

        let result = build_write_metadata(&event, None).unwrap();

        assert!(result.success);
        assert_eq!(result.exit_code, Some(0));

        // Parse JSON output
        let json: serde_json::Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(json["author"], "Claude <noreply@anthropic.com>");
        assert!(json["message"].as_str().unwrap().contains("[aiki]"));
        assert!(json["message"].as_str().unwrap().contains("author=claude"));
        // Session ID is a truncated UUID v5 hash (8 hex chars)
        let message = json["message"].as_str().unwrap();
        let session_start = message.find("session=").expect("Should have session=");
        let session_end = message[session_start + 8..]
            .find('\n')
            .map(|i| session_start + 8 + i)
            .unwrap_or(message.len());
        let session_value = &message[session_start + 8..session_end];
        assert_eq!(
            session_value.len(),
            8,
            "Session ID should be 8 hex chars, got: {}",
            session_value
        );
        assert!(
            session_value.chars().all(|c| c.is_ascii_hexdigit()),
            "Session ID should be hex, got: {}",
            session_value
        );
    }

    #[test]
    fn test_build_write_metadata_with_cursor() {
        let session = AikiSession::new(
            AgentType::Cursor,
            "cursor-session".to_string(),
            None::<&str>,
            crate::provenance::DetectionMethod::Hook,
            SessionMode::Interactive,
        );
        let event = AikiChangeCompletedPayload {
            session,
            cwd: std::path::PathBuf::from("/tmp"),
            timestamp: chrono::Utc::now(),
            tool_name: "Edit".to_string(),
            success: true,
            turn: crate::events::Turn::unknown(),
            operation: ChangeOperation::Write(WriteOperation {
                file_paths: vec!["/tmp/file.rs".to_string()],
                edit_details: vec![],
            }),
        };

        let result = build_write_metadata(&event, None).unwrap();

        assert!(result.success);
        let json: serde_json::Value = serde_json::from_str(&result.stdout).unwrap();
        assert_eq!(json["author"], "Cursor <noreply@cursor.com>");
        assert!(json["message"].as_str().unwrap().contains("author=cursor"));
    }

    // =========================================================================
    // Edit Classification Tests (using change events)
    // =========================================================================

    #[test]
    fn test_classify_edits_change_exact_match() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");

        // Create file with AI's changes applied
        fs::write(&file_path, "Hello World").unwrap();

        let event = create_write_change_event(
            temp_dir.path(),
            vec!["test.txt".to_string()],
            vec![EditDetail::new("test.txt", "Hello", "Hello World")],
        );

        let result = classify_edits_change(&event).unwrap();
        assert_eq!(result.stdout, "ExactMatch");
    }

    #[test]
    fn test_classify_edits_change_additive() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");

        // User added extra content after AI's edit
        fs::write(&file_path, "Hello World\nExtra line by user").unwrap();

        let event = create_write_change_event(
            temp_dir.path(),
            vec!["test.txt".to_string()],
            vec![EditDetail::new("test.txt", "", "Hello World")],
        );

        let result = classify_edits_change(&event).unwrap();
        // AI's edit is present but file has extra content → ExactMatch
        assert_eq!(result.stdout, "ExactMatch");
    }

    #[test]
    fn test_classify_edits_change_overlapping() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");

        // Old string still present (edit not applied)
        fs::write(&file_path, "Hello").unwrap();

        let event = create_write_change_event(
            temp_dir.path(),
            vec!["test.txt".to_string()],
            vec![EditDetail::new("test.txt", "Hello", "Hello World")],
        );

        let result = classify_edits_change(&event).unwrap();
        assert_eq!(result.stdout, "OverlappingUserEdits");
    }

    #[test]
    fn test_classify_edits_change_extra_files() {
        let temp_dir = TempDir::new().unwrap();
        let file1 = temp_dir.path().join("test1.txt");
        let file2 = temp_dir.path().join("test2.txt");

        fs::write(&file1, "Content 1").unwrap();
        fs::write(&file2, "Content 2").unwrap();

        let event = create_write_change_event(
            temp_dir.path(),
            vec!["test1.txt".to_string(), "test2.txt".to_string()],
            vec![EditDetail::new("test1.txt", "", "Content 1")],
        );

        let result = classify_edits_change(&event).unwrap();
        // Extra files detected → AdditiveUserEdits
        assert_eq!(result.stdout, "AdditiveUserEdits");
    }

    #[test]
    fn test_classify_edits_change_no_edit_details() {
        let temp_dir = TempDir::new().unwrap();

        let event =
            create_write_change_event(temp_dir.path(), vec!["test.txt".to_string()], vec![]);

        let result = classify_edits_change(&event).unwrap();
        // No edit details → assume AI-only
        assert_eq!(result.stdout, "ExactMatch");
    }
}
