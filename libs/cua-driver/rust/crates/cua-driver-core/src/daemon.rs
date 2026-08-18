//! Shared Cua Driver daemon wire protocol and synchronous client.
//!
//! The daemon, CLI, MCP proxy, and imported SDK all use these exact request,
//! response, probing, timeout, and framing rules. Keeping them outside the
//! binary crate prevents language bindings from growing an independent
//! implementation of the native transport.

use serde::{Deserialize, Serialize};

/// Versioned identity returned by the daemon before an imported SDK or an MCP
/// proxy is allowed to treat an endpoint as ready.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonMetadata {
    pub driver_version: String,
    pub contract_version: String,
    pub tools_list_schema_version: String,
    pub capability_version: String,
    pub mcp_protocol_version: String,
    pub pid: u32,
    pub embedded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_bundle_id: Option<String>,
}

pub fn current_daemon_metadata() -> DaemonMetadata {
    DaemonMetadata {
        driver_version: env!("CARGO_PKG_VERSION").to_owned(),
        contract_version: cua_driver_contract::CONTRACT_VERSION.to_owned(),
        tools_list_schema_version: cua_driver_contract::TOOLS_LIST_SCHEMA_VERSION.to_owned(),
        capability_version: cua_driver_contract::CAPABILITY_VERSION.to_owned(),
        mcp_protocol_version: cua_driver_contract::MCP_PROTOCOL_VERSION.to_owned(),
        pid: std::process::id(),
        embedded: crate::embedded_mode(),
        host_bundle_id: std::env::var(crate::HOST_BUNDLE_ID_ENV)
            .ok()
            .filter(|value| !value.is_empty()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolObservationOrigin {
    McpProxy,
    Direct,
}

/// Closed, content-free identity for a direct daemon client. This is carried
/// separately from `observation_origin` so older daemons safely ignore the
/// additive field instead of rejecting a newer enum variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonClientKind {
    Cli,
    PythonSdk,
    TypescriptSdk,
    Unknown,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonRequest {
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
    /// Transport-owned connection identity. Public session identity remains
    /// in `args.session`; callers must not use this field to select policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Bounded internal routing metadata for exactly-once completion
    /// observation. Older peers ignore this additive field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_origin: Option<ToolObservationOrigin>,
    /// Privacy-bounded direct-client identity used only for aggregate product
    /// telemetry. Raw package names, application names, and host identifiers
    /// are never accepted here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_kind: Option<DaemonClientKind>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DaemonResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

impl DaemonResponse {
    pub fn ok(result: serde_json::Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
            exit_code: None,
        }
    }

    pub fn err(message: impl Into<String>, exit_code: i32) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(message.into()),
            exit_code: Some(exit_code),
        }
    }
}

/// Perform the daemon identity/version handshake used by embedded startup and
/// imported SDK diagnostics. Unlike a connect-only probe, this proves the
/// endpoint speaks the expected Cua wire protocol.
pub fn request_daemon_metadata(socket_path: &str) -> anyhow::Result<DaemonMetadata> {
    let response = send_request(
        socket_path,
        &DaemonRequest {
            method: "metadata".into(),
            name: None,
            args: None,
            session_id: None,
            observation_origin: None,
            client_kind: None,
        },
    )?;
    if !response.ok {
        anyhow::bail!(
            "daemon metadata request failed: {}",
            response.error.unwrap_or_else(|| "unknown error".into())
        );
    }
    serde_json::from_value(
        response
            .result
            .ok_or_else(|| anyhow::anyhow!("daemon metadata response omitted result"))?,
    )
    .map_err(Into::into)
}

/// Return the platform socket or pipe path for an installed-product namespace.
/// The executable chooses its release/local namespace; imported SDKs use the
/// release namespace unless the caller supplies an explicit path.
pub fn socket_path_for_namespace(namespace: &str) -> String {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        format!("{home}/Library/Caches/{namespace}/{namespace}.sock")
    }
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        format!("{home}/.cache/{namespace}/{namespace}.sock")
    }
    #[cfg(target_os = "windows")]
    {
        format!(r"\\.\pipe\{namespace}")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        format!("/tmp/{namespace}.sock")
    }
}

