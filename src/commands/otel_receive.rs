use crate::cache::debug_log;
use crate::editors::codex::otel::{self, CodexOtelContext, CodexOtelEvent, ToolKind};
use crate::error::Result;
use crate::event_bus;
use crate::events::{
    AikiChangeCompletedPayload, AikiChangePermissionAskedPayload, AikiEvent, ChangeOperation,
    DeleteOperation, MoveOperation, WriteOperation,
};
use crate::provenance::record::{AgentType, DetectionMethod};
use crate::session::{AikiSession, AikiSessionFile, SessionMode};
use chrono::Utc;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Run the OTel receiver: read HTTP from stdin, parse OTLP, update session state.
///
/// Socket-activated: reads a single HTTP request, processes it, responds 200 OK, exits.
/// All errors are non-fatal (always responds 200 OK to never block Codex).
pub fn run(_agent: String) -> Result<()> {
    // Read HTTP request from stdin
    let request = match read_http_request() {
        Ok(r) => r,
        Err(e) => {
            debug_log(|| format!("Failed to read HTTP request: {}", e));
            write_http_response();
            return Ok(());
        }
    };

    // Validate endpoint and content type
    if request.path.contains("/v1/traces") {
        debug_log(|| {
            "Received traces request at /v1/traces - only /v1/logs is supported. \
             Update Codex config: endpoint = \"http://127.0.0.1:19876/v1/logs\""
                .to_string()
        });
        write_http_response();
        return Ok(());
    }

    if let Some(ref ct) = request.content_type {
        let ct_lower = ct.to_lowercase();
        if ct_lower.contains("application/json") {
            debug_log(|| {
                format!(
                    "OTLP JSON payload detected (Content-Type: {}). Protobuf parsing may fail.",
                    ct
                )
            });
        } else if !ct_lower.contains("application/x-protobuf")
            && !ct_lower.contains("application/protobuf")
        {
            debug_log(|| {
                format!(
                    "Unexpected Content-Type: {}. Expected application/x-protobuf.",
                    ct
                )
            });
            // Still try to parse - some clients may not set content-type correctly
        }
    }

    // Decompress if gzip (or if payload starts with gzip magic bytes)
    let body_is_gzip = body_looks_gzipped(&request.body);
    let wants_gzip = request.content_encoding.iter().any(|enc| enc == "gzip");
    if body_is_gzip && !wants_gzip {
        debug_log(|| "Body looks gzipped but Content-Encoding is missing gzip".to_string());
    }

    let body = if wants_gzip || body_is_gzip {
        match decompress_gzip(&request.body) {
            Ok(decompressed) => decompressed,
            Err(e) => {
                debug_log(|| format!("Failed to decompress gzip body: {}", e));
                write_http_response();
                return Ok(());
            }
        }
    } else {
        request.body
    };

    // Parse OTLP protobuf and process events
    // Try traces first (Codex sends traces even to /v1/logs endpoint)
    let mut events = otel::parse_otlp_traces(&body);

    // Fall back to logs parsing if no trace events found
    if events.is_empty() {
        events = otel::parse_otlp_logs(&body);
    }

    if events.is_empty() && !body.is_empty() {
        debug_log(|| {
            format!(
                "No OTel events parsed (content_type={:?}, body_len={})",
                request.content_type,
                body.len()
            )
        });
        dump_otlp_payload(&body, request.content_type.as_deref(), &request.path);
    }

    for (event, context) in events {
        process_event(event, &context);
    }

    // Always respond 200 OK
    write_http_response();
    Ok(())
}

/// Parsed HTTP request from stdin
struct HttpRequest {
    body: Vec<u8>,
    content_encoding: Vec<String>,
    /// Request path (e.g., "/v1/logs" or "/v1/traces")
    path: String,
    /// Content-Type header value
    content_type: Option<String>,
}

