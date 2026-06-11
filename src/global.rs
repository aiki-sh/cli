//! Global aiki directory helpers
//!
//! Provides paths for global aiki state that lives outside of individual repositories:
//! - Session files: `~/.aiki/sessions/`
//! - Global JJ repo: `~/.aiki/.jj/` (for conversation history)
//!
//! The global directory defaults to `~/.aiki/` but can be overridden with `AIKI_HOME`.

use std::path::PathBuf;

/// Process-wide mutex for tests that modify the `AIKI_HOME` env var.
///
/// Every test module that touches `AIKI_HOME` **must** lock this mutex
/// to avoid races with tests in other modules (Rust runs tests in the
/// same process in parallel by default).
#[cfg(test)]
pub static AIKI_HOME_TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Environment variable to override the global aiki directory
pub const AIKI_HOME_ENV: &str = "AIKI_HOME";

/// Get the global aiki directory.
///
/// Resolution order:
/// 1. `AIKI_HOME` environment variable (if set to a non-empty value;
///    an empty string is treated as unset)
/// 2. `~/.aiki/` (default)
///
/// # Panics
///
/// Panics if the home directory cannot be determined when falling back
/// to the default.
#[must_use]
pub fn global_aiki_dir() -> PathBuf {
    if let Some(aiki_home) = std::env::var(AIKI_HOME_ENV)
        .ok()
        .filter(|s| !s.is_empty())
    {
        return PathBuf::from(aiki_home);
    }

    // Default: ~/.aiki/
    dirs::home_dir()
        .expect("Could not determine home directory")
        .join(".aiki")
}

/// Get the global sessions directory: `$AIKI_HOME/sessions/`
#[must_use]
pub fn global_sessions_dir() -> PathBuf {
    global_aiki_dir().join("sessions")
}

/// Get the global JJ repository directory: `$AIKI_HOME/.jj/`
///
/// This JJ repository stores conversation history on the `aiki/conversations` branch.
#[must_use]
pub fn global_jj_dir() -> PathBuf {
    global_aiki_dir().join(".jj")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    /// Helper to run tests with a temporary AIKI_HOME value
    /// Serializes access to prevent parallel test interference
    fn with_aiki_home<F, R>(value: Option<&str>, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        // Handle potentially poisoned mutex (from panic tests)
        let _lock = super::AIKI_HOME_TEST_MUTEX
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        // Save original value
        let original = env::var(AIKI_HOME_ENV).ok();

        // Set or unset the env var
        match value {
            Some(v) => env::set_var(AIKI_HOME_ENV, v),
            None => env::remove_var(AIKI_HOME_ENV),
        }

        let result = f();

        // Restore original value
        match original {
            Some(v) => env::set_var(AIKI_HOME_ENV, v),
            None => env::remove_var(AIKI_HOME_ENV),
        }

        result
    }

    #[test]
    fn test_global_aiki_dir_default() {
        with_aiki_home(None, || {
            let dir = global_aiki_dir();
            assert!(dir.ends_with(".aiki"), "Default should end with .aiki");

            // Should be in home directory
            let home = dirs::home_dir().expect("home dir");
            assert_eq!(dir, home.join(".aiki"));
        });
    }

    #[test]
    fn test_global_aiki_dir_with_env_var() {
        with_aiki_home(Some("/custom/aiki/path"), || {
            let dir = global_aiki_dir();
            assert_eq!(dir, PathBuf::from("/custom/aiki/path"));
        });
    }

    #[test]
    fn test_global_aiki_dir_empty_env_var_treated_as_unset() {
        with_aiki_home(Some(""), || {
            let dir = global_aiki_dir();
            let home = dirs::home_dir().expect("home dir");
            assert_eq!(
                dir,
                home.join(".aiki"),
                "Empty AIKI_HOME should fall back to the default"
            );
        });
    }

    #[test]
    fn test_global_sessions_dir() {
        with_aiki_home(Some("/test/aiki"), || {
            let dir = global_sessions_dir();
            assert_eq!(dir, PathBuf::from("/test/aiki/sessions"));
        });
    }

    #[test]
    fn test_global_jj_dir() {
        with_aiki_home(Some("/test/aiki"), || {
            let dir = global_jj_dir();
            assert_eq!(dir, PathBuf::from("/test/aiki/.jj"));
        });
    }
}
