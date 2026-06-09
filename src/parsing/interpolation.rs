//! Shared variable interpolation primitives
//!
//! Provides core parsing and substitution logic for {{var}} syntax
//! used by both hooks (VariableResolver) and templates (VariableContext).

/// A variable reference found during template parsing
#[derive(Debug, Clone, PartialEq)]
pub struct VariableRef {
    /// Start position in original string (byte offset)
    pub start: usize,
    /// End position in original string (byte offset, exclusive)
    pub end: usize,
    /// Variable name (trimmed, without {{ }})
    pub name: String,
}

/// Parse variable references from text using {{var}} syntax
///
/// Returns vector of variable references with byte offsets.
/// Handles edge cases:
/// - Unclosed braces: `{{var` treated as literal text
/// - Empty refs: `{{}}` treated as literal text
/// - Whitespace: `{{ var }}` trimmed to `var`
/// - Nested braces: Only outermost `{{}}` pairs are recognized
pub fn parse_template(text: &str) -> Vec<VariableRef> {
    let mut refs = Vec::new();
    // Use char_indices() to get byte offsets, not char positions.
    // This is critical: chars().enumerate() gives sequential char counts,
    // which differ from byte offsets for non-ASCII text. Since start/end
    // are used to slice the original string (text[start..end]), they must
    // be valid byte offsets.
    let mut chars = text.char_indices().peekable();

    while let Some((pos, c)) = chars.next() {
        if c == '{' {
            if chars.peek().map(|(_, ch)| *ch) == Some('{') {
                // Start of variable reference: {{
                chars.next(); // consume second {
                let ref_start = pos;

                // Read until }}
                let mut var_name = String::new();
                let mut found_close = false;

                while let Some((end_pos, c2)) = chars.next() {
                    if c2 == '}' {
                        if chars.peek().map(|(_, ch)| *ch) == Some('}') {
                            chars.next(); // consume second }
                                          // end_pos is byte offset of first }, +2 for both } chars
                            let ref_end = end_pos + 2;
                            found_close = true;

                            // Only add if variable name is non-empty after trimming
                            let trimmed = var_name.trim();
                            if !trimmed.is_empty() {
                                refs.push(VariableRef {
                                    start: ref_start,
                                    end: ref_end,
                                    name: trimmed.to_string(),
                                });
                            }
                            break;
                        }
                        // Single }, not end of variable
                        var_name.push(c2);
                    } else {
                        var_name.push(c2);
                    }
                }

                // If unclosed, continue scanning from after {{
                if !found_close {
                    // Already advanced by the while loop
                }
            }
        }
    }

    refs
}

/// Substitute variables in text using a lookup function
///
/// The lookup function receives variable names and returns their values.
/// Returns `Err` with list of unknown variable names if any variables couldn't be resolved.
pub fn substitute_template<F>(text: &str, mut lookup: F) -> Result<String, Vec<String>>
where
    F: FnMut(&str) -> Option<String>,
{
    let refs = parse_template(text);

    if refs.is_empty() {
        return Ok(text.to_string());
    }

    let mut result = String::with_capacity(text.len());
    let mut last_pos = 0;
    let mut unknown_vars = Vec::new();

    for var_ref in refs {
        // Add text before this variable
        result.push_str(&text[last_pos..var_ref.start]);

        // Try to resolve variable
        match lookup(&var_ref.name) {
            Some(value) => result.push_str(&value),
            None => {
                unknown_vars.push(var_ref.name.clone());
                // Still include the original text for error context
                result.push_str(&text[var_ref.start..var_ref.end]);
            }
        }

        last_pos = var_ref.end;
    }

    // Add remaining text
    result.push_str(&text[last_pos..]);

    if unknown_vars.is_empty() {
        Ok(result)
    } else {
        Err(unknown_vars)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_template_single_var() {
        let refs = parse_template("Hello {{name}}");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "name");
        assert_eq!(refs[0].start, 6);
        assert_eq!(refs[0].end, 14);
    }

    #[test]
    fn test_parse_template_multiple_vars() {
        let refs = parse_template("{{a}} and {{b}} and {{c}}");
        assert_eq!(refs.len(), 3);
        assert_eq!(refs[0].name, "a");
        assert_eq!(refs[1].name, "b");
        assert_eq!(refs[2].name, "c");
    }

    #[test]
    fn test_parse_template_with_whitespace() {
        let refs = parse_template("{{ name }}");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "name");
    }

    #[test]
    fn test_parse_template_empty_braces() {
        let refs = parse_template("Empty {{}} braces");
        assert_eq!(refs.len(), 0);
    }

    #[test]
    fn test_parse_template_unclosed() {
        let refs = parse_template("Unclosed {{brace");
        assert_eq!(refs.len(), 0);
    }

    #[test]
    fn test_parse_template_single_brace() {
        let refs = parse_template("Single {brace}");
        assert_eq!(refs.len(), 0);
    }

    #[test]
    fn test_parse_template_dotted_names() {
        let refs = parse_template("{{data.key}} and {{event.task.id}}");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].name, "data.key");
        assert_eq!(refs[1].name, "event.task.id");
    }

    #[test]
    fn test_parse_template_non_ascii() {
        // Multi-byte chars before variable: byte offsets != char positions
        let text = "Héllo {{name}}"; // é is 2 bytes in UTF-8
        let refs = parse_template(text);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "name");
        // Verify byte offsets allow correct slicing
        assert_eq!(&text[refs[0].start..refs[0].end], "{{name}}");
        assert_eq!(&text[..refs[0].start], "Héllo ");
    }

    #[test]
    fn test_substitute_template_non_ascii() {
        let result = substitute_template("こんにちは {{name}}!", |var| {
            if var == "name" {
                Some("世界".to_string())
            } else {
                None
            }
        });
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "こんにちは 世界!");
    }

    #[test]
    fn test_substitute_template_basic() {
        let result = substitute_template("Hello {{name}}!", |var| {
            if var == "name" {
                Some("World".to_string())
            } else {
                None
            }
        });
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "Hello World!");
    }

    #[test]
    fn test_substitute_template_multiple() {
        let result = substitute_template("{{a}} + {{b}} = {{c}}", |var| match var {
            "a" => Some("1".to_string()),
            "b" => Some("2".to_string()),
            "c" => Some("3".to_string()),
            _ => None,
        });
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "1 + 2 = 3");
    }

    #[test]
    fn test_substitute_template_missing_var() {
        let result = substitute_template("Hello {{name}}!", |_| None);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), vec!["name"]);
    }

    #[test]
    fn test_substitute_template_no_vars() {
        let result = substitute_template("Plain text", |_| None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "Plain text");
    }
}
