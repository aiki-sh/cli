//! Built-in functions for the aiki/core hook namespace
//!
//! This module contains native Rust implementations of functions that can be called
//! from flow definitions using the function call syntax.

mod functions;

#[allow(unused_imports)]
pub use functions::{
    // Commit integration
    generate_coauthors,
    // Change event functions (unified mutations: write, delete, move)
    build_delete_metadata, build_human_metadata_change_post, build_human_metadata_change_pre,
    build_move_metadata, build_write_metadata, classify_edits_change, prepare_separation_change,
    restore_original_files_change, write_ai_files_change,
    // Task system functions
    task_in_progress, task_list_size, task_list_size_for_agent,
    // Workspace isolation functions
    detect_workspace_conflicts, workspace_absorb_all, workspace_create_if_concurrent,
};
