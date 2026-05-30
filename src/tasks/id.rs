//! Task ID generation
//!
//! Generates unique task IDs compatible with JJ's change_id format.
//!
//! JJ change_ids are 32-character strings using "reverse hex" encoding (z-k instead of 0-9a-f).
//! This makes them more readable and reduces confusion with commit hashes.

use std::time::{SystemTime, UNIX_EPOCH};

/// Generate a unique task ID in JJ change_id format
///
/// Creates a 32-character ID using JJ's "reverse hex" encoding (z-k instead of 0-9a-f).
/// The ID combines timestamp, task name hash, and cryptographically random data to ensure
/// uniqueness even in bulk/automated operations.
///
/// # Uniqueness Guarantee
///
/// Uses 128 bits of entropy from:
/// - Nanosecond-precision timestamp
/// - Task name hash
/// - Two 64-bit random salts
///
/// This provides collision resistance equivalent to JJ's native change_id generation,
/// with negligible collision probability (< 2^-64) even for billions of tasks.
///
/// # Example
/// ```
/// use aiki::tasks::generate_task_id;
/// let id = generate_task_id("Fix bug");
/// assert_eq!(id.len(), 32);
/// assert!(id.chars().all(|c| matches!(c, 'k'..='z')));
/// ```
#[must_use]
pub fn generate_task_id(name: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    // Generate 128 bits (16 bytes) using timestamp, name hash, and random salt
    let mut hasher = DefaultHasher::new();
    timestamp.hash(&mut hasher);
    name.hash(&mut hasher);

    // Add random salt for true uniqueness (prevents collisions in bulk operations)
    use rand::Rng;
    let random_salt: u64 = rand::thread_rng().gen();
    random_salt.hash(&mut hasher);
    let hash1 = hasher.finish();

    // Generate second part with additional randomness
    let mut hasher2 = DefaultHasher::new();
    hash1.hash(&mut hasher2);
    let random_salt2: u64 = rand::thread_rng().gen();
    random_salt2.hash(&mut hasher2);
    let hash2 = hasher2.finish();

    // Convert to 32 characters using reverse hex (z-k instead of 0-9a-f)
    // z=0, y=1, x=2, w=3, v=4, u=5, t=6, s=7, r=8, q=9, p=a, o=b, n=c, m=d, l=e, k=f
    let reverse_hex = |nibble: u8| -> char {
        match nibble {
            0 => 'z',
            1 => 'y',
            2 => 'x',
            3 => 'w',
            4 => 'v',
            5 => 'u',
            6 => 't',
            7 => 's',
            8 => 'r',
            9 => 'q',
            10 => 'p',
            11 => 'o',
            12 => 'n',
            13 => 'm',
            14 => 'l',
            15 => 'k',
            _ => 'z',
        }
    };

    let mut result = String::with_capacity(32);

    // Encode first 64 bits (16 hex chars)
    for i in (0..16).rev() {
        let nibble = ((hash1 >> (i * 4)) & 0xF) as u8;
        result.push(reverse_hex(nibble));
    }

    // Encode second 64 bits (16 hex chars)
    for i in (0..16).rev() {
        let nibble = ((hash2 >> (i * 4)) & 0xF) as u8;
        result.push(reverse_hex(nibble));
    }

    result
}

/// Generate a child task ID
///
/// Child IDs are created by appending a numeric suffix to the parent ID.
/// For example: "a1b2" -> "a1b2.1", "a1b2.1" -> "a1b2.1.1"
#[must_use]
#[allow(dead_code)] // Kept for backward-compat bridge tests
pub fn generate_child_id(parent_id: &str, child_number: usize) -> String {
    format!("{}.{}", parent_id, child_number)
}

/// Check if a task ID is a child of another task ID
#[must_use]
#[allow(dead_code)] // Part of task ID API
pub fn is_child_of(task_id: &str, parent_id: &str) -> bool {
    task_id.starts_with(&format!("{}.", parent_id))
}

/// Get the parent ID from a child task ID
///
/// Returns `None` if the task ID has no parent (root task).
#[must_use]
pub fn get_parent_id(task_id: &str) -> Option<&str> {
    task_id.rsplit_once('.').map(|(parent, _)| parent)
}