/// Read an HTTP request from stdin.
///
/// Parses the HTTP/1.1 request line and headers to find Content-Length
/// and Content-Encoding. Then reads the body (Content-Length or chunked).
fn read_http_request() -> io::Result<HttpRequest> {
    let mut stdin = io::stdin().lock();
    let mut header_buf = Vec::with_capacity(4096);

    // Read headers byte by byte until we find \r\n\r\n
    let mut last_four = [0u8; 4];
    loop {
        let mut byte = [0u8; 1];
        stdin.read_exact(&mut byte)?;
        header_buf.push(byte[0]);

        // Shift last_four window
        last_four[0] = last_four[1];
        last_four[1] = last_four[2];
        last_four[2] = last_four[3];
        last_four[3] = byte[0];

        if &last_four == b"\r\n\r\n" {
            break;
        }

        // Safety: prevent unbounded header reads
        if header_buf.len() > 65536 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP headers too large",
            ));
        }
    }

    // Parse headers
    let headers_str = String::from_utf8_lossy(&header_buf);
    let mut content_length: Option<usize> = None;
    let mut content_encoding = Vec::new();
    let mut transfer_encoding = Vec::new();
    let mut method = String::new();
    let mut path = String::new();
    let mut content_type = None;

    for (i, line) in headers_str.lines().enumerate() {
        if i == 0 {
            // Parse request line: "POST /v1/logs HTTP/1.1"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                method = parts[0].to_string();
                path = parts[1].to_string();
            }
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_lowercase();
        let value = value.trim();

        match name.as_str() {
            "content-length" => {
                content_length = value.parse().ok();
            }
            "content-encoding" => {
                content_encoding.extend(parse_header_list(value));
            }
            "transfer-encoding" => {
                transfer_encoding.extend(parse_header_list(value));
            }
            "content-type" => {
                content_type = Some(value.to_string());
            }
            _ => {}
        }
    }

    let is_chunked = transfer_encoding.iter().any(|enc| enc == "chunked");

    // Read body
    let body = if is_chunked {
        read_chunked_body(&mut stdin)?
    } else {
        let len = content_length.unwrap_or(0);
        let mut body = vec![0u8; len];
        if len > 0 {
            stdin.read_exact(&mut body)?;
        }
        body
    };

    debug_log(|| {
        format!(
            "OTel HTTP: method={}, path={}, content_length={:?}, transfer_encoding={:?}, content_encoding={:?}, content_type={:?}, body_len={}",
            if method.is_empty() { "-" } else { method.as_str() },
            if path.is_empty() { "-" } else { path.as_str() },
            content_length,
            transfer_encoding,
            content_encoding,
            content_type,
            body.len()
        )
    });

    Ok(HttpRequest {
        body,
        content_encoding,
        path,
        content_type,
    })
}

fn parse_header_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|v| v.trim().to_lowercase())
        .filter(|v| !v.is_empty())
        .collect()
}

fn read_crlf_line(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut last_two = [0u8; 2];

    loop {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte)?;
        buf.push(byte[0]);

        last_two[0] = last_two[1];
        last_two[1] = byte[0];

        if &last_two == b"\r\n" {
            buf.truncate(buf.len() - 2);
            return Ok(buf);
        }

        if buf.len() > 65536 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP line too long",
            ));
        }
    }
}

fn read_chunked_body(reader: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();

    loop {
        let line = read_crlf_line(reader)?;
        let line_str = String::from_utf8_lossy(&line);
        let size_str = line_str.split(';').next().unwrap_or("").trim();

        if size_str.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Missing chunk size",
            ));
        }

        let size = usize::from_str_radix(size_str, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid chunk size"))?;

        if size == 0 {
            // Consume trailer headers (if any), then final CRLF
            loop {
                let trailer = read_crlf_line(reader)?;
                if trailer.is_empty() {
                    break;
                }
            }
            break;
        }

        let mut chunk = vec![0u8; size];
        reader.read_exact(&mut chunk)?;
        body.extend_from_slice(&chunk);

        let mut crlf = [0u8; 2];
        reader.read_exact(&mut crlf)?;
        if &crlf != b"\r\n" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Invalid chunk terminator",
            ));
        }
    }

    Ok(body)
}

#[inline]
fn body_looks_gzipped(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b
}

/// Decompress gzip-encoded body
fn decompress_gzip(data: &[u8]) -> io::Result<Vec<u8>> {
    use flate2::read::GzDecoder;
    let mut decoder = GzDecoder::new(data);
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed)?;
    Ok(decompressed)
}

/// Write HTTP 200 OK response to stdout
fn write_http_response() {
    let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
    let _ = io::stdout().write_all(response.as_bytes());
    let _ = io::stdout().flush();
}

