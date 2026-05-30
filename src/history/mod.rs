//! Prompt history system for Aiki
//!
//! Provides conversation history recording with:
//! - Event-sourced storage on `aiki/conversations` branch
//! - Content truncation for large prompts/responses

pub mod recorder;
pub mod storage;
pub mod types;

pub use recorder::{record_autoreply, record_prompt, record_response, record_session_end, record_session_start};
#[allow(unused_imports)]
pub use storage::{
    ensure_conversations_branch, get_current_turn_info, get_current_turn_number,
    get_last_prompt_turn, get_latest_prompt_change_id, get_prompt_by_change_id,
    has_pending_autoreply, has_session_started_event, list_conversations, read_events,
    write_event,
};
#[allow(unused_imports)]
pub use types::{AgentType, ConversationEvent, ConversationSummary, Session, TurnSource, CONVERSATIONS_BRANCH, METADATA_END, METADATA_START};
