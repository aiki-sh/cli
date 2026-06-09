//! Centralized output utilities.
//!
//! # Output Conventions
//!
//! | Mode           | stdout                              | stderr              |
//! |----------------|-------------------------------------|----------------------|
//! | Default        | Human-readable output               | Errors/warnings only |
//! | `--output id`  | Bare task IDs, one per line          | Errors/warnings only |

/// Emit formatted output to stdout.
///
/// # Example
/// ```ignore
/// emit(|| {
///     let content = format_command_output(&output);
///     MdBuilder::new().build(&content)
/// });
/// ```
pub fn emit(formatter: impl FnOnce() -> String) {
    println!("{}", formatter());
}