/// Dump raw OTLP payload to a file (always on parse failure).
fn dump_otlp_payload(body: &[u8], content_type: Option<&str>, request_path: &str) {
    let dump_dir = std::path::PathBuf::from("/tmp");
    if let Err(e) = std::fs::create_dir_all(&dump_dir) {
        debug_log(|| format!("Failed to create OTel dump dir: {}", e));
        return;
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let file_name = format!(
        "aiki-otel-dump-{}-{}-{}.bin",
        ts.as_secs(),
        ts.subsec_millis(),
        std::process::id()
    );
    let bin_path = dump_dir.join(file_name);

    if let Err(e) = std::fs::write(&bin_path, body) {
        debug_log(|| format!("Failed to write OTel dump: {}", e));
        return;
    }

    let meta = format!(
        "request_path={}\ncontent_type={}\nbody_len={}\n",
        request_path,
        content_type.unwrap_or("-"),
        body.len()
    );
    let meta_path = bin_path.with_extension("bin.meta");
    if let Err(e) = std::fs::write(&meta_path, meta) {
        debug_log(|| format!("Failed to write OTel dump metadata: {}", e));
    }

    debug_log(|| format!("Wrote OTel payload dump to {}", bin_path.display()));
}

/// Get the PID of the process that connected to our socket.
///
/// In inetd-compatibility mode (launchd/systemd socket activation), stdin (fd 0)
/// IS the accepted socket. The socket is TCP (127.0.0.1), so LOCAL_PEERPID (Unix
/// domain only) does not work. Instead we:
/// 1. Call getpeername(0) to learn the peer's ephemeral port
/// 2. Use `lsof` to find which process owns that port
///
/// On Linux, SO_PEERCRED works for Unix domain sockets. For TCP, we fall back
/// to the same lsof approach.
#[allow(dead_code)]
fn get_socket_peer_pid() -> Option<u32> {
    // Try Unix domain socket methods first (cheap, no subprocess)
    if let Some(pid) = get_unix_socket_peer_pid() {
        return Some(pid);
    }

    // Fall back to TCP: getpeername + lsof
    get_tcp_peer_pid()
}

/// Try LOCAL_PEERPID (macOS) or SO_PEERCRED (Linux) on fd 0.
/// Only works if the socket is AF_UNIX.
#[allow(dead_code)]
fn get_unix_socket_peer_pid() -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        const SOL_LOCAL: libc::c_int = 0;
        const LOCAL_PEERPID: libc::c_int = 2;
        let mut pid: libc::pid_t = 0;
        let mut len: libc::socklen_t = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
        let ret = unsafe {
            libc::getsockopt(
                0,
                SOL_LOCAL,
                LOCAL_PEERPID,
                &mut pid as *mut libc::pid_t as *mut libc::c_void,
                &mut len,
            )
        };
        if ret == 0 && pid > 0 {
            debug_log(|| format!("OTel: LOCAL_PEERPID = {}", pid));
            return Some(pid as u32);
        }
    }

    #[cfg(target_os = "linux")]
    {
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut len: libc::socklen_t = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let ret = unsafe {
            libc::getsockopt(
                0,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut cred as *mut libc::ucred as *mut libc::c_void,
                &mut len,
            )
        };
        if ret == 0 && cred.pid > 0 {
            debug_log(|| format!("OTel: SO_PEERCRED = {}", cred.pid));
            return Some(cred.pid as u32);
        }
    }

    None
}

/// Get the peer PID for a TCP loopback connection on fd 0.
///
/// Calls getpeername(0) to get the peer's ephemeral port, then runs
/// `lsof -i TCP@127.0.0.1:{port} -sTCP:ESTABLISHED -t` to resolve the PID.
#[allow(dead_code)]
fn get_tcp_peer_pid() -> Option<u32> {
    // getpeername on fd 0 to learn the peer's address:port
    let mut addr: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    let mut len: libc::socklen_t = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    let ret = unsafe {
        libc::getpeername(
            0,
            &mut addr as *mut libc::sockaddr_in as *mut libc::sockaddr,
            &mut len,
        )
    };
    if ret != 0 {
        debug_log(|| format!("OTel: getpeername failed: {}", io::Error::last_os_error()));
        return None;
    }

    let peer_port = u16::from_be(addr.sin_port);
    if peer_port == 0 {
        debug_log(|| "OTel: getpeername returned port 0".to_string());
        return None;
    }

    debug_log(|| format!("OTel: TCP peer port = {}", peer_port));

    // Use lsof to find which process owns this ephemeral port
    let output = match std::process::Command::new("lsof")
        .args([
            "-i",
            &format!("TCP@127.0.0.1:{}", peer_port),
            "-sTCP:ESTABLISHED",
            "-t",
        ])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            debug_log(|| format!("OTel: lsof failed: {}", e));
            return None;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    // lsof -t outputs one PID per line; pick the first that isn't us
    let our_pid = std::process::id();
    for line in stdout.lines() {
        if let Ok(pid) = line.trim().parse::<u32>() {
            if pid != our_pid && pid > 0 {
                debug_log(|| format!("OTel: socket peer PID = {} (via lsof)", pid));
                return Some(pid);
            }
        }
    }

    debug_log(|| format!("OTel: lsof found no peer PID for port {}", peer_port));
    None
}

