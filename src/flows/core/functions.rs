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

use crate::provenance::authors::{AuthorScope, AuthorsCommand, OutputFormat};
use crate::cache::debug_log;
use crate::error::{AikiError, Result};
use crate::events::{
    AikiChangeCompletedPayload, AikiChangePermissionAskedPayload, AikiCommitMessageStartedPayload,
    ChangeOperation,
};
use crate::flows::state::ActionResult;
use crate::provenance::record::ProvenanceRecord;
use crate::tasks::{manager, storage};
use anyhow::Context;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use crate::jj::jj_cmd;

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
/// - `event.session_id` - Session identifier
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
        format!("session={}", session.external_id()),
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

/// Run jj split to create AI change and user change
#[allow(dead_code)] // Reserved for future split-based separation strategy
fn run_jj_split(cwd: &Path, message: &str, author: &str, files: &[String]) -> Result<String> {
    // Build jj split command
    // Note: jj split doesn't support --author, we'll set it separately
    let mut cmd = jj_cmd();
    cmd.current_dir(cwd);
    cmd.arg("split");
    cmd.arg("--message").arg(message);

    // Add file paths
    for file in files {
        cmd.arg(file);
    }

    debug_log(|| format!("[flows/core] Running: {:?}", cmd));

    let output = cmd
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to execute jj split: {}", e)))?;

    if !output.status.success() {
        return Err(AikiError::JjCommandFailed(format!(
            "jj split failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let split_output = String::from_utf8_lossy(&output.stdout).to_string();

    // Step 2: Set author on the first part (AI change) using jj metaedit
    // After split, the first part is at @- (parent of working copy)
    let mut metaedit_cmd = jj_cmd();
    metaedit_cmd.current_dir(cwd);
    metaedit_cmd.arg("metaedit");
    metaedit_cmd.arg("-r").arg("@-");
    metaedit_cmd.arg("--author").arg(author);
    metaedit_cmd.arg("--no-edit"); // Don't open editor

    debug_log(|| {
        format!(
            "[flows/core] Setting author on AI change: {:?}",
            metaedit_cmd
        )
    });

    let metaedit_output = metaedit_cmd
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to execute jj metaedit: {}", e)))?;

    if !metaedit_output.status.success() {
        return Err(AikiError::JjCommandFailed(format!(
            "jj metaedit failed: {}",
            String::from_utf8_lossy(&metaedit_output.stderr)
        )));
    }

    Ok(split_output)
}

/// Parse jj split output to extract change IDs
#[allow(dead_code)] // Reserved for future split-based separation strategy
fn parse_split_output(output: &str) -> Result<(String, String)> {
    // jj split output format (example):
    // "First part: qpvuntsm 12345678 AI changes
    //  Second part: rlvkpnrz 87654321 (no description set)"
    //
    // The format is: "First part: <change_hash_short> <change_hash_long> <description>"
    // We want the long hash (position 3 in split_whitespace)

    let lines: Vec<&str> = output.lines().collect();

    let ai_change_id = lines
        .iter()
        .find(|l| l.contains("First part"))
        .and_then(|l| {
            // Extract third word (long change ID hash)
            // Words: [0]="First", [1]="part:", [2]=short_hash, [3]=long_hash
            l.split_whitespace().nth(3).map(String::from)
        })
        .ok_or_else(|| {
            AikiError::Other(anyhow::anyhow!(
                "Failed to parse AI change ID from jj split output"
            ))
        })?;

    let user_change_id = lines
        .iter()
        .find(|l| l.contains("Second part"))
        .and_then(|l| {
            // Extract third word (long change ID hash)
            l.split_whitespace().nth(3).map(String::from)
        })
        .ok_or_else(|| {
            AikiError::Other(anyhow::anyhow!(
                "Failed to parse user change ID from jj split output"
            ))
        })?;

    Ok((ai_change_id, user_change_id))
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
pub fn task_list_size_for_agent(cwd: &Path, agent: &crate::agents::AgentType) -> Result<ActionResult> {
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

/// Get the size of the ready task queue (all tasks, no filtering)
///
/// Returns the number of open, unblocked tasks in the ready queue.
/// This is used for context injection in flows to show task count.
#[allow(dead_code)] // Part of flow function API
pub fn task_list_size(cwd: &Path) -> Result<ActionResult> {
    let events = crate::tasks::storage::read_events(cwd)?;
    let graph = crate::tasks::graph::materialize_graph(&events);
    let ready = crate::tasks::manager::get_ready_queue(&graph);

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

/// Create an isolated workspace if concurrent sessions are detected.
///
/// Called from `turn.started` and `repo.changed` hooks.
/// Checks how many sessions are active in the current repo:
/// - count == 1: skip (zero overhead for solo sessions)
/// - count > 1: create isolated workspace
///
/// Creation is idempotent — no-op if workspace already exists.
///
/// # Returns
/// ActionResult with stdout being the absolute workspace path (if created/exists)
/// or empty string (if skipped or failed)
pub fn workspace_create_if_concurrent(
    session: &crate::session::AikiSession,
    cwd: &Path,
) -> Result<ActionResult> {
    use crate::session::isolation;

    let repo_root = match isolation::find_jj_root(cwd) {
        Some(root) => root,
        None => {
            debug_log(|| "[workspace] Not in a JJ repo, skipping workspace creation");
            return Ok(ActionResult {
                success: true,
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            });
        }
    };

    let repo_id = match crate::repos::ensure_repo_id(&repo_root) {
        Ok(id) => id,
        Err(_) => {
            debug_log(|| "[workspace] No repo-id found, skipping workspace creation");
            return Ok(ActionResult {
                success: true,
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            });
        }
    };

    let session_uuid = session.uuid();

    // Register session in repo (for O(1) session counting and repo-transition detection)
    isolation::register_session_in_repo(&repo_id, session_uuid);

    let session_count = isolation::count_sessions_in_repo(&repo_id);

    // Check if a workspace already exists for this session
    let workspace_path = crate::session::isolation::workspaces_dir()
        .join(&repo_id)
        .join(session_uuid);

    if session_count <= 1 && !workspace_path.exists() {
        debug_log(|| {
            format!(
                "[workspace] Only {} session(s) in repo {}, skipping isolation",
                session_count, repo_id
            )
        });
        return Ok(ActionResult {
            success: true,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        });
    }

    match isolation::create_isolated_workspace(&repo_root, session_uuid) {
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
            // Workspace creation failure is non-fatal — fall back to main workspace
            eprintln!("[aiki] Warning: workspace creation failed, continuing in main workspace: {}", e);
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
pub fn workspace_absorb_all(
    session: &crate::session::AikiSession,
) -> Result<ActionResult> {
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
    let mut last_conflict_id: Option<String> = None;
    let mut last_conflicted_files: Vec<String> = Vec::new();

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

        let workspace = isolation::IsolatedWorkspace {
            name: workspace_name,
            path: session_ws_dir,
            repo_root: repo_root.clone(),
            session_uuid: session_uuid.to_string(),
        };

        match isolation::absorb_workspace(
            &repo_root,
            &workspace,
            parent_session_uuid.as_deref(),
        ) {
            Ok(isolation::AbsorbResult::Absorbed) => {
                absorbed += 1;
                let _ = isolation::cleanup_workspace(&repo_root, &workspace);
                // Unregister session from this repo's sidecar
                if let Some(repo_id) = repo_id_dir.file_name().and_then(|n| n.to_str()) {
                    isolation::unregister_session_from_repo(repo_id, session_uuid);
                }
            }
            Ok(isolation::AbsorbResult::Conflicts { conflict_id, conflicted_files }) => {
                // Don't cleanup — workspace stays alive for conflict resolution
                has_conflicts = true;
                debug_log(|| {
                    format!(
                        "[workspace] Conflicts remain after auto-resolve: conflict_id={}, files={:?}",
                        conflict_id, conflicted_files
                    )
                });
                last_conflict_id = Some(conflict_id);
                last_conflicted_files = conflicted_files;
            }
            Ok(isolation::AbsorbResult::Skipped) => {
                let _ = isolation::cleanup_workspace(&repo_root, &workspace);
                // Unregister session from this repo's sidecar
                if let Some(repo_id) = repo_id_dir.file_name().and_then(|n| n.to_str()) {
                    isolation::unregister_session_from_repo(repo_id, session_uuid);
                }
            }
            Err(e) => {
                eprintln!(
                    "[aiki] Warning: failed to absorb workspace '{}': {}",
                    workspace.name, e
                );
                let _ = isolation::cleanup_workspace(&repo_root, &workspace);
                // Unregister session from this repo's sidecar
                if let Some(repo_id) = repo_id_dir.file_name().and_then(|n| n.to_str()) {
                    isolation::unregister_session_from_repo(repo_id, session_uuid);
                }
            }
        }
    }

    // Unconditionally unregister this session from any repo it's registered in.
    // This handles solo sessions that registered in by-repo/ but never created
    // a workspace (the loop above only covers sessions with workspace dirs).
    if let Some(repo_id) = isolation::find_session_repo(session_uuid) {
        isolation::unregister_session_from_repo(&repo_id, session_uuid);
    }

    // Opportunistically clean up orphaned JJ workspaces for the current repo.
    // This prevents the jj workspace list from growing unbounded with dead sessions.
    if let Some(repo_root) = isolation::find_jj_root(&std::env::current_dir().unwrap_or_default()) {
        if let Err(e) = isolation::cleanup_orphaned_workspaces(&repo_root) {
            debug_log(|| format!("[workspace] Orphaned workspace cleanup failed: {}", e));
        }
    }

    debug_log(|| format!("[workspace] Absorbed {} workspace(s)", absorbed));

    let stdout = if has_conflicts {
        // Return JSON so the hook can access fields via {{absorb_result.conflict_id}}
        let conflict_id = last_conflict_id.unwrap_or_default();
        let files = last_conflicted_files.join(", ");
        serde_json::json!({
            "status": "conflicts",
            "conflict_id": conflict_id,
            "conflicted_files": files,
        })
        .to_string()
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

/// Detect conflicts in the agent's workspace and return formatted details.
///
/// Finds the workspace for the current session, runs `jj resolve --list`
/// to get conflicted files, reads each file's content showing conflict markers,
/// and returns a formatted string for the autoreply.
pub fn detect_workspace_conflicts(
    session: &crate::session::AikiSession,
) -> Result<ActionResult> {
    use crate::session::isolation;

    let session_uuid = session.uuid();
    let workspaces_dir = isolation::workspaces_dir();

    if !workspaces_dir.exists() {
        return Ok(ActionResult {
            success: true,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        });
    }

    // Find workspace path for this session (scan repo-id dirs)
    let mut workspace_path: Option<std::path::PathBuf> = None;
    if let Ok(entries) = std::fs::read_dir(&workspaces_dir) {
        for entry in entries.flatten() {
            let repo_id_dir = entry.path();
            if !repo_id_dir.is_dir() {
                continue;
            }
            let session_ws_dir = repo_id_dir.join(session_uuid);
            if session_ws_dir.exists() {
                workspace_path = Some(session_ws_dir);
                break;
            }
        }
    }

    let workspace_path: std::path::PathBuf = match workspace_path {
        Some(p) => p,
        None => {
            return Ok(ActionResult {
                success: true,
                exit_code: Some(0),
                stdout: String::new(),
                stderr: String::new(),
            });
        }
    };

    // Run jj resolve --list to get conflicted files
    let conflict_output = crate::jj::jj_cmd()
        .current_dir(&workspace_path)
        .args(["resolve", "--list"])
        .output()
        .map_err(|e| {
            crate::error::AikiError::Other(anyhow::anyhow!(
                "Failed to list conflicts: {}",
                e
            ))
        })?;

    let conflict_list = String::from_utf8_lossy(&conflict_output.stdout);
    if conflict_list.trim().is_empty() {
        return Ok(ActionResult {
            success: true,
            exit_code: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        });
    }

    let mut output = String::from("Conflicted files:\n");
    let max_lines_per_file = 100;

    for line in conflict_list.lines() {
        // jj resolve --list output format: "filename    <description>"
        let file_path = line.split_whitespace().next().unwrap_or("").trim();
        if file_path.is_empty() {
            continue;
        }

        output.push_str(&format!("\n--- {} ---\n", file_path));

        // Read file content from workspace
        let full_path = workspace_path.join(file_path);
        match std::fs::read_to_string(&full_path) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                if lines.len() > max_lines_per_file {
                    for line in &lines[..max_lines_per_file] {
                        output.push_str(line);
                        output.push('\n');
                    }
                    output.push_str(&format!(
                        "\n... truncated ({} more lines) ...\n",
                        lines.len() - max_lines_per_file
                    ));
                } else {
                    output.push_str(&content);
                    if !content.ends_with('\n') {
                        output.push('\n');
                    }
                }
            }
            Err(e) => {
                output.push_str(&format!("(could not read file: {})\n", e));
            }
        }
    }

    Ok(ActionResult {
        success: true,
        exit_code: Some(0),
        stdout: output,
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
            crate::provenance::DetectionMethod::Hook, SessionMode::Interactive,
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
            crate::provenance::DetectionMethod::Hook, SessionMode::Interactive,
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
            crate::provenance::DetectionMethod::Hook, SessionMode::Interactive,
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

    // =========================================================================
    // Helper Function Tests
    // =========================================================================

    #[test]
    fn test_parse_split_output() {
        let output = "First part: qpvuntsm 12345678 AI changes\nSecond part: rlvkpnrz 87654321 (no description set)\n";

        let (ai_id, user_id) = parse_split_output(output).unwrap();

        assert_eq!(ai_id, "12345678");
        assert_eq!(user_id, "87654321");
    }
}
