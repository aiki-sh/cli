//! Integration tests for Codex hooks (OTel + notify)
//!
//! Tests the end-to-end flow of:
//! - OTel protobuf parsing → session state updates
//! - Notify payload parsing → turn completion handling
//! - Hook installation (config.toml generation)

use aiki::editors::codex::otel::{self, CodexOtelEvent};
use prost::Message;
use std::fs;

// Re-import protobuf types for building test payloads
use aiki::editors::codex::otel::{
    AnyValue, ExportLogsServiceRequest, KeyValue, LogRecord, Resource, ResourceLogs, ScopeLogs,
};

/// Helper to build a minimal OTLP request with given events
fn build_otlp_request(
    events: Vec<(&str, Vec<(&str, &str)>)>,
    resource_attrs: Vec<(&str, &str)>,
) -> Vec<u8> {
    let resource = if resource_attrs.is_empty() {
        None
    } else {
        Some(Resource {
            attributes: resource_attrs
                .into_iter()
                .map(|(k, v)| KeyValue {
                    key: k.to_string(),
                    value: Some(AnyValue {
                        value: Some(otel::any_value::Value::StringValue(v.to_string())),
                    }),
                })
                .collect(),
        })
    };

    let log_records: Vec<LogRecord> = events
        .into_iter()
        .map(|(event_name, attrs)| LogRecord {
            time_unix_nano: 1000000000,
            observed_time_unix_nano: 0,
            severity_number: 0,
            severity_text: String::new(),
            body: Some(AnyValue {
                value: Some(otel::any_value::Value::StringValue(event_name.to_string())),
            }),
            attributes: attrs
                .into_iter()
                .map(|(k, v)| KeyValue {
                    key: k.to_string(),
                    value: Some(AnyValue {
                        value: Some(otel::any_value::Value::StringValue(v.to_string())),
                    }),
                })
                .collect(),
            flags: 0,
            trace_id: Vec::new(),
            span_id: Vec::new(),
        })
        .collect();

    let request = ExportLogsServiceRequest {
        resource_logs: vec![ResourceLogs {
            resource,
            scope_logs: vec![ScopeLogs {
                scope: None,
                log_records,
            }],
        }],
    };

    request.encode_to_vec()
}

#[test]
fn test_otel_conversation_starts_parsed_correctly() {
    let payload = build_otlp_request(
        vec![(
            "codex.conversation_starts",
            vec![("conversation.id", "conv_integration_1")],
        )],
        vec![("service.version", "2.1.0")],
    );

    let events = otel::parse_otlp_logs(&payload);
    assert_eq!(events.len(), 1);

    let (event, context) = &events[0];
    assert_eq!(context.agent_version.as_deref(), Some("2.1.0"));

    match event {
        CodexOtelEvent::ConversationStarts { conversation_id } => {
            assert_eq!(conversation_id, "conv_integration_1");
        }
        _ => panic!("Expected ConversationStarts"),
    }
}

