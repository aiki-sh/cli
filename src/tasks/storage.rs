//! Event storage on aiki/tasks branch
//!
//! Tasks are stored as fileless JJ changes on the `aiki/tasks` branch.
//! Each event is a JJ change with metadata in the description.

use crate::error::{AikiError, Result};
use crate::jj::jj_cmd;
use chrono::{DateTime, Utc};
use std::path::Path;

use super::types::{TaskEvent, TaskOutcome, TaskPriority};

const TASKS_BRANCH: &str = "aiki/tasks";
const METADATA_START: &str = "[aiki-task]";
const METADATA_END: &str = "[/aiki-task]";

/// Ensure the aiki/tasks branch exists (cached per process)
pub fn ensure_tasks_branch(cwd: &Path) -> Result<()> {
    crate::jj::ensure_branch(cwd, TASKS_BRANCH)
}

/// Write a task event to the aiki/tasks branch
///
/// Uses `jj new --no-edit` to create the event change without affecting the working copy.
pub fn write_event(cwd: &Path, event: &TaskEvent) -> Result<()> {
    ensure_tasks_branch(cwd)?;

    let metadata = event_to_metadata_block(event);

    // Create a new change as child of aiki/tasks WITHOUT switching working copy
    let result = jj_cmd()
        .current_dir(cwd)
        .args(["new", TASKS_BRANCH, "--no-edit", "--ignore-working-copy", "-m", &metadata])
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to create task event: {}", e)))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(AikiError::JjCommandFailed(format!(
            "Failed to write task event: {}",
            stderr
        )));
    }

    Ok(())
}

/// Write multiple task events as a single atomic jj commit.
///
/// All events are serialized as separate `[aiki-task]...[/aiki-task]` blocks
/// within a single commit message. This ensures that either all events are
/// written or none are — useful for close+spawn sequences where partial
/// writes would leave the system in an inconsistent state.
pub fn write_events_batch(cwd: &Path, events: &[TaskEvent]) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    if events.len() == 1 {
        return write_event(cwd, &events[0]);
    }

    ensure_tasks_branch(cwd)?;

    let metadata = events
        .iter()
        .map(|e| event_to_metadata_block(e))
        .collect::<Vec<_>>()
        .join("\n");

    let result = jj_cmd()
        .current_dir(cwd)
        .args(["new", TASKS_BRANCH, "--no-edit", "--ignore-working-copy", "-m", &metadata])
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to create batch task event: {}", e)))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(AikiError::JjCommandFailed(format!(
            "Failed to write batch task event: {}",
            stderr
        )));
    }

    Ok(())
}

/// Write a LinkAdded event with mandatory validation.
///
/// This is the canonical way to emit a link. All validation happens here:
/// - Target normalization (short ID resolution, file: prefix, task_only check)
/// - Idempotency (skip if link already exists)
/// - Cycle detection (for blocked-by and subtask-of)
/// - Cardinality enforcement (single-link auto-replace with supersedes)
///
/// Returns Ok(true) if a new link was written, Ok(false) if it was a no-op
/// (duplicate link).
pub fn write_link_event(
    cwd: &Path,
    graph: &super::graph::TaskGraph,
    kind: &str,
    from: &str,
    to: &str,
) -> Result<bool> {
    write_link_event_with_autorun(cwd, graph, kind, from, to, None)
}

/// Write a LinkAdded event with an optional autorun flag.
///
/// Like `write_link_event` but allows setting autorun on the created link.
/// When `autorun` is `Some(true)`, the `from` task will auto-start when
/// the `to` (blocker) task closes.
pub fn write_link_event_with_autorun(
    cwd: &Path,
    graph: &super::graph::TaskGraph,
    kind: &str,
    from: &str,
    to: &str,
    autorun: Option<bool>,
) -> Result<bool> {
    use super::graph::find_link_kind;
    use super::md::short_id;

    // 1. Normalize the target
    let to_normalized = normalize_link_target_for_graph(to, kind, graph)?;

    // 2. Idempotency: skip if link already exists
    if graph.edges.has_link(from, &to_normalized, kind) {
        return Ok(false);
    }

    // 3. Cycle detection for blocking and hierarchical kinds
    let needs_cycle_check = kind == "subtask-of"
        || find_link_kind(kind).map_or(false, |lk| lk.blocks_ready);
    if needs_cycle_check {
        if graph.would_create_cycle(from, &to_normalized, kind) {
            return Err(AikiError::LinkCycle {
                kind: kind.to_string(),
            });
        }
    }

    // 4. Cardinality enforcement with auto-replace
    let link_kind = find_link_kind(kind);
    let emit_supersedes = kind == "implements-plan" || kind == "orchestrates";
    let timestamp = chrono::Utc::now();

    if let Some(lk) = link_kind {
        // Forward cardinality: max links from this node
        if let Some(max) = lk.max_forward {
            let existing: Vec<String> = graph.edges.targets(from, kind).to_vec();
            if existing.len() >= max {
                for old_target in &existing {
                    let remove_event = TaskEvent::LinkRemoved {
                        from: from.to_string(),
                        to: old_target.clone(),
                        kind: kind.to_string(),
                        reason: Some(format!("Replaced by link to {}", short_id(&to_normalized))),
                        timestamp,
                    };
                    write_event(cwd, &remove_event)?;

                    if emit_supersedes {
                        // supersedes is task_only — skip if old_target is a file/external ref
                        if !has_external_ref_prefix(old_target) {
                            let supersedes_event = TaskEvent::LinkAdded {
                                from: from.to_string(),
                                to: old_target.clone(),
                                kind: "supersedes".to_string(),
                                autorun: None,
                                timestamp,
                            };
                            write_event(cwd, &supersedes_event)?;
                            eprintln!(
                                "Superseded: {} previously {} {}",
                                short_id(old_target),
                                if kind == "implements-plan" { "implemented" } else { "orchestrated" },
                                short_id(&to_normalized)
                            );
                        }
                        // If old_target is a file path, skip the supersedes link silently (bug #4 fix)
                    } else if kind == "subtask-of" {
                        eprintln!(
                            "Re-parented: {} moved from {} to {}",
                            short_id(from),
                            short_id(old_target),
                            short_id(&to_normalized)
                        );
                    }
                }
            }
        }

        // Reverse cardinality: max referrers to the target
        if let Some(max) = lk.max_reverse {
            let existing: Vec<String> = graph.edges.referrers(&to_normalized, kind).to_vec();
            if existing.len() >= max {
                for old_from in &existing {
                    let remove_event = TaskEvent::LinkRemoved {
                        from: old_from.clone(),
                        to: to_normalized.clone(),
                        kind: kind.to_string(),
                        reason: Some(format!(
                            "Replaced by {} linking to {}",
                            short_id(from),
                            short_id(&to_normalized)
                        )),
                        timestamp,
                    };
                    write_event(cwd, &remove_event)?;

                    if emit_supersedes {
                        // old_from is always a task ID in reverse cardinality
                        let supersedes_event = TaskEvent::LinkAdded {
                            from: from.to_string(),
                            to: old_from.clone(),
                            kind: "supersedes".to_string(),
                            autorun: None,
                            timestamp,
                        };
                        write_event(cwd, &supersedes_event)?;
                        eprintln!(
                            "Superseded: {} previously {} {}",
                            short_id(old_from),
                            if kind == "orchestrates" { "orchestrated" } else { "implemented" },
                            short_id(&to_normalized)
                        );
                    }
                }
            }
        }
    }

    // 5. Write the LinkAdded event
    let event = TaskEvent::LinkAdded {
        from: from.to_string(),
        to: to_normalized,
        kind: kind.to_string(),
        autorun,
        timestamp,
    };
    write_event(cwd, &event)?;

    Ok(true)
}

/// Check if a string has an external reference prefix
fn has_external_ref_prefix(s: &str) -> bool {
    s.starts_with("file:")
        || s.starts_with("prompt:")
        || s.starts_with("comment:")
        || s.starts_with("issue:")
}

