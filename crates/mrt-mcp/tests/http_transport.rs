//! Streamable HTTP transport tests against a tiny hand-rolled MCP server on 127.0.0.1.
//!
//! The server speaks just enough HTTP/1.1 (one request per connection, `Connection: close`) to
//! answer either `application/json` or `text/event-stream`, hand out `Mcp-Session-Id`s, check the
//! `MCP-Protocol-Version` and custom headers, expire a session on demand and resume a broken SSE
//! stream from `Last-Event-ID`.

use mrt_mcp::{McpHostManager, McpHttpHeader, MrtMcpServerState, LATEST_PROTOCOL_VERSION};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const TOKEN: &str = "Bearer test-token-123";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Json,
    Sse,
}

#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    headers: HashMap<String, String>,
    body: Value,
}

#[derive(Clone)]
struct TestServer {
    addr: SocketAddr,
    mode: Mode,
    log: Arc<Mutex<Vec<Recorded>>>,
    sessions_issued: Arc<AtomicUsize>,
    current_session: Arc<Mutex<String>>,
    expire_next: Arc<AtomicBool>,
    pending_resume: Arc<Mutex<HashMap<String, Value>>>,
}

impl TestServer {
    async fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let server = TestServer {
            addr: listener.local_addr().unwrap(),
            mode,
            log: Arc::default(),
            sessions_issued: Arc::default(),
            current_session: Arc::default(),
            expire_next: Arc::default(),
            pending_resume: Arc::default(),
        };
        let s = server.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let s = s.clone();
                tokio::spawn(async move {
                    let _ = s.handle(stream).await;
                });
            }
        });
        server
    }

    fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    fn requests(&self) -> Vec<Recorded> {
        self.log.lock().unwrap().clone()
    }

    fn posts_of(&self, method: &str) -> Vec<Recorded> {
        self.requests()
            .into_iter()
            .filter(|r| r.method == "POST" && r.body.get("method").and_then(|m| m.as_str()) == Some(method))
            .collect()
    }

    async fn handle(&self, mut stream: TcpStream) -> std::io::Result<()> {
        // Read the head.
        let mut buf = Vec::new();
        let head_end = loop {
            let mut chunk = [0u8; 4096];
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.split("\r\n");
        let method = lines.next().unwrap_or("").split(' ').next().unwrap_or("").to_string();
        let mut headers = HashMap::new();
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        let mut body = buf[head_end..].to_vec();
        while body.len() < len {
            let mut chunk = [0u8; 4096];
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        self.log.lock().unwrap().push(Recorded {
            method: method.clone(),
            headers: headers.clone(),
            body: body.clone(),
        });

        match method.as_str() {
            "DELETE" => write_plain(&mut stream, "200 OK", "").await,
            "GET" => self.handle_get(&mut stream, &headers).await,
            "POST" => self.handle_post(&mut stream, &headers, body).await,
            _ => write_plain(&mut stream, "405 Method Not Allowed", "").await,
        }
    }

    async fn handle_get(&self, stream: &mut TcpStream, headers: &HashMap<String, String>) -> std::io::Result<()> {
        if let Some(last) = headers.get("last-event-id") {
            let pending = self.pending_resume.lock().unwrap().remove(last);
            return match pending {
                Some(resp) => write_sse(stream, &format!("id: ev-2\ndata: {resp}\n\n")).await,
                None => write_plain(stream, "404 Not Found", "").await,
            };
        }
        if self.mode == Mode::Json {
            return write_plain(stream, "405 Method Not Allowed", "").await;
        }
        // Server-initiated listen stream: one notification, then keep the stream open a while.
        let note = json!({
            "jsonrpc": "2.0",
            "method": "notifications/message",
            "params": { "level": "info", "data": "hello from the GET stream" }
        });
        stream.write_all(SSE_HEAD.as_bytes()).await?;
        stream.write_all(format!("data: {note}\n\n").as_bytes()).await?;
        stream.flush().await?;
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok(())
    }

    async fn handle_post(
        &self,
        stream: &mut TcpStream,
        headers: &HashMap<String, String>,
        body: Value,
    ) -> std::io::Result<()> {
        let accept = headers.get("accept").cloned().unwrap_or_default();
        if !(accept.contains("application/json") && accept.contains("text/event-stream")) {
            return write_plain(stream, "406 Not Acceptable", "").await;
        }
        if headers.get("authorization").map(String::as_str) != Some(TOKEN) {
            return write_plain(stream, "401 Unauthorized", "missing token").await;
        }
        let Some(method) = body.get("method").and_then(|m| m.as_str()) else {
            return write_plain(stream, "202 Accepted", "").await; // a response from the client
        };
        let Some(id) = body.get("id").cloned() else {
            return write_plain(stream, "202 Accepted", "").await; // a notification
        };

        if method == "initialize" {
            let n = self.sessions_issued.fetch_add(1, Ordering::SeqCst) + 1;
            let sid = format!("sess-{n}");
            *self.current_session.lock().unwrap() = sid.clone();
            let resp = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "test-http-mcp", "version": "9.9.9" }
                }
            });
            let extra = format!("Mcp-Session-Id: {sid}\r\n");
            return write_json(stream, &resp, &extra).await;
        }

        let sid = self.current_session.lock().unwrap().clone();
        if headers.get("mcp-session-id") != Some(&sid) {
            return write_plain(stream, "400 Bad Request", "unknown session").await;
        }
        if self.expire_next.swap(false, Ordering::SeqCst) {
            return write_plain(stream, "404 Not Found", "session expired").await;
        }
        if headers.get("mcp-protocol-version").map(String::as_str) != Some(LATEST_PROTOCOL_VERSION) {
            return write_plain(stream, "400 Bad Request", "bad protocol version").await;
        }

        let params = body.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "tools/list" if params.get("cursor").is_none() => json!({
                "tools": [{ "name": "echo", "description": "Echo text", "inputSchema": { "type": "object" } }],
                "nextCursor": "page-2"
            }),
            "tools/list" => json!({ "tools": [{ "name": "whoami", "inputSchema": { "type": "object" } }] }),
            "tools/call" => {
                let text = params
                    .pointer("/arguments/text")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                json!({ "content": [{ "type": "text", "text": text }], "isError": false })
            }
            _ => {
                let err = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "nope" } });
                return write_json(stream, &err, "").await;
            }
        };
        let resp = json!({ "jsonrpc": "2.0", "id": id, "result": result });

        if self.mode == Mode::Json {
            return write_json(stream, &resp, "").await;
        }

        // SSE: a resumable stream that breaks before the response...
        if params.pointer("/arguments/text").and_then(|t| t.as_str()) == Some("resume") {
            self.pending_resume.lock().unwrap().insert("ev-1".into(), resp);
            return write_sse(stream, "id: ev-1\nretry: 10\ndata:\n\n").await;
        }
        // ...or the full flow: comment, priming event, progress notification, a server ping
        // request, then the response.
        let progress = json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": { "progressToken": 1, "progress": 50 }
        });
        let ping = json!({ "jsonrpc": "2.0", "id": "srv-ping", "method": "ping" });
        let body = format!(
            ": keep-alive\n\nid: ev-0\ndata:\n\nevent: message\ndata: {progress}\n\ndata: {ping}\n\nid: ev-9\ndata: {resp}\n\n"
        );
        write_sse(stream, &body).await
    }
}