/// Info resolved from the socket peer's process tree.
#[allow(dead_code)]
struct SocketPeerInfo {
    /// The PID of the codex process found in the ancestor chain.
    codex_pid: u32,
    /// The cwd of the codex process (if available from sysinfo).
    cwd: Option<PathBuf>,
}

/// Resolve the Codex process PID and cwd from the socket peer.
///
/// Gets the peer PID from the socket, then walks up its process tree to find
/// the actual "codex" process. This is needed because the OTel exporter may
/// be a child thread/process of codex, not codex itself.
///
/// Also captures the cwd of the codex process via sysinfo, which avoids
/// the race condition of reading the `.jsonl` session file.
#[allow(dead_code)]
fn resolve_codex_info_from_socket() -> Option<SocketPeerInfo> {
    let peer_pid = get_socket_peer_pid()?;

    // Walk up from the peer PID to find "codex" in ancestors
    // We need to start from the peer, not from ourselves (the OTel receiver is
    // not a child of codex — it's spawned by launchd/systemd).
    use sysinfo::{ProcessesToUpdate, System};

    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::All, true);

    let mut pid = sysinfo::Pid::from_u32(peer_pid);

    // Check the peer process itself first
    if let Some(process) = system.process(pid) {
        let name = process.name().to_string_lossy().to_lowercase();
        if name.contains("codex") {
            let cwd = process.cwd().map(|p| p.to_path_buf());
            debug_log(|| format!("OTel: peer PID {} is codex, cwd={:?}", peer_pid, cwd));
            return Some(SocketPeerInfo {
                codex_pid: peer_pid,
                cwd,
            });
        }
    }

    // Walk up ancestors
    loop {
        let Some(process) = system.process(pid) else {
            break;
        };
        let Some(parent_pid) = process.parent() else {
            break;
        };
        if parent_pid == pid {
            break;
        }

        if let Some(parent_process) = system.process(parent_pid) {
            let name = parent_process.name().to_string_lossy().to_lowercase();
            if name.contains("codex") {
                let cwd = parent_process.cwd().map(|p| p.to_path_buf());
                debug_log(|| {
                    format!(
                        "OTel: found codex ancestor at PID {} (peer was {}), cwd={:?}",
                        parent_pid.as_u32(),
                        peer_pid,
                        cwd
                    )
                });
                return Some(SocketPeerInfo {
                    codex_pid: parent_pid.as_u32(),
                    cwd,
                });
            }
        }

        pid = parent_pid;
    }

    debug_log(|| format!("OTel: no codex ancestor found for peer PID {}", peer_pid));
    None
}

