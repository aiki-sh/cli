//! File location types for review issues.

use std::collections::HashMap;

use crate::error::{AikiError, Result};

/// A file location referenced by a review issue.
#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    pub path: String,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
}

impl Location {
    /// Parse a location string in the format `path`, `path:line`, or `path:line-end_line`.
    pub fn parse(s: &str) -> Result<Location> {
        let s = s.trim();
        if s.is_empty() {
            return Err(AikiError::InvalidArgument(
                "Location path must not be empty".into(),
            ));
        }

        if let Some(colon_pos) = s.rfind(':') {
            let path = &s[..colon_pos];
            let line_spec = &s[colon_pos + 1..];

            if !line_spec.is_empty() && line_spec.chars().all(|c| c.is_ascii_digit() || c == '-') {
                if path.is_empty() {
                    return Err(AikiError::InvalidArgument(
                        "Location path must not be empty".into(),
                    ));
                }
                if let Some(dash_pos) = line_spec.find('-') {
                    let start_str = &line_spec[..dash_pos];
                    let end_str = &line_spec[dash_pos + 1..];
                    let start: u32 = start_str.parse().map_err(|_| {
                        AikiError::InvalidArgument(format!("Invalid start line: {}", start_str))
                    })?;
                    let end: u32 = end_str.parse().map_err(|_| {
                        AikiError::InvalidArgument(format!("Invalid end line: {}", end_str))
                    })?;
                    if start == 0 || end == 0 {
                        return Err(AikiError::InvalidArgument(
                            "Line numbers must be positive".into(),
                        ));
                    }
                    if end < start {
                        return Err(AikiError::InvalidArgument(format!(
                            "End line ({}) must be >= start line ({})",
                            end, start
                        )));
                    }
                    return Ok(Location {
                        path: path.to_string(),
                        start_line: Some(start),
                        end_line: Some(end),
                    });
                } else {
                    let line: u32 = line_spec.parse().map_err(|_| {
                        AikiError::InvalidArgument(format!("Invalid line number: {}", line_spec))
                    })?;
                    if line == 0 {
                        return Err(AikiError::InvalidArgument(
                            "Line numbers must be positive".into(),
                        ));
                    }
                    return Ok(Location {
                        path: path.to_string(),
                        start_line: Some(line),
                        end_line: None,
                    });
                }
            }
        }

        Ok(Location {
            path: s.to_string(),
            start_line: None,
            end_line: None,
        })
    }
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.path)?;
        if let Some(start) = self.start_line {
            write!(f, ":{}", start)?;
            if let Some(end) = self.end_line {
                if end != start {
                    write!(f, "-{}", end)?;
                }
            }
        }
        Ok(())
    }
}

/// Parse locations from a task comment's data fields.
///
/// Handles both single-file format (`path`/`start_line`/`end_line` keys) and
/// multi-file format (comma-separated `locations` key).
pub fn parse_locations(data: &HashMap<String, String>) -> Vec<Location> {
    if let Some(locations_str) = data.get("locations") {
        return locations_str
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .filter_map(|s| Location::parse(s.trim()).ok())
            .collect();
    }

    if let Some(path) = data.get("path") {
        if path.is_empty() {
            return vec![];
        }
        let start_line = data.get("start_line").and_then(|s| s.parse::<u32>().ok());
        let end_line = data.get("end_line").and_then(|s| s.parse::<u32>().ok());
        return vec![Location {
            path: path.clone(),
            start_line,
            end_line,
        }];
    }

    vec![]
}

/// Format a `Vec<Location>` for display as a parenthesized suffix.
pub fn format_locations(locations: &[Location]) -> String {
    if locations.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = locations.iter().map(|l| l.to_string()).collect();
    format!("({})", parts.join(", "))
}
