use crate::cache::debug_log;
use prost::Message;
use std::path::PathBuf;

// ============================================================================
// OTLP Log Protobuf Types (minimal subset for Codex event parsing)
//
// These match the opentelemetry-proto definitions for logs:
// https://github.com/open-telemetry/opentelemetry-proto/blob/main/opentelemetry/proto/logs/v1/logs.proto
// https://github.com/open-telemetry/opentelemetry-proto/blob/main/opentelemetry/proto/common/v1/common.proto
// https://github.com/open-telemetry/opentelemetry-proto/blob/main/opentelemetry/proto/resource/v1/resource.proto
//
// We only define fields we actually read to keep the implementation minimal.
// ============================================================================

/// Top-level OTLP/HTTP request for logs
#[derive(Clone, PartialEq, Message)]
pub struct ExportLogsServiceRequest {
    #[prost(message, repeated, tag = "1")]
    pub resource_logs: Vec<ResourceLogs>,
}

/// A collection of logs from a resource
#[derive(Clone, PartialEq, Message)]
pub struct ResourceLogs {
    #[prost(message, optional, tag = "1")]
    pub resource: Option<Resource>,
    #[prost(message, repeated, tag = "2")]
    pub scope_logs: Vec<ScopeLogs>,
}

/// A collection of log records from an instrumentation scope
#[derive(Clone, PartialEq, Message)]
pub struct ScopeLogs {
    #[prost(message, optional, tag = "1")]
    pub scope: Option<InstrumentationScope>,
    #[prost(message, repeated, tag = "2")]
    pub log_records: Vec<LogRecord>,
}

/// A log record (represents a single OTel event)
#[derive(Clone, PartialEq, Message)]
pub struct LogRecord {
    /// Time when the event occurred (unix nano)
    #[prost(fixed64, tag = "1")]
    pub time_unix_nano: u64,
    /// Time when the event was observed (unix nano)
    #[prost(fixed64, tag = "11")]
    pub observed_time_unix_nano: u64,
    /// Severity number
    #[prost(int32, tag = "2")]
    pub severity_number: i32,
    /// Severity text
    #[prost(string, tag = "3")]
    pub severity_text: String,
    /// Body of the log record (often contains event data)
    #[prost(message, optional, tag = "5")]
    pub body: Option<AnyValue>,
    /// Additional attributes
    #[prost(message, repeated, tag = "6")]
    pub attributes: Vec<KeyValue>,
    /// Flags
    #[prost(fixed32, tag = "8")]
    pub flags: u32,
    /// Trace ID
    #[prost(bytes = "vec", tag = "9")]
    pub trace_id: Vec<u8>,
    /// Span ID
    #[prost(bytes = "vec", tag = "10")]
    pub span_id: Vec<u8>,
}

/// Resource information (service.name, service.version, etc.)
#[derive(Clone, PartialEq, Message)]
pub struct Resource {
    #[prost(message, repeated, tag = "1")]
    pub attributes: Vec<KeyValue>,
}

/// Instrumentation scope
#[derive(Clone, PartialEq, Message)]
pub struct InstrumentationScope {
    #[prost(string, tag = "1")]
    pub name: String,
    #[prost(string, tag = "2")]
    pub version: String,
}

/// Key-value pair for attributes
#[derive(Clone, PartialEq, Message)]
pub struct KeyValue {
    #[prost(string, tag = "1")]
    pub key: String,
    #[prost(message, optional, tag = "2")]
    pub value: Option<AnyValue>,
}

/// Any value type (string, bool, int, double, array, kvlist, bytes)
#[derive(Clone, PartialEq, Message)]
pub struct AnyValue {
    #[prost(oneof = "any_value::Value", tags = "1, 2, 3, 4, 5, 6, 7")]
    pub value: Option<any_value::Value>,
}

pub mod any_value {
    use super::*;

    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Value {
        #[prost(string, tag = "1")]
        StringValue(String),
        #[prost(bool, tag = "2")]
        BoolValue(bool),
        #[prost(int64, tag = "3")]
        IntValue(i64),
        #[prost(double, tag = "4")]
        DoubleValue(f64),
        #[prost(message, tag = "5")]
        ArrayValue(ArrayValue),
        #[prost(message, tag = "6")]
        KvlistValue(KeyValueList),
        #[prost(bytes, tag = "7")]
        BytesValue(Vec<u8>),
    }
}

/// Array of values
#[derive(Clone, PartialEq, Message)]
pub struct ArrayValue {
    #[prost(message, repeated, tag = "1")]
    pub values: Vec<AnyValue>,
}

/// Key-value list
#[derive(Clone, PartialEq, Message)]
pub struct KeyValueList {
    #[prost(message, repeated, tag = "1")]
    pub values: Vec<KeyValue>,
}