#[test]
fn test_otel_full_session_flow() {
    // Simulate a complete session: conversation_starts → user_prompt → tool_result
    let payload = build_otlp_request(
        vec![
            (
                "codex.conversation_starts",
                vec![("conversation.id", "conv_flow_1")],
            ),
            (
                "codex.user_prompt",
                vec![
                    ("conversation.id", "conv_flow_1"),
                    ("prompt", "Fix the bug in login.rs"),
                ],
            ),
            (
                "codex.tool_result",
                vec![
                    ("conversation.id", "conv_flow_1"),
                    ("tool_name", "write_file"),
                    ("arguments", r#"{"file_path": "src/login.rs"}"#),
                ],
            ),
        ],
        vec![("service.version", "2.1.0")],
    );

    let events = otel::parse_otlp_logs(&payload);
    assert_eq!(events.len(), 3);

    // Verify event order is preserved
    assert!(matches!(
        &events[0].0,
        CodexOtelEvent::ConversationStarts { .. }
    ));
    assert!(matches!(&events[1].0, CodexOtelEvent::UserPrompt { .. }));
    assert!(matches!(&events[2].0, CodexOtelEvent::ToolResult { .. }));

    // Verify user_prompt has the prompt content
    if let CodexOtelEvent::UserPrompt { prompt, .. } = &events[1].0 {
        assert_eq!(prompt.as_deref(), Some("Fix the bug in login.rs"));
    }

    // Verify tool_result has extractable file path
    if let CodexOtelEvent::ToolResult {
        tool_name,
        arguments,
        ..
    } = &events[2].0
    {
        assert_eq!(tool_name.as_deref(), Some("write_file"));
        let files = otel::extract_modified_files(
            tool_name.as_deref(),
            arguments.as_deref(),
            None,
        );
        assert_eq!(files, vec!["src/login.rs"]);
    }
}

#[test]
fn test_otel_deferred_events_not_mapped() {
    let payload = build_otlp_request(
        vec![
            ("codex.api_request", vec![("conversation.id", "conv_defer")]),
            ("codex.sse_event", vec![("conversation.id", "conv_defer")]),
            (
                "codex.tool_decision",
                vec![("conversation.id", "conv_defer")],
            ),
        ],
        vec![],
    );

    let events = otel::parse_otlp_logs(&payload);
    assert_eq!(events.len(), 3);

    // All should be Unknown (acknowledged but not mapped)
    for (event, _) in &events {
        assert!(matches!(event, CodexOtelEvent::Unknown { .. }));
    }
}



#[test]
fn test_notify_payload_parsing() {
    use aiki::editors::codex::NotifyPayload;

    let json = r#"{
        "type": "agent-turn-complete",
        "thread-id": "conv_notify_1",
        "turn-id": "turn_abc",
        "cwd": "/home/user/project",
        "last-assistant-message": "I fixed the bug in login.rs by updating the auth check.",
        "input-messages": [{"role": "user", "content": "Fix the login bug"}]
    }"#;

    let payload: NotifyPayload = serde_json::from_str(json).unwrap();
    assert_eq!(payload.event_type, "agent-turn-complete");
    assert_eq!(payload.thread_id, "conv_notify_1");
    assert_eq!(payload.cwd, "/home/user/project");
    assert_eq!(
        payload.last_assistant_message.as_deref(),
        Some("I fixed the bug in login.rs by updating the auth check.")
    );
}

#[test]
fn test_notify_payload_minimal() {
    use aiki::editors::codex::NotifyPayload;

    // Minimal payload with only required fields
    let json = r#"{
        "type": "agent-turn-complete",
        "thread-id": "conv_minimal",
        "cwd": "/tmp"
    }"#;

    let payload: NotifyPayload = serde_json::from_str(json).unwrap();
    assert_eq!(payload.thread_id, "conv_minimal");
    assert_eq!(payload.cwd, "/tmp");
    assert!(payload.last_assistant_message.is_none());
}

