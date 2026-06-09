// Command modules for Aiki CLI
//
// This module organizes the CLI command implementations into separate files
// for better maintainability and organization.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Output format for commands that support `--output` / `-o`.
///
/// Currently only `Id` is supported. Future formats (e.g., `Json`) can be
/// added here and become available on all commands that use this enum.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ValueEnum)]
pub enum OutputFormat {
    /// Bare task ID (full 32-char), one per line
    Id,
}

pub mod acp;
pub mod agents_template;
pub mod authors;
pub mod benchmark;
pub mod blame;
pub mod build;
pub mod config;
pub mod decompose;
pub mod doctor;
pub mod epic;
pub mod event;
pub mod explore;
pub mod fix;
pub mod hooks;
pub mod init;
pub mod input;
pub mod loop_cmd;
pub mod otel_receive;
pub mod output;
pub mod plan;
pub mod plugin;
pub mod resolve;
pub mod review;
pub mod run;
pub mod session;

pub mod task;
pub mod tldr;
pub mod zed_detection;
