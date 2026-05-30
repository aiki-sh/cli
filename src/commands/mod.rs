// Command modules for Aiki CLI
//
// This module organizes the CLI command implementations into separate files
// for better maintainability and organization.

use clap::ValueEnum;

/// Output format for commands that support `--output` / `-o`.
///
/// Currently only `Id` is supported. Future formats (e.g., `Json`) can be
/// added here and become available on all commands that use this enum.
#[derive(Clone, Debug, ValueEnum)]
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
pub mod explore;
pub mod session;
pub mod doctor;
pub mod event;
pub mod fix;
pub mod hooks;
pub mod output;
pub mod decompose;
pub mod epic;
pub mod plugin;
pub mod init;
pub mod otel_receive;
pub mod review;
pub mod plan;

pub mod task;
pub mod verify;
pub mod wait;
pub mod zed_detection;