const SSE_HEAD: &str =
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n";

async fn write_sse(stream: &mut TcpStream, body: &str) -> std::io::Result<()> {
    stream.write_all(SSE_HEAD.as_bytes()).await?;
    // Split the body so the client has to reassemble events across chunks.
    let mid = body.len() / 2;
    let (a, b) = body.as_bytes().split_at(mid);
    stream.write_all(a).await?;
    stream.flush().await?;
    tokio::time::sleep(Duration::from_millis(5)).await;
    stream.write_all(b).await?;
    stream.shutdown().await
}

async fn write_json(stream: &mut TcpStream, value: &Value, extra_headers: &str) -> std::io::Result<()> {
    let body = value.to_string();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await
}

async fn write_plain(stream: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await
}

fn auth_headers() -> Vec<McpHttpHeader> {
    vec![McpHttpHeader {
        name: "Authorization".into(),
        value: TOKEN.into(),
    }]
}

async fn connect(server: &TestServer) -> McpHostManager {
    let mut host = McpHostManager::new(MrtMcpServerState::default());
    let status = host
        .connect_external_http_server("remote".into(), server.url(), auth_headers())
        .await
        .unwrap();
    assert!(status.connected, "connect failed: {:?}", status.last_error);
    assert_eq!(status.server_version, "9.9.9");
    let names: Vec<&str> = status.tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["echo", "whoami"], "tools/list pagination");
    host
}

async fn echo(host: &McpHostManager, text: &str) -> String {
    let res = host
        .call_tool("remote", "echo", json!({ "text": text }))
        .await
        .unwrap();
    assert!(!res.is_error);
    res.content[0].text.clone()
}