// ============================================================================
// OTLP Trace Protobuf Types
//
// Codex sends trace data (ExportTraceServiceRequest) even when configured for
// the /v1/logs endpoint. Events are embedded in span events with attributes
// like event.name, conversation.id, etc.
// ============================================================================

/// Top-level OTLP/HTTP request for traces
#[derive(Clone, PartialEq, Message)]
pub struct ExportTraceServiceRequest {
    #[prost(message, repeated, tag = "1")]
    pub resource_spans: Vec<ResourceSpans>,
}

/// A collection of spans from a resource
#[derive(Clone, PartialEq, Message)]
pub struct ResourceSpans {
    #[prost(message, optional, tag = "1")]
    pub resource: Option<Resource>,
    #[prost(message, repeated, tag = "2")]
    pub scope_spans: Vec<ScopeSpans>,
}

/// A collection of spans from an instrumentation scope
#[derive(Clone, PartialEq, Message)]
pub struct ScopeSpans {
    #[prost(message, optional, tag = "1")]
    pub scope: Option<InstrumentationScope>,
    #[prost(message, repeated, tag = "2")]
    pub spans: Vec<Span>,
}

/// A trace span (contains events with Codex telemetry data)
#[derive(Clone, PartialEq, Message)]
pub struct Span {
    #[prost(string, tag = "5")]
    pub name: String,
    #[prost(message, repeated, tag = "9")]
    pub attributes: Vec<KeyValue>,
    #[prost(message, repeated, tag = "11")]
    pub events: Vec<SpanEvent>,
}

/// A span event (contains Codex event data like codex.user_prompt, codex.tool_decision)
#[derive(Clone, PartialEq, Message)]
pub struct SpanEvent {
    #[prost(string, tag = "2")]
    pub name: String,
    #[prost(message, repeated, tag = "3")]
    pub attributes: Vec<KeyValue>,
}

// ============================================================================
// Codex OTel Event Types
// ============================================================================

/// Parsed Codex OTel event
#[derive(Debug, Clone)]
pub enum CodexOtelEvent {
    /// `codex.conversation_starts` - New session started
    ConversationStarts {
        conversation_id: String,
    },
    /// `codex.user_prompt` - Turn started (user submitted prompt)
    UserPrompt {
        conversation_id: String,
        prompt: Option<String>,
    },
    /// `codex.tool_result` - Tool execution completed (may contain file modifications)
    ToolResult {
        conversation_id: String,
        tool_name: Option<String>,
        arguments: Option<String>,
    },
    /// Unrecognized event (acknowledged but not processed)
    Unknown {
        event_name: String,
    },
}

/// Additional context captured from OTel resource/log attributes.
#[derive(Debug, Clone, Default)]
pub struct CodexOtelContext {
    pub agent_version: Option<String>,
    pub agent_pid: Option<u32>,
    pub cwd: Option<PathBuf>,
}

/// Parse an OTLP/HTTP protobuf payload into Codex events
///
/// Handles `ExportLogsServiceRequest` payloads.
/// Returns events in order within each batch.
pub fn parse_otlp_logs(data: &[u8]) -> Vec<(CodexOtelEvent, CodexOtelContext)> {
    let request = match ExportLogsServiceRequest::decode(data) {
        Ok(r) => r,
        Err(e) => {
            debug_log(|| format!("Failed to decode OTLP protobuf: {}", e));
            return Vec::new();
        }
    };

    let mut events = Vec::new();

    for (ri, resource_logs) in request.resource_logs.iter().enumerate() {
        // Dump resource attributes
        if let Some(ref resource) = resource_logs.resource {
            debug_log(|| format!("OTLP LOG resource[{}] attributes:", ri));
            for kv in &resource.attributes {
                debug_log(|| format!("  {} = {}", kv.key, format_any_value(&kv.value)));
            }
        }

        let resource_context = build_context_from_resource(resource_logs.resource.as_ref());

        for (si, scope_logs) in resource_logs.scope_logs.iter().enumerate() {
            for (li, log_record) in scope_logs.log_records.iter().enumerate() {
                debug_log(|| {
                    let body_str = get_body_string(log_record)
                        .unwrap_or_else(|| "<no body>".to_string());
                    format!(
                        "OTLP LOG record[{}.{}.{}] body={:?} attributes:",
                        ri, si, li, body_str
                    )
                });
                for kv in &log_record.attributes {
                    debug_log(|| format!("  {} = {}", kv.key, format_any_value(&kv.value)));
                }

                if let Some(event) = parse_log_record(log_record) {
                    let mut context = resource_context.clone();
                    merge_context_from_attributes(&mut context, &log_record.attributes);
                    events.push((event, context));
                }
            }
        }
    }

    events
}

