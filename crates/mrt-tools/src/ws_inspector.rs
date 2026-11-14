use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use mrt_core::StructuredOutput;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::protocol::Message;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WsFrameEntry {
    pub direction: String, // "TX" or "RX"
    pub offset_ms: u64,
    pub frame_type: String, // "Text", "Binary", "Ping", "Pong", "Close"
    pub payload: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsSessionReport {
    pub url: String,
    pub handshake_ms: u64,
    pub frames: Vec<WsFrameEntry>,
    pub status: String,
}

/// Parses `:ws` input into (url, optional_message_to_send, listen_timeout_ms).
/// Example syntax:
///   `:ws ws://echo.websocket.events`
///   `:ws wss://echo.websocket.events hello world`
///   `:ws localhost:8080/events --send ping --timeout 3000`
pub fn parse_ws_command(input: &str) -> Result<(String, Option<String>, u64)> {
    let mut raw = input.trim();
    if let Some(stripped) = raw.strip_prefix(":ws") {
        raw = stripped.trim();
    }

    if raw.is_empty() {
        bail!("Missing WebSocket URL. Usage: :ws <ws:// or wss:// URL> [message]");
    }

    let mut parts: Vec<&str> = raw.split_whitespace().collect();
    let mut timeout_ms = 2500u64;
    let mut send_msg: Option<String> = None;

    // Extract --timeout flag if present
    if let Some(pos) = parts.iter().position(|&p| p == "--timeout") {
        if pos + 1 < parts.len() {
            if let Ok(t) = parts[pos + 1].parse::<u64>() {
                timeout_ms = t;
            }
            parts.remove(pos + 1);
            parts.remove(pos);
        }
    }

    // Extract --send flag if present
    if let Some(pos) = parts.iter().position(|&p| p == "--send") {
        let msg_parts = parts.split_off(pos + 1);
        parts.pop(); // remove "--send"
        if !msg_parts.is_empty() {
            send_msg = Some(msg_parts.join(" "));
        }
    }

    if parts.is_empty() {
        bail!("Missing WebSocket URL");
    }

    let url_raw = parts[0];
    let mut url = url_raw.to_string();
    if url.starts_with("http://") {
        url = format!("ws://{}", &url["http://".len()..]);
    } else if url.starts_with("https://") {
        url = format!("wss://{}", &url["https://".len()..]);
    } else if !url.starts_with("ws://") && !url.starts_with("wss://") {
        url = format!("ws://{url}");
    }

    // If there were remaining parts after URL and no explicit --send, treat them as message
    if send_msg.is_none() && parts.len() > 1 {
        send_msg = Some(parts[1..].join(" "));
    }

    Ok((url, send_msg, timeout_ms))
}

/// Connects to a WebSocket server, optionally sends an initial message,
/// records incoming frames within `listen_ms`, and returns both a structured
/// table and plain text log.
pub async fn inspect_websocket(
    query: &str,
) -> Result<(StructuredOutput, String, WsSessionReport)> {
    let (url, initial_msg, timeout_ms) = parse_ws_command(query)?;

    let connect_start = Instant::now();
    let connect_fut = connect_async(&url);
    let (mut ws_stream, response) = tokio::time::timeout(Duration::from_millis(3000), connect_fut)
        .await
        .context("WebSocket connection timed out (3000ms)")?
        .with_context(|| format!("Failed to connect to WebSocket at '{url}'"))?;

    let handshake_ms = connect_start.elapsed().as_millis() as u64;
    let mut frames = Vec::new();
    let session_start = Instant::now();

    // 1. Send initial message if requested
    if let Some(msg_text) = initial_msg {
        let offset = session_start.elapsed().as_millis() as u64;
        let send_res = ws_stream.send(Message::Text(msg_text.clone().into())).await;
        if let Err(e) = send_res {
            bail!("Failed to send initial WebSocket message: {e}");
        }
        frames.push(WsFrameEntry {
            direction: "TX".to_string(),
            offset_ms: offset,
            frame_type: "Text".to_string(),
            payload: msg_text,
        });
    }

    // 2. Listen for incoming frames until timeout
    let listen_deadline = Instant::now() + Duration::from_millis(timeout_ms);
    while Instant::now() < listen_deadline {
        let remaining = listen_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        match tokio::time::timeout(remaining, ws_stream.next()).await {
            Ok(Some(Ok(msg))) => {
                let offset = session_start.elapsed().as_millis() as u64;
                match msg {
                    Message::Text(t) => {
                        frames.push(WsFrameEntry {
                            direction: "RX".to_string(),
                            offset_ms: offset,
                            frame_type: "Text".to_string(),
                            payload: t.to_string(),
                        });
                    }
                    Message::Binary(b) => {
                        frames.push(WsFrameEntry {
                            direction: "RX".to_string(),
                            offset_ms: offset,
                            frame_type: "Binary".to_string(),
                            payload: format!("<{} bytes>", b.len()),
                        });
                    }
                    Message::Ping(p) => {
                        frames.push(WsFrameEntry {
                            direction: "RX".to_string(),
                            offset_ms: offset,
                            frame_type: "Ping".to_string(),
                            payload: format!("<{} bytes>", p.len()),
                        });
                        let _ = ws_stream.send(Message::Pong(p)).await;
                    }
                    Message::Pong(_) => {
                        frames.push(WsFrameEntry {
                            direction: "RX".to_string(),
                            offset_ms: offset,
                            frame_type: "Pong".to_string(),
                            payload: String::new(),
                        });
                    }
                    Message::Close(frame) => {
                        let reason = frame
                            .map(|f| format!("code {}: {}", f.code, f.reason))
                            .unwrap_or_else(|| "clean close".to_string());
                        frames.push(WsFrameEntry {
                            direction: "RX".to_string(),
                            offset_ms: offset,
                            frame_type: "Close".to_string(),
                            payload: reason,
                        });
                        break;
                    }
                    Message::Frame(_) => {}
                }
            }
            Ok(Some(Err(e))) => {
                frames.push(WsFrameEntry {
                    direction: "ERR".to_string(),
                    offset_ms: session_start.elapsed().as_millis() as u64,
                    frame_type: "Error".to_string(),
                    payload: format!("{e}"),
                });
                break;
            }
            Ok(None) => {
                // Connection closed by server
                break;
            }
            Err(_) => {
                // Timeout reached
                break;
            }
        }
    }

    // Close session gracefully
    let _ = ws_stream.close(None).await;

    let status_str = format!("HTTP {}", response.status());

    let report = WsSessionReport {
        url: url.clone(),
        handshake_ms,
        frames: frames.clone(),
        status: status_str,
    };

    // Build Structured Table
    let headers = vec![
        "Dir".to_string(),
        "Offset".to_string(),
        "Frame Type".to_string(),
        "Payload".to_string(),
    ];
    let rows: Vec<Vec<String>> = frames
        .iter()
        .map(|f| {
            vec![
                f.direction.clone(),
                format!("+{}ms", f.offset_ms),
                f.frame_type.clone(),
                f.payload.clone(),
            ]
        })
        .collect();

    let structured = StructuredOutput::Table { headers, rows };

    // Build plain text log
    let mut plain = String::new();
    plain.push_str(&format!(
        "WebSocket Inspector: {url}\nHandshake: {handshake_ms}ms · Status: Connected · Frames: {}\n\n",
        frames.len()
    ));

    for f in &frames {
        let arrow = if f.direction == "TX" { "->" } else { "<-" };
        plain.push_str(&format!(
            "  {} [{:2}] (+{:4}ms) [{:6}] {}\n",
            arrow, f.direction, f.offset_ms, f.frame_type, f.payload
        ));
    }

    if frames.is_empty() {
        plain.push_str("  (No frames received within timeout)\n");
    }

    plain.push_str("\nConnection closed.\n");

    Ok((structured, plain, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ws_command_basic() {
        let (url, msg, timeout) = parse_ws_command(":ws ws://echo.websocket.events").unwrap();
        assert_eq!(url, "ws://echo.websocket.events");
        assert_eq!(msg, None);
        assert_eq!(timeout, 2500);
    }

    #[test]
    fn test_parse_ws_command_with_message() {
        let (url, msg, timeout) =
            parse_ws_command(":ws wss://echo.websocket.events hello world").unwrap();
        assert_eq!(url, "wss://echo.websocket.events");
        assert_eq!(msg, Some("hello world".into()));
        assert_eq!(timeout, 2500);
    }

    #[test]
    fn test_parse_ws_command_http_scheme_upgrade() {
        let (url, _, _) = parse_ws_command("https://api.example.com/socket").unwrap();
        assert_eq!(url, "wss://api.example.com/socket");

        let (url2, _, _) = parse_ws_command("http://localhost:8080").unwrap();
        assert_eq!(url2, "ws://localhost:8080");
    }

    #[test]
    fn test_parse_ws_command_flags() {
        let (url, msg, timeout) = parse_ws_command(
            ":ws echo.events --timeout 5000 --send test payload 123",
        )
        .unwrap();
        assert_eq!(url, "ws://echo.events");
        assert_eq!(msg, Some("test payload 123".into()));
        assert_eq!(timeout, 5000);
    }

    #[test]
    fn test_parse_ws_command_empty_fails() {
        assert!(parse_ws_command(":ws").is_err());
        assert!(parse_ws_command("   ").is_err());
    }
}