#[tokio::test]
async fn json_responses_initialize_list_call_and_reuse_session() {
    let server = TestServer::start(Mode::Json).await;
    let host = connect(&server).await;

    assert_eq!(echo(&host, "first").await, "first");
    assert_eq!(echo(&host, "second").await, "second");

    // One session for the whole lifetime: a single initialize.
    assert_eq!(server.posts_of("initialize").len(), 1);
    let init = &server.posts_of("initialize")[0];
    assert!(!init.headers.contains_key("mcp-session-id"));
    assert!(!init.headers.contains_key("mcp-protocol-version"));
    assert_eq!(init.body["params"]["protocolVersion"], LATEST_PROTOCOL_VERSION);

    // Every later POST carries the session id, the negotiated version and the custom header.
    let later: Vec<Recorded> = server
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST" && r.body.get("method") != Some(&json!("initialize")))
        .collect();
    assert!(later.len() >= 5, "initialized + 2 x tools/list + resources/list + 2 x tools/call");
    for r in &later {
        assert_eq!(r.headers.get("mcp-session-id").map(String::as_str), Some("sess-1"));
        assert_eq!(
            r.headers.get("mcp-protocol-version").map(String::as_str),
            Some(LATEST_PROTOCOL_VERSION)
        );
        assert_eq!(r.headers.get("authorization").map(String::as_str), Some(TOKEN));
    }
    assert_eq!(server.posts_of("notifications/initialized").len(), 1);
    assert_eq!(server.posts_of("tools/call").len(), 2);
}

#[tokio::test]
async fn sse_responses_notifications_server_requests_and_resume() {
    let server = TestServer::start(Mode::Sse).await;
    let host = connect(&server).await;

    assert_eq!(echo(&host, "streamed").await, "streamed");

    // MRT answered the server's `ping` request on the stream with an empty result.
    let ping_reply = server
        .requests()
        .into_iter()
        .find(|r| r.body.get("id") == Some(&json!("srv-ping")) && r.body.get("result").is_some());
    assert!(ping_reply.is_some(), "client should reply to the server ping");

    // A stream that breaks before the response is resumed with GET + Last-Event-ID.
    assert_eq!(echo(&host, "resume").await, "resume");
    let resumed = server
        .requests()
        .into_iter()
        .any(|r| r.method == "GET" && r.headers.get("last-event-id").map(String::as_str) == Some("ev-1"));
    assert!(resumed, "client should resume from Last-Event-ID");

    // Notifications from the POST stream and from the GET listen stream are collected.
    let mut methods: Vec<String> = Vec::new();
    for _ in 0..40 {
        methods.extend(
            host.drain_server_events("remote")
                .await
                .into_iter()
                .filter_map(|e| e.get("method").and_then(|m| m.as_str()).map(str::to_string)),
        );
        if methods.iter().any(|m| m == "notifications/message") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(methods.iter().any(|m| m == "notifications/progress"), "{methods:?}");
    assert!(methods.iter().any(|m| m == "notifications/message"), "{methods:?}");
    assert!(methods.iter().any(|m| m == "ping"), "{methods:?}");

    // The listen GET carried the session id.
    let listen = server
        .requests()
        .into_iter()
        .find(|r| r.method == "GET" && !r.headers.contains_key("last-event-id"))
        .expect("listen stream opened");
    assert_eq!(listen.headers.get("mcp-session-id").map(String::as_str), Some("sess-1"));
    assert_eq!(listen.headers.get("accept").map(String::as_str), Some("text/event-stream"));
}

#[tokio::test]
async fn expired_session_reinitializes_and_retries() {
    let server = TestServer::start(Mode::Json).await;
    let host = connect(&server).await;
    assert_eq!(echo(&host, "before").await, "before");

    server.expire_next.store(true, Ordering::SeqCst);
    assert_eq!(echo(&host, "after").await, "after");

    assert_eq!(server.posts_of("initialize").len(), 2);
    let last_call = server.posts_of("tools/call").pop().unwrap();
    assert_eq!(last_call.headers.get("mcp-session-id").map(String::as_str), Some("sess-2"));
    // The dead session was ended with DELETE.
    assert!(server
        .requests()
        .iter()
        .any(|r| r.method == "DELETE" && r.headers.get("mcp-session-id").map(String::as_str) == Some("sess-1")));
}

#[tokio::test]
async fn http_errors_are_reported_without_header_values() {
    let server = TestServer::start(Mode::Json).await;
    let mut host = McpHostManager::new(MrtMcpServerState::default());
    let wrong = vec![McpHttpHeader {
        name: "Authorization".into(),
        value: "Bearer wrong-secret-value".into(),
    }];
    let status = host
        .connect_external_http_server("remote".into(), server.url(), wrong)
        .await
        .unwrap();
    assert!(!status.connected);
    let err = status.last_error.clone().unwrap();
    assert!(err.contains("401"), "{err}");
    assert!(!err.contains("wrong-secret-value"));
    assert!(!format!("{status:?}").contains("wrong-secret-value"));
    assert!(!status.config.target_display().contains("wrong-secret-value"));

    let bad_url = host
        .connect_external_http_server("bad".into(), "ftp://example.com".into(), Vec::new())
        .await
        .unwrap();
    assert!(!bad_url.connected);
}