/// Parse an OTLP/HTTP trace protobuf payload into Codex events
///
/// Codex sends trace data (ExportTraceServiceRequest) even when configured for
/// the logs endpoint. Events are embedded as span events with attributes like:
/// - event.name: "codex.user_prompt", "codex.tool_decision", etc.
/// - conversation.id: the session identifier
/// - app.version: agent version
pub fn parse_otlp_traces(data: &[u8]) -> Vec<(CodexOtelEvent, CodexOtelContext)> {
    let request = match ExportTraceServiceRequest::decode(data) {
        Ok(r) => r,
        Err(e) => {
            debug_log(|| format!("Failed to decode OTLP trace protobuf: {}", e));
            return Vec::new();
        }
    };

    let mut events = Vec::new();

    for (ri, resource_spans) in request.resource_spans.iter().enumerate() {
        // Dump resource attributes
        if let Some(ref resource) = resource_spans.resource {
            debug_log(|| format!("OTLP TRACE resource[{}] attributes:", ri));
            for kv in &resource.attributes {
                debug_log(|| format!("  {} = {}", kv.key, format_any_value(&kv.value)));
            }
        }

        let resource_context = build_context_from_resource(resource_spans.resource.as_ref());

        for (si, scope_spans) in resource_spans.scope_spans.iter().enumerate() {
            for (spi, span) in scope_spans.spans.iter().enumerate() {
                debug_log(|| format!(
                    "OTLP TRACE span[{}.{}.{}] name={:?} attributes:",
                    ri, si, spi, span.name
                ));
                for kv in &span.attributes {
                    debug_log(|| format!("  {} = {}", kv.key, format_any_value(&kv.value)));
                }

                // Dump and parse span events
                for (ei, span_event) in span.events.iter().enumerate() {
                    debug_log(|| format!(
                        "OTLP TRACE span_event[{}.{}.{}.{}] name={:?} attributes:",
                        ri, si, spi, ei, span_event.name
                    ));
                    for kv in &span_event.attributes {
                        debug_log(|| format!("  {} = {}", kv.key, format_any_value(&kv.value)));
                    }

                    if let Some(event) = parse_span_event(span_event) {
                        let mut context = resource_context.clone();
                        merge_context_from_attributes(&mut context, &span_event.attributes);
                        events.push((event, context));
                    }
                }
            }
        }
    }

    events
}

/// Parse a span event into a Codex event
fn parse_span_event(event: &SpanEvent) -> Option<CodexOtelEvent> {
    // The event.name attribute contains the Codex event type
    let event_name = get_string_attribute(&event.attributes, "event.name")?;

    // Extract conversation.id from attributes
    let conversation_id = get_string_attribute(&event.attributes, "conversation.id")
        .or_else(|| get_string_attribute(&event.attributes, "conversation_id"))
        .unwrap_or_default();

    // Skip events without conversation.id (internal tracing noise)
    if conversation_id.is_empty() {
        return None;
    }

    match event_name.as_str() {
        "codex.conversation_starts" => Some(CodexOtelEvent::ConversationStarts {
            conversation_id,
        }),
        "codex.user_prompt" => {
            let prompt = get_string_attribute(&event.attributes, "prompt")
                .or_else(|| get_string_attribute(&event.attributes, "content"));
            Some(CodexOtelEvent::UserPrompt {
                conversation_id,
                prompt,
            })
        }
        "codex.tool_result" => {
            let tool_name = get_string_attribute(&event.attributes, "tool_name")
                .or_else(|| get_string_attribute(&event.attributes, "name"));
            let arguments = get_string_attribute(&event.attributes, "arguments");
            Some(CodexOtelEvent::ToolResult {
                conversation_id,
                tool_name,
                arguments,
            })
        }
        // Deferred events: acknowledged but not mapped
        "codex.api_request" | "codex.sse_event" | "codex.tool_decision" => {
            Some(CodexOtelEvent::Unknown {
                event_name: event_name.clone(),
            })
        }
        _ => None, // Skip unknown trace events (lots of internal tracing)
    }
}