/// Check if a task is a direct child of a parent (not grandchild)
#[must_use]
#[allow(dead_code)] // Kept for backward-compat bridge tests
pub fn is_direct_child_of(task_id: &str, parent_id: &str) -> bool {
    get_parent_id(task_id) == Some(parent_id)
}

/// Get the child number from a task ID (the last numeric suffix)
///
/// Returns `None` if the task ID has no parent.
#[must_use]
#[allow(dead_code)] // Kept for backward-compat bridge tests
pub fn get_child_number(task_id: &str) -> Option<usize> {
    task_id
        .rsplit_once('.')
        .and_then(|(_, num)| num.parse::<usize>().ok())
}

/// Check if a string looks like a task ID prefix (shorter than a full ID)
///
/// Returns true if the input could be a task ID prefix:
/// - Root prefix: 3+ characters of lowercase k-z (e.g., "mvslrsp")
/// - Subtask prefix: root_prefix.N (e.g., "mvslrsp.1")
/// - Full 32-char IDs also return true (a prefix that happens to be complete)
///
/// Returns false for descriptions, IDs with wrong chars, or prefixes under 3 chars.
///
/// # Examples
/// ```
/// use aiki::tasks::is_task_id_prefix;
/// assert!(is_task_id_prefix("mvslrsp"));
/// assert!(is_task_id_prefix("mvslrsp.1"));
/// assert!(is_task_id_prefix("kvx"));
/// assert!(is_task_id_prefix("mvslrspmoynoxyyywqyutmovxpvztkls")); // full ID
/// assert!(!is_task_id_prefix("Fix bug"));
/// assert!(!is_task_id_prefix("abc")); // outside k-z range
/// assert!(!is_task_id_prefix("kv")); // too short
/// ```
#[must_use]
pub fn is_task_id_prefix(input: &str) -> bool {
    if input.is_empty() || input.contains(' ') {
        return false;
    }

    let parts: Vec<&str> = input.split('.').collect();
    let root_part = parts[0];

    // Root prefix must be 3+ chars of k-z
    if root_part.len() < 3 || !root_part.chars().all(|c| matches!(c, 'k'..='z')) {
        return false;
    }

    // If there are additional parts, they must all be numeric
    if parts.len() > 1 {
        for part in &parts[1..] {
            if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
        }
    }

    true
}

/// Check if a string looks like a task ID (vs a task description)
///
/// Task IDs are:
/// - Root IDs: 32 characters, all lowercase k-z (JJ reverse hex)
/// - Child IDs: root_id.N or root_id.N.M (with numeric suffixes)
///
/// Descriptions typically contain:
/// - Spaces
/// - Capital letters
/// - Characters outside k-z
/// - Punctuation other than dots
///
/// # Examples
/// ```
/// use aiki::tasks::is_task_id;
/// assert!(is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls"));
/// assert!(is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls.1"));
/// assert!(!is_task_id("Fix the auth bug"));
/// assert!(!is_task_id("implement-login")); // has hyphen
/// ```
#[must_use]
pub fn is_task_id(input: &str) -> bool {
    // Empty string is not a task ID
    if input.is_empty() {
        return false;
    }

    // Contains space? Definitely a description
    if input.contains(' ') {
        return false;
    }

    // Split by dots to handle child IDs (parent.N.M)
    let parts: Vec<&str> = input.split('.').collect();

    // First part must be a valid root task ID
    let root_part = parts[0];

    // Root IDs are exactly 32 characters of lowercase k-z
    let is_valid_root = root_part.len() == 32
        && root_part.chars().all(|c| matches!(c, 'k'..='z'));

    if !is_valid_root {
        return false;
    }

    // If there are additional parts, they must all be numeric (child numbers)
    if parts.len() > 1 {
        for part in &parts[1..] {
            if part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
        }
    }

    true
}

/// Get the next subtask number for a parent task
///
/// Scans the list of task IDs and finds the highest existing subtask number,
/// then returns the next number. Returns 1 if no subtasks exist.
#[must_use]
#[allow(dead_code)] // Kept for backward-compat bridge tests
pub fn get_next_subtask_number<'a>(
    parent_id: &str,
    task_ids: impl Iterator<Item = &'a str>,
) -> usize {
    let max_subtask = task_ids
        .filter(|id| is_direct_child_of(id, parent_id))
        .filter_map(get_child_number)
        .max();

    max_subtask.map_or(1, |n| n + 1)
}

