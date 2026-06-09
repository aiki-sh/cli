//! Centralized caching infrastructure for performance-critical static values.
//!
//! This module provides process-level caching for values that are expensive to compute
//! but immutable for the lifetime of the process:
//!
//! - `DEBUG_ENABLED`: Whether debug mode is enabled (checked once)
//! - `AIKI_BINARY_PATH`: Path to the aiki binary (resolved once)
//! - `get_core_hook()`: The parsed core hook YAML (parsed once)
//!
//! ## Design Constraints
//!
//! **Do NOT cache environment variables globally.** Runtime mutations via
//! `std::env::set_var` (e.g., `AIKI_COMMIT_MSG_FILE`, `CLAUDE_SESSION_ID`)
//! would be invisible after first access. Use lazy per-key lookup instead.

use std::sync::{LazyLock, OnceLock};

use crate::flows::types::Hook;

/// Debug mode flag - checked once per process.
///
/// This caches the result of `std::env::var("AIKI_DEBUG").is_ok()` so we don't
/// repeatedly check the environment variable on every debug log call.
pub static DEBUG_ENABLED: LazyLock<bool> = LazyLock::new(|| std::env::var("AIKI_DEBUG").is_ok());

/// Aiki binary path - resolved once per process.
///
/// Resolution order:
/// 1. `AIKI_BIN` env var (explicit override, useful for tests)
/// 2. `which aiki` (PATH lookup — preferred because current_exe() can return
///    stale workspace paths like `/private/tmp/aiki/.../target/debug/aiki`)
/// 3. `std::env::current_exe()` (fallback when not on PATH)
///
/// # Panics
///
/// Panics if all resolution methods fail. This should never happen
/// in practice since we're running from this binary, but could theoretically occur
/// if the path contains invalid UTF-8 or there are OS-level issues.
pub static AIKI_BINARY_PATH: LazyLock<String> = LazyLock::new(|| {
    resolve_aiki_binary_path().expect(
        "Failed to resolve aiki binary path. This should never happen since \
         we're currently running from this binary. Please report this as a bug.",
    )
});

/// Cached core hook (parsed once per process).
static CORE_HOOK: OnceLock<Hook> = OnceLock::new();

/// Debug logging helper with lazy evaluation.
///
/// Only prints if `AIKI_DEBUG` environment variable is set.
/// Uses the cached `DEBUG_ENABLED` flag for efficiency.
///
/// **The closure is only called when debug is enabled**, so formatting
/// work is skipped entirely in production.
///
/// # Example
///
/// ```ignore
/// use aiki::cache::debug_log;
///
/// debug_log(|| "Processing event");
/// debug_log(|| format!("File: {}", file_path));
/// ```
#[inline]
pub fn debug_log<F, D>(f: F)
where
    F: FnOnce() -> D,
    D: std::fmt::Display,
{
    if *DEBUG_ENABLED {
        eprintln!("[aiki] {}", f());
    }
}

/// Get the cached core hook (parsed once per process).
///
/// The core hook is embedded in the binary and handles all event types.
/// This function parses the YAML only on first access, then returns
/// a reference to the cached result.
///
/// # Panics
///
/// Panics if the bundled core hook YAML fails to parse. This should never
/// happen in production since the YAML is embedded and known-good.
#[must_use]
pub fn get_core_hook() -> &'static Hook {
    CORE_HOOK.get_or_init(|| {
        crate::flows::load_core_hook_uncached().expect("Failed to parse bundled core hook")
    })
}

/// Resolve the path to the aiki binary.
///
/// Resolution order:
/// 1. `AIKI_BIN` env var — explicit override (tests, custom deployments)
/// 2. `which aiki` — PATH lookup (preferred; current_exe can return stale workspace paths)
/// 3. `std::env::current_exe()` — fallback when not on PATH
fn resolve_aiki_binary_path() -> Result<String, String> {
    // 1. Explicit override via environment variable
    if let Ok(path) = std::env::var("AIKI_BIN") {
        if !path.is_empty() {
            return Ok(path);
        }
    }

    // 2. PATH lookup — preferred because current_exe() can return stale
    //    workspace paths (e.g. /private/tmp/aiki/.../target/debug/aiki)
    //    that no longer exist after workspace cleanup.
    if let Ok(output) = std::process::Command::new("which").arg("aiki").output() {
        if output.status.success() {
            if let Ok(path) = String::from_utf8(output.stdout) {
                let path = path.trim().to_string();
                if !path.is_empty() {
                    return Ok(path);
                }
            }
        }
    }

    // 3. Fallback: current executable path
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(path_str) = current_exe.to_str() {
            return Ok(path_str.to_string());
        }
    }

    Err("Could not find 'aiki' binary in PATH.\n\
         Please install aiki or ensure it's in your PATH:\n\
         • cargo install --path .\n\
         • Or add the target directory to PATH"
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_debug_enabled_is_consistent() {
        // The value should be consistent across multiple accesses
        let first = *DEBUG_ENABLED;
        let second = *DEBUG_ENABLED;
        assert_eq!(first, second);
    }

    #[test]
    fn test_debug_log_does_not_panic() {
        // Should not panic regardless of DEBUG_ENABLED state
        debug_log(|| "test message");
        debug_log(|| format!("formatted: {}", 42));
    }

    #[test]
    fn test_aiki_binary_path_is_cached() {
        // Should return a valid path without panicking
        let path = &*AIKI_BINARY_PATH;
        assert!(!path.is_empty(), "Binary path should not be empty");

        // Verify it's cached (same reference on multiple accesses)
        let path2 = &*AIKI_BINARY_PATH;
        assert!(std::ptr::eq(path, path2));
    }

    #[test]
    fn test_get_core_hook_returns_valid_hook() {
        let hook = get_core_hook();
        assert_eq!(hook.name, "Aiki Core");
        assert_eq!(hook.version, "1");
    }

    #[test]
    fn test_get_core_hook_is_cached() {
        // Multiple calls should return the same reference
        let hook1 = get_core_hook();
        let hook2 = get_core_hook();
        assert!(std::ptr::eq(hook1, hook2));
    }
}