/// Parse a single log record into a Codex event
fn parse_log_record(record: &LogRecord) -> Option<CodexOtelEvent> {
    // The event name is typically in the body or a specific attribute
    // Codex may use the log record body or event.name attribute for the event name
    let event_name = get_body_string(record)
        .or_else(|| get_string_attribute(&record.attributes, "event.name"))
        .or_else(|| get_string_attribute(&record.attributes, "name"))?;

    // Extract conversation.id from attributes
    let conversation_id = get_string_attribute(&record.attributes, "conversation.id")
        .or_else(|| get_string_attribute(&record.attributes, "conversation_id"))
        .unwrap_or_default();

    match event_name.as_str() {
        "codex.conversation_starts" => Some(CodexOtelEvent::ConversationStarts {
            conversation_id,
        }),
        "codex.user_prompt" => {
            let prompt = get_string_attribute(&record.attributes, "prompt")
                .or_else(|| get_string_attribute(&record.attributes, "content"));
            Some(CodexOtelEvent::UserPrompt {
                conversation_id,
                prompt,
            })
        }
        "codex.tool_result" => {
            let tool_name = get_string_attribute(&record.attributes, "tool_name")
                .or_else(|| get_string_attribute(&record.attributes, "name"));
            let arguments = get_string_attribute(&record.attributes, "arguments");
            Some(CodexOtelEvent::ToolResult {
                conversation_id,
                tool_name,
                arguments,
            })
        }
        // Deferred events: acknowledged but not mapped
        "codex.api_request" | "codex.sse_event" | "codex.tool_decision" => {
            Some(CodexOtelEvent::Unknown {
                event_name: event_name.clone(),
            })
        }
        _ => {
            debug_log(|| format!("Unknown Codex OTel event: {}", event_name));
            Some(CodexOtelEvent::Unknown { event_name })
        }
    }
}

/// Format an AnyValue for debug logging
fn format_any_value(value: &Option<AnyValue>) -> String {
    let Some(v) = value else {
        return "<none>".to_string();
    };
    match &v.value {
        Some(any_value::Value::StringValue(s)) => {
            // Truncate long strings
            if s.len() > 200 {
                format!("{:?}... ({} bytes)", &s[..200], s.len())
            } else {
                format!("{:?}", s)
            }
        }
        Some(any_value::Value::IntValue(i)) => format!("{}", i),
        Some(any_value::Value::DoubleValue(d)) => format!("{}", d),
        Some(any_value::Value::BoolValue(b)) => format!("{}", b),
        Some(any_value::Value::BytesValue(b)) => format!("<bytes len={}>", b.len()),
        Some(any_value::Value::ArrayValue(arr)) => {
            format!("<array len={}>", arr.values.len())
        }
        Some(any_value::Value::KvlistValue(kvs)) => {
            let items: Vec<String> = kvs.values.iter().take(10).map(|kv| {
                format!("{}={}", kv.key, format_any_value(&kv.value))
            }).collect();
            if kvs.values.len() > 10 {
                format!("{{{}, ... +{}}}", items.join(", "), kvs.values.len() - 10)
            } else {
                format!("{{{}}}", items.join(", "))
            }
        }
        None => "<empty>".to_string(),
    }
}

fn build_context_from_resource(resource: Option<&Resource>) -> CodexOtelContext {
    let mut context = CodexOtelContext::default();
    let Some(resource) = resource else {
        return context;
    };

    context.agent_version = get_string_attribute(&resource.attributes, "service.version");
    context.agent_pid = get_pid_from_attributes(&resource.attributes);
    context.cwd = extract_cwd_from_attributes(&resource.attributes);

    context
}

fn merge_context_from_attributes(context: &mut CodexOtelContext, attributes: &[KeyValue]) {
    if context.agent_version.is_none() {
        context.agent_version = get_string_attribute(attributes, "service.version");
    }
    if context.agent_pid.is_none() {
        context.agent_pid = get_pid_from_attributes(attributes);
    }
    if context.cwd.is_none() {
        context.cwd = extract_cwd_from_attributes(attributes);
    }
}

/// Extract the body of a log record as a string
fn get_body_string(record: &LogRecord) -> Option<String> {
    record.body.as_ref().and_then(|body| {
        if let Some(any_value::Value::StringValue(s)) = &body.value {
            Some(s.clone())
        } else {
            None
        }
    })
}

/// Get a string attribute value by key
fn get_string_attribute(attributes: &[KeyValue], key: &str) -> Option<String> {
    attributes.iter().find(|kv| kv.key == key).and_then(|kv| {
        kv.value.as_ref().and_then(|v| {
            if let Some(any_value::Value::StringValue(s)) = &v.value {
                Some(s.clone())
            } else {
                None
            }
        })
    })
}

fn get_u32_attribute(attributes: &[KeyValue], key: &str) -> Option<u32> {
    attributes
        .iter()
        .find(|kv| kv.key == key)
        .and_then(|kv| kv.value.as_ref())
        .and_then(|v| match &v.value {
            Some(any_value::Value::IntValue(value)) => u32::try_from(*value).ok(),
            Some(any_value::Value::StringValue(value)) => value.parse::<u32>().ok(),
            _ => None,
        })
}

fn get_pid_from_attributes(attributes: &[KeyValue]) -> Option<u32> {
    get_u32_attribute(attributes, "process.pid")
        .or_else(|| get_u32_attribute(attributes, "process_pid"))
        .or_else(|| get_u32_attribute(attributes, "pid"))
}

