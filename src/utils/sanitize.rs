//! Terminal-safe rendering of untrusted text fields.
//!
//! Quarantine xattr values, code-signing summaries, and filesystem paths are
//! all attacker-controllable bytes that end up in warnings and audit reports.
//! A crafted value containing ANSI escapes or carriage returns could otherwise
//! rewrite the very report meant to describe it. Every untrusted field must
//! render through [`sanitize_field`] before reaching a terminal.
//!
//! This is DISPLAY rendering only (terminal-safe). Shell rendering
//! (shell-safe quoting for copy-pasteable commands) is a different concern
//! and must stay a separate function — do not merge the two.

/// Maximum rendered content length before truncation.
const MAX_FIELD_LEN: usize = 256;

/// Appended when the rendered content exceeds [`MAX_FIELD_LEN`].
const TRUNCATION_MARKER: &str = "…[truncated]";

/// Render an untrusted field safe for terminal display.
///
/// Control characters (including ESC) become visible literal escapes
/// (`\n`, `\r`, `\t`, `\x1b`, …) instead of bytes the terminal would
/// interpret, and the rendered content is capped at 256 chars with a
/// truncation marker appended.
pub fn sanitize_field(input: &str) -> String {
    let mut out = String::new();
    let mut rendered_len = 0usize;
    for ch in input.chars() {
        let escaped: Option<String> = match ch {
            '\n' => Some("\\n".to_string()),
            '\r' => Some("\\r".to_string()),
            '\t' => Some("\\t".to_string()),
            '\x1b' => Some("\\x1b".to_string()),
            c if c.is_control() => Some(format!("\\x{:02x}", c as u32)),
            _ => None,
        };
        // Escape strings are ASCII, so byte length == char count.
        let add = escaped.as_deref().map_or(1, str::len);
        if rendered_len + add > MAX_FIELD_LEN {
            out.push_str(TRUNCATION_MARKER);
            return out;
        }
        rendered_len += add;
        match escaped {
            Some(e) => out.push_str(&e),
            None => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newline_renders_as_visible_escape() {
        let rendered = sanitize_field("downloaded by:\nChrome");
        assert_eq!(rendered, "downloaded by:\\nChrome");
        assert!(!rendered.contains('\n'), "no raw newline may survive");
    }

    #[test]
    fn carriage_return_renders_as_visible_escape() {
        let rendered = sanitize_field("ok\rall clear");
        assert_eq!(rendered, "ok\\rall clear");
        assert!(!rendered.contains('\r'), "no raw CR may survive");
    }

    #[test]
    fn ansi_escape_sequence_renders_as_visible_escape() {
        let rendered = sanitize_field("\x1b[2J\x1b[31mEVIL");
        assert_eq!(rendered, "\\x1b[2J\\x1b[31mEVIL");
        assert!(!rendered.contains('\x1b'), "no raw ESC may survive");
    }

    #[test]
    fn other_control_bytes_render_as_hex_escapes() {
        let rendered = sanitize_field("a\x07b\x00c");
        assert_eq!(rendered, "a\\x07b\\x00c");
        assert!(
            rendered.chars().all(|c| !c.is_control()),
            "no raw control chars may survive"
        );
    }

    #[test]
    fn hostile_agent_string_field_is_neutralized() {
        // Quarantine agent-string position: `flags;time;AGENT;uuid` third field.
        let hostile = "Chrome\x1b[1A\x1b[2K0083;0;aiki verified safe;UUID";
        let rendered = sanitize_field(hostile);
        assert!(rendered.contains("\\x1b[1A"));
        assert!(rendered.contains("\\x1b[2K"));
        assert!(!rendered.contains('\x1b'));
    }

    #[test]
    fn hostile_undetermined_raw_value_is_neutralized() {
        let hostile = "not-hex;\r\n\x1b]0;title\x07";
        let rendered = sanitize_field(hostile);
        assert!(rendered.contains("\\r\\n"));
        assert!(rendered.contains("\\x1b]0;title\\x07"));
        assert!(rendered.chars().all(|c| !c.is_control()));
    }

    #[test]
    fn over_length_field_truncates_at_cap_with_marker() {
        let long = "x".repeat(MAX_FIELD_LEN + 50);
        let rendered = sanitize_field(&long);
        assert!(rendered.ends_with(TRUNCATION_MARKER));
        let content = rendered.trim_end_matches(TRUNCATION_MARKER);
        assert_eq!(content.chars().count(), MAX_FIELD_LEN);
    }

    #[test]
    fn cap_counts_rendered_chars_not_input_chars() {
        // 200 newlines render as 400 chars — must truncate even though the
        // input is under the cap.
        let input = "\n".repeat(200);
        let rendered = sanitize_field(&input);
        assert!(rendered.ends_with(TRUNCATION_MARKER));
        let content = rendered.trim_end_matches(TRUNCATION_MARKER);
        assert_eq!(content.chars().count(), MAX_FIELD_LEN);
    }

    #[test]
    fn at_cap_field_is_untouched() {
        let exact = "y".repeat(MAX_FIELD_LEN);
        assert_eq!(sanitize_field(&exact), exact);
    }

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(
            sanitize_field("0083;689ab12c;Chrome;UUID"),
            "0083;689ab12c;Chrome;UUID"
        );
    }
}
