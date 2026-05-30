//! Common imports shared by event handlers
pub use super::result::{Decision, HookResult};
pub use crate::cache::debug_log;
pub use crate::error::{AikiError, Result};
pub use crate::flows::composer::{EventType, HookComposer};
pub use crate::flows::loader::HookLoader;
pub use crate::flows::{AikiState, HookEngine, HookOutcome};
pub use crate::session::AikiSession;
pub use chrono::{DateTime, Utc};
pub use serde::{Deserialize, Serialize};
pub use std::path::PathBuf;

/// Execute hooks for an event.
///
/// This function:
/// 1. Always executes bundled aiki/core first (immutable, cannot be overridden)
/// 2. Then executes user's "{project}/.aiki/hooks.yml" if it exists (optional)
///
/// Note: aiki/core always runs and cannot be overridden.
/// Users should create .aiki/hooks.yml to add custom behavior.
///
/// # Arguments
///
/// * `event_type` - The event type to handle
/// * `state` - Mutable execution state
/// * `bundled_statements` - The statements from the bundled core hook for this event
///
/// # Returns
///
/// The [`HookOutcome`] from hook execution.
pub fn execute_hook(
    event_type: EventType,
    state: &mut AikiState,
    bundled_statements: &[crate::flows::types::HookStatement],
) -> Result<HookOutcome> {
    // Step 1: Always execute bundled aiki/core first
    debug_log(|| "Executing bundled aiki/core");
    state.hook_name = Some("aiki/core".to_string());
    let core_result = HookEngine::execute_statements(bundled_statements, state)?;

    // If core hook blocked or stopped, don't continue to user's hooks.yml
    match core_result {
        HookOutcome::FailedBlock | HookOutcome::FailedStop => {
            return Ok(core_result);
        }
        _ => {}
    }

    // Step 2: Try to execute user's .aiki/hooks.yml
    match HookLoader::with_start_dir(state.cwd()) {
        Ok(mut loader) => {
            let hookfile_path = loader.project_root().join(".aiki/hooks.yml");
            let mut composer = HookComposer::new(&mut loader);

            // No user hooks.yml - just return core result
            if !hookfile_path.exists() {
                return Ok(core_result);
            }

            // Execute user's hooks.yml
            debug_log(|| "Executing user's .aiki/hooks.yml");
            let user_result =
                composer.compose_hook_from_path(&hookfile_path, event_type, state)?;

            // Combine results: if either failed, return the failure
            match (core_result, user_result) {
                (HookOutcome::Success, user) => Ok(user),
                (core, HookOutcome::Success) => Ok(core),
                (_, user_fail) => Ok(user_fail), // User failure takes precedence
            }
        }
        Err(AikiError::NotInAikiProject { .. }) => {
            // Not in an Aiki project - just return core result
            Ok(core_result)
        }
        Err(e) => Err(e),
    }
}
