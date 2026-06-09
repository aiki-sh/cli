//! Context assembly system for building prompts, autoreplies, and commit messages.
//!
//! This module provides a unified syntax for prepending and appending content to messages
//! across different event types (PrePrompt, SessionEnd, PrepareCommitMessage).
//!
//! # Core Types
//!
//! - [`ContextChunk`]: Represents content to prepend and/or append
//! - [`ContextAssembler`]: Stateful builder that accumulates chunks and produces final output
//! - [`TextLines`]: YAML value that can be a single string or array of strings
//!
//! # Usage Example
//!
//! ```rust
//! use aiki::flows::context::{ContextChunk, ContextAssembler, TextLines};
//!
//! let mut assembler = ContextAssembler::new(Some("original content".to_string()), "\n");
//!
//! // Add chunks from different flows
//! let chunk1 = ContextChunk {
//!     prepend: Some(TextLines::Single("First line".to_string())),
//!     append: Some(TextLines::Single("Last line".to_string())),
//! };
//! assembler.add_chunk(chunk1);
//!
//! // Build final message
//! let result = assembler.build_with_original();
//! // Result: "First line\noriginal content\nLast line"
//! ```

use serde::{Deserialize, Serialize};

/// One or more lines of text for message assembly.
///
/// This type handles the YAML flexibility where users can write:
/// ```yaml
/// prepend: "single line"
/// ```
/// or:
/// ```yaml
/// prepend:
///   - "line 1"
///   - "line 2"
/// ```
///
/// # Normalization
///
/// Trailing newlines are automatically stripped during deserialization to ensure that
/// YAML block scalars (`|`) and inline strings produce identical results.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum TextLines {
    /// Single line of text (normalized - trailing newlines stripped)
    Single(String),
    /// Multiple lines of text (each normalized - trailing newlines stripped)
    Multiple(Vec<String>),
}

impl<'de> serde::Deserialize<'de> for TextLines {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, Visitor};
        use std::fmt;

        struct TextLinesVisitor;

        impl<'de> Visitor<'de> for TextLinesVisitor {
            type Value = TextLines;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a string or array of strings")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                // Normalize: strip trailing newlines from YAML block scalars
                Ok(TextLines::Single(value.trim_end_matches('\n').to_string()))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                // Normalize: strip trailing newlines from YAML block scalars
                Ok(TextLines::Single(value.trim_end_matches('\n').to_string()))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                let mut vec = Vec::new();
                while let Some(value) = seq.next_element::<String>()? {
                    // Normalize each item: strip trailing newlines
                    vec.push(value.trim_end_matches('\n').to_string());
                }
                Ok(TextLines::Multiple(vec))
            }
        }

        deserializer.deserialize_any(TextLinesVisitor)
    }
}

impl TextLines {
    /// Convert to a vector of strings, consuming self.
    ///
    /// The data is already normalized during deserialization, so trailing newlines
    /// have been stripped. This ensures `prepend: |` and `prepend: ["line"]` behave identically.
    ///
    /// # Returns
    ///
    /// - `Single(s)` → `vec![s]`
    /// - `Multiple(v)` → `v`
    #[must_use]
    pub fn to_vec(self) -> Vec<String> {
        match self {
            Self::Single(s) => vec![s],
            Self::Multiple(v) => v,
        }
    }

    /// Check if the content is empty (empty string or empty array).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Single(s) => s.is_empty(),
            Self::Multiple(v) => v.is_empty() || v.iter().all(String::is_empty),
        }
    }
}