/// Process a single Codex OTel event
///
/// Turn tracking uses JJ history (same as stdin integrations).
/// Modified files from tool_result are ignored - they come from JJ file tracking.
fn process_event(event: CodexOtelEvent, context: &CodexOtelContext) {
    match event {
        CodexOtelEvent::ConversationStarts { conversation_id } => {
            debug_log(|| {
                format!(
                    "OTel: conversation_starts ignored (native SessionStart is authoritative): {}",
                    conversation_id
                )
            });
        }

        CodexOtelEvent::UserPrompt {
            conversation_id,
            prompt,
        } => {
            debug_log(|| {
                format!(
                    "OTel: user_prompt ignored (native UserPromptSubmit is authoritative): conv={}, prompt_len={}",
                    conversation_id,
                    prompt.as_ref().map_or(0, |p| p.len())
                )
            });
        }

        CodexOtelEvent::ToolResult {
            conversation_id,
            tool_name,
            arguments,
        } => {
            let kind = tool_name
                .as_deref()
                .map(otel::classify_tool)
                .unwrap_or(ToolKind::Other);

            debug_log(|| {
                format!(
                    "OTel: tool_result: conv={} tool={:?} kind={:?}",
                    conversation_id, tool_name, kind
                )
            });

            match kind {
                ToolKind::Write => {
                    maybe_emit_change_completed(
                        &conversation_id,
                        context,
                        &tool_name,
                        &arguments,
                    );
                }
                ToolKind::Read | ToolKind::Shell | ToolKind::Other => {
                    debug_log(|| {
                        format!(
                            "OTel: tool_result {:?} ({:?}) — no provenance needed",
                            tool_name, kind
                        )
                    });
                }
            }
        }

        CodexOtelEvent::ToolDecision {
            conversation_id,
            tool_name,
            arguments,
            decision,
        } => {
            let kind = tool_name
                .as_deref()
                .map(otel::classify_tool)
                .unwrap_or(ToolKind::Other);

            debug_log(|| {
                format!(
                    "OTel: tool_decision: conv={} tool={:?} kind={:?} decision={:?}",
                    conversation_id, tool_name, kind, decision
                )
            });

            match kind {
                ToolKind::Write => {
                    maybe_emit_change_permission_asked(
                        &conversation_id,
                        context,
                        &tool_name,
                        &arguments,
                        &decision,
                    );
                }
                ToolKind::Read | ToolKind::Shell | ToolKind::Other => {
                    debug_log(|| {
                        format!(
                            "OTel: tool_decision {:?} ({:?}) ignored to avoid duplicate permission events",
                            tool_name, kind
                        )
                    });
                }
            }
        }

        CodexOtelEvent::Unknown { event_name } => {
            debug_log(|| format!("OTel: acknowledged (not mapped): {}", event_name));
        }
    }
}

fn maybe_emit_change_permission_asked(
    conversation_id: &str,
    context: &CodexOtelContext,
    tool_name: &Option<String>,
    arguments: &Option<String>,
    decision: &Option<String>,
) {
    if !decision
        .as_deref()
        .map(|d| d.eq_ignore_ascii_case("approved"))
        .unwrap_or(false)
    {
        debug_log(|| {
            format!(
                "OTel: apply_patch pre-change skipped for {} (decision={:?})",
                conversation_id, decision
            )
        });
        return;
    }

    let Some(arguments) = arguments else {
        debug_log(|| {
            format!(
                "OTel: apply_patch pre-change skipped for {} (no arguments)",
                conversation_id
            )
        });
        return;
    };

    let Some(patch_content) = otel::extract_patch_content(arguments) else {
        debug_log(|| {
            format!(
                "OTel: apply_patch pre-change skipped for {} (no patch content)",
                conversation_id
            )
        });
        return;
    };

    if patch_content.is_empty() {
        debug_log(|| {
            format!(
                "OTel: apply_patch pre-change skipped for {} (empty patch content)",
                conversation_id
            )
        });
        return;
    }

    let operations = otel::parse_apply_patch(&patch_content);
    if operations.is_empty() {
        debug_log(|| {
            format!(
                "OTel: apply_patch pre-change skipped for {} (no operations parsed)",
                conversation_id
            )
        });
        return;
    }

    let cwd = match context.cwd.clone() {
        Some(c) => c,
        None => match lookup_cwd_from_codex_session(conversation_id) {
            Some(c) => c,
            None => {
                debug_log(|| {
                    format!(
                        "OTel: apply_patch pre-change skipped for {} (no cwd)",
                        conversation_id
                    )
                });
                return;
            }
        },
    };

    let session = AikiSession::new(
        AgentType::Codex,
        conversation_id,
        context.agent_version.as_deref(),
        DetectionMethod::Hook,
        SessionMode::Interactive,
    )
    .with_parent_pid(context.agent_pid);

    let session_file = AikiSessionFile::new(&session);
    if !session_file.exists() {
        debug_log(|| {
            format!(
                "OTel: apply_patch pre-change skipped for {} (no session file)",
                conversation_id
            )
        });
        return;
    }

    let tool = tool_name
        .clone()
        .unwrap_or_else(|| "apply_patch".to_string());
    let now = Utc::now();

    for (op_type, file_info) in &operations {
        let Some(operation) = build_change_operation_from_parsed_patch(op_type, file_info) else {
            continue;
        };

        debug_log(|| {
            format!(
                "OTel: dispatching change.permission_asked for {} ({}: {})",
                conversation_id, op_type, file_info
            )
        });

        let event = AikiEvent::ChangePermissionAsked(AikiChangePermissionAskedPayload {
            session: session.clone(),
            cwd: cwd.clone(),
            timestamp: now,
            tool_name: tool.clone(),
            operation,
        });

        if let Err(e) = event_bus::dispatch(event) {
            debug_log(|| {
                format!(
                    "Failed to dispatch change.permission_asked for {}: {}",
                    file_info, e
                )
            });
        }
    }
}