/// Normalize a link target using a TaskGraph for lookups.
///
/// This is the graph-aware variant used by write_link_event.
fn normalize_link_target_for_graph(
    input: &str,
    kind: &str,
    graph: &super::graph::TaskGraph,
) -> Result<String> {
    use super::graph::is_task_only_kind;
    use super::id::is_task_id;
    use super::manager::resolve_task_id;

    // 1. Strip task: prefix if present
    let stripped = input.strip_prefix("task:").unwrap_or(input);

    // 2. If it's already a full 32-char task ID, use it directly
    if is_task_id(stripped) {
        if graph.tasks.contains_key(stripped) {
            return Ok(stripped.to_string());
        }
        // Full-length ID but not found
        if is_task_only_kind(kind) {
            return Err(AikiError::InvalidLinkTarget {
                kind: kind.to_string(),
                target: stripped.to_string(),
            });
        }
        return Ok(stripped.to_string());
    }

    // 3. If it has an external reference prefix
    if has_external_ref_prefix(stripped) {
        if is_task_only_kind(kind) {
            return Err(AikiError::InvalidLinkTarget {
                kind: kind.to_string(),
                target: stripped.to_string(),
            });
        }
        return Ok(stripped.to_string());
    }

    // 4. Try resolving as a short task ID prefix
    match resolve_task_id(&graph.tasks, stripped) {
        Ok(full_id) => Ok(full_id),
        Err(AikiError::TaskNotFound(_)) if !is_task_only_kind(kind) => {
            // Flexible-target kinds: treat unresolved input as file path
            Ok(format!("file:{}", stripped))
        }
        Err(AikiError::TaskNotFound(_)) => {
            // Task-only kinds: wrap as InvalidLinkTarget for clearer messaging
            Err(AikiError::InvalidLinkTarget {
                kind: kind.to_string(),
                target: stripped.to_string(),
            })
        }
        // AmbiguousTaskId, PrefixTooShort — propagate for all kinds
        Err(e) => Err(e),
    }
}

/// Read all task events from the aiki/tasks branch
pub fn read_events(cwd: &Path) -> Result<Vec<TaskEvent>> {
    if !crate::jj::branch_exists(cwd, TASKS_BRANCH)? {
        return Ok(Vec::new());
    }

    // Read all task events: children of any ancestor of the bookmark
    // This finds chain events, orphaned events, and new flat events
    let output = jj_cmd()
        .current_dir(cwd)
        .args([
            "log",
            "-r",
            &format!(
                "children(ancestors({})) & description(substring:\"{}\")",
                TASKS_BRANCH, METADATA_START
            ),
            "--no-graph",
            "-T",
            "description ++ \"\\n---EVENT-SEPARATOR---\\n\"",
            "--reversed",
            "--ignore-working-copy",
        ])
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to read task events: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::JjCommandFailed(format!(
            "Failed to read task events: {}",
            stderr
        )));
    }

    let descriptions = String::from_utf8_lossy(&output.stdout);
    let mut events = Vec::new();

    // Split by our separator and parse each description
    for desc in descriptions.split("---EVENT-SEPARATOR---") {
        let desc = desc.trim();
        if desc.is_empty() {
            continue;
        }

        // Parse all metadata blocks in this commit (supports batch writes)
        parse_all_metadata_blocks(desc, &mut events);
    }

    // Sort by timestamp to ensure consistent ordering (flat model doesn't guarantee position order)
    events.sort_by_key(|e| e.timestamp());

    Ok(events)
}

/// Event with its JJ change_id
#[derive(Debug, Clone)]
pub struct EventWithId {
    /// The JJ change_id of the commit containing this event
    pub change_id: String,
    /// The parsed event
    pub event: TaskEvent,
}

/// Read all task events with their change_ids from the aiki/tasks branch
///
/// This is used when we need to track which JJ change each event came from,
/// particularly for generating comment IDs (source: comment:<change_id>).
pub fn read_events_with_ids(cwd: &Path) -> Result<Vec<EventWithId>> {
    if !crate::jj::branch_exists(cwd, TASKS_BRANCH)? {
        return Ok(Vec::new());
    }

    // Read all task events with change_ids: children of any ancestor of the bookmark
    let output = jj_cmd()
        .current_dir(cwd)
        .args([
            "log",
            "-r",
            &format!(
                "children(ancestors({})) & description(substring:\"{}\")",
                TASKS_BRANCH, METADATA_START
            ),
            "--no-graph",
            "-T",
            "\"---CHANGE-ID:\" ++ change_id ++ \"---\\n\" ++ description ++ \"\\n---EVENT-SEPARATOR---\\n\"",
            "--reversed",
            "--ignore-working-copy",
        ])
        .output()
        .map_err(|e| AikiError::JjCommandFailed(format!("Failed to read task events: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AikiError::JjCommandFailed(format!(
            "Failed to read task events: {}",
            stderr
        )));
    }

    let output_str = String::from_utf8_lossy(&output.stdout);
    let mut events = Vec::new();

    // Split by our separator and parse each entry
    for entry in output_str.split("---EVENT-SEPARATOR---") {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }

        // Extract change_id from the entry
        let change_id = if let Some(start) = entry.find("---CHANGE-ID:") {
            if let Some(end) = entry[start + 13..].find("---") {
                Some(entry[start + 13..start + 13 + end].to_string())
            } else {
                None
            }
        } else {
            None
        };

        let Some(change_id) = change_id else {
            continue;
        };

        // Parse all metadata blocks in this commit (supports batch writes)
        let mut commit_events = Vec::new();
        parse_all_metadata_blocks(entry, &mut commit_events);
        for event in commit_events {
            events.push(EventWithId { change_id: change_id.clone(), event });
        }
    }

    // Sort by timestamp to ensure consistent ordering
    events.sort_by_key(|e| e.event.timestamp());

    Ok(events)
}

/// Escape a string value for metadata storage
/// Encodes characters that would break key=value parsing: %, =, \n, \r
fn escape_metadata_value(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '%' => result.push_str("%25"),
            '=' => result.push_str("%3D"),
            '\n' => result.push_str("%0A"),
            '\r' => result.push_str("%0D"),
            _ => result.push(c),
        }
    }
    result
}

