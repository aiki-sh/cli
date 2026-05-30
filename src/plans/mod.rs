//! Plan management — PlanGraph and metadata parsing
//!
//! Provides first-class plan management with:
//! - O(1) reverse index from plan files to implementing tasks
//! - Plan metadata parsing from markdown files
//! - Status inference (Draft → Planned → Implementing → Implemented)

pub mod graph;
pub mod parser;

pub use graph::{normalize_plan_path, Plan, PlanGraph, PlanStatus};
pub use parser::{parse_plan_metadata, PlanMetadata};