/// Emit change.completed events for Codex apply_patch tool results.
///
/// Parses the patch content from tool arguments, extracts file operations,
/// and dispatches change.completed events so the hooks flow can write provenance
/// metadata (including task= IDs) to JJ change descriptions.
fn maybe_emit_change_completed(
    conversation_id: &str,
    context: &CodexOtelContext,
    tool_name: &Option<String>,
    arguments: &Option<String>,
) {
    let patch_content = match arguments {
        Some(args) => match otel::extract_patch_content(args) {
            Some(content) => content,
            None => {
                debug_log(|| "OTel: apply_patch skipped (no patch content in args)".to_string());
                return;
            }
        },
        None => {
            debug_log(|| "OTel: apply_patch skipped (no arguments)".to_string());
            return;
        }
    };

    if patch_content.is_empty() {
        debug_log(|| "OTel: apply_patch skipped (empty patch content)".to_string());
        return;
    }

    let operations = otel::parse_apply_patch(&patch_content);
    if operations.is_empty() {
        debug_log(|| "OTel: apply_patch skipped (no operations parsed)".to_string());
        return;
    }

    let cwd = match context.cwd.clone() {
        Some(c) => c,
        None => match lookup_cwd_from_codex_session(conversation_id) {
            Some(c) => c,
            None => {
                debug_log(|| {
                    format!(
                        "OTel: apply_patch skipped for {} (no cwd)",
                        conversation_id
                    )
                });
                return;
            }
        },
    };

    let session = AikiSession::new(
        AgentType::Codex,
        conversation_id,
        context.agent_version.as_deref(),
        DetectionMethod::Hook,
        SessionMode::Interactive,
    )
    .with_parent_pid(context.agent_pid);

    let session_file = AikiSessionFile::new(&session);
    if !session_file.exists() {
        debug_log(|| {
            format!(
                "OTel: apply_patch skipped for {} (no session file)",
                conversation_id
            )
        });
        return;
    }

    let tool = tool_name
        .clone()
        .unwrap_or_else(|| "apply_patch".to_string());
    let now = Utc::now();

    for (op_type, file_info) in &operations {
        let Some(operation) = build_change_operation_from_parsed_patch(op_type, file_info) else {
            continue;
        };

        debug_log(|| {
            format!(
                "OTel: dispatching change.completed for {} ({}: {})",
                conversation_id, op_type, file_info
            )
        });

        let event = AikiEvent::ChangeCompleted(AikiChangeCompletedPayload {
            session: session.clone(),
            cwd: cwd.clone(),
            timestamp: now,
            tool_name: tool.clone(),
            success: true,
            turn: crate::events::Turn::unknown(),
            operation,
        });

        if let Err(e) = event_bus::dispatch(event) {
            debug_log(|| {
                format!(
                    "Failed to dispatch change.completed for {}: {}",
                    file_info, e
                )
            });
        }
    }
}

