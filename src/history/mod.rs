//! Prompt history system for Aiki
//!
//! Provides conversation history recording with:
//! - Event-sourced storage on `aiki/conversations` branch
//! - Content truncation for large prompts/responses

pub mod recorder;
pub mod storage;
pub mod types;

pub use recorder::{
    record_autoreply, record_model_changed, record_prompt, record_response, record_session_end,
    record_session_start,
};
pub use storage::{
    get_current_turn_info, get_latest_prompt_change_id,
    get_prompt_by_change_id, has_pending_autoreply, last_session_model,
};
pub use types::TurnSource;