fn extract_cwd_from_attributes(attributes: &[KeyValue]) -> Option<PathBuf> {
    const CWD_KEYS: [&str; 9] = [
        "cwd",
        "process.cwd",
        "process.working_directory",
        "working_directory",
        "workdir",
        "workspace",
        "workspace.root",
        "project_root",
        "repo_root",
    ];

    for key in CWD_KEYS {
        if let Some(value) = get_string_attribute(attributes, key) {
            if !value.is_empty() {
                return Some(PathBuf::from(value));
            }
        }
    }

    None
}

/// Extract modified file paths from a tool_result arguments field.
///
/// Only extracts paths from tools that modify files (write, edit, patch).
/// Skips gracefully if arguments is absent or unparseable.
/// Resolves relative paths against the provided `cwd`.
pub fn extract_modified_files(tool_name: Option<&str>, arguments: Option<&str>, cwd: Option<&std::path::Path>) -> Vec<String> {
    let args = match arguments {
        Some(a) if !a.is_empty() => a,
        _ => return Vec::new(),
    };

    // Only extract from file-modifying tools
    let is_file_tool = match tool_name {
        Some(name) => {
            let lower = name.to_lowercase();
            lower.contains("write")
                || lower.contains("edit")
                || lower.contains("patch")
                || lower.contains("create")
                || lower.contains("apply")
        }
        None => true, // If tool name unknown, try to extract anyway
    };

    if !is_file_tool {
        return Vec::new();
    }

    // Try to parse arguments as JSON to extract file paths
    let mut paths = Vec::new();

    if let Ok(json) = serde_json::from_str::<serde_json::Value>(args) {
        // Common patterns for file paths in tool arguments:
        // {"file_path": "..."}, {"path": "..."}, {"filename": "..."}
        for key in &["file_path", "path", "filename", "file", "target"] {
            if let Some(serde_json::Value::String(p)) = json.get(key) {
                let resolved = resolve_path(p, cwd);
                paths.push(resolved);
            }
        }

        // Also check for array of files
        if let Some(serde_json::Value::Array(files)) = json.get("files") {
            for file in files {
                if let serde_json::Value::String(p) = file {
                    let resolved = resolve_path(p, cwd);
                    paths.push(resolved);
                }
            }
        }
    } else {
        // If not valid JSON, try treating the whole string as a file path
        // (some tools pass just a path string)
        let trimmed = args.trim().trim_matches('"');
        if !trimmed.is_empty() && !trimmed.contains(' ') && trimmed.len() < 500 {
            let resolved = resolve_path(trimmed, cwd);
            paths.push(resolved);
        }
    }

    paths
}

