//! Transports for MRT's MCP client: `stdio` (a spawned child process speaking newline-delimited
//! JSON-RPC) and Streamable HTTP (MCP spec revision 2025-11-25,
//! <https://modelcontextprotocol.io/specification/2025-11-25/basic/transports#streamable-http>).
//!
//! Both transports expose the same request / notify / close surface so the session logic in
//! [`crate::client`] does not care how the bytes travel.
//!
//! Custom HTTP headers usually carry credentials (`Authorization: Bearer ...`). Their values are
//! never logged, never put into error messages and are redacted from `Debug` output.

use crate::protocol::JsonRpcResponse;
use anyhow::{anyhow, bail, Context, Result};
use reqwest::header::{HeaderName, HeaderValue, ACCEPT, CONTENT_TYPE};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::fmt;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;

/// Protocol revision MRT offers in `initialize`. The server may answer with an older one; the
/// negotiated value is then sent in the `MCP-Protocol-Version` header on every later HTTP request.
pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

/// How many server-initiated messages are kept per server before the oldest are dropped.
const MAX_SERVER_EVENTS: usize = 200;
/// How often MRT tries to resume a broken SSE response stream with `Last-Event-ID`.
const MAX_SSE_RESUMES: u32 = 5;
/// Consecutive failed (or empty) GET listen streams before MRT stops reopening it.
const MAX_LISTEN_FAILURES: u32 = 5;

const SESSION_HEADER: &str = "mcp-session-id";
const PROTOCOL_HEADER: &str = "mcp-protocol-version";
const LAST_EVENT_ID_HEADER: &str = "last-event-id";

/// Headers the transport sets itself; users may not override them.
const RESERVED_HEADERS: &[&str] = &[
    "accept",
    "content-type",
    "content-length",
    "host",
    "connection",
    "transfer-encoding",
    SESSION_HEADER,
    PROTOCOL_HEADER,
    LAST_EVENT_ID_HEADER,
];

/// Which transport an external MCP server uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpTransportKind {
    #[default]
    Stdio,
    Http,
}

impl McpTransportKind {
    pub fn label(self) -> &'static str {
        match self {
            McpTransportKind::Stdio => "stdio",
            McpTransportKind::Http => "HTTP",
        }
    }
}

/// A user-supplied HTTP header sent with every request to a Streamable HTTP MCP server.
/// `Debug` redacts the value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpHttpHeader {
    pub name: String,
    pub value: String,
}

impl fmt::Debug for McpHttpHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpHttpHeader")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Parses one `Name: value` header per line. Blank lines and lines starting with `#` are skipped.
/// Error messages name the line number and header name, never the value.
pub fn parse_http_headers(text: &str) -> Result<Vec<McpHttpHeader>> {
    let mut headers = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line_no = idx + 1;
        let Some((name, value)) = line.split_once(':') else {
            bail!("Header line {line_no} must look like `Name: value`");
        };
        let name = name.trim();
        let value = value.trim();
        if HeaderName::from_bytes(name.as_bytes()).is_err() {
            bail!("Header line {line_no}: `{name}` is not a valid HTTP header name");
        }
        if RESERVED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            bail!("Header line {line_no}: `{name}` is managed by MRT and cannot be set");
        }
        if HeaderValue::from_str(value).is_err() {
            bail!("Header line {line_no}: the value of `{name}` contains characters HTTP does not allow");
        }
        headers.push(McpHttpHeader {
            name: name.to_string(),
            value: value.to_string(),
        });
    }
    Ok(headers)
}

/// Returned when the server no longer knows the session (HTTP 404 with a session id) or a stdio
/// server exited. The host reconnects once and retries.
#[derive(Debug)]
pub struct SessionExpired(pub String);

impl fmt::Display for SessionExpired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MCP session ended: {}", self.0)
    }
}

impl std::error::Error for SessionExpired {}

/// Server-initiated messages (notifications and requests) received outside a response,
/// newest last.
#[derive(Clone, Default)]
pub struct ServerEventLog(Arc<Mutex<VecDeque<Value>>>);