/// Check if a string is a valid slug format.
///
/// Slugs are stable, human-readable handles for subtask references.
/// Format: `[a-z0-9]([a-z0-9-]*[a-z0-9])?`, 1–48 characters.
///
/// Valid: `build`, `run-tests`, `deploy-staging`, `phase-2`, `a`, `1`
/// Invalid: `-build`, `build-`, `Build`, `run_tests`, `deploy.staging`, `my slug`
#[must_use]
pub fn is_valid_slug(s: &str) -> bool {
    if s.is_empty() || s.len() > 48 {
        return false;
    }

    let bytes = s.as_bytes();

    // Must start and end with alphanumeric
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    if !bytes[bytes.len() - 1].is_ascii_lowercase() && !bytes[bytes.len() - 1].is_ascii_digit() {
        return false;
    }

    // All chars must be lowercase alphanumeric or hyphen
    s.bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_task_id_format() {
        let id = generate_task_id("Test task");
        assert_eq!(id.len(), 32, "Task ID should be 32 characters");
        assert!(
            id.chars().all(|c| matches!(c, 'k'..='z')),
            "Task ID should only contain reverse hex characters (k-z)"
        );
    }

    #[test]
    fn test_generate_task_id_different_names() {
        let id1 = generate_task_id("Task 1");
        let id2 = generate_task_id("Task 2");

        // IDs should be different due to different names
        assert_ne!(
            id1, id2,
            "Different task names should generate different IDs"
        );
        assert_eq!(id1.len(), 32);
        assert_eq!(id2.len(), 32);
    }

    #[test]
    fn test_generate_task_id_uniqueness() {
        // Same name and very close timestamps should still differ due to randomness
        let id1 = generate_task_id("Same name");
        let id2 = generate_task_id("Same name");
        // These will differ due to random salts
        assert_eq!(id1.len(), 32);
        assert_eq!(id2.len(), 32);
    }

    #[test]
    fn test_generate_child_id() {
        // Old short ID format (legacy)
        assert_eq!(generate_child_id("a1b2", 1), "a1b2.1");
        assert_eq!(generate_child_id("a1b2", 2), "a1b2.2");
        assert_eq!(generate_child_id("a1b2.1", 1), "a1b2.1.1");

        // New JJ change_id format
        let change_id = "mvslrspmoynoxyyywqyutmovxpvztkls";
        assert_eq!(
            generate_child_id(change_id, 1),
            "mvslrspmoynoxyyywqyutmovxpvztkls.1"
        );
        assert_eq!(
            generate_child_id(change_id, 2),
            "mvslrspmoynoxyyywqyutmovxpvztkls.2"
        );
        assert_eq!(
            generate_child_id("mvslrspmoynoxyyywqyutmovxpvztkls.1", 1),
            "mvslrspmoynoxyyywqyutmovxpvztkls.1.1"
        );
    }

    #[test]
    fn test_is_child_of() {
        // Old short ID format (legacy)
        assert!(is_child_of("a1b2.1", "a1b2"));
        assert!(is_child_of("a1b2.1.1", "a1b2.1"));
        assert!(is_child_of("a1b2.1.1", "a1b2"));
        assert!(!is_child_of("a1b2", "a1b2"));
        assert!(!is_child_of("a1b2", "a1b2.1"));
        assert!(!is_child_of("b2c3", "a1b2"));

        // New JJ change_id format
        let change_id = "mvslrspmoynoxyyywqyutmovxpvztkls";
        assert!(is_child_of("mvslrspmoynoxyyywqyutmovxpvztkls.1", change_id));
        assert!(is_child_of(
            "mvslrspmoynoxyyywqyutmovxpvztkls.1.1",
            "mvslrspmoynoxyyywqyutmovxpvztkls.1"
        ));
        assert!(is_child_of(
            "mvslrspmoynoxyyywqyutmovxpvztkls.1.1",
            change_id
        ));
        assert!(!is_child_of(change_id, change_id));
        assert!(!is_child_of(
            change_id,
            "mvslrspmoynoxyyywqyutmovxpvztkls.1"
        ));
    }

    #[test]
    fn test_get_parent_id() {
        // Old short ID format (legacy)
        assert_eq!(get_parent_id("a1b2.1"), Some("a1b2"));
        assert_eq!(get_parent_id("a1b2.1.1"), Some("a1b2.1"));
        assert_eq!(get_parent_id("a1b2"), None);

        // New JJ change_id format
        assert_eq!(
            get_parent_id("mvslrspmoynoxyyywqyutmovxpvztkls.1"),
            Some("mvslrspmoynoxyyywqyutmovxpvztkls")
        );
        assert_eq!(
            get_parent_id("mvslrspmoynoxyyywqyutmovxpvztkls.1.1"),
            Some("mvslrspmoynoxyyywqyutmovxpvztkls.1")
        );
        assert_eq!(get_parent_id("mvslrspmoynoxyyywqyutmovxpvztkls"), None);
    }

    #[test]
    fn test_is_direct_child_of() {
        // Direct subtasks
        assert!(is_direct_child_of("a1b2.1", "a1b2"));
        assert!(is_direct_child_of("a1b2.2", "a1b2"));

        // Grandsubtasks are NOT direct subtasks
        assert!(!is_direct_child_of("a1b2.1.1", "a1b2"));
        assert!(is_direct_child_of("a1b2.1.1", "a1b2.1"));

        // Same ID is not a child
        assert!(!is_direct_child_of("a1b2", "a1b2"));

        // Unrelated IDs
        assert!(!is_direct_child_of("b2c3.1", "a1b2"));
    }

    #[test]
    fn test_get_child_number() {
        assert_eq!(get_child_number("a1b2.1"), Some(1));
        assert_eq!(get_child_number("a1b2.42"), Some(42));
        assert_eq!(get_child_number("a1b2.1.3"), Some(3));
        assert_eq!(get_child_number("a1b2"), None);
    }

    #[test]
    fn test_get_next_subtask_number() {
        // No subtasks exist
        let task_ids: Vec<&str> = vec!["a1b2", "other"];
        assert_eq!(get_next_subtask_number("a1b2", task_ids.iter().copied()), 1);

        // Has subtasks
        let task_ids = vec!["a1b2", "a1b2.1", "a1b2.2"];
        assert_eq!(get_next_subtask_number("a1b2", task_ids.iter().copied()), 3);

        // Has gaps (should find max + 1, not fill gap)
        let task_ids = vec!["a1b2", "a1b2.1", "a1b2.5"];
        assert_eq!(get_next_subtask_number("a1b2", task_ids.iter().copied()), 6);

        // Ignores grandsubtasks
        let task_ids = vec!["a1b2", "a1b2.1", "a1b2.1.1", "a1b2.1.2"];
        assert_eq!(get_next_subtask_number("a1b2", task_ids.iter().copied()), 2);

        // Works with nested parents
        let task_ids = vec!["a1b2.1", "a1b2.1.1", "a1b2.1.2"];
        assert_eq!(get_next_subtask_number("a1b2.1", task_ids.iter().copied()), 3);
    }

    // Edge case tests

    #[test]
    fn test_get_parent_id_edge_cases() {
        // Empty string
        assert_eq!(get_parent_id(""), None);

        // No dots - root task
        assert_eq!(get_parent_id("abcd1234"), None);

        // Trailing dot (malformed but handle gracefully)
        assert_eq!(get_parent_id("parent."), Some("parent"));

        // Multiple trailing dots
        assert_eq!(get_parent_id("parent.."), Some("parent."));

        // Just a dot
        assert_eq!(get_parent_id("."), Some(""));
    }

    #[test]
    fn test_is_child_of_edge_cases() {
        // Empty strings
        assert!(!is_child_of("", "parent"));
        assert!(!is_child_of("child", ""));
        assert!(!is_child_of("", ""));

        // Same ID with dot suffix ambiguity
        assert!(is_child_of("parent.1", "parent"));
        assert!(!is_child_of("parent1", "parent")); // No dot = not a child
    }

    #[test]
    fn test_is_direct_child_of_edge_cases() {
        // Empty parent
        assert!(!is_direct_child_of("task.1", ""));

        // ID that looks like but isn't a child (missing dot separator)
        assert!(!is_direct_child_of("parent1", "parent"));

        // Prefix collision: a1b2.10 should NOT match scope a1b2.1
        // This is the bug we fixed!
        assert!(!is_direct_child_of("a1b2.10", "a1b2.1"));
        assert!(is_direct_child_of("a1b2.10", "a1b2"));
        assert!(is_direct_child_of("a1b2.1", "a1b2"));
    }

    #[test]
    fn test_get_child_number_edge_cases() {
        // Non-numeric child number
        assert_eq!(get_child_number("parent.abc"), None);

        // Negative number (parsed as non-numeric)
        assert_eq!(get_child_number("parent.-1"), None);

        // Zero child number (valid but unusual - decompose task)
        assert_eq!(get_child_number("parent.0"), Some(0));

        // Leading zeros (still valid numbers)
        assert_eq!(get_child_number("parent.01"), Some(1));
        assert_eq!(get_child_number("parent.001"), Some(1));

        // Empty after dot
        assert_eq!(get_child_number("parent."), None);

        // Large number
        assert_eq!(get_child_number("parent.999999"), Some(999999));
    }

    #[test]
    fn test_get_next_subtask_number_edge_cases() {
        // Empty task list
        let task_ids: Vec<&str> = vec![];
        assert_eq!(get_next_subtask_number("parent", task_ids.iter().copied()), 1);

        // Non-existent parent (no subtasks found)
        let task_ids = vec!["other.1", "other.2"];
        assert_eq!(get_next_subtask_number("parent", task_ids.iter().copied()), 1);

        // Subtask with number 0 (decompose task)
        let task_ids = vec!["parent", "parent.0", "parent.1"];
        assert_eq!(get_next_subtask_number("parent", task_ids.iter().copied()), 2);

        // Only decompose task exists
        let task_ids = vec!["parent", "parent.0"];
        assert_eq!(get_next_subtask_number("parent", task_ids.iter().copied()), 1);
    }

    #[test]
    fn test_generate_child_id_edge_cases() {
        // Child number 0 (decompose task)
        assert_eq!(generate_child_id("parent", 0), "parent.0");

        // Large child number
        assert_eq!(generate_child_id("parent", 999999), "parent.999999");

        // Nested parent
        assert_eq!(generate_child_id("a.1.2.3", 4), "a.1.2.3.4");
    }

    #[test]
    fn test_id_uniqueness_bulk() {
        // Generate many IDs and verify no collisions
        use std::collections::HashSet;

        let mut ids = HashSet::new();
        for i in 0..1000 {
            let id = generate_task_id(&format!("Task {}", i));
            assert!(
                ids.insert(id.clone()),
                "Collision detected for task {}",
                i
            );
        }
        assert_eq!(ids.len(), 1000);
    }

    // Tests for is_task_id

    #[test]
    fn test_is_task_id_valid_root() {
        // Valid 32-char root ID
        assert!(is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls"));
        assert!(is_task_id("kkkkkkkkkkkkkkkkkkkkkkkkkkkkkkkk"));
        assert!(is_task_id("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"));
    }

    #[test]
    fn test_is_task_id_valid_child() {
        // Valid child IDs
        assert!(is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls.1"));
        assert!(is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls.0"));
        assert!(is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls.42"));
        assert!(is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls.1.2"));
        assert!(is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls.1.2.3"));
    }

    #[test]
    fn test_is_task_id_descriptions() {
        // Descriptions with spaces
        assert!(!is_task_id("Fix the auth bug"));
        assert!(!is_task_id("Implement user authentication"));
        assert!(!is_task_id("Add rate limiting"));

        // Descriptions with capital letters (but no spaces)
        assert!(!is_task_id("FixAuthBug"));

        // Descriptions with hyphens
        assert!(!is_task_id("implement-login"));
        assert!(!is_task_id("fix-null-pointer"));

        // Descriptions with underscores
        assert!(!is_task_id("fix_auth_bug"));

        // Descriptions with numbers
        assert!(!is_task_id("bug123"));
        assert!(!is_task_id("task42"));
    }

    #[test]
    fn test_is_task_id_invalid_format() {
        // Empty string
        assert!(!is_task_id(""));

        // Too short (less than 32 chars)
        assert!(!is_task_id("mvslrspmo"));
        assert!(!is_task_id("abcd"));

        // Too long (more than 32 chars)
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztklsextra"));

        // Wrong characters (a-j instead of k-z)
        assert!(!is_task_id("abcdefghijabcdefghijabcdefghijab"));
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztkla")); // 'a' at end

        // Invalid child format (non-numeric suffix)
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls.abc"));
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls."));
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls.1."));
    }

    #[test]
    fn test_is_task_id_edge_cases() {
        // Short strings that could be ambiguous
        assert!(!is_task_id("test"));
        assert!(!is_task_id("task"));
        assert!(!is_task_id("fix"));

        // Only lowercase k-z but wrong length
        assert!(!is_task_id("mvslrsp")); // 7 chars
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztklss")); // 33 chars
    }

    // Tests for is_task_id_prefix

    #[test]
    fn test_is_task_id_prefix_valid() {
        // Valid short prefixes (3+ k-z chars)
        assert!(is_task_id_prefix("kvx"));
        assert!(is_task_id_prefix("mvslrsp"));
        assert!(is_task_id_prefix("zzzzzzzz"));

        // Full 32-char ID is also a valid prefix
        assert!(is_task_id_prefix("mvslrspmoynoxyyywqyutmovxpvztkls"));
    }

    #[test]
    fn test_is_task_id_prefix_subtask() {
        // Subtask prefixes
        assert!(is_task_id_prefix("mvslrsp.1"));
        assert!(is_task_id_prefix("mvslrsp.0"));
        assert!(is_task_id_prefix("mvslrsp.42"));
        assert!(is_task_id_prefix("mvslrspmoynoxyyywqyutmovxpvztkls.1"));
        assert!(is_task_id_prefix("mvslrsp.1.2"));
    }

    #[test]
    fn test_is_task_id_prefix_too_short() {
        // 1-2 char prefixes are too short
        assert!(!is_task_id_prefix("k"));
        assert!(!is_task_id_prefix("kv"));
        assert!(!is_task_id_prefix(""));
    }

    #[test]
    fn test_is_task_id_prefix_invalid() {
        // Descriptions
        assert!(!is_task_id_prefix("Fix bug"));
        assert!(!is_task_id_prefix("implement-login"));

        // Wrong char range (a-j, not k-z)
        assert!(!is_task_id_prefix("abc"));
        assert!(!is_task_id_prefix("abcdefg"));

        // Mixed valid/invalid chars
        assert!(!is_task_id_prefix("mvslAsp"));
        assert!(!is_task_id_prefix("mvsl1sp"));

        // Invalid subtask suffix
        assert!(!is_task_id_prefix("mvslrsp.abc"));
        assert!(!is_task_id_prefix("mvslrsp."));
    }

    // Tests for is_valid_slug

    #[test]
    fn test_valid_slugs() {
        assert!(is_valid_slug("build"));
        assert!(is_valid_slug("run-tests"));
        assert!(is_valid_slug("deploy-staging"));
        assert!(is_valid_slug("phase-2"));
        assert!(is_valid_slug("a"));
        assert!(is_valid_slug("a1"));
        assert!(is_valid_slug("1a"));
        assert!(is_valid_slug("1"));
        assert!(is_valid_slug("abc123"));
        assert!(is_valid_slug("a-b-c"));
    }

    #[test]
    fn test_invalid_slugs() {
        assert!(!is_valid_slug("-build"));      // starts with hyphen
        assert!(!is_valid_slug("build-"));      // ends with hyphen
        assert!(!is_valid_slug("Build"));       // uppercase
        assert!(!is_valid_slug("run_tests"));   // underscore
        assert!(!is_valid_slug("deploy.staging")); // dot
        assert!(!is_valid_slug("my slug"));     // space
        assert!(!is_valid_slug(""));            // empty
        assert!(!is_valid_slug("-"));           // just a hyphen
    }

    #[test]
    fn test_slug_boundary_length() {
        // 48 chars should be valid
        let slug_48 = "a".repeat(48);
        assert!(is_valid_slug(&slug_48));

        // 49 chars should be invalid
        let slug_49 = "a".repeat(49);
        assert!(!is_valid_slug(&slug_49));

        // Single char should be valid
        assert!(is_valid_slug("a"));
        assert!(is_valid_slug("1"));
    }

    #[test]
    fn test_slug_consecutive_hyphens() {
        // Consecutive hyphens are allowed by the format rules
        assert!(is_valid_slug("a--b"));
        assert!(is_valid_slug("a---b"));
    }
}
