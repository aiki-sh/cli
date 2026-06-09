//! Shared transcript parsing types and aggregation.
//!
//! Agent-specific parsers (Claude Code, Codex, etc.) each know how to walk
//! their transcript format, but they all produce `Vec<TranscriptEntry>`.
//! The shared [`TurnTranscript::from_entries`] handles aggregation:
//! summing token usage and taking the last response/model.

use crate::events::TokenUsage;

/// One API call's worth of data extracted from a transcript line.
///
/// Agent parsers emit one of these per assistant message (Claude Code) or
/// per `token_count` event (Codex). Fields are optional because not every
/// format carries all data (e.g. Codex gets response text from the hook
/// payload, not the transcript).
#[derive(Debug, Clone, Default)]
pub struct TranscriptEntry {
    pub response: Option<String>,
    pub tokens: Option<TokenUsage>,
    pub model: Option<String>,
}

/// Aggregated result from a full turn's transcript entries.
#[derive(Debug, Clone, Default)]
pub struct TurnTranscript {
    pub response: String,
    pub tokens: Option<TokenUsage>,
    pub model: Option<String>,
}

impl TurnTranscript {
    /// Parse a transcript file using an agent-specific line parser.
    ///
    /// Reads the file, passes the content to `parse_lines` which returns
    /// per-API-call entries, then aggregates them via [`from_entries`].
    /// Returns `Default` if the file can't be read or produces no entries.
    ///
    /// If the first read produces no entries, retries once after a short delay.
    /// This works around a race condition where Claude Code fires the Stop hook
    /// before flushing the final assistant entry to the transcript JSONL file.
    pub fn parse(path: &str, parse_lines: fn(&str) -> Vec<TranscriptEntry>) -> Self {
        for attempt in 0..2 {
            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => return Self::default(),
            };
            let entries = parse_lines(&content);
            if !entries.is_empty() {
                return Self::from_entries(entries);
            }
            if attempt == 0 {
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        }
        Self::default()
    }

    /// Aggregate a sequence of per-API-call entries into a single turn transcript.
    ///
    /// - Tokens are summed across all entries.
    /// - Response text and model are taken from the last entry that has them.
    pub fn from_entries(entries: Vec<TranscriptEntry>) -> Self {
        let tokens = Self::sum_tokens(&entries);

        let mut last_response: Option<String> = None;
        let mut last_model: Option<String> = None;

        for entry in entries {
            if let Some(response) = entry.response {
                if !response.is_empty() {
                    last_response = Some(response);
                }
            }
            if let Some(model) = entry.model {
                last_model = Some(model);
            }
        }

        Self {
            response: last_response.unwrap_or_default(),
            tokens,
            model: last_model,
        }
    }

    fn sum_tokens(entries: &[TranscriptEntry]) -> Option<TokenUsage> {
        entries
            .iter()
            .filter_map(|e| e.tokens.clone())
            .reduce(|a, b| a + b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_entries_sums_tokens() {
        let entries = vec![
            TranscriptEntry {
                tokens: Some(TokenUsage {
                    input: 100,
                    output: 50,
                    cache_read: 80,
                    cache_created: 10,
                }),
                ..Default::default()
            },
            TranscriptEntry {
                tokens: Some(TokenUsage {
                    input: 200,
                    output: 80,
                    cache_read: 150,
                    cache_created: 20,
                }),
                ..Default::default()
            },
        ];
        let t = TurnTranscript::from_entries(entries);
        let tokens = t.tokens.unwrap();
        assert_eq!(tokens.input, 300);
        assert_eq!(tokens.output, 130);
        assert_eq!(tokens.cache_read, 230);
        assert_eq!(tokens.cache_created, 30);
    }

    #[test]
    fn test_from_entries_takes_last_response_and_model() {
        let entries = vec![
            TranscriptEntry {
                response: Some("first".to_string()),
                model: Some("model-a".to_string()),
                ..Default::default()
            },
            TranscriptEntry {
                response: Some("second".to_string()),
                model: Some("model-b".to_string()),
                ..Default::default()
            },
        ];
        let t = TurnTranscript::from_entries(entries);
        assert_eq!(t.response, "second");
        assert_eq!(t.model.as_deref(), Some("model-b"));
    }

    #[test]
    fn test_from_entries_skips_empty_responses() {
        let entries = vec![
            TranscriptEntry {
                response: Some("real text".to_string()),
                ..Default::default()
            },
            TranscriptEntry {
                response: Some(String::new()),
                ..Default::default()
            },
        ];
        let t = TurnTranscript::from_entries(entries);
        assert_eq!(t.response, "real text");
    }

    #[test]
    fn test_from_entries_empty_returns_default() {
        let t = TurnTranscript::from_entries(vec![]);
        assert_eq!(t.response, "");
        assert!(t.tokens.is_none());
        assert!(t.model.is_none());
    }

    #[test]
    fn test_from_entries_no_data_returns_default() {
        let entries = vec![TranscriptEntry::default(), TranscriptEntry::default()];
        let t = TurnTranscript::from_entries(entries);
        assert_eq!(t.response, "");
        assert!(t.tokens.is_none());
    }

    #[test]
    fn test_from_entries_tokens_only() {
        let entries = vec![TranscriptEntry {
            tokens: Some(TokenUsage {
                input: 500,
                output: 100,
                ..Default::default()
            }),
            ..Default::default()
        }];
        let t = TurnTranscript::from_entries(entries);
        assert_eq!(t.response, "");
        assert_eq!(t.tokens.unwrap().input, 500);
    }
}