/// A chunk of content to prepend and/or append to a message.
///
/// This is the fundamental building block for context assembly. Each flow can
/// contribute a `ContextChunk` that specifies what to add before and/or after
/// the existing content.
///
/// # YAML Syntax
///
/// ## Prepend only
/// ```yaml
/// prepend: "Add this before"
/// ```
///
/// ## Append only
/// ```yaml
/// append: "Add this after"
/// ```
///
/// ## Both
/// ```yaml
/// prepend: "Before"
/// append: "After"
/// ```
///
/// ## Arrays
/// ```yaml
/// prepend:
///   - "Line 1"
///   - "Line 2"
/// append:
///   - "Line 3"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextChunk {
    /// Content to add before the existing message
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepend: Option<TextLines>,

    /// Content to add after the existing message
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append: Option<TextLines>,
}

impl ContextChunk {
    /// Create a new empty `ContextChunk`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            prepend: None,
            append: None,
        }
    }

    /// Get prepend items as a vector of strings.
    ///
    /// Returns an empty vector if prepend is None.
    #[must_use]
    pub fn prepend_items(&self) -> Vec<String> {
        self.prepend.clone().map(|p| p.to_vec()).unwrap_or_default()
    }

    /// Get append items as a vector of strings.
    ///
    /// Returns an empty vector if append is None.
    #[must_use]
    pub fn append_items(&self) -> Vec<String> {
        self.append.clone().map(|a| a.to_vec()).unwrap_or_default()
    }

    /// Validate the chunk structure.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Both prepend and append are None (empty chunk)
    /// - prepend or append is empty
    pub fn validate(&self) -> crate::error::Result<()> {
        if self.prepend.is_none() && self.append.is_none() {
            return Err(crate::error::AikiError::InvalidContextChunk(
                "ContextChunk must have at least prepend or append".to_string(),
            ));
        }

        if let Some(ref p) = self.prepend {
            if p.is_empty() {
                return Err(crate::error::AikiError::InvalidContextChunk(
                    "prepend cannot be empty".to_string(),
                ));
            }
        }

        if let Some(ref a) = self.append {
            if a.is_empty() {
                return Err(crate::error::AikiError::InvalidContextChunk(
                    "append cannot be empty".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Resolve variables in this chunk using the provided resolver function
    ///
    /// # Arguments
    ///
    /// * `resolver` - A function that resolves variables in strings
    ///
    /// # Returns
    ///
    /// A new ContextChunk with all variables resolved
    pub fn resolve_variables<F>(self, mut resolver: F) -> crate::error::Result<Self>
    where
        F: FnMut(&str) -> Result<String, crate::error::AikiError>,
    {
        let resolved_prepend: Option<TextLines> = self
            .prepend
            .map(|text_lines| -> crate::error::Result<TextLines> {
                match text_lines {
                    TextLines::Single(s) => Ok(TextLines::Single(resolver(&s)?)),
                    TextLines::Multiple(lines) => {
                        let resolved: Result<Vec<_>, crate::error::AikiError> =
                            lines.iter().map(|line| resolver(line)).collect();
                        Ok(TextLines::Multiple(resolved?))
                    }
                }
            })
            .transpose()?;

        let resolved_append: Option<TextLines> = self
            .append
            .map(|text_lines| -> crate::error::Result<TextLines> {
                match text_lines {
                    TextLines::Single(s) => Ok(TextLines::Single(resolver(&s)?)),
                    TextLines::Multiple(lines) => {
                        let resolved: Result<Vec<_>, crate::error::AikiError> =
                            lines.iter().map(|line| resolver(line)).collect();
                        Ok(TextLines::Multiple(resolved?))
                    }
                }
            })
            .transpose()?;

        Ok(Self {
            prepend: resolved_prepend,
            append: resolved_append,
        })
    }
}

impl Default for ContextChunk {
    fn default() -> Self {
        Self::new()
    }
}

/// A stateful builder that accumulates context chunks and produces a final assembled message.
///
/// # Usage Pattern
///
/// 1. Create assembler with optional original content
/// 2. Add chunks from different flows
/// 3. Call `build_with_original()` to produce final message
///
/// # Example
///
/// ```rust
/// use aiki::flows::context::{ContextChunk, ContextAssembler, TextLines};
///
/// let mut assembler = ContextAssembler::new(Some("Middle".to_string()), "\n");
///
/// let chunk1 = ContextChunk {
///     prepend: Some(TextLines::Single("Top".to_string())),
///     append: None,
/// };
/// assembler.add_chunk(chunk1);
///
/// let chunk2 = ContextChunk {
///     prepend: None,
///     append: Some(TextLines::Single("Bottom".to_string())),
/// };
/// assembler.add_chunk(chunk2);
///
/// assert_eq!(assembler.build_with_original(), "Top\nMiddle\nBottom");
/// ```
#[derive(Debug, Clone)]
pub struct ContextAssembler {
    /// Accumulated chunks in order they were added
    chunks: Vec<ContextChunk>,
    /// Original message content (e.g., user's prompt, existing commit message)
    original: Option<String>,
    /// Separator to use when joining sections
    separator: String,
}

impl ContextAssembler {
    /// Create a new `ContextAssembler`.
    ///
    /// # Arguments
    ///
    /// - `original`: Optional original content (e.g., user's prompt)
    /// - `separator`: String to use when joining sections (typically "\n" or "\n\n")
    #[must_use]
    pub fn new(original: Option<String>, separator: impl Into<String>) -> Self {
        Self {
            chunks: Vec::new(),
            original,
            separator: separator.into(),
        }
    }

    /// Add a chunk to the assembler.
    ///
    /// Chunks are processed in the order they are added.
    pub fn add_chunk(&mut self, chunk: ContextChunk) {
        self.chunks.push(chunk);
    }

    /// Build the final message by combining all chunks with the original content.
    ///
    /// # Assembly Order
    ///
    /// 1. All prepend items from all chunks (joined with "\n")
    /// 2. Original content (if any)
    /// 3. All append items from all chunks (joined with "\n")
    ///
    /// Sections are then joined with the configured separator (typically "\n\n").
    ///
    /// # Returns
    ///
    /// The assembled message as a single string.
    #[must_use]
    #[allow(dead_code)]
    pub fn build_with_original(&self) -> String {
        self.build_inner(true)
    }

    /// Build the final message from accumulated chunks without including original content.
    ///
    /// This is useful for hook protocols that already carry the user's original
    /// prompt separately and only need Aiki-injected context.
    #[must_use]
    pub fn build_without_original(&self) -> String {
        self.build_inner(false)
    }

    fn build_inner(&self, include_original: bool) -> String {
        let mut prepends = Vec::new();
        let mut appends = Vec::new();

        // Collect all prepends and appends in order
        for chunk in &self.chunks {
            prepends.extend(chunk.prepend_items());
            appends.extend(chunk.append_items());
        }

        // Build final message
        let mut parts = Vec::new();

        if !prepends.is_empty() {
            parts.push(prepends.join("\n"));
        }

        if include_original {
            if let Some(ref orig) = self.original {
                if !orig.is_empty() {
                    parts.push(orig.clone());
                }
            }
        }

        if !appends.is_empty() {
            parts.push(appends.join("\n"));
        }

        parts.join(&self.separator)
    }

    /// Check if the assembler has any chunks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_or_array_single() {
        let single = TextLines::Single("test".to_string());
        assert!(!single.is_empty());
        assert_eq!(single.to_vec(), vec!["test".to_string()]);
    }

    #[test]
    fn test_string_or_array_multiple() {
        let multiple = TextLines::Multiple(vec!["a".to_string(), "b".to_string()]);
        assert!(!multiple.is_empty());
        assert_eq!(multiple.to_vec(), vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_string_or_array_empty() {
        let empty_single = TextLines::Single("".to_string());
        assert!(empty_single.is_empty());

        let empty_multiple = TextLines::Multiple(vec![]);
        assert!(empty_multiple.is_empty());
    }

    #[test]
    fn test_prepend_only() {
        let chunk = ContextChunk {
            prepend: Some(TextLines::Single("prepended".to_string())),
            append: None,
        };

        let mut assembler = ContextAssembler::new(Some("original".to_string()), "\n");
        assembler.add_chunk(chunk);

        assert_eq!(assembler.build_with_original(), "prepended\noriginal");
    }

    #[test]
    fn test_append_only() {
        let chunk = ContextChunk {
            prepend: None,
            append: Some(TextLines::Single("appended".to_string())),
        };

        let mut assembler = ContextAssembler::new(Some("original".to_string()), "\n");
        assembler.add_chunk(chunk);

        assert_eq!(assembler.build_with_original(), "original\nappended");
    }

    #[test]
    fn test_both_prepend_and_append() {
        let chunk = ContextChunk {
            prepend: Some(TextLines::Single("before".to_string())),
            append: Some(TextLines::Single("after".to_string())),
        };

        let mut assembler = ContextAssembler::new(Some("middle".to_string()), "\n");
        assembler.add_chunk(chunk);

        assert_eq!(assembler.build_with_original(), "before\nmiddle\nafter");
    }

    #[test]
    fn test_with_arrays() {
        let chunk = ContextChunk {
            prepend: Some(TextLines::Multiple(vec![
                "line1".to_string(),
                "line2".to_string(),
            ])),
            append: Some(TextLines::Single("line3".to_string())),
        };

        let mut assembler = ContextAssembler::new(Some("original".to_string()), "\n");
        assembler.add_chunk(chunk);

        assert_eq!(assembler.build_with_original(), "line1\nline2\noriginal\nline3");
    }

    #[test]
    fn test_yaml_block_scalar_trailing_newline_stripped() {
        // YAML block scalars (|) include a trailing newline, but our system
        // should handle this gracefully
        let yaml_str = r#"
prepend: |
  Line 1
  Line 2
"#;

        let chunk: ContextChunk = serde_yaml::from_str(yaml_str).unwrap();
        let mut assembler = ContextAssembler::new(Some("original".to_string()), "\n");
        assembler.add_chunk(chunk);

        let result = assembler.build_with_original();
        // The block scalar preserves the trailing newline in the string
        assert!(result.starts_with("Line 1\nLine 2"));
    }

    #[test]
    fn test_yaml_array_form() {
        let yaml_str = r#"
prepend:
  - "Line 1"
  - "Line 2"
append:
  - "Line 3"
"#;

        let chunk: ContextChunk = serde_yaml::from_str(yaml_str).unwrap();
        let mut assembler = ContextAssembler::new(Some("original".to_string()), "\n");
        assembler.add_chunk(chunk);

        assert_eq!(assembler.build_with_original(), "Line 1\nLine 2\noriginal\nLine 3");
    }

    #[test]
    fn test_yaml_forms_produce_same_output() {
        // Block scalar form
        let yaml_block = r#"
prepend: "Header"
append: "Footer"
"#;

        // Array form
        let yaml_array = r#"
prepend:
  - "Header"
append:
  - "Footer"
"#;

        let chunk1: ContextChunk = serde_yaml::from_str(yaml_block).unwrap();
        let chunk2: ContextChunk = serde_yaml::from_str(yaml_array).unwrap();

        let mut assembler1 = ContextAssembler::new(Some("Body".to_string()), "\n");
        assembler1.add_chunk(chunk1);

        let mut assembler2 = ContextAssembler::new(Some("Body".to_string()), "\n");
        assembler2.add_chunk(chunk2);

        assert_eq!(
            assembler1.build_with_original(),
            assembler2.build_with_original()
        );
    }

    #[test]
    fn test_validate_empty_chunk() {
        let chunk = ContextChunk {
            prepend: None,
            append: None,
        };

        assert!(chunk.validate().is_err());
    }

    #[test]
    fn test_validate_valid_chunk() {
        let chunk = ContextChunk {
            prepend: Some(TextLines::Single("test".to_string())),
            append: None,
        };

        assert!(chunk.validate().is_ok());
    }

    #[test]
    fn test_prepend_items() {
        let chunk = ContextChunk {
            prepend: Some(TextLines::Multiple(vec!["a".to_string(), "b".to_string()])),
            append: None,
        };

        assert_eq!(
            chunk.prepend_items(),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn test_append_items() {
        let chunk = ContextChunk {
            prepend: None,
            append: Some(TextLines::Multiple(vec!["x".to_string(), "y".to_string()])),
        };

        assert_eq!(chunk.append_items(), vec!["x".to_string(), "y".to_string()]);
    }
}

#[cfg(test)]
mod assembler_tests {
    use super::*;

    #[test]
    fn test_build_with_single_chunk() {
        let mut assembler = ContextAssembler::new(Some("original".to_string()), "\n");

        let chunk = ContextChunk {
            prepend: Some(TextLines::Single("before".to_string())),
            append: Some(TextLines::Single("after".to_string())),
        };
        assembler.add_chunk(chunk);

        assert_eq!(assembler.build_with_original(), "before\noriginal\nafter");
    }

    #[test]
    fn test_build_with_multiple_chunks() {
        let mut assembler = ContextAssembler::new(Some("original".to_string()), "\n");

        let chunk1 = ContextChunk {
            prepend: Some(TextLines::Single("first".to_string())),
            append: None,
        };
        assembler.add_chunk(chunk1);

        let chunk2 = ContextChunk {
            prepend: Some(TextLines::Single("second".to_string())),
            append: Some(TextLines::Single("end".to_string())),
        };
        assembler.add_chunk(chunk2);

        // Order: all prepends, original, all appends
        assert_eq!(assembler.build_with_original(), "first\nsecond\noriginal\nend");
    }

    #[test]
    fn test_build_without_original_when_original_exists() {
        let mut assembler = ContextAssembler::new(Some("original".to_string()), "\n\n");

        assembler.add_chunk(ContextChunk {
            prepend: Some(TextLines::Single("before".to_string())),
            append: Some(TextLines::Single("after".to_string())),
        });

        assert_eq!(assembler.build_without_original(), "before\n\nafter");
    }

    #[test]
    fn test_build_without_original() {
        let mut assembler = ContextAssembler::new(None, "\n");

        let chunk = ContextChunk {
            prepend: Some(TextLines::Single("start".to_string())),
            append: Some(TextLines::Single("end".to_string())),
        };
        assembler.add_chunk(chunk);

        assert_eq!(assembler.build_with_original(), "start\nend");
    }

    #[test]
    fn test_build_with_custom_separator() {
        let mut assembler = ContextAssembler::new(Some("middle".to_string()), " | ");

        let chunk = ContextChunk {
            prepend: Some(TextLines::Single("A".to_string())),
            append: Some(TextLines::Single("B".to_string())),
        };
        assembler.add_chunk(chunk);

        assert_eq!(assembler.build_with_original(), "A | middle | B");
    }

    #[test]
    fn test_build_empty_chunks() {
        let assembler = ContextAssembler::new(Some("only original".to_string()), "\n");

        assert_eq!(assembler.build_with_original(), "only original");
    }

    #[test]
    fn test_build_empty_original() {
        let mut assembler = ContextAssembler::new(Some("".to_string()), "\n");

        let chunk = ContextChunk {
            prepend: Some(TextLines::Single("before".to_string())),
            append: Some(TextLines::Single("after".to_string())),
        };
        assembler.add_chunk(chunk);

        // Empty original should be skipped
        assert_eq!(assembler.build_with_original(), "before\nafter");
    }

    #[test]
    fn test_is_empty() {
        let mut assembler = ContextAssembler::new(None, "\n");
        assert!(assembler.is_empty());

        assembler.add_chunk(ContextChunk::new());
        assert!(!assembler.is_empty());
    }

    #[test]
    fn test_build_with_double_newline_separator() {
        // PrePrompt and SessionEnd use double-newline separator
        let mut assembler = ContextAssembler::new(Some("middle".to_string()), "\n\n");

        let chunk = ContextChunk {
            prepend: Some(TextLines::Single("before".to_string())),
            append: Some(TextLines::Single("after".to_string())),
        };
        assembler.add_chunk(chunk);

        // Should have double newlines between sections
        assert_eq!(assembler.build_with_original(), "before\n\nmiddle\n\nafter");
    }

    #[test]
    fn test_preprompt_style_message_assembly() {
        // Simulate PrePrompt event usage with double-newline separator
        let mut assembler =
            ContextAssembler::new(Some("User's original prompt".to_string()), "\n\n");

        // Flow A adds context
        assembler.add_chunk(ContextChunk {
            prepend: Some(TextLines::Single("Read ARCHITECTURE.md first".to_string())),
            append: None,
        });

        // Flow B adds constraints
        assembler.add_chunk(ContextChunk {
            prepend: Some(TextLines::Single("Follow security guidelines".to_string())),
            append: None,
        });

        // Flow C adds footer
        assembler.add_chunk(ContextChunk {
            prepend: None,
            append: Some(TextLines::Single("Confirm you understand".to_string())),
        });

        let result = assembler.build_with_original();

        // Verify structure: prepends (joined with \n), original, appends
        // with \n\n between major sections
        assert!(result.starts_with("Read ARCHITECTURE.md first\nFollow security guidelines\n\n"));
        assert!(result.contains("\n\nUser's original prompt\n\n"));
        assert!(result.ends_with("\n\nConfirm you understand"));
    }

    #[test]
    fn test_single_newline_separator_for_commit_messages() {
        // PrepareCommitMessage uses single-newline separator for trailers
        let mut assembler = ContextAssembler::new(None, "\n");

        assembler.add_chunk(ContextChunk {
            prepend: None,
            append: Some(TextLines::Multiple(vec![
                "Co-authored-by: Alice <alice@example.com>".to_string(),
                "Co-authored-by: Bob <bob@example.com>".to_string(),
                "Ticket: PROJ-123".to_string(),
            ])),
        });

        let result = assembler.build_with_original();

        // Trailers should be joined with single newline (not double)
        assert_eq!(
            result,
            "Co-authored-by: Alice <alice@example.com>\nCo-authored-by: Bob <bob@example.com>\nTicket: PROJ-123"
        );
        // Should NOT have double newlines
        assert!(!result.contains("\n\n"));
    }

    #[test]
    fn test_unicode_content() {
        // Test various Unicode characters: emoji, CJK, RTL, accents
        let mut assembler = ContextAssembler::new(Some("中文内容 🚀".to_string()), "\n\n");

        assembler.add_chunk(ContextChunk {
            prepend: Some(TextLines::Single("🎯 Priority: مرتفع".to_string())),
            append: Some(TextLines::Single("Café ☕ — Naïve résumé".to_string())),
        });

        let result = assembler.build_with_original();

        // Verify Unicode is preserved correctly
        assert!(result.contains("🎯 Priority: مرتفع"));
        assert!(result.contains("中文内容 🚀"));
        assert!(result.contains("Café ☕ — Naïve résumé"));

        // Verify structure is maintained
        assert_eq!(
            result,
            "🎯 Priority: مرتفع\n\n中文内容 🚀\n\nCafé ☕ — Naïve résumé"
        );
    }

    #[test]
    fn test_very_long_content() {
        // Test with very long strings (simulating large documentation)
        let long_prepend = "A".repeat(10_000);
        let long_original = "B".repeat(50_000);
        let long_append = "C".repeat(10_000);

        let mut assembler = ContextAssembler::new(Some(long_original.clone()), "\n\n");

        assembler.add_chunk(ContextChunk {
            prepend: Some(TextLines::Single(long_prepend.clone())),
            append: Some(TextLines::Single(long_append.clone())),
        });

        let result = assembler.build_with_original();

        // Verify all content is present
        assert!(result.starts_with(&long_prepend));
        assert!(result.contains(&long_original));
        assert!(result.ends_with(&long_append));

        // Verify expected length (with separators)
        let expected_len = long_prepend.len() + 2 + long_original.len() + 2 + long_append.len();
        assert_eq!(result.len(), expected_len);
    }

    #[test]
    fn test_chunk_order_preservation_explicit() {
        // Explicit test that chunks are processed in the order they're added
        let mut assembler = ContextAssembler::new(Some("ORIGINAL".to_string()), " | ");

        // Add chunks in specific order
        assembler.add_chunk(ContextChunk {
            prepend: Some(TextLines::Single("FIRST_PRE".to_string())),
            append: Some(TextLines::Single("FIRST_APP".to_string())),
        });

        assembler.add_chunk(ContextChunk {
            prepend: Some(TextLines::Single("SECOND_PRE".to_string())),
            append: Some(TextLines::Single("SECOND_APP".to_string())),
        });

        assembler.add_chunk(ContextChunk {
            prepend: Some(TextLines::Single("THIRD_PRE".to_string())),
            append: Some(TextLines::Single("THIRD_APP".to_string())),
        });

        let result = assembler.build_with_original();

        // Expected: all prepends joined with \n, then separator, then original, then separator, then all appends joined with \n
        // Note: prepends/appends are ALWAYS joined with \n internally, separator is only between major sections
        let expected =
            "FIRST_PRE\nSECOND_PRE\nTHIRD_PRE | ORIGINAL | FIRST_APP\nSECOND_APP\nTHIRD_APP";
        assert_eq!(result, expected);

        // Verify positions are in correct order
        let first_pre_pos = result.find("FIRST_PRE").unwrap();
        let second_pre_pos = result.find("SECOND_PRE").unwrap();
        let third_pre_pos = result.find("THIRD_PRE").unwrap();
        let original_pos = result.find("ORIGINAL").unwrap();
        let first_app_pos = result.find("FIRST_APP").unwrap();
        let second_app_pos = result.find("SECOND_APP").unwrap();
        let third_app_pos = result.find("THIRD_APP").unwrap();

        assert!(first_pre_pos < second_pre_pos);
        assert!(second_pre_pos < third_pre_pos);
        assert!(third_pre_pos < original_pos);
        assert!(original_pos < first_app_pos);
        assert!(first_app_pos < second_app_pos);
        assert!(second_app_pos < third_app_pos);
    }

    #[test]
    fn test_chunk_order_with_mixed_prepend_append() {
        // Test order preservation when chunks have different combinations
        let mut assembler = ContextAssembler::new(Some("MID".to_string()), "-");

        // Chunk 1: prepend only
        assembler.add_chunk(ContextChunk {
            prepend: Some(TextLines::Single("P1".to_string())),
            append: None,
        });

        // Chunk 2: append only
        assembler.add_chunk(ContextChunk {
            prepend: None,
            append: Some(TextLines::Single("A1".to_string())),
        });

        // Chunk 3: both
        assembler.add_chunk(ContextChunk {
            prepend: Some(TextLines::Single("P2".to_string())),
            append: Some(TextLines::Single("A2".to_string())),
        });

        // Chunk 4: prepend only
        assembler.add_chunk(ContextChunk {
            prepend: Some(TextLines::Single("P3".to_string())),
            append: None,
        });

        let result = assembler.build_with_original();

        // All prepends should come first in order (P1, P2, P3) joined with \n
        // Then separator (-)
        // Then original (MID)
        // Then separator (-)
        // Then all appends in order (A1, A2) joined with \n
        // Note: Within prepend/append sections, items are joined with \n (not the separator)
        assert_eq!(result, "P1\nP2\nP3-MID-A1\nA2");
    }
}
