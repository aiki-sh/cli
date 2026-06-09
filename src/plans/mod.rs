//! Plan management — PlanGraph and metadata parsing
//!
//! Provides first-class plan management with:
//! - O(1) reverse index from plan files to implementing tasks
//! - Plan metadata parsing from markdown files
//! - Status inference (Draft → Planned → Implementing → Implemented)

pub mod graph;
pub mod parser;

pub use graph::PlanGraph;
pub use parser::parse_plan_metadata;
