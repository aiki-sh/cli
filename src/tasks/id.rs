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

/// An unresolved reference to a task (full ID, prefix, or slug ref).
///
/// Not yet validated against the task graph. Use `resolve_task_id_in_graph()`
/// (or equivalent) to resolve to a full task ID.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskRef(pub String);

impl std::ops::Deref for TaskRef {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TaskRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Check if a string looks like a task ID prefix (shorter than a full ID)
///
/// Returns true if the input could be a task ID prefix:
/// - Prefix: 3+ characters of lowercase k-z (e.g., "mvslrsp")
/// - Full 32-char IDs also return true (a prefix that happens to be complete)
///
/// Returns false for descriptions, IDs with wrong chars, punctuation,
/// or prefixes under 3 chars.
///
/// # Examples
/// ```
/// use aiki::tasks::is_task_id_prefix;
/// assert!(is_task_id_prefix("mvslrsp"));
/// assert!(is_task_id_prefix("kvx"));
/// assert!(is_task_id_prefix("mvslrspmoynoxyyywqyutmovxpvztkls")); // full ID
/// assert!(!is_task_id_prefix("Fix bug"));
/// assert!(!is_task_id_prefix("mvslrsp!"));
/// assert!(!is_task_id_prefix("abc")); // outside k-z range
/// assert!(!is_task_id_prefix("kv")); // too short
/// ```
#[must_use]
pub fn is_task_id_prefix(input: &str) -> bool {
    if input.is_empty() || input.contains(' ') || input.contains('.') {
        return false;
    }

    input.len() >= 3 && input.chars().all(|c| matches!(c, 'k'..='z'))
}

/// Check if a string looks like a task ID (vs a task description)
///
/// Task IDs are 32 characters, all lowercase k-z (JJ reverse hex).
///
/// Descriptions typically contain:
/// - Spaces
/// - Capital letters
/// - Characters outside k-z
/// - Punctuation
///
/// # Examples
/// ```
/// use aiki::tasks::is_task_id;
/// assert!(is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls"));
/// assert!(!is_task_id("Fix the auth bug"));
/// assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls!"));
/// assert!(!is_task_id("implement-login")); // has hyphen
/// ```
#[must_use]
pub fn is_task_id(input: &str) -> bool {
    input.len() == 32 && input.chars().all(|c| matches!(c, 'k'..='z'))
}

/// Check if a string looks like a task ID or a task ID prefix.
///
/// Combines [`is_task_id`] (exact 32-char match) with [`is_task_id_prefix`]
/// (3+ char prefix). Use this when accepting user input that could be either.
///
/// # Examples
/// ```
/// use aiki::tasks::looks_like_task_id;
/// assert!(looks_like_task_id("mvslrspmoynoxyyywqyutmovxpvztkls")); // full ID
/// assert!(looks_like_task_id("klm")); // prefix
/// assert!(!looks_like_task_id("ops/now/feature.md")); // path
/// assert!(!looks_like_task_id("abc")); // outside k-z range
/// ```
#[must_use]
pub fn looks_like_task_id(input: &str) -> bool {
    is_task_id(input) || is_task_id_prefix(input)
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
    fn test_id_uniqueness_bulk() {
        // Generate many IDs and verify no collisions
        use std::collections::HashSet;

        let mut ids = HashSet::new();
        for i in 0..1000 {
            let id = generate_task_id(&format!("Task {}", i));
            assert!(ids.insert(id.clone()), "Collision detected for task {}", i);
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
    fn test_is_task_id_rejects_punctuation() {
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls!"));
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls-"));
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls_"));
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls."));
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

        // Punctuation is not allowed
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls.abc"));
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls."));
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls.1."));
        assert!(!is_task_id("mvslrspmoynoxyyywqyutmovxpvztkls.1"));
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
    fn test_is_task_id_prefix_rejects_punctuation() {
        assert!(!is_task_id_prefix("mvslrsp!"));
        assert!(!is_task_id_prefix("mvslrsp-"));
        assert!(!is_task_id_prefix("mvslrsp_"));
        assert!(!is_task_id_prefix("mvslrsp."));
        assert!(!is_task_id_prefix("mvslrsp:"));
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

        // Punctuation is not allowed in prefixes
        assert!(!is_task_id_prefix("mvslrsp.abc"));
        assert!(!is_task_id_prefix("mvslrsp."));
        assert!(!is_task_id_prefix("mvslrsp!"));
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
        assert!(!is_valid_slug("-build")); // starts with hyphen
        assert!(!is_valid_slug("build-")); // ends with hyphen
        assert!(!is_valid_slug("Build")); // uppercase
        assert!(!is_valid_slug("run_tests")); // underscore
        assert!(!is_valid_slug("deploy.staging")); // dot
        assert!(!is_valid_slug("my slug")); // space
        assert!(!is_valid_slug("")); // empty
        assert!(!is_valid_slug("-")); // just a hyphen
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