/// Probe whether a daemon is listening on `socket_path`.
///
/// On Windows this uses `WaitNamedPipeW` without consuming a pipe instance.
/// Unix probes with the protocol's read-only `list` request.
pub fn is_daemon_listening(socket_path: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "kernel32")]
        extern "system" {
            fn WaitNamedPipeW(lp_named_pipe_name: *const u16, timeout_ms: u32) -> i32;
        }

        let wide: Vec<u16> = std::ffi::OsStr::new(socket_path)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // NMPWAIT_NOWAIT == 1.
        unsafe { WaitNamedPipeW(wide.as_ptr(), 1) != 0 }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let request = DaemonRequest {
            method: "list".into(),
            name: None,
            args: None,
            session_id: None,
            observation_origin: None,
            client_kind: None,
        };
        send_request(socket_path, &request)
            .ok()
            .is_some_and(|response| response.ok)
    }
}

/// Whether a failed connect names a transient condition worth one more
/// attempt. `ConnectionRefused` is the listener's backlog being momentarily
/// full (or the accept loop between binds); `WouldBlock` / `Interrupted` are
/// ordinary syscall interruptions. Everything else — above all `NotFound`,
/// which means there is no socket and therefore no daemon — fails immediately
/// so a stopped-daemon probe stays fast.
#[cfg(unix)]
fn connect_error_is_transient(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::Interrupted
    )
}

/// Connect to the daemon socket, retrying only transient failures within a
/// bounded window. This never re-sends a request (nothing has been written
/// yet), so it cannot make a non-idempotent tool call run twice.
#[cfg(unix)]
fn connect_with_bounded_retry(
    socket_path: &str,
) -> std::io::Result<std::os::unix::net::UnixStream> {
    use std::os::unix::net::UnixStream;
    use std::time::{Duration, Instant};

    const CONNECT_WINDOW: Duration = Duration::from_millis(1_000);
    const CONNECT_BACKOFF: Duration = Duration::from_millis(25);

    let deadline = Instant::now() + CONNECT_WINDOW;
    loop {
        match UnixStream::connect(socket_path) {
            Ok(stream) => return Ok(stream),
            Err(error) if connect_error_is_transient(error.kind()) && Instant::now() < deadline => {
                std::thread::sleep(CONNECT_BACKOFF);
            }
            Err(error) => return Err(error),
        }
    }
}

/// Send one newline-delimited request and read one response.
///
/// Unix reads and writes retry transient timeouts until a 120-second overall
/// deadline so slow accessibility walks and large screenshots are supported,
/// and the initial connect rides out a bounded window of transient refusals.
#[cfg(unix)]
pub fn send_request(socket_path: &str, request: &DaemonRequest) -> anyhow::Result<DaemonResponse> {
    use std::io::Read;
    use std::time::{Duration, Instant};

    let mut stream = connect_with_bounded_retry(socket_path)
        .map_err(|error| anyhow::anyhow!("connect to {socket_path}: {error}"))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;

    let mut writer = stream.try_clone()?;
    let line = serde_json::to_string(request)? + "\n";
    let write_deadline = Instant::now() + Duration::from_secs(120);
    crate::socket_io::write_all_with_retry(&mut writer, line.as_bytes(), write_deadline)?;

    let overall_deadline = Instant::now() + Duration::from_secs(120);
    let mut buffer = Vec::with_capacity(64 * 1024);
    let mut chunk = [0_u8; 64 * 1024];
    let response_line = loop {
        if let Some(newline) = buffer.iter().position(|&byte| byte == b'\n') {
            break String::from_utf8_lossy(&buffer[..newline]).into_owned();
        }
        match stream.read(&mut chunk) {
            Ok(0) if buffer.is_empty() => {
                anyhow::bail!("daemon closed connection without response");
            }
            Ok(0) => break String::from_utf8_lossy(&buffer).into_owned(),
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if Instant::now() >= overall_deadline {
                    anyhow::bail!(
                        "timed out after 120s waiting for daemon response (received {} bytes so far)",
                        buffer.len()
                    );
                }
            }
            Err(error) => return Err(error.into()),
        }
    };

    Ok(serde_json::from_str(&response_line)?)
}