fn build_change_operation_from_parsed_patch(
    op_type: &str,
    file_info: &str,
) -> Option<ChangeOperation> {
    match op_type {
        "write" => Some(ChangeOperation::Write(WriteOperation {
            file_paths: vec![file_info.to_string()],
            edit_details: vec![],
        })),
        "delete" => Some(ChangeOperation::Delete(DeleteOperation {
            file_paths: vec![file_info.to_string()],
        })),
        "move" => {
            let parts: Vec<&str> = file_info.splitn(2, ':').collect();
            if parts.len() == 2 {
                Some(ChangeOperation::Move(MoveOperation::from_move_paths(vec![
                    parts[0].to_string(),
                    parts[1].to_string(),
                ])))
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_change_operation_from_patch_write() {
        let op = build_change_operation_from_parsed_patch("write", "src/main.rs");
        assert!(matches!(
            op,
            Some(ChangeOperation::Write(WriteOperation { file_paths, .. }))
            if file_paths == vec!["src/main.rs".to_string()]
        ));
    }

    #[test]
    fn build_change_operation_from_patch_move() {
        let op = build_change_operation_from_parsed_patch("move", "old.rs:new.rs");
        assert!(matches!(
            op,
            Some(ChangeOperation::Move(MoveOperation { source_paths, destination_paths, .. }))
            if source_paths == vec!["old.rs".to_string()]
                && destination_paths == vec!["new.rs".to_string()]
        ));
    }

    #[test]
    fn build_change_operation_from_patch_invalid_move() {
        let op = build_change_operation_from_parsed_patch("move", "old.rs");
        assert!(op.is_none());
    }
}

/// Look up the working directory from Codex's own session file.
///
/// Codex writes session files to `~/.codex/sessions/{YYYY}/{MM}/{DD}/rollout-{date}-{conv_id}.jsonl`.
/// The first line is a `session_meta` JSON object containing `"cwd"`.
fn lookup_cwd_from_codex_session(conversation_id: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let sessions_dir = home.join(".codex").join("sessions");

    if !sessions_dir.is_dir() {
        debug_log(|| "OTel: ~/.codex/sessions/ not found for cwd fallback".to_string());
        return None;
    }

    // The file name ends with `-{conversation_id}.jsonl`.
    // Walk today's date directory first, then search more broadly.
    //
    // Race condition: Codex writes this file at roughly the same time as
    // it fires the conversation_starts OTel event. Poll briefly if not found.
    let suffix = format!("-{}.jsonl", conversation_id);

    let session_file = match find_file_with_suffix_retry(&sessions_dir, &suffix) {
        Some(f) => f,
        None => {
            debug_log(|| {
                format!(
                    "OTel: no Codex session file found for conv {} (after retries)",
                    conversation_id
                )
            });
            return None;
        }
    };

    // Read only the first line (session_meta)
    let first_line = match std::fs::read_to_string(&session_file) {
        Ok(content) => match content.lines().next() {
            Some(line) => line.to_string(),
            None => return None,
        },
        Err(e) => {
            debug_log(|| format!("OTel: failed to read Codex session file: {}", e));
            return None;
        }
    };

    // Parse JSON and extract cwd from payload
    let json: serde_json::Value = match serde_json::from_str(&first_line) {
        Ok(v) => v,
        Err(e) => {
            debug_log(|| format!("OTel: failed to parse session_meta JSON: {}", e));
            return None;
        }
    };

    let cwd = json
        .get("payload")
        .and_then(|p| p.get("cwd"))
        .and_then(|c| c.as_str())
        .map(PathBuf::from);

    if let Some(ref c) = cwd {
        debug_log(|| {
            format!(
                "OTel: resolved cwd from Codex session file: {}",
                c.display()
            )
        });
    }

    cwd
}

/// Find a file with the given suffix, retrying briefly if not found.
///
/// Codex writes its session `.jsonl` file at roughly the same time as the
/// `conversation_starts` OTel event fires. This function polls for the file
/// up to ~500ms (10 attempts, 50ms apart) to handle the race condition.
fn find_file_with_suffix_retry(dir: &std::path::Path, suffix: &str) -> Option<PathBuf> {
    // First attempt (no delay)
    if let Some(found) = find_file_with_suffix(dir, suffix) {
        return Some(found);
    }

    // Retry with short polling
    for attempt in 1..=10 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if let Some(found) = find_file_with_suffix(dir, suffix) {
            debug_log(|| {
                format!(
                    "OTel: found Codex session file on retry {} ({}ms)",
                    attempt,
                    attempt * 50
                )
            });
            return Some(found);
        }
    }

    None
}

/// Recursively find a file whose name ends with the given suffix.
///
/// Searches in reverse-sorted order (newest date directories first) to find
/// the most recent match quickly.
fn find_file_with_suffix(dir: &std::path::Path, suffix: &str) -> Option<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .collect();
    // Sort descending so newest date directories are checked first
    entries.sort_by(|a, b| b.file_name().cmp(&a.file_name()));

    for entry in entries {
        let path = entry.path();
        if path.is_file() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.ends_with(suffix) {
                    return Some(path);
                }
            }
        } else if path.is_dir() {
            if let Some(found) = find_file_with_suffix(&path, suffix) {
                return Some(found);
            }
        }
    }

    None
}