/// Resolve a path against cwd if it's relative
fn resolve_path(path: &str, cwd: Option<&std::path::Path>) -> String {
    let p = std::path::Path::new(path);
    if p.is_relative() {
        if let Some(cwd) = cwd {
            return cwd.join(p).to_string_lossy().to_string();
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty_payload() {
        let events = parse_otlp_logs(&[]);
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_valid_conversation_starts() {
        // Build a minimal ExportLogsServiceRequest with a conversation_starts event
        let request = ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.version".to_string(),
                        value: Some(AnyValue {
                            value: Some(any_value::Value::StringValue("1.2.3".to_string())),
                        }),
                    }],
                }),
                scope_logs: vec![ScopeLogs {
                    scope: None,
                    log_records: vec![LogRecord {
                        time_unix_nano: 1000000000,
                        observed_time_unix_nano: 0,
                        severity_number: 0,
                        severity_text: String::new(),
                        body: Some(AnyValue {
                            value: Some(any_value::Value::StringValue(
                                "codex.conversation_starts".to_string(),
                            )),
                        }),
                        attributes: vec![KeyValue {
                            key: "conversation.id".to_string(),
                            value: Some(AnyValue {
                                value: Some(any_value::Value::StringValue(
                                    "conv_test123".to_string(),
                                )),
                            }),
                        }],
                        flags: 0,
                        trace_id: Vec::new(),
                        span_id: Vec::new(),
                    }],
                }],
            }],
        };

        let encoded = request.encode_to_vec();
        let events = parse_otlp_logs(&encoded);

        assert_eq!(events.len(), 1);
        let (event, context) = &events[0];
        assert_eq!(context.agent_version.as_deref(), Some("1.2.3"));

        match event {
            CodexOtelEvent::ConversationStarts { conversation_id } => {
                assert_eq!(conversation_id, "conv_test123");
            }
            _ => panic!("Expected ConversationStarts event"),
        }
    }

    #[test]
    fn test_parse_context_pid_and_cwd_from_resource() {
        let request = ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: Some(Resource {
                    attributes: vec![
                        KeyValue {
                            key: "process.pid".to_string(),
                            value: Some(AnyValue {
                                value: Some(any_value::Value::IntValue(4242)),
                            }),
                        },
                        KeyValue {
                            key: "cwd".to_string(),
                            value: Some(AnyValue {
                                value: Some(any_value::Value::StringValue(
                                    "/tmp/test-repo".to_string(),
                                )),
                            }),
                        },
                    ],
                }),
                scope_logs: vec![ScopeLogs {
                    scope: None,
                    log_records: vec![LogRecord {
                        time_unix_nano: 1000000000,
                        observed_time_unix_nano: 0,
                        severity_number: 0,
                        severity_text: String::new(),
                        body: Some(AnyValue {
                            value: Some(any_value::Value::StringValue(
                                "codex.conversation_starts".to_string(),
                            )),
                        }),
                        attributes: vec![KeyValue {
                            key: "conversation.id".to_string(),
                            value: Some(AnyValue {
                                value: Some(any_value::Value::StringValue(
                                    "conv_context".to_string(),
                                )),
                            }),
                        }],
                        flags: 0,
                        trace_id: Vec::new(),
                        span_id: Vec::new(),
                    }],
                }],
            }],
        };

        let encoded = request.encode_to_vec();
        let events = parse_otlp_logs(&encoded);
        assert_eq!(events.len(), 1);

        let (_, context) = &events[0];
        assert_eq!(context.agent_pid, Some(4242));
        assert_eq!(
            context.cwd.as_ref().map(|p| p.to_string_lossy().to_string()),
            Some("/tmp/test-repo".to_string())
        );
    }

    #[test]
    fn test_parse_user_prompt_event() {
        let request = ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: None,
                scope_logs: vec![ScopeLogs {
                    scope: None,
                    log_records: vec![LogRecord {
                        time_unix_nano: 2000000000,
                        observed_time_unix_nano: 0,
                        severity_number: 0,
                        severity_text: String::new(),
                        body: Some(AnyValue {
                            value: Some(any_value::Value::StringValue(
                                "codex.user_prompt".to_string(),
                            )),
                        }),
                        attributes: vec![
                            KeyValue {
                                key: "conversation.id".to_string(),
                                value: Some(AnyValue {
                                    value: Some(any_value::Value::StringValue(
                                        "conv_456".to_string(),
                                    )),
                                }),
                            },
                            KeyValue {
                                key: "prompt".to_string(),
                                value: Some(AnyValue {
                                    value: Some(any_value::Value::StringValue(
                                        "Fix the login bug".to_string(),
                                    )),
                                }),
                            },
                        ],
                        flags: 0,
                        trace_id: Vec::new(),
                        span_id: Vec::new(),
                    }],
                }],
            }],
        };

        let encoded = request.encode_to_vec();
        let events = parse_otlp_logs(&encoded);

        assert_eq!(events.len(), 1);
        match &events[0].0 {
            CodexOtelEvent::UserPrompt {
                conversation_id,
                prompt,
            } => {
                assert_eq!(conversation_id, "conv_456");
                assert_eq!(prompt.as_deref(), Some("Fix the login bug"));
            }
            _ => panic!("Expected UserPrompt event"),
        }
    }

    #[test]
    fn test_parse_tool_result_event() {
        let request = ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: None,
                scope_logs: vec![ScopeLogs {
                    scope: None,
                    log_records: vec![LogRecord {
                        time_unix_nano: 3000000000,
                        observed_time_unix_nano: 0,
                        severity_number: 0,
                        severity_text: String::new(),
                        body: Some(AnyValue {
                            value: Some(any_value::Value::StringValue(
                                "codex.tool_result".to_string(),
                            )),
                        }),
                        attributes: vec![
                            KeyValue {
                                key: "conversation.id".to_string(),
                                value: Some(AnyValue {
                                    value: Some(any_value::Value::StringValue(
                                        "conv_789".to_string(),
                                    )),
                                }),
                            },
                            KeyValue {
                                key: "tool_name".to_string(),
                                value: Some(AnyValue {
                                    value: Some(any_value::Value::StringValue(
                                        "write_file".to_string(),
                                    )),
                                }),
                            },
                            KeyValue {
                                key: "arguments".to_string(),
                                value: Some(AnyValue {
                                    value: Some(any_value::Value::StringValue(
                                        r#"{"file_path": "src/main.rs"}"#.to_string(),
                                    )),
                                }),
                            },
                        ],
                        flags: 0,
                        trace_id: Vec::new(),
                        span_id: Vec::new(),
                    }],
                }],
            }],
        };

        let encoded = request.encode_to_vec();
        let events = parse_otlp_logs(&encoded);

        assert_eq!(events.len(), 1);
        match &events[0].0 {
            CodexOtelEvent::ToolResult {
                conversation_id,
                tool_name,
                arguments,
            } => {
                assert_eq!(conversation_id, "conv_789");
                assert_eq!(tool_name.as_deref(), Some("write_file"));
                assert!(arguments.is_some());
            }
            _ => panic!("Expected ToolResult event"),
        }
    }

    #[test]
    fn test_parse_multiple_events_in_batch() {
        let request = ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: None,
                scope_logs: vec![ScopeLogs {
                    scope: None,
                    log_records: vec![
                        LogRecord {
                            time_unix_nano: 1000,
                            observed_time_unix_nano: 0,
                            severity_number: 0,
                            severity_text: String::new(),
                            body: Some(AnyValue {
                                value: Some(any_value::Value::StringValue(
                                    "codex.conversation_starts".to_string(),
                                )),
                            }),
                            attributes: vec![KeyValue {
                                key: "conversation.id".to_string(),
                                value: Some(AnyValue {
                                    value: Some(any_value::Value::StringValue(
                                        "batch_conv".to_string(),
                                    )),
                                }),
                            }],
                            flags: 0,
                            trace_id: Vec::new(),
                            span_id: Vec::new(),
                        },
                        LogRecord {
                            time_unix_nano: 2000,
                            observed_time_unix_nano: 0,
                            severity_number: 0,
                            severity_text: String::new(),
                            body: Some(AnyValue {
                                value: Some(any_value::Value::StringValue(
                                    "codex.user_prompt".to_string(),
                                )),
                            }),
                            attributes: vec![KeyValue {
                                key: "conversation.id".to_string(),
                                value: Some(AnyValue {
                                    value: Some(any_value::Value::StringValue(
                                        "batch_conv".to_string(),
                                    )),
                                }),
                            }],
                            flags: 0,
                            trace_id: Vec::new(),
                            span_id: Vec::new(),
                        },
                    ],
                }],
            }],
        };

        let encoded = request.encode_to_vec();
        let events = parse_otlp_logs(&encoded);

        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0].0, CodexOtelEvent::ConversationStarts { .. }));
        assert!(matches!(&events[1].0, CodexOtelEvent::UserPrompt { .. }));
    }

    #[test]
    fn test_parse_deferred_events() {
        let request = ExportLogsServiceRequest {
            resource_logs: vec![ResourceLogs {
                resource: None,
                scope_logs: vec![ScopeLogs {
                    scope: None,
                    log_records: vec![LogRecord {
                        time_unix_nano: 0,
                        observed_time_unix_nano: 0,
                        severity_number: 0,
                        severity_text: String::new(),
                        body: Some(AnyValue {
                            value: Some(any_value::Value::StringValue(
                                "codex.api_request".to_string(),
                            )),
                        }),
                        attributes: Vec::new(),
                        flags: 0,
                        trace_id: Vec::new(),
                        span_id: Vec::new(),
                    }],
                }],
            }],
        };

        let encoded = request.encode_to_vec();
        let events = parse_otlp_logs(&encoded);

        assert_eq!(events.len(), 1);
        match &events[0].0 {
            CodexOtelEvent::Unknown { event_name } => {
                assert_eq!(event_name, "codex.api_request");
            }
            _ => panic!("Expected Unknown event for deferred event type"),
        }
    }

    #[test]
    fn test_extract_modified_files_json() {
        let files = extract_modified_files(
            Some("write_file"),
            Some(r#"{"file_path": "src/main.rs"}"#),
            None,
        );
        assert_eq!(files, vec!["src/main.rs"]);
    }

    #[test]
    fn test_extract_modified_files_relative_path() {
        let cwd = std::path::Path::new("/home/user/project");
        let files = extract_modified_files(
            Some("edit"),
            Some(r#"{"file_path": "src/lib.rs"}"#),
            Some(cwd),
        );
        assert_eq!(files, vec!["/home/user/project/src/lib.rs"]);
    }

    #[test]
    fn test_extract_modified_files_absolute_path() {
        let cwd = std::path::Path::new("/home/user/project");
        let files = extract_modified_files(
            Some("write"),
            Some(r#"{"file_path": "/tmp/output.txt"}"#),
            Some(cwd),
        );
        assert_eq!(files, vec!["/tmp/output.txt"]);
    }

    #[test]
    fn test_extract_modified_files_non_file_tool() {
        let files = extract_modified_files(
            Some("web_search"),
            Some(r#"{"query": "rust async"}"#),
            None,
        );
        assert!(files.is_empty());
    }

    #[test]
    fn test_extract_modified_files_no_arguments() {
        let files = extract_modified_files(Some("write"), None, None);
        assert!(files.is_empty());

        let files = extract_modified_files(Some("write"), Some(""), None);
        assert!(files.is_empty());
    }

    #[test]
    fn test_extract_modified_files_invalid_json() {
        // Plain path string (not JSON)
        let files = extract_modified_files(Some("write"), Some("src/foo.rs"), None);
        assert_eq!(files, vec!["src/foo.rs"]);
    }

    #[test]
    fn test_malformed_protobuf() {
        let events = parse_otlp_logs(b"not a valid protobuf");
        assert!(events.is_empty());
    }

    // ========================================================================
    // Trace parsing tests
    // ========================================================================

    #[test]
    fn test_parse_traces_empty_payload() {
        let events = parse_otlp_traces(&[]);
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_traces_user_prompt() {
        // Build a trace request with a user_prompt event in span events
        let request = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.version".to_string(),
                        value: Some(AnyValue {
                            value: Some(any_value::Value::StringValue("0.89.0".to_string())),
                        }),
                    }],
                }),
                scope_spans: vec![ScopeSpans {
                    scope: None,
                    spans: vec![Span {
                        name: "handle_responses".to_string(),
                        attributes: Vec::new(),
                        events: vec![SpanEvent {
                            name: "event otel/src/traces/otel_manager.rs:362".to_string(),
                            attributes: vec![
                                KeyValue {
                                    key: "event.name".to_string(),
                                    value: Some(AnyValue {
                                        value: Some(any_value::Value::StringValue(
                                            "codex.user_prompt".to_string(),
                                        )),
                                    }),
                                },
                                KeyValue {
                                    key: "conversation.id".to_string(),
                                    value: Some(AnyValue {
                                        value: Some(any_value::Value::StringValue(
                                            "019bf548-9109-7f52-bce2-b66bb20c68dd".to_string(),
                                        )),
                                    }),
                                },
                                KeyValue {
                                    key: "prompt".to_string(),
                                    value: Some(AnyValue {
                                        value: Some(any_value::Value::StringValue(
                                            "Fix the login bug".to_string(),
                                        )),
                                    }),
                                },
                            ],
                        }],
                    }],
                }],
            }],
        };

        let encoded = request.encode_to_vec();
        let events = parse_otlp_traces(&encoded);

        assert_eq!(events.len(), 1);
        let (event, context) = &events[0];
        assert_eq!(context.agent_version.as_deref(), Some("0.89.0"));

        match event {
            CodexOtelEvent::UserPrompt {
                conversation_id,
                prompt,
            } => {
                assert_eq!(conversation_id, "019bf548-9109-7f52-bce2-b66bb20c68dd");
                assert_eq!(prompt.as_deref(), Some("Fix the login bug"));
            }
            _ => panic!("Expected UserPrompt event"),
        }
    }

    #[test]
    fn test_parse_traces_tool_decision() {
        // Tool decision events should be parsed as Unknown (deferred)
        let request = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: None,
                scope_spans: vec![ScopeSpans {
                    scope: None,
                    spans: vec![Span {
                        name: "dispatch_tool_call".to_string(),
                        attributes: Vec::new(),
                        events: vec![SpanEvent {
                            name: "event".to_string(),
                            attributes: vec![
                                KeyValue {
                                    key: "event.name".to_string(),
                                    value: Some(AnyValue {
                                        value: Some(any_value::Value::StringValue(
                                            "codex.tool_decision".to_string(),
                                        )),
                                    }),
                                },
                                KeyValue {
                                    key: "conversation.id".to_string(),
                                    value: Some(AnyValue {
                                        value: Some(any_value::Value::StringValue(
                                            "conv-123".to_string(),
                                        )),
                                    }),
                                },
                            ],
                        }],
                    }],
                }],
            }],
        };

        let encoded = request.encode_to_vec();
        let events = parse_otlp_traces(&encoded);

        assert_eq!(events.len(), 1);
        match &events[0].0 {
            CodexOtelEvent::Unknown { event_name } => {
                assert_eq!(event_name, "codex.tool_decision");
            }
            _ => panic!("Expected Unknown event for tool_decision"),
        }
    }

    #[test]
    fn test_parse_traces_skips_events_without_conversation_id() {
        // Internal tracing events without conversation.id should be skipped
        let request = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: None,
                scope_spans: vec![ScopeSpans {
                    scope: None,
                    spans: vec![Span {
                        name: "internal_span".to_string(),
                        attributes: Vec::new(),
                        events: vec![SpanEvent {
                            name: "some internal event".to_string(),
                            attributes: vec![KeyValue {
                                key: "event.name".to_string(),
                                value: Some(AnyValue {
                                    value: Some(any_value::Value::StringValue(
                                        "codex.user_prompt".to_string(),
                                    )),
                                }),
                            }],
                            // Note: no conversation.id attribute
                        }],
                    }],
                }],
            }],
        };

        let encoded = request.encode_to_vec();
        let events = parse_otlp_traces(&encoded);

        assert!(events.is_empty(), "Should skip events without conversation.id");
    }

    #[test]
    fn test_parse_traces_malformed() {
        let events = parse_otlp_traces(b"not a valid protobuf");
        assert!(events.is_empty());
    }
}