#[cfg(not(unix))]
pub fn send_request(socket_path: &str, request: &DaemonRequest) -> anyhow::Result<DaemonResponse> {
    #[cfg(target_os = "windows")]
    {
        use std::io::{BufRead, BufReader, Write};
        use std::time::Duration;

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let pipe = loop {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(socket_path)
            {
                Ok(pipe) => break pipe,
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(error) => anyhow::bail!("connect to named pipe {socket_path}: {error}"),
            }
        };

        let mut writer = pipe.try_clone()?;
        writer.write_all((serde_json::to_string(request)? + "\n").as_bytes())?;
        writer.flush()?;

        let response_line = BufReader::new(pipe)
            .lines()
            .next()
            .ok_or_else(|| anyhow::anyhow!("daemon closed connection without response"))??;
        Ok(serde_json::from_str(&response_line)?)
    }
    #[cfg(not(target_os = "windows"))]
    {
        anyhow::bail!("daemon client not supported on this platform (socket path: {socket_path})");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        current_daemon_metadata, socket_path_for_namespace, DaemonClientKind, DaemonRequest,
        DaemonResponse, ToolObservationOrigin,
    };

    /// A missing socket is an answer, not a transient failure: the absent-
    /// daemon probe must stay fast while a momentarily-refused connect is
    /// retried inside its bounded window.
    #[cfg(unix)]
    #[test]
    fn connect_retries_only_transient_failures_and_fails_fast_on_missing_socket() {
        use std::io::ErrorKind;
        assert!(super::connect_error_is_transient(
            ErrorKind::ConnectionRefused
        ));
        assert!(super::connect_error_is_transient(ErrorKind::WouldBlock));
        assert!(super::connect_error_is_transient(ErrorKind::Interrupted));
        assert!(!super::connect_error_is_transient(ErrorKind::NotFound));
        assert!(!super::connect_error_is_transient(
            ErrorKind::PermissionDenied
        ));

        let missing = std::env::temp_dir().join(format!(
            "cua-driver-absent-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let started = std::time::Instant::now();
        let error = super::connect_with_bounded_retry(missing.to_str().expect("utf-8 path"))
            .expect_err("absent socket must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(
            started.elapsed() < std::time::Duration::from_millis(250),
            "absent-daemon connect must not enter the retry window: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn socket_path_keeps_the_selected_namespace() {
        let path = socket_path_for_namespace("cua-driver-test");
        assert!(path.contains("cua-driver-test"));
    }

    #[test]
    fn legacy_request_without_additive_fields_deserializes() {
        let request: DaemonRequest = serde_json::from_value(serde_json::json!({
            "method": "call",
            "name": "get_screen_size",
            "args": {}
        }))
        .unwrap();
        assert_eq!(request.session_id, None);
        assert_eq!(request.observation_origin, None);
        assert_eq!(request.client_kind, None);
    }

    #[test]
    fn direct_client_kind_is_bounded_and_snake_case() {
        let request = DaemonRequest {
            method: "call".into(),
            name: Some("start_session".into()),
            args: None,
            session_id: None,
            observation_origin: Some(ToolObservationOrigin::Direct),
            client_kind: Some(DaemonClientKind::TypescriptSdk),
        };
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["client_kind"], "typescript_sdk");
        assert!(value.get("application_name").is_none());
        assert!(value.get("package_name").is_none());
    }

    #[test]
    fn response_helpers_preserve_wire_shape() {
        let ok = DaemonResponse::ok(serde_json::json!({"value": 1}));
        assert!(ok.ok);
        assert_eq!(ok.result.unwrap()["value"], 1);

        let error = DaemonResponse::err("nope", 64);
        assert!(!error.ok);
        assert_eq!(error.error.as_deref(), Some("nope"));
        assert_eq!(error.exit_code, Some(64));
    }

    #[test]
    fn observation_origin_is_snake_case() {
        assert_eq!(
            serde_json::to_value(ToolObservationOrigin::McpProxy).unwrap(),
            "mcp_proxy"
        );
    }

    #[test]
    fn metadata_is_versioned_and_process_bound() {
        let metadata = current_daemon_metadata();
        assert_eq!(metadata.pid, std::process::id());
        assert_eq!(
            metadata.contract_version,
            cua_driver_contract::CONTRACT_VERSION
        );
        assert_eq!(
            metadata.mcp_protocol_version,
            cua_driver_contract::MCP_PROTOCOL_VERSION
        );
    }
}