impl ServerEventLog {
    pub fn push(&self, msg: Value) {
        if let Ok(mut q) = self.0.lock() {
            if q.len() >= MAX_SERVER_EVENTS {
                q.pop_front();
            }
            q.push_back(msg);
        }
    }

    pub fn drain(&self) -> Vec<Value> {
        self.0
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }
}

/// MRT's answer to a server-to-client request: `ping` gets an empty result, everything else
/// (sampling, roots, elicitation) is declined with "method not found".
fn reply_to_server_request(msg: &Value) -> Option<Value> {
    let id = msg.get("id")?;
    let method = msg.get("method")?.as_str()?;
    Some(if method == "ping" {
        json!({ "jsonrpc": "2.0", "id": id, "result": {} })
    } else {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32601, "message": format!("MRT does not support `{method}`") }
        })
    })
}

fn is_response_for(msg: &Value, id: &Value) -> bool {
    msg.get("method").is_none() && msg.get("id") == Some(id)
}

/// Records a server-initiated message and returns the reply MRT owes for it, if any.
fn handle_server_message(events: &ServerEventLog, msg: Value) -> Option<Value> {
    if msg.get("method").is_none() {
        return None; // a stray response to nothing we are waiting for
    }
    let reply = reply_to_server_request(&msg);
    events.push(msg);
    reply
}

/// One transport behind an MCP client session.
pub(crate) enum Transport {
    Stdio(StdioTransport),
    Http(HttpTransport),
}

impl Transport {
    /// Sends a JSON-RPC request (which must carry an `id`) and waits for its response.
    pub async fn request(&mut self, msg: &Value) -> Result<JsonRpcResponse> {
        match self {
            Transport::Stdio(t) => t.request(msg).await,
            Transport::Http(t) => t.request(msg).await,
        }
    }

    pub async fn notify(&mut self, msg: &Value) -> Result<()> {
        match self {
            Transport::Stdio(t) => t.write(msg).await,
            Transport::Http(t) => t.notify(msg).await,
        }
    }

    /// Records the version negotiated by `initialize`; HTTP sends it on every later request,
    /// starting with `notifications/initialized`.
    pub fn set_protocol_version(&mut self, protocol_version: &str) {
        if let Transport::Http(t) = self {
            t.ctx.set_protocol_version(protocol_version);
        }
    }

    /// Called once the handshake finished: HTTP opens its optional GET listen stream.
    pub fn on_initialized(&mut self) {
        if let Transport::Http(t) = self {
            t.start_listen_stream();
        }
    }

    pub fn events(&self) -> ServerEventLog {
        match self {
            Transport::Stdio(t) => t.events.clone(),
            Transport::Http(t) => t.ctx.events.clone(),
        }
    }

    pub async fn close(&mut self) {
        match self {
            Transport::Stdio(t) => {
                let _ = t.child.kill().await;
            }
            Transport::Http(t) => t.close().await,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// stdio
// ---------------------------------------------------------------------------------------------

pub(crate) struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    events: ServerEventLog,
}

impl StdioTransport {
    pub fn spawn(command: &str, args: &[String]) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(target_os = "windows")]
        {
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to launch MCP server '{command}'"))?;
        let stdin = child.stdin.take().context("Failed to open MCP stdin")?;
        let stdout = child.stdout.take().context("Failed to open MCP stdout")?;
        Ok(Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            events: ServerEventLog::default(),
        })
    }

    async fn write(&mut self, msg: &Value) -> Result<()> {
        let line = serde_json::to_string(msg)? + "\n";
        let res = async {
            self.stdin.write_all(line.as_bytes()).await?;
            self.stdin.flush().await
        }
        .await;
        res.map_err(|e| anyhow!(SessionExpired(format!("cannot write to the server: {e}"))))
    }

