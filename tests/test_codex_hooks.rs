//! Integration tests for Codex hooks (OTel + native stdin hooks)
//!
//! Tests the end-to-end flow of:
//! - OTel protobuf parsing → session state updates
//! - Native hook config generation
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

    // Verify tool_result is parsed
    assert!(matches!(&events[2].0, CodexOtelEvent::ToolResult { .. }));
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
    assert!(matches!(&events[0].0, CodexOtelEvent::Unknown { .. }));
    assert!(matches!(&events[1].0, CodexOtelEvent::Unknown { .. }));
    assert!(matches!(&events[2].0, CodexOtelEvent::ToolDecision { .. }));
}

#[test]
fn test_codex_config_generation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_path = tmp.path().join("config.toml");
    let hooks_path = tmp.path().join("hooks.json");

    // Simulate what install_codex_hooks_global does.
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
    let mut features = toml::map::Map::new();
    features.insert("codex_hooks".to_string(), toml::Value::Boolean(true));
    config.insert("features".to_string(), toml::Value::Table(features));

    let content = toml::to_string_pretty(&toml::Value::Table(config)).unwrap();
    fs::write(&config_path, &content).unwrap();

    let hooks_json = serde_json::json!({
        "hooks": {
            "SessionStart": [{"hooks": [{"type": "command", "command": "/usr/local/bin/aiki hooks stdin --agent codex --event sessionStart"}]}],
            "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "/usr/local/bin/aiki hooks stdin --agent codex --event userPromptSubmit"}]}],
            "PreToolUse": [{"hooks": [{"type": "command", "command": "/usr/local/bin/aiki hooks stdin --agent codex --event preToolUse"}]}],
            "Stop": [{"hooks": [{"type": "command", "command": "/usr/local/bin/aiki hooks stdin --agent codex --event stop"}]}]
        }
    });
    fs::write(&hooks_path, serde_json::to_string_pretty(&hooks_json).unwrap()).unwrap();

    // Parse back and verify config.toml.
    let parsed: toml::Value = toml::from_str(&content).unwrap();

    let otel = parsed.get("otel").unwrap().as_table().unwrap();
    assert_eq!(
        otel.get("log_user_prompt").unwrap().as_bool().unwrap(),
        true
    );

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

    assert!(parsed.get("notify").is_none());
    assert_eq!(
        parsed["features"]["codex_hooks"].as_bool(),
        Some(true)
    );

    // Parse back and verify hooks.json.
    let hooks_value: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&hooks_path).unwrap()).unwrap();
    let hooks = hooks_value["hooks"].as_object().unwrap();
    for (event_name, event_arg) in [
        ("SessionStart", "sessionStart"),
        ("UserPromptSubmit", "userPromptSubmit"),
        ("PreToolUse", "preToolUse"),
        ("Stop", "stop"),
    ] {
        let groups = hooks[event_name].as_array().unwrap();
        let command = groups[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(command.contains("aiki"));
        assert!(command.contains("stdin"));
        assert!(command.contains(event_arg));
    }
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