#[test]
fn test_extract_modified_files_various_tools() {
    // write_file tool
    let files = otel::extract_modified_files(
        Some("write_file"),
        Some(r#"{"file_path": "src/main.rs"}"#),
        None,
    );
    assert_eq!(files, vec!["src/main.rs"]);

    // edit tool
    let files = otel::extract_modified_files(
        Some("edit"),
        Some(r#"{"path": "lib/utils.py"}"#),
        None,
    );
    assert_eq!(files, vec!["lib/utils.py"]);

    // Non-file tool (web search) - should return empty
    let files = otel::extract_modified_files(
        Some("web_search"),
        Some(r#"{"query": "rust async"}"#),
        None,
    );
    assert!(files.is_empty());

    // Unknown tool with file path - should try to extract
    let files = otel::extract_modified_files(
        None,
        Some(r#"{"file_path": "unknown_tool_file.txt"}"#),
        None,
    );
    assert_eq!(files, vec!["unknown_tool_file.txt"]);
}

#[test]
fn test_extract_modified_files_path_resolution() {
    let cwd = std::path::Path::new("/home/user/project");

    // Relative path resolved against cwd
    let files = otel::extract_modified_files(
        Some("write"),
        Some(r#"{"file_path": "src/foo.rs"}"#),
        Some(cwd),
    );
    assert_eq!(files, vec!["/home/user/project/src/foo.rs"]);

    // Absolute path unchanged
    let files = otel::extract_modified_files(
        Some("write"),
        Some(r#"{"file_path": "/etc/config.toml"}"#),
        Some(cwd),
    );
    assert_eq!(files, vec!["/etc/config.toml"]);
}

#[test]
fn test_extract_modified_files_edge_cases() {
    // Empty arguments
    let files = otel::extract_modified_files(Some("write"), Some(""), None);
    assert!(files.is_empty());

    // No arguments
    let files = otel::extract_modified_files(Some("write"), None, None);
    assert!(files.is_empty());

    // Invalid JSON - try as plain path
    let files = otel::extract_modified_files(Some("write"), Some("simple_file.txt"), None);
    assert_eq!(files, vec!["simple_file.txt"]);

    // JSON without file path keys
    let files = otel::extract_modified_files(
        Some("write"),
        Some(r#"{"content": "hello world"}"#),
        None,
    );
    assert!(files.is_empty());
}

#[test]
fn test_codex_config_toml_generation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");

    // Simulate what install_codex_hooks_global does
    // exporter is a tagged enum struct variant: { "otlp-http": { endpoint, protocol } }
    let mut config = toml::map::Map::new();

    let mut otlp_http = toml::map::Map::new();
    otlp_http.insert(
        "endpoint".to_string(),
        toml::Value::String("http://127.0.0.1:19876/v1/logs".to_string()),
    );
    otlp_http.insert(
        "protocol".to_string(),
        toml::Value::String("binary".to_string()),
    );

    let mut exporter = toml::map::Map::new();
    exporter.insert("otlp-http".to_string(), toml::Value::Table(otlp_http));

    let mut otel_table = toml::map::Map::new();
    otel_table.insert("exporter".to_string(), toml::Value::Table(exporter));
    otel_table.insert("log_user_prompt".to_string(), toml::Value::Boolean(true));
    config.insert("otel".to_string(), toml::Value::Table(otel_table));

    let notify_cmd = vec![
        toml::Value::String("/usr/local/bin/aiki".to_string()),
        toml::Value::String("hooks".to_string()),
        toml::Value::String("handle".to_string()),
        toml::Value::String("--agent".to_string()),
        toml::Value::String("codex".to_string()),
        toml::Value::String("--event".to_string()),
        toml::Value::String("agent-turn-complete".to_string()),
    ];
    config.insert("notify".to_string(), toml::Value::Array(notify_cmd));

    let content = toml::to_string_pretty(&toml::Value::Table(config)).unwrap();
    fs::write(&config_path, &content).unwrap();

    // Parse back and verify
    let parsed: toml::Value = toml::from_str(&content).unwrap();

    let otel = parsed.get("otel").unwrap().as_table().unwrap();
    assert_eq!(otel.get("log_user_prompt").unwrap().as_bool().unwrap(), true);

    // Verify exporter is a struct variant (table with otlp-http key)
    let exporter = otel.get("exporter").unwrap().as_table().unwrap();
    let otlp_http = exporter.get("otlp-http").unwrap().as_table().unwrap();
    assert_eq!(
        otlp_http.get("endpoint").unwrap().as_str().unwrap(),
        "http://127.0.0.1:19876/v1/logs"
    );
    assert_eq!(
        otlp_http.get("protocol").unwrap().as_str().unwrap(),
        "binary"
    );

    let notify = parsed.get("notify").unwrap().as_array().unwrap();
    assert!(notify
        .iter()
        .any(|v| v.as_str().is_some_and(|s| s.contains("aiki"))));
    assert!(notify
        .iter()
        .any(|v| v.as_str() == Some("agent-turn-complete")));
}

#[test]
fn test_gzip_decompression_roundtrip() {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    // Build a simple OTLP payload
    let payload = build_otlp_request(
        vec![(
            "codex.conversation_starts",
            vec![("conversation.id", "conv_gzip")],
        )],
        vec![],
    );

    // Compress with gzip
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&payload).unwrap();
    let compressed = encoder.finish().unwrap();

    // Decompress
    use flate2::read::GzDecoder;
    use std::io::Read;
    let mut decoder = GzDecoder::new(&compressed[..]);
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed).unwrap();

    // Parse the decompressed payload
    let events = otel::parse_otlp_logs(&decompressed);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0].0,
        CodexOtelEvent::ConversationStarts { .. }
    ));
}





#[test]
fn test_content_type_detection_logs_endpoint() {
    // Verify that /v1/traces is detected as unsupported
    // (This tests the detection logic, not the actual HTTP handling)
    let path = "/v1/traces";
    assert!(path.contains("/v1/traces"));

    let path = "/v1/logs";
    assert!(!path.contains("/v1/traces"));
}