    async fn request(&mut self, msg: &Value) -> Result<JsonRpcResponse> {
        let id = msg.get("id").cloned().context("JSON-RPC request without id")?;
        self.write(msg).await?;
        loop {
            let line = self
                .lines
                .next_line()
                .await
                .map_err(|e| anyhow!(SessionExpired(format!("cannot read from the server: {e}"))))?
                .ok_or_else(|| anyhow!(SessionExpired("the server closed its output".into())))?;
            let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
                continue; // not JSON-RPC (a stray log line)
            };
            if is_response_for(&value, &id) {
                return Ok(serde_json::from_value(value)?);
            }
            if let Some(reply) = handle_server_message(&self.events, value) {
                self.write(&reply).await?;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Streamable HTTP
// ---------------------------------------------------------------------------------------------

/// Shared by the request path and the background GET listen stream.
#[derive(Clone)]
struct HttpCtx {
    client: reqwest::Client,
    url: String,
    headers: Vec<McpHttpHeader>,
    session_id: Arc<Mutex<Option<String>>>,
    protocol_version: Arc<Mutex<Option<String>>>,
    events: ServerEventLog,
}

impl HttpCtx {
    fn session_id(&self) -> Option<String> {
        self.session_id.lock().ok().and_then(|s| s.clone())
    }

    fn set_protocol_version(&self, version: &str) {
        if let Ok(mut v) = self.protocol_version.lock() {
            *v = Some(version.to_string());
        }
    }

    fn builder(&self, method: Method, accept: &'static str) -> reqwest::RequestBuilder {
        let mut b = self.client.request(method, &self.url).header(ACCEPT, accept);
        for h in &self.headers {
            b = b.header(h.name.as_str(), h.value.as_str());
        }
        if let Some(sid) = self.session_id() {
            b = b.header(SESSION_HEADER, sid);
        }
        if let Some(v) = self.protocol_version.lock().ok().and_then(|v| v.clone()) {
            b = b.header(PROTOCOL_HEADER, v);
        }
        b
    }

    /// Fails with `SessionExpired` on 404 while a session is active, as the spec asks clients to
    /// start a new session then.
    async fn check_status(&self, resp: reqwest::Response, had_session: bool) -> Result<reqwest::Response> {
        let status = resp.status();
        if status == StatusCode::NOT_FOUND && had_session {
            bail!(SessionExpired("the server no longer knows this session (HTTP 404)".into()));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let snippet: String = body.chars().take(300).collect();
            bail!("MCP server returned HTTP {status}: {}", snippet.trim());
        }
        Ok(resp)
    }

    async fn post(&self, msg: &Value) -> Result<reqwest::Response> {
        let had_session = self.session_id().is_some();
        let resp = self
            .builder(Method::POST, "application/json, text/event-stream")
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(msg)?)
            .send()
            .await
            .with_context(|| format!("HTTP request to MCP server {} failed", self.url))?;
        if !had_session {
            if let Some(sid) = resp.headers().get(SESSION_HEADER).and_then(|v| v.to_str().ok()) {
                if let Ok(mut s) = self.session_id.lock() {
                    *s = Some(sid.to_string());
                }
            }
        }
        self.check_status(resp, had_session).await
    }

    /// Opens an SSE stream with GET. `Ok(None)` means the server does not offer one (405).
    async fn get_stream(&self, last_event_id: Option<&str>) -> Result<Option<reqwest::Response>> {
        let had_session = self.session_id().is_some();
        let mut b = self.builder(Method::GET, "text/event-stream");
        if let Some(id) = last_event_id {
            b = b.header(LAST_EVENT_ID_HEADER, id);
        }
        let resp = b
            .send()
            .await
            .with_context(|| format!("HTTP GET to MCP server {} failed", self.url))?;
        if resp.status() == StatusCode::METHOD_NOT_ALLOWED {
            return Ok(None);
        }
        let resp = self.check_status(resp, had_session).await?;
        if !content_type(&resp).starts_with("text/event-stream") {
            bail!("MCP server answered GET without an event stream");
        }
        Ok(Some(resp))
    }

    /// Sends MRT's reply to a server request. The server answers 202; failures are ignored.
    async fn reply(&self, reply: Value) {
        if let Err(e) = self.post(&reply).await {
            tracing::debug!("MCP: could not reply to a server request: {e}");
        }
    }

    async fn handle(&self, msg: Value) {
        if let Some(reply) = handle_server_message(&self.events, msg) {
            self.reply(reply).await;
        }
    }

    /// Reads an SSE response until the JSON-RPC response with `id` arrives, handling server
    /// messages on the way and resuming with `Last-Event-ID` if the stream breaks first.
    async fn read_sse_response(&self, mut resp: reqwest::Response, id: &Value) -> Result<JsonRpcResponse> {
        let mut last_event_id: Option<String> = None;
        let mut retry = Duration::from_millis(500);
        let mut resumes = 0;
        loop {
            let mut parser = SseParser::default();
            while let Ok(Some(chunk)) = resp.chunk().await {
                for ev in parser.feed(&chunk) {
                    if ev.id.is_some() {
                        last_event_id = ev.id;
                    }
                    if let Some(ms) = ev.retry {
                        retry = Duration::from_millis(ms);
                    }
                    let Ok(msg) = serde_json::from_str::<Value>(&ev.data) else {
                        continue; // priming event (empty data) or not JSON
                    };
                    if is_response_for(&msg, id) {
                        return Ok(serde_json::from_value(msg)?);
                    }
                    self.handle(msg).await;
                }
            }
            let Some(resume_from) = last_event_id.as_deref() else {
                bail!("MCP event stream closed before the server sent a response");
            };
            resumes += 1;
            if resumes > MAX_SSE_RESUMES {
                bail!("MCP event stream kept closing before the server sent a response");
            }
            tokio::time::sleep(retry.min(Duration::from_secs(10))).await;
            resp = self
                .get_stream(Some(resume_from))
                .await?
                .context("MCP event stream closed and the server does not allow resuming it")?;
        }
    }
}

fn content_type(resp: &reqwest::Response) -> String {
    resp.headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase()
}

pub(crate) struct HttpTransport {
    ctx: HttpCtx,
    listen: Option<JoinHandle<()>>,
}

impl HttpTransport {
    pub fn new(url: &str, headers: Vec<McpHttpHeader>) -> Result<Self> {
        let parsed = reqwest::Url::parse(url.trim()).context("Invalid MCP server URL")?;
        if !matches!(parsed.scheme(), "http" | "https") {
            bail!("MCP server URL must start with http:// or https://");
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .user_agent(concat!("ModernRustTerminal/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            ctx: HttpCtx {
                client,
                url: parsed.to_string(),
                headers,
                session_id: Arc::default(),
                protocol_version: Arc::default(),
                events: ServerEventLog::default(),
            },
            listen: None,
        })
    }

    async fn request(&mut self, msg: &Value) -> Result<JsonRpcResponse> {
        let id = msg.get("id").cloned().context("JSON-RPC request without id")?;
        let resp = self.ctx.post(msg).await?;
        if resp.status() == StatusCode::ACCEPTED {
            bail!("MCP server accepted the request but sent no response");
        }
        if content_type(&resp).starts_with("text/event-stream") {
            return self.ctx.read_sse_response(resp, &id).await;
        }
        let body = resp.bytes().await.context("Failed to read MCP server response")?;
        let value: Value =
            serde_json::from_slice(&body).context("MCP server sent a response that is not JSON")?;
        let messages = match value {
            Value::Array(items) => items,
            other => vec![other],
        };
        let mut found = None;
        for msg in messages {
            if is_response_for(&msg, &id) {
                found = Some(msg);
            } else {
                self.ctx.handle(msg).await;
            }
        }
        let msg = found.context("MCP server's reply did not answer the request")?;
        Ok(serde_json::from_value(msg)?)
    }

    async fn notify(&mut self, msg: &Value) -> Result<()> {
        self.ctx.post(msg).await.map(|_| ())
    }

    /// Opens the optional GET stream on which the server may send notifications and requests
    /// unrelated to any POST. Servers that do not offer it answer 405 and the task ends.
    fn start_listen_stream(&mut self) {
        if self.listen.is_some() {
            return;
        }
        let ctx = self.ctx.clone();
        self.listen = Some(tokio::spawn(async move {
            let mut last_event_id: Option<String> = None;
            let mut retry = Duration::from_secs(1);
            let mut failures = 0;
            loop {
                match ctx.get_stream(last_event_id.as_deref()).await {
                    Ok(None) => return,
                    Ok(Some(mut resp)) => {
                        let mut parser = SseParser::default();
                        let mut got_event = false;
                        while let Ok(Some(chunk)) = resp.chunk().await {
                            for ev in parser.feed(&chunk) {
                                got_event = true;
                                if ev.id.is_some() {
                                    last_event_id = ev.id;
                                }
                                if let Some(ms) = ev.retry {
                                    retry = Duration::from_millis(ms);
                                }
                                if let Ok(msg) = serde_json::from_str::<Value>(&ev.data) {
                                    ctx.handle(msg).await;
                                }
                            }
                        }
                        failures = if got_event { 0 } else { failures + 1 };
                    }
                    Err(e) => {
                        if e.downcast_ref::<SessionExpired>().is_some() {
                            return;
                        }
                        tracing::debug!("MCP: listen stream failed: {e}");
                        failures += 1;
                    }
                }
                if failures >= MAX_LISTEN_FAILURES {
                    return;
                }
                tokio::time::sleep(retry.min(Duration::from_secs(30))).await;
            }
        }));
    }

    /// Stops the listen stream and ends the session with DELETE (a 405 answer is fine).
    async fn close(&mut self) {
        if let Some(task) = self.listen.take() {
            task.abort();
        }
        if self.ctx.session_id().is_some() {
            let _ = self.ctx.builder(Method::DELETE, "application/json").send().await;
        }
    }
}

impl Drop for HttpTransport {
    fn drop(&mut self) {
        if let Some(task) = self.listen.take() {
            task.abort();
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Server-Sent Events parsing
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq)]
struct SseEvent {
    data: String,
    id: Option<String>,
    retry: Option<u64>,
}

/// Incremental parser for `text/event-stream` bodies. Chunks may split lines or UTF-8 characters.
#[derive(Default)]
struct SseParser {
    buf: Vec<u8>,
    data: String,
    has_data: bool,
    id: Option<String>,
    retry: Option<u64>,
}

impl SseParser {
    fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut raw: Vec<u8> = self.buf.drain(..=pos).collect();
            raw.pop();
            if raw.last() == Some(&b'\r') {
                raw.pop();
            }
            let line = String::from_utf8_lossy(&raw);
            if line.is_empty() {
                if self.has_data || self.id.is_some() || self.retry.is_some() {
                    out.push(SseEvent {
                        data: std::mem::take(&mut self.data),
                        id: self.id.take(),
                        retry: self.retry.take(),
                    });
                }
                self.has_data = false;
                continue;
            }
            if line.starts_with(':') {
                continue; // comment / keep-alive
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line.as_ref(), ""),
            };
            match field {
                "data" => {
                    if self.has_data {
                        self.data.push('\n');
                    }
                    self.data.push_str(value);
                    self.has_data = true;
                }
                "id" => self.id = Some(value.to_string()),
                "retry" => self.retry = value.trim().parse().ok(),
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parser_handles_split_chunks_comments_and_priming() {
        let mut p = SseParser::default();
        assert!(p.feed(b": keep-alive\n").is_empty());
        assert!(p.feed(b"id: 7\nretry: 250\ndata:\n\n").len() == 1);
        let mut evs = p.feed(b"event: message\r\ndata: {\"a\":");
        assert!(evs.is_empty());
        evs = p.feed(b"1}\r\ndata: x\r\n\r\n");
        assert_eq!(
            evs,
            vec![SseEvent {
                data: "{\"a\":1}\nx".into(),
                id: None,
                retry: None
            }]
        );
    }

    #[test]
    fn header_parsing_validates_and_redacts() {
        let h = parse_http_headers("# comment\nAuthorization: Bearer abc123\n\nX-Team:  ops ").unwrap();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].name, "Authorization");
        assert_eq!(h[0].value, "Bearer abc123");
        assert_eq!(h[1].value, "ops");
        assert!(!format!("{h:?}").contains("abc123"));

        let err = parse_http_headers("Authorization Bearer secret").unwrap_err().to_string();
        assert!(!err.contains("secret"));
        assert!(parse_http_headers("Mcp-Session-Id: x").is_err());
        assert!(parse_http_headers("Bad Name: x").is_err());
    }
}