/// Unescape a metadata value
fn unescape_metadata_value(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            // Read two hex characters
            let hex: String = chars.by_ref().take(2).collect();
            match hex.as_str() {
                "25" => result.push('%'),
                "3D" | "3d" => result.push('='),
                "0A" | "0a" => result.push('\n'),
                "0D" | "0d" => result.push('\r'),
                _ => {
                    // Unknown escape, keep as-is
                    result.push('%');
                    result.push_str(&hex);
                }
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Helper to add metadata field (for safe values like task_id, event type)
fn add_metadata(key: &str, value: impl std::fmt::Display, lines: &mut Vec<String>) {
    lines.push(format!("{}={}", key, value));
}

/// Helper to add metadata field with escaping (for user-provided text)
fn add_metadata_escaped(key: &str, value: &str, lines: &mut Vec<String>) {
    lines.push(format!("{}={}", key, escape_metadata_value(value)));
}

/// Helper to add timestamp metadata field
fn add_metadata_timestamp(timestamp: &chrono::DateTime<chrono::Utc>, lines: &mut Vec<String>) {
    add_metadata("timestamp", timestamp.to_rfc3339(), lines);
}

/// Convert a TaskEvent to a metadata block string
fn event_to_metadata_block(event: &TaskEvent) -> String {
    let mut lines = vec![METADATA_START.to_string()];

    match event {
        TaskEvent::Created {
            task_id,
            name,
            slug,
            task_type,
            priority,
            assignee,
            sources,
            template,
            working_copy,
            instructions,
            data,
            timestamp,
        } => {
            add_metadata("event", "created", &mut lines);
            add_metadata("task_id", task_id, &mut lines);
            add_metadata_escaped("name", name, &mut lines);
            if let Some(slug) = slug {
                add_metadata("slug", slug, &mut lines);
            }
            if let Some(task_type) = task_type {
                add_metadata("type", task_type, &mut lines);
            }
            add_metadata("priority", priority, &mut lines);
            if let Some(assignee) = assignee {
                add_metadata("assignee", assignee, &mut lines);
            }
            // Add source= lines (one per source)
            for source in sources {
                add_metadata("source", source, &mut lines);
            }
            // Add template if present
            if let Some(template) = template {
                add_metadata("template", template, &mut lines);
            }
            // Add working_copy if present
            if let Some(wc) = working_copy {
                add_metadata("working_copy", wc, &mut lines);
            }
            // Add instructions if present (escaped to handle newlines and special chars)
            if let Some(instr) = instructions {
                add_metadata_escaped("instructions", instr, &mut lines);
            }
            // Add data= lines (key:value pairs)
            for (key, value) in data {
                add_metadata_escaped("data", &format!("{}:{}", key, value), &mut lines);
            }
            add_metadata_timestamp(timestamp, &mut lines);
        }
        TaskEvent::Started {
            task_ids,
            agent_type,
            session_id,
            turn_id,
            timestamp,
        } => {
            add_metadata("event", "started", &mut lines);
            for task_id in task_ids {
                add_metadata("task_id", task_id, &mut lines);
            }
            add_metadata("agent_type", agent_type, &mut lines);
            if let Some(sid) = session_id {
                add_metadata("session_id", sid, &mut lines);
            }
            if let Some(tid) = turn_id {
                add_metadata("turn_id", tid, &mut lines);
            }
            add_metadata_timestamp(timestamp, &mut lines);
        }
        TaskEvent::Stopped {
            task_ids,
            reason,
            turn_id,
            timestamp,
        } => {
            add_metadata("event", "stopped", &mut lines);
            for task_id in task_ids {
                add_metadata("task_id", task_id, &mut lines);
            }
            if let Some(reason) = reason {
                add_metadata_escaped("reason", reason, &mut lines);
            }
            if let Some(tid) = turn_id {
                add_metadata("turn_id", tid, &mut lines);
            }
            add_metadata_timestamp(timestamp, &mut lines);
        }
        TaskEvent::Closed {
            task_ids,
            outcome,
            summary,
            turn_id,
            timestamp,
        } => {
            add_metadata("event", "closed", &mut lines);
            for task_id in task_ids {
                add_metadata("task_id", task_id, &mut lines);
            }
            add_metadata("outcome", outcome, &mut lines);
            if let Some(summary) = summary {
                add_metadata_escaped("summary", summary, &mut lines);
            }
            if let Some(tid) = turn_id {
                add_metadata("turn_id", tid, &mut lines);
            }
            add_metadata_timestamp(timestamp, &mut lines);
        }
        TaskEvent::Reopened {
            task_id,
            reason,
            timestamp,
        } => {
            add_metadata("event", "reopened", &mut lines);
            add_metadata("task_id", task_id, &mut lines);
            add_metadata_escaped("reason", reason, &mut lines);
            add_metadata_timestamp(timestamp, &mut lines);
        }
        TaskEvent::CommentAdded {
            task_ids,
            text,
            data,
            timestamp,
        } => {
            add_metadata("event", "comment_added", &mut lines);
            for task_id in task_ids {
                add_metadata("task_id", task_id, &mut lines);
            }
            add_metadata_escaped("text", text, &mut lines);
            // Add data= lines (key:value pairs)
            for (key, value) in data {
                add_metadata_escaped("data", &format!("{}:{}", key, value), &mut lines);
            }
            add_metadata_timestamp(timestamp, &mut lines);
        }
        TaskEvent::Updated {
            task_id,
            name,
            priority,
            assignee,
            data,
            instructions,
            timestamp,
        } => {
            add_metadata("event", "updated", &mut lines);
            add_metadata("task_id", task_id, &mut lines);
            if let Some(name) = name {
                add_metadata_escaped("name", name, &mut lines);
            }
            if let Some(priority) = priority {
                add_metadata("priority", priority, &mut lines);
            }
            // Serialize assignee: Some(a) = "assignee=<value>", None = no change (omit)
            if let Some(ref a) = assignee {
                add_metadata("assignee", a, &mut lines);
            }
            if let Some(data) = data {
                for (key, value) in data {
                    add_metadata_escaped("data", &format!("{}:{}", key, value), &mut lines);
                }
            }
            // Add instructions if present (escaped to handle newlines and special chars)
            if let Some(instr) = instructions {
                add_metadata_escaped("instructions", instr, &mut lines);
            }
            add_metadata_timestamp(timestamp, &mut lines);
        }
        TaskEvent::FieldsCleared {
            task_id,
            fields,
            timestamp,
        } => {
            add_metadata("event", "fields_cleared", &mut lines);
            add_metadata("task_id", task_id, &mut lines);
            add_metadata("fields", &fields.join(","), &mut lines);
            add_metadata_timestamp(timestamp, &mut lines);
        }
        TaskEvent::LinkAdded {
            from,
            to,
            kind,
            autorun,
            timestamp,
        } => {
            add_metadata("event", "link_added", &mut lines);
            add_metadata("from", from, &mut lines);
            add_metadata_escaped("to", to, &mut lines);
            add_metadata("kind", kind, &mut lines);
            if let Some(ar) = autorun {
                add_metadata("autorun", ar, &mut lines);
            }
            add_metadata_timestamp(timestamp, &mut lines);
        }
        TaskEvent::LinkRemoved {
            from,
            to,
            kind,
            reason,
            timestamp,
        } => {
            add_metadata("event", "link_removed", &mut lines);
            add_metadata("from", from, &mut lines);
            add_metadata_escaped("to", to, &mut lines);
            add_metadata("kind", kind, &mut lines);
            if let Some(reason) = reason {
                add_metadata_escaped("reason", reason, &mut lines);
            }
            add_metadata_timestamp(timestamp, &mut lines);
        }
    }

    lines.push(METADATA_END.to_string());
    lines.join("\n")
}

/// Parse all `[aiki-task]...[/aiki-task]` blocks from a commit description.
///
/// Supports both single-event commits and batch commits (from `write_events_batch`)
/// where multiple metadata blocks are concatenated in a single commit message.
fn parse_all_metadata_blocks(desc: &str, events: &mut Vec<TaskEvent>) {
    let mut search_from = 0;
    while let Some(start_idx) = desc[search_from..].find(METADATA_START) {
        let abs_start = search_from + start_idx + METADATA_START.len();
        if let Some(end_offset) = desc[abs_start..].find(METADATA_END) {
            let block = &desc[abs_start..abs_start + end_offset];
            if let Some(event) = parse_metadata_block(block) {
                events.push(event);
            }
            search_from = abs_start + end_offset + METADATA_END.len();
        } else {
            break;
        }
    }
}

/// Parse a metadata block into a TaskEvent
fn parse_metadata_block(block: &str) -> Option<TaskEvent> {
    let mut fields: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();

    // Collect all values for each key (to handle multiple task_id= lines)
    for line in block.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once('=') {
            fields
                .entry(key.trim())
                .or_insert_with(Vec::new)
                .push(value.trim());
        }
    }

    let event_type = fields.get("event")?.first()?;
    let timestamp = fields
        .get("timestamp")
        .and_then(|v| v.first())
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);

    match *event_type {
        "created" => {
            let task_id = fields.get("task_id")?.first()?.to_string();
            let name = unescape_metadata_value(fields.get("name")?.first()?);
            // Parse slug
            let slug = fields
                .get("slug")
                .and_then(|v| v.first())
                .map(|s| s.to_string());
            // Parse task_type
            let task_type = fields
                .get("type")
                .and_then(|v| v.first())
                .map(|s| s.to_string());
            let priority = fields
                .get("priority")
                .and_then(|v| v.first())
                .and_then(|s| TaskPriority::from_str(s))
                .unwrap_or_default();
            let assignee = fields
                .get("assignee")
                .and_then(|v| v.first())
                .map(|s| s.to_string());
            // Parse sources (multiple source= lines)
            let sources = fields
                .get("source")
                .map(|v| v.iter().map(|s| s.to_string()).collect())
                .unwrap_or_else(Vec::new);
            // Parse template
            let template = fields
                .get("template")
                .and_then(|v| v.first())
                .map(|s| s.to_string());
            // Parse working_copy
            let working_copy = fields
                .get("working_copy")
                .and_then(|v| v.first())
                .map(|s| s.to_string());
            // Parse instructions (escaped value)
            let instructions = fields
                .get("instructions")
                .and_then(|v| v.first())
                .map(|s| unescape_metadata_value(s));
            // Parse data (multiple data= lines with key:value format)
            let data = fields
                .get("data")
                .map(|v| {
                    v.iter()
                        .filter_map(|s| {
                            let s = unescape_metadata_value(s);
                            let (key, value) = s.split_once(':')?;
                            Some((key.to_string(), value.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();

            Some(TaskEvent::Created {
                task_id,
                name,
                slug,
                task_type,
                priority,
                assignee,
                sources,
                template,
                working_copy,
                instructions,
                data,
                timestamp,
            })
        }
        "started" => {
            let task_ids = fields
                .get("task_id")?
                .iter()
                .map(|s| s.to_string())
                .collect();
            let agent_type = fields
                .get("agent_type")
                .and_then(|v| v.first())
                .unwrap_or(&"unknown")
                .to_string();
            let session_id = fields
                .get("session_id")
                .and_then(|v| v.first())
                .map(|s| s.to_string());
            let turn_id = fields
                .get("turn_id")
                .and_then(|v| v.first())
                .map(|s| s.to_string());

            Some(TaskEvent::Started {
                task_ids,
                agent_type,
                session_id,
                turn_id,
                timestamp,
            })
        }
        "stopped" => {
            let task_ids = fields
                .get("task_id")?
                .iter()
                .map(|s| s.to_string())
                .collect();
            let reason = fields
                .get("reason")
                .and_then(|v| v.first())
                .map(|s| unescape_metadata_value(s));
            let turn_id = fields
                .get("turn_id")
                .and_then(|v| v.first())
                .map(|s| s.to_string());
            // Note: blocked_reason field is ignored for backward compatibility
            // (old events may still contain it, but it's no longer part of the type)

            Some(TaskEvent::Stopped {
                task_ids,
                reason,
                turn_id,
                timestamp,
            })
        }
        "closed" => {
            let task_ids = fields
                .get("task_id")?
                .iter()
                .map(|s| s.to_string())
                .collect();
            let outcome = fields
                .get("outcome")
                .and_then(|v| v.first())
                .and_then(|s| TaskOutcome::from_str(s))
                .unwrap_or(TaskOutcome::Done);
            let summary = fields
                .get("summary")
                .and_then(|v| v.first())
                .map(|s| unescape_metadata_value(s));
            let turn_id = fields
                .get("turn_id")
                .and_then(|v| v.first())
                .map(|s| s.to_string());

            Some(TaskEvent::Closed {
                task_ids,
                outcome,
                summary,
                turn_id,
                timestamp,
            })
        }
        "reopened" => {
            let task_id = fields.get("task_id")?.first()?.to_string();
            let reason = unescape_metadata_value(fields.get("reason")?.first()?);

            Some(TaskEvent::Reopened {
                task_id,
                reason,
                timestamp,
            })
        }
        "comment_added" => {
            let task_ids = fields
                .get("task_id")?
                .iter()
                .map(|s| s.to_string())
                .collect();
            let text = unescape_metadata_value(fields.get("text")?.first()?);
            // Parse data (multiple data= lines with key:value format)
            let data = fields
                .get("data")
                .map(|v| {
                    v.iter()
                        .filter_map(|s| {
                            let s = unescape_metadata_value(s);
                            let (key, value) = s.split_once(':')?;
                            Some((key.to_string(), value.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();

            Some(TaskEvent::CommentAdded {
                task_ids,
                text,
                data,
                timestamp,
            })
        }
        "updated" => {
            let task_id = fields.get("task_id")?.first()?.to_string();
            let name = fields
                .get("name")
                .and_then(|v| v.first())
                .map(|s| unescape_metadata_value(s));
            let priority = fields
                .get("priority")
                .and_then(|v| v.first())
                .and_then(|s| TaskPriority::from_str(s));
            // Parse assignee: absent=None, value=Some(value), empty=error
            let assignee = match fields.get("assignee") {
                Some(v) => {
                    let value = v.first().map(|s| *s).unwrap_or("");
                    if value.is_empty() {
                        eprintln!("Warning: ignoring updated event with empty assignee for task {task_id}. Use `aiki task unset <id> assignee` to clear the assignee.");
                        return None;
                    }
                    Some(value.to_string())
                }
                None => None,
            };

            // Parse data fields (key:value pairs) - None if no data= lines present
            let data = fields.get("data").map(|v| {
                v.iter()
                    .filter_map(|s| {
                        let s = unescape_metadata_value(s);
                        let (key, value) = s.split_once(':')?;
                        Some((key.to_string(), value.to_string()))
                    })
                    .collect()
            });

            // Parse instructions (escaped value)
            let instructions = fields
                .get("instructions")
                .and_then(|v| v.first())
                .map(|s| unescape_metadata_value(s));

            Some(TaskEvent::Updated {
                task_id,
                name,
                priority,
                assignee,
                data,
                instructions,
                timestamp,
            })
        }
        "fields_cleared" => {
            let task_id = fields.get("task_id")?.first()?.to_string();
            let fields_str = fields.get("fields")?.first()?;
            let cleared_fields: Vec<String> = fields_str
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();

            Some(TaskEvent::FieldsCleared {
                task_id,
                fields: cleared_fields,
                timestamp,
            })
        }
        "link_added" => {
            let from = fields.get("from")?.first()?.to_string();
            let to = fields
                .get("to")
                .and_then(|v| v.first())
                .map(|s| unescape_metadata_value(s))?;
            let kind = fields.get("kind")?.first()?.to_string();
            let autorun = fields
                .get("autorun")
                .and_then(|v| v.first())
                .map(|s| *s == "true");

            Some(TaskEvent::LinkAdded {
                from,
                to,
                kind,
                autorun,
                timestamp,
            })
        }
        "link_removed" => {
            let from = fields.get("from")?.first()?.to_string();
            let to = fields
                .get("to")
                .and_then(|v| v.first())
                .map(|s| unescape_metadata_value(s))?;
            let kind = fields.get("kind")?.first()?.to_string();
            let reason = fields
                .get("reason")
                .and_then(|v| v.first())
                .map(|s| unescape_metadata_value(s));

            Some(TaskEvent::LinkRemoved {
                from,
                to,
                kind,
                reason,
                timestamp,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_to_metadata_block_created() {
        let event = TaskEvent::Created {
            task_id: "a1b2".to_string(),
            name: "Fix auth bug".to_string(),
            slug: None,
            task_type: None,
            priority: TaskPriority::P2,
            assignee: Some("claude-code".to_string()),
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data: std::collections::HashMap::new(),
            timestamp: DateTime::parse_from_rfc3339("2026-01-09T10:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };

        let block = event_to_metadata_block(&event);
        assert!(block.contains("[aiki-task]"));
        assert!(block.contains("event=created"));
        assert!(block.contains("task_id=a1b2"));
        assert!(block.contains("name=Fix auth bug"));
        assert!(block.contains("priority=p2"));
        assert!(block.contains("assignee=claude-code"));
        assert!(block.contains("[/aiki-task]"));
    }

    #[test]
    fn test_parse_metadata_block_created() {
        let block = r#"
event=created
task_id=a1b2
name=Fix auth bug
priority=p2
assignee=claude-code
timestamp=2026-01-09T10:30:00Z
"#;

        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::Created {
                task_id,
                name,
                priority,
                assignee,
                ..
            } => {
                assert_eq!(task_id, "a1b2");
                assert_eq!(name, "Fix auth bug");
                assert_eq!(priority, TaskPriority::P2);
                assert_eq!(assignee, Some("claude-code".to_string()));
            }
            _ => panic!("Expected Created event"),
        }
    }

    #[test]
    fn test_parse_metadata_block_started() {
        let block = r#"
event=started
task_id=a1b2
task_id=c3d4
agent_type=claude-code
timestamp=2026-01-09T10:30:00Z
"#;

        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::Started {
                task_ids,
                agent_type,
                ..
            } => {
                assert_eq!(task_ids, vec!["a1b2", "c3d4"]);
                assert_eq!(agent_type, "claude-code");
            }
            _ => panic!("Expected Started event"),
        }
    }

    #[test]
    fn test_parse_metadata_block_stopped() {
        let block = r#"
event=stopped
task_id=a1b2
task_id=c3d4
reason=Need design decision
timestamp=2026-01-09T10:30:00Z
"#;

        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::Stopped {
                task_ids, reason, ..
            } => {
                assert_eq!(task_ids, vec!["a1b2", "c3d4"]);
                assert_eq!(reason, Some("Need design decision".to_string()));
            }
            _ => panic!("Expected Stopped event"),
        }
    }

    #[test]
    fn test_parse_metadata_block_closed() {
        let block = r#"
event=closed
task_id=a1b2
task_id=c3d4
outcome=done
timestamp=2026-01-09T10:30:00Z
"#;

        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::Closed {
                task_ids, outcome, ..
            } => {
                assert_eq!(task_ids, vec!["a1b2", "c3d4"]);
                assert_eq!(outcome, TaskOutcome::Done);
            }
            _ => panic!("Expected Closed event"),
        }
    }

    #[test]
    fn test_roundtrip_created() {
        let original = TaskEvent::Created {
            task_id: "test".to_string(),
            name: "Test task".to_string(),
            slug: None,
            task_type: None,
            priority: TaskPriority::P1,
            assignee: None,
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data: std::collections::HashMap::new(),
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        // Extract the content between markers
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::Created {
                    task_id: id1,
                    name: name1,
                    priority: p1,
                    ..
                },
                TaskEvent::Created {
                    task_id: id2,
                    name: name2,
                    priority: p2,
                    ..
                },
            ) => {
                assert_eq!(id1, id2);
                assert_eq!(name1, name2);
                assert_eq!(p1, p2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_started() {
        let original = TaskEvent::Started {
            task_ids: vec!["task1".to_string(), "task2".to_string()],
            agent_type: "claude-code".to_string(),
            session_id: Some("test-session-uuid".to_string()),
            turn_id: None,
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::Started {
                    task_ids: ids1,
                    agent_type: agent1,
                    ..
                },
                TaskEvent::Started {
                    task_ids: ids2,
                    agent_type: agent2,
                    ..
                },
            ) => {
                assert_eq!(ids1, ids2);
                assert_eq!(agent1, agent2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_stopped() {
        let original = TaskEvent::Stopped {
            task_ids: vec!["task1".to_string()],
            reason: Some("Need more info".to_string()),
            turn_id: None,
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::Stopped {
                    task_ids: ids1,
                    reason: reason1,
                    ..
                },
                TaskEvent::Stopped {
                    task_ids: ids2,
                    reason: reason2,
                    ..
                },
            ) => {
                assert_eq!(ids1, ids2);
                assert_eq!(reason1, reason2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_closed() {
        let original = TaskEvent::Closed {
            task_ids: vec!["task1".to_string(), "task2".to_string()],
            outcome: TaskOutcome::WontDo,
            summary: None,
            turn_id: None,
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::Closed {
                    task_ids: ids1,
                    outcome: outcome1,
                    ..
                },
                TaskEvent::Closed {
                    task_ids: ids2,
                    outcome: outcome2,
                    ..
                },
            ) => {
                assert_eq!(ids1, ids2);
                assert_eq!(outcome1, outcome2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_closed_with_summary() {
        let original = TaskEvent::Closed {
            task_ids: vec!["task1".to_string()],
            outcome: TaskOutcome::Done,
            summary: Some("Fixed the auth bug".to_string()),
            turn_id: None,
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match parsed {
            TaskEvent::Closed {
                task_ids,
                outcome,
                summary,
                ..
            } => {
                assert_eq!(task_ids, vec!["task1".to_string()]);
                assert_eq!(outcome, TaskOutcome::Done);
                assert_eq!(summary, Some("Fixed the auth bug".to_string()));
            }
            _ => panic!("Expected Closed event"),
        }
    }

    #[test]
    fn test_roundtrip_closed_summary_with_special_chars() {
        let original = TaskEvent::Closed {
            task_ids: vec!["task1".to_string()],
            outcome: TaskOutcome::Done,
            summary: Some("Fixed bug: added null check\nnew line here".to_string()),
            turn_id: None,
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match parsed {
            TaskEvent::Closed { summary, .. } => {
                assert_eq!(
                    summary,
                    Some("Fixed bug: added null check\nnew line here".to_string())
                );
            }
            _ => panic!("Expected Closed event"),
        }
    }

    // Edge case tests

    #[test]
    fn test_parse_missing_event_type() {
        let block = r#"
task_id=a1b2
name=Some task
"#;
        assert!(parse_metadata_block(block).is_none());
    }

    #[test]
    fn test_parse_unknown_event_type() {
        let block = r#"
event=unknown
task_id=a1b2
"#;
        assert!(parse_metadata_block(block).is_none());
    }

    #[test]
    fn test_parse_missing_required_fields_created() {
        // Missing task_id
        let block = r#"
event=created
name=Some task
"#;
        assert!(parse_metadata_block(block).is_none());

        // Missing name
        let block = r#"
event=created
task_id=a1b2
"#;
        assert!(parse_metadata_block(block).is_none());
    }

    #[test]
    fn test_parse_missing_timestamp_uses_default() {
        let block = r#"
event=created
task_id=a1b2
name=Some task
"#;
        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::Created { timestamp, .. } => {
                // Should use current time as default (within last second)
                let now = Utc::now();
                let diff = (now - timestamp).num_seconds().abs();
                assert!(diff < 2, "Timestamp should be recent");
            }
            _ => panic!("Expected Created event"),
        }
    }

    #[test]
    fn test_parse_invalid_priority_uses_default() {
        let block = r#"
event=created
task_id=a1b2
name=Some task
priority=invalid
timestamp=2026-01-09T10:30:00Z
"#;
        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::Created { priority, .. } => {
                assert_eq!(priority, TaskPriority::default()); // P2
            }
            _ => panic!("Expected Created event"),
        }
    }

    #[test]
    fn test_parse_whitespace_handling() {
        let block = r#"
  event = created
  task_id = a1b2
  name = Fix auth bug
  timestamp = 2026-01-09T10:30:00Z
"#;
        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::Created { task_id, name, .. } => {
                assert_eq!(task_id, "a1b2");
                assert_eq!(name, "Fix auth bug");
            }
            _ => panic!("Expected Created event"),
        }
    }

    #[test]
    fn test_parse_special_characters_in_name() {
        let block = r#"
event=created
task_id=a1b2
name=Fix <bug> & "error" 'handling'
timestamp=2026-01-09T10:30:00Z
"#;
        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::Created { name, .. } => {
                assert_eq!(name, r#"Fix <bug> & "error" 'handling'"#);
            }
            _ => panic!("Expected Created event"),
        }
    }

    #[test]
    fn test_parse_empty_block() {
        let block = "";
        assert!(parse_metadata_block(block).is_none());

        let block = "   \n\n   ";
        assert!(parse_metadata_block(block).is_none());
    }

    #[test]
    fn test_parse_started_basic() {
        let block = r#"
event=started
task_id=a1b2
agent_type=claude-code
timestamp=2026-01-09T10:30:00Z
"#;
        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::Started { task_ids, .. } => {
                assert_eq!(task_ids, vec!["a1b2"]);
            }
            _ => panic!("Expected Started event"),
        }
    }

    #[test]
    fn test_parse_stopped_with_no_reason() {
        let block = r#"
event=stopped
task_id=a1b2
timestamp=2026-01-09T10:30:00Z
"#;
        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::Stopped {
                reason,
                ..
            } => {
                assert!(reason.is_none());
            }
            _ => panic!("Expected Stopped event"),
        }
    }

    #[test]
    fn test_escape_unescape_roundtrip() {
        let test_cases = [
            "simple text",
            "with=equals",
            "with\nnewline",
            "with\r\nwindows newline",
            "with%percent",
            "complex=value\nwith%all=special\rchars",
            "",
            "===",
            "\n\n\n",
            "100% done = success\nNext line",
        ];

        for original in &test_cases {
            let escaped = escape_metadata_value(original);
            let unescaped = unescape_metadata_value(&escaped);
            assert_eq!(
                original, &unescaped,
                "Roundtrip failed for: {:?}",
                original
            );
        }
    }

    #[test]
    fn test_escape_produces_safe_output() {
        // Escaped output should not contain newlines or unescaped equals
        let input = "key=value\nwith\rnewlines";
        let escaped = escape_metadata_value(input);

        assert!(!escaped.contains('\n'), "Should not contain newline");
        assert!(!escaped.contains('\r'), "Should not contain carriage return");
        assert!(!escaped.contains('='), "Should not contain unescaped equals");
    }

    #[test]
    fn test_roundtrip_created_with_special_chars() {
        let original = TaskEvent::Created {
            task_id: "test".to_string(),
            name: "Fix bug = critical\nSee issue #123".to_string(),
            slug: None,
            task_type: None,
            priority: TaskPriority::P1,
            assignee: None,
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data: std::collections::HashMap::new(),
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        // Extract the content between markers
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::Created {
                    name: name1,
                    ..
                },
                TaskEvent::Created {
                    name: name2,
                    ..
                },
            ) => {
                assert_eq!(name1, name2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_comment_with_special_chars() {
        let original = TaskEvent::CommentAdded {
            task_ids: vec!["a1b2".to_string()],
            text: "This is a comment with\nmultiple lines\nand = signs".to_string(),
            data: std::collections::HashMap::new(),
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::CommentAdded { text: text1, .. },
                TaskEvent::CommentAdded { text: text2, .. },
            ) => {
                assert_eq!(text1, text2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_comment_added_with_data() {
        use std::collections::HashMap;

        let mut data = HashMap::new();
        data.insert("file".to_string(), "src/auth.ts".to_string());
        data.insert("line".to_string(), "42".to_string());
        data.insert("severity".to_string(), "error".to_string());
        data.insert("category".to_string(), "quality".to_string());

        let original = TaskEvent::CommentAdded {
            task_ids: vec!["xqrmnpst".to_string()],
            text: "Potential null pointer dereference".to_string(),
            data,
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::CommentAdded {
                    task_ids: ids1,
                    text: text1,
                    data: data1,
                    ..
                },
                TaskEvent::CommentAdded {
                    task_ids: ids2,
                    text: text2,
                    data: data2,
                    ..
                },
            ) => {
                assert_eq!(ids1, ids2);
                assert_eq!(text1, text2);
                assert_eq!(data1, data2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_reopened() {
        let timestamp = DateTime::parse_from_rfc3339("2026-01-09T10:30:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let original = TaskEvent::Reopened {
            task_id: "a1b2".to_string(),
            reason: "Found new info".to_string(),
            timestamp,
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::Reopened {
                    task_id: id1,
                    reason: r1,
                    timestamp: t1,
                },
                TaskEvent::Reopened {
                    task_id: id2,
                    reason: r2,
                    timestamp: t2,
                },
            ) => {
                assert_eq!(id1, id2);
                assert_eq!(r1, r2);
                assert_eq!(t1, t2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_reopened_with_special_chars() {
        let original = TaskEvent::Reopened {
            task_id: "a1b2".to_string(),
            reason: "Need to fix = sign\nand newline".to_string(),
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::Reopened { reason: r1, .. },
                TaskEvent::Reopened { reason: r2, .. },
            ) => {
                assert_eq!(r1, r2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_updated_name_only() {
        let timestamp = DateTime::parse_from_rfc3339("2026-01-09T10:30:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let original = TaskEvent::Updated {
            task_id: "a1b2".to_string(),
            name: Some("New name".to_string()),
            priority: None,
            assignee: None,
            data: None,
            instructions: None,
            timestamp,
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::Updated {
                    task_id: id1,
                    name: n1,
                    priority: p1,
                    assignee: a1,
                    timestamp: t1,
                    ..
                },
                TaskEvent::Updated {
                    task_id: id2,
                    name: n2,
                    priority: p2,
                    assignee: a2,
                    timestamp: t2,
                    ..
                },
            ) => {
                assert_eq!(id1, id2);
                assert_eq!(n1, n2);
                assert_eq!(p1, p2);
                assert_eq!(a1, a2);
                assert_eq!(t1, t2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_updated_priority_only() {
        let timestamp = DateTime::parse_from_rfc3339("2026-01-09T10:30:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let original = TaskEvent::Updated {
            task_id: "a1b2".to_string(),
            name: None,
            priority: Some(TaskPriority::P0),
            assignee: None,
            data: None,
            instructions: None,
            timestamp,
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::Updated {
                    name: n1,
                    priority: p1,
                    ..
                },
                TaskEvent::Updated {
                    name: n2,
                    priority: p2,
                    ..
                },
            ) => {
                assert_eq!(n1, n2);
                assert_eq!(p1, p2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_updated_both_fields() {
        let original = TaskEvent::Updated {
            task_id: "a1b2".to_string(),
            name: Some("Updated name".to_string()),
            priority: Some(TaskPriority::P1),
            assignee: None,
            data: None,
            instructions: None,
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::Updated {
                    name: n1,
                    priority: p1,
                    ..
                },
                TaskEvent::Updated {
                    name: n2,
                    priority: p2,
                    ..
                },
            ) => {
                assert_eq!(n1, n2);
                assert_eq!(p1, p2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_updated_with_special_chars() {
        let original = TaskEvent::Updated {
            task_id: "a1b2".to_string(),
            name: Some("Name = special\nwith newlines".to_string()),
            priority: None,
            assignee: None,
            data: None,
            instructions: None,
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::Updated { name: n1, .. },
                TaskEvent::Updated { name: n2, .. },
            ) => {
                assert_eq!(n1, n2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_updated_with_instructions() {
        let original = TaskEvent::Updated {
            task_id: "a1b2".to_string(),
            name: None,
            priority: None,
            assignee: None,
            data: None,
            instructions: Some("1. Check token validation in `auth_handler.rs`\n2. Handle the null `claims` field\n3. Write tests".to_string()),
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::Updated { instructions: i1, .. },
                TaskEvent::Updated { instructions: i2, .. },
            ) => {
                assert_eq!(i1, i2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_updated_instructions_with_special_chars() {
        let original = TaskEvent::Updated {
            task_id: "a1b2".to_string(),
            name: None,
            priority: None,
            assignee: None,
            data: None,
            instructions: Some("Fix bug: x = y + 1\nCheck 100% coverage\nUse `backticks` and \"quotes\"".to_string()),
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::Updated { instructions: i1, .. },
                TaskEvent::Updated { instructions: i2, .. },
            ) => {
                assert_eq!(i1, i2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_updated_no_instructions() {
        let original = TaskEvent::Updated {
            task_id: "a1b2".to_string(),
            name: Some("Updated".to_string()),
            priority: None,
            assignee: None,
            data: None,
            instructions: None,
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        assert!(!block.contains("instructions="), "Should not contain instructions when None");

        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match parsed {
            TaskEvent::Updated { instructions, .. } => {
                assert!(instructions.is_none());
            }
            _ => panic!("Expected Updated event"),
        }
    }

    #[test]
    fn test_parse_metadata_block_reopened() {
        let block = r#"
event=reopened
task_id=a1b2
reason=Found new info
timestamp=2026-01-09T10:30:00Z
"#;

        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::Reopened {
                task_id,
                reason,
                ..
            } => {
                assert_eq!(task_id, "a1b2");
                assert_eq!(reason, "Found new info");
            }
            _ => panic!("Expected Reopened event"),
        }
    }

    #[test]
    fn test_parse_metadata_block_comment_added() {
        let block = r#"
event=comment_added
task_id=a1b2
text=This is a comment
timestamp=2026-01-09T10:30:00Z
"#;

        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::CommentAdded { task_ids, text, .. } => {
                assert_eq!(task_ids, vec!["a1b2"]);
                assert_eq!(text, "This is a comment");
            }
            _ => panic!("Expected CommentAdded event"),
        }
    }

    #[test]
    fn test_parse_metadata_block_updated() {
        let block = r#"
event=updated
task_id=a1b2
name=New name
priority=p0
timestamp=2026-01-09T10:30:00Z
"#;

        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::Updated {
                task_id,
                name,
                priority,
                ..
            } => {
                assert_eq!(task_id, "a1b2");
                assert_eq!(name, Some("New name".to_string()));
                assert_eq!(priority, Some(TaskPriority::P0));
            }
            _ => panic!("Expected Updated event"),
        }
    }

    #[test]
    fn test_parse_metadata_block_updated_partial() {
        // Test with only name, no priority
        let block = r#"
event=updated
task_id=a1b2
name=New name only
timestamp=2026-01-09T10:30:00Z
"#;

        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::Updated {
                name, priority, ..
            } => {
                assert_eq!(name, Some("New name only".to_string()));
                assert_eq!(priority, None);
            }
            _ => panic!("Expected Updated event"),
        }
    }

    #[test]
    fn test_event_to_metadata_block_reopened() {
        let event = TaskEvent::Reopened {
            task_id: "a1b2".to_string(),
            reason: "New information found".to_string(),
            timestamp: DateTime::parse_from_rfc3339("2026-01-09T10:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };

        let block = event_to_metadata_block(&event);
        assert!(block.contains("[aiki-task]"));
        assert!(block.contains("event=reopened"));
        assert!(block.contains("task_id=a1b2"));
        assert!(block.contains("reason=New information found"));
        assert!(block.contains("[/aiki-task]"));
    }

    #[test]
    fn test_event_to_metadata_block_comment_added() {
        let event = TaskEvent::CommentAdded {
            task_ids: vec!["a1b2".to_string()],
            text: "This is a comment".to_string(),
            data: std::collections::HashMap::new(),
            timestamp: DateTime::parse_from_rfc3339("2026-01-09T10:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };

        let block = event_to_metadata_block(&event);
        assert!(block.contains("[aiki-task]"));
        assert!(block.contains("event=comment_added"));
        assert!(block.contains("task_id=a1b2"));
        assert!(block.contains("text=This is a comment"));
        assert!(block.contains("[/aiki-task]"));
    }

    #[test]
    fn test_event_to_metadata_block_updated() {
        let event = TaskEvent::Updated {
            task_id: "a1b2".to_string(),
            name: Some("New task name".to_string()),
            priority: Some(TaskPriority::P1),
            assignee: None,
            data: None,
            instructions: None,
            timestamp: DateTime::parse_from_rfc3339("2026-01-09T10:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };

        let block = event_to_metadata_block(&event);
        assert!(block.contains("[aiki-task]"));
        assert!(block.contains("event=updated"));
        assert!(block.contains("task_id=a1b2"));
        assert!(block.contains("name=New task name"));
        assert!(block.contains("priority=p1"));
        assert!(block.contains("[/aiki-task]"));
    }

    #[test]
    fn test_roundtrip_fields_cleared() {
        let timestamp = DateTime::parse_from_rfc3339("2026-01-09T10:30:00Z")
            .unwrap()
            .with_timezone(&Utc);

        let original = TaskEvent::FieldsCleared {
            task_id: "a1b2".to_string(),
            fields: vec![
                "assignee".to_string(),
                "instructions".to_string(),
                "data.mykey".to_string(),
            ],
            timestamp,
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::FieldsCleared {
                    task_id: id1,
                    fields: f1,
                    timestamp: t1,
                },
                TaskEvent::FieldsCleared {
                    task_id: id2,
                    fields: f2,
                    timestamp: t2,
                },
            ) => {
                assert_eq!(id1, id2);
                assert_eq!(f1, f2);
                assert_eq!(t1, t2);
            }
            _ => panic!("Expected FieldsCleared events"),
        }
    }

    #[test]
    fn test_roundtrip_fields_cleared_single_field() {
        let original = TaskEvent::FieldsCleared {
            task_id: "a1b2".to_string(),
            fields: vec!["assignee".to_string()],
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");
        match parsed {
            TaskEvent::FieldsCleared { fields, .. } => {
                assert_eq!(fields, vec!["assignee".to_string()]);
            }
            _ => panic!("Expected FieldsCleared event"),
        }
    }

    #[test]
    fn test_parse_fields_cleared() {
        let block = r#"
event=fields_cleared
task_id=a1b2
fields=assignee,instructions,data.mykey
timestamp=2026-01-09T10:30:00Z
"#;

        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::FieldsCleared {
                task_id, fields, ..
            } => {
                assert_eq!(task_id, "a1b2");
                assert_eq!(
                    fields,
                    vec![
                        "assignee".to_string(),
                        "instructions".to_string(),
                        "data.mykey".to_string()
                    ]
                );
            }
            _ => panic!("Expected FieldsCleared event"),
        }
    }

    #[test]
    fn test_empty_assignee_is_rejected() {
        // Empty assignee= is invalid — use `aiki task unset <id> assignee` instead.
        let block = r#"
event=updated
task_id=a1b2
assignee=
timestamp=2026-01-09T10:30:00Z
"#;

        let result = parse_metadata_block(block);
        assert!(result.is_none(), "Empty assignee should be rejected");
    }

    #[test]
    fn test_roundtrip_updated_with_assignee() {
        let original = TaskEvent::Updated {
            task_id: "a1b2".to_string(),
            name: None,
            priority: None,
            assignee: Some("claude-code".to_string()),
            data: None,
            instructions: None,
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        assert!(block.contains("assignee=claude-code"));

        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");
        match parsed {
            TaskEvent::Updated { assignee, .. } => {
                assert_eq!(assignee, Some("claude-code".to_string()));
            }
            _ => panic!("Expected Updated event"),
        }
    }

    #[test]
    fn test_roundtrip_link_added() {
        let original = TaskEvent::LinkAdded {
            from: "mvslrspmoynoxyyywqyutmovxpvztkls".to_string(),
            to: "nqrtxsypzkwolmnrstvuqxyzplmrwknos".to_string(),
            kind: "blocked-by".to_string(),
            autorun: None,
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::LinkAdded {
                    from: f1,
                    to: t1,
                    kind: k1,
                    ..
                },
                TaskEvent::LinkAdded {
                    from: f2,
                    to: t2,
                    kind: k2,
                    ..
                },
            ) => {
                assert_eq!(f1, f2);
                assert_eq!(t1, t2);
                assert_eq!(k1, k2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_link_added_external_ref() {
        let original = TaskEvent::LinkAdded {
            from: "mvslrspmoynoxyyywqyutmovxpvztkls".to_string(),
            to: "file:ops/now/design.md".to_string(),
            kind: "sourced-from".to_string(),
            autorun: None,
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match parsed {
            TaskEvent::LinkAdded { from, to, kind, .. } => {
                assert_eq!(from, "mvslrspmoynoxyyywqyutmovxpvztkls");
                assert_eq!(to, "file:ops/now/design.md");
                assert_eq!(kind, "sourced-from");
            }
            _ => panic!("Expected LinkAdded event"),
        }
    }

    #[test]
    fn test_roundtrip_link_removed() {
        let original = TaskEvent::LinkRemoved {
            from: "mvslrspmoynoxyyywqyutmovxpvztkls".to_string(),
            to: "nqrtxsypzkwolmnrstvuqxyzplmrwknos".to_string(),
            kind: "blocked-by".to_string(),
            reason: Some("Blocker resolved".to_string()),
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match (original, parsed) {
            (
                TaskEvent::LinkRemoved {
                    from: f1,
                    to: t1,
                    kind: k1,
                    reason: r1,
                    ..
                },
                TaskEvent::LinkRemoved {
                    from: f2,
                    to: t2,
                    kind: k2,
                    reason: r2,
                    ..
                },
            ) => {
                assert_eq!(f1, f2);
                assert_eq!(t1, t2);
                assert_eq!(k1, k2);
                assert_eq!(r1, r2);
            }
            _ => panic!("Event type mismatch"),
        }
    }

    #[test]
    fn test_roundtrip_link_removed_no_reason() {
        let original = TaskEvent::LinkRemoved {
            from: "mvslrspmoynoxyyywqyutmovxpvztkls".to_string(),
            to: "nqrtxsypzkwolmnrstvuqxyzplmrwknos".to_string(),
            kind: "subtask-of".to_string(),
            reason: None,
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];

        let parsed = parse_metadata_block(content).expect("Should parse");

        match parsed {
            TaskEvent::LinkRemoved { reason, .. } => {
                assert!(reason.is_none());
            }
            _ => panic!("Expected LinkRemoved event"),
        }
    }

    #[test]
    fn test_parse_link_added() {
        let block = r#"
event=link_added
from=mvslrspmoynoxyyywqyutmovxpvztkls
to=nqrtxsypzkwolmnrstvuqxyzplmrwknos
kind=blocked-by
timestamp=2026-02-10T14:30:00Z
"#;

        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::LinkAdded {
                from, to, kind, ..
            } => {
                assert_eq!(from, "mvslrspmoynoxyyywqyutmovxpvztkls");
                assert_eq!(to, "nqrtxsypzkwolmnrstvuqxyzplmrwknos");
                assert_eq!(kind, "blocked-by");
            }
            _ => panic!("Expected LinkAdded event"),
        }
    }

    #[test]
    fn test_roundtrip_link_added_with_autorun() {
        let original = TaskEvent::LinkAdded {
            from: "mvslrspmoynoxyyywqyutmovxpvztkls".to_string(),
            to: "nqrtxsypzkwolmnrstvuqxyzplmrwknos".to_string(),
            kind: "validates".to_string(),
            autorun: Some(true),
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        assert!(block.contains("autorun=true"), "Serialized block should contain autorun=true");

        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];
        let parsed = parse_metadata_block(content).expect("Should parse");

        match parsed {
            TaskEvent::LinkAdded { autorun, .. } => {
                assert_eq!(autorun, Some(true));
            }
            _ => panic!("Expected LinkAdded event"),
        }
    }

    #[test]
    fn test_roundtrip_link_added_autorun_false() {
        let original = TaskEvent::LinkAdded {
            from: "mvslrspmoynoxyyywqyutmovxpvztkls".to_string(),
            to: "nqrtxsypzkwolmnrstvuqxyzplmrwknos".to_string(),
            kind: "depends-on".to_string(),
            autorun: Some(false),
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        assert!(block.contains("autorun=false"), "Serialized block should contain autorun=false");

        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];
        let parsed = parse_metadata_block(content).expect("Should parse");

        match parsed {
            TaskEvent::LinkAdded { autorun, .. } => {
                assert_eq!(autorun, Some(false));
            }
            _ => panic!("Expected LinkAdded event"),
        }
    }

    #[test]
    fn test_parse_link_added_without_autorun_backward_compat() {
        // Old events without autorun field should parse with autorun=None
        let block = r#"
event=link_added
from=mvslrspmoynoxyyywqyutmovxpvztkls
to=nqrtxsypzkwolmnrstvuqxyzplmrwknos
kind=blocked-by
timestamp=2026-02-10T14:30:00Z
"#;
        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::LinkAdded { autorun, .. } => {
                assert_eq!(autorun, None, "Missing autorun field should parse as None");
            }
            _ => panic!("Expected LinkAdded event"),
        }
    }

    #[test]
    fn test_link_added_no_autorun_omitted_in_serialization() {
        let original = TaskEvent::LinkAdded {
            from: "mvslrspmoynoxyyywqyutmovxpvztkls".to_string(),
            to: "nqrtxsypzkwolmnrstvuqxyzplmrwknos".to_string(),
            kind: "blocked-by".to_string(),
            autorun: None,
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        assert!(!block.contains("autorun"), "None autorun should not be serialized");
    }

    #[test]
    fn test_parse_link_removed() {
        let block = r#"
event=link_removed
from=mvslrspmoynoxyyywqyutmovxpvztkls
to=nqrtxsypzkwolmnrstvuqxyzplmrwknos
kind=blocked-by
reason=No longer needed
timestamp=2026-02-10T14:30:00Z
"#;

        let event = parse_metadata_block(block).expect("Should parse");
        match event {
            TaskEvent::LinkRemoved {
                from,
                to,
                kind,
                reason,
                ..
            } => {
                assert_eq!(from, "mvslrspmoynoxyyywqyutmovxpvztkls");
                assert_eq!(to, "nqrtxsypzkwolmnrstvuqxyzplmrwknos");
                assert_eq!(kind, "blocked-by");
                assert_eq!(reason, Some("No longer needed".to_string()));
            }
            _ => panic!("Expected LinkRemoved event"),
        }
    }

    #[test]
    fn test_roundtrip_started_with_turn_id() {
        let original = TaskEvent::Started {
            task_ids: vec!["task1".to_string()],
            agent_type: "claude-code".to_string(),
            session_id: Some("sess-123".to_string()),
            turn_id: Some("turn-abc-1".to_string()),
            timestamp: Utc::now(),

        };

        let block = event_to_metadata_block(&original);
        assert!(block.contains("turn_id=turn-abc-1"), "Serialized block should contain turn_id");

        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];
        let parsed = parse_metadata_block(content).expect("Should parse");

        match parsed {
            TaskEvent::Started { turn_id, .. } => {
                assert_eq!(turn_id, Some("turn-abc-1".to_string()));
            }
            _ => panic!("Expected Started event"),
        }
    }

    #[test]
    fn test_roundtrip_stopped_with_turn_id() {
        let original = TaskEvent::Stopped {
            task_ids: vec!["task1".to_string()],
            reason: Some("blocked".to_string()),
            turn_id: Some("turn-xyz-5".to_string()),
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        assert!(block.contains("turn_id=turn-xyz-5"), "Serialized block should contain turn_id");

        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];
        let parsed = parse_metadata_block(content).expect("Should parse");

        match parsed {
            TaskEvent::Stopped { turn_id, .. } => {
                assert_eq!(turn_id, Some("turn-xyz-5".to_string()));
            }
            _ => panic!("Expected Stopped event"),
        }
    }

    #[test]
    fn test_roundtrip_closed_with_turn_id() {
        let original = TaskEvent::Closed {
            task_ids: vec!["task1".to_string()],
            outcome: TaskOutcome::Done,
            summary: Some("All done".to_string()),
            turn_id: Some("turn-def-3".to_string()),
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        assert!(block.contains("turn_id=turn-def-3"), "Serialized block should contain turn_id");

        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];
        let parsed = parse_metadata_block(content).expect("Should parse");

        match parsed {
            TaskEvent::Closed { turn_id, .. } => {
                assert_eq!(turn_id, Some("turn-def-3".to_string()));
            }
            _ => panic!("Expected Closed event"),
        }
    }

    #[test]
    fn test_roundtrip_started_without_turn_id() {
        // Backward compatibility: turn_id=None should serialize without turn_id line
        let original = TaskEvent::Started {
            task_ids: vec!["task1".to_string()],
            agent_type: "claude-code".to_string(),
            session_id: None,
            turn_id: None,
            timestamp: Utc::now(),

        };

        let block = event_to_metadata_block(&original);
        assert!(!block.contains("turn_id="), "Should not contain turn_id when None");

        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];
        let parsed = parse_metadata_block(content).expect("Should parse");

        match parsed {
            TaskEvent::Started { turn_id, .. } => {
                assert_eq!(turn_id, None);
            }
            _ => panic!("Expected Started event"),
        }
    }

    #[test]
    fn test_roundtrip_created_with_slug() {
        let original = TaskEvent::Created {
            task_id: "test".to_string(),
            name: "Build feature".to_string(),
            slug: Some("build".to_string()),
            task_type: None,
            priority: TaskPriority::P2,
            assignee: None,
            sources: Vec::new(),
            template: None,
            working_copy: None,
            instructions: None,
            data: std::collections::HashMap::new(),
            timestamp: Utc::now(),
        };

        let block = event_to_metadata_block(&original);
        assert!(block.contains("slug=build"), "Serialized block should contain slug");

        let start = block.find("[aiki-task]").unwrap() + "[aiki-task]".len();
        let end = block.find("[/aiki-task]").unwrap();
        let content = &block[start..end];
        let parsed = parse_metadata_block(content).expect("Should parse");

        match parsed {
            TaskEvent::Created { slug, .. } => {
                assert_eq!(slug, Some("build".to_string()));
            }
            _ => panic!("Expected Created event"),
        }
    }

    #[test]
    fn test_backward_compat_created_without_slug() {
        // Simulate an old-style metadata block without slug field
        let block = "event=created\ntask_id=test123\nname=Old task\npriority=p2\ntimestamp=2026-01-01T00:00:00Z\n";
        let parsed = parse_metadata_block(block).expect("Should parse");

        match parsed {
            TaskEvent::Created { slug, .. } => {
                assert_eq!(slug, None, "Old events without slug should deserialize as None");
            }
            _ => panic!("Expected Created event"),
        }
    }

    #[test]
    fn test_parse_all_metadata_blocks_single() {
        let desc = "[aiki-task]\nevent=created\ntask_id=abc\nname=Test\npriority=p2\ntimestamp=2026-01-09T10:30:00Z\n[/aiki-task]";
        let mut events = Vec::new();
        parse_all_metadata_blocks(desc, &mut events);
        assert_eq!(events.len(), 1);
        match &events[0] {
            TaskEvent::Created { task_id, .. } => assert_eq!(task_id, "abc"),
            _ => panic!("Expected Created"),
        }
    }

    #[test]
    fn test_parse_all_metadata_blocks_batch() {
        // Simulate a batch commit with two events (close + reopen)
        let desc = "[aiki-task]\nevent=closed\ntask_id=task1\noutcome=done\ntimestamp=2026-01-09T10:30:00Z\n[/aiki-task]\n[aiki-task]\nevent=reopened\ntask_id=task1\nreason=Spawning subtask\ntimestamp=2026-01-09T10:30:01Z\n[/aiki-task]";
        let mut events = Vec::new();
        parse_all_metadata_blocks(desc, &mut events);
        assert_eq!(events.len(), 2);
        match &events[0] {
            TaskEvent::Closed { task_ids, .. } => assert_eq!(task_ids, &["task1"]),
            _ => panic!("Expected Closed, got {:?}", events[0]),
        }
        match &events[1] {
            TaskEvent::Reopened { task_id, reason, .. } => {
                assert_eq!(task_id, "task1");
                assert_eq!(reason, "Spawning subtask");
            }
            _ => panic!("Expected Reopened, got {:?}", events[1]),
        }
    }

    #[test]
    fn test_parse_all_metadata_blocks_three_events() {
        let desc = "\
[aiki-task]\nevent=created\ntask_id=a\nname=Task A\npriority=p2\ntimestamp=2026-01-09T10:30:00Z\n[/aiki-task]\n\
[aiki-task]\nevent=created\ntask_id=b\nname=Task B\npriority=p1\ntimestamp=2026-01-09T10:30:01Z\n[/aiki-task]\n\
[aiki-task]\nevent=link_added\nfrom=b\nto=a\nkind=subtask-of\ntimestamp=2026-01-09T10:30:02Z\n[/aiki-task]";
        let mut events = Vec::new();
        parse_all_metadata_blocks(desc, &mut events);
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn test_parse_all_metadata_blocks_empty() {
        let mut events = Vec::new();
        parse_all_metadata_blocks("no metadata here", &mut events);
        assert!(events.is_empty());
    }
}
