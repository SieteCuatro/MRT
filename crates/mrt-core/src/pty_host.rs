//! PTYs owned by the session host (`mrt session-host`), so full-screen programs and
//! agents' PTYs keep running when the window that started them closes or crashes.
//!
//! The window sets the host's endpoint with [`set_pty_host`]; from then on
//! [`InteractivePty::spawn_with_owner`] starts PTYs in the host and talks to each
//! over a long-lived **event stream** instead of polling:
//!
//! - host → window, one JSON object per line: `{"ok": {"pid", "cols", "rows"}}` once,
//!   then `{"out": "<text>"}` as output appears and `{"exit": <code>}` at the end;
//! - window → host: `{"in": "<base64 bytes>"}`, `{"resize": [cols, rows]}`,
//!   `{"kill": true}`.
//!
//! A stream starts at a byte offset of the PTY's transcript, so a window that
//! reattaches with offset 0 gets the whole session again and rebuilds the screen.

use crate::pty::{InteractivePty, ShellProfile};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

/// Port and token of the session host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostEndpoint {
    pub port: u16,
    pub token: String,
}

static PTY_HOST: RwLock<Option<HostEndpoint>> = RwLock::new(None);

/// Runs new PTYs in this session host (`None`: in this process).
pub fn set_pty_host(endpoint: Option<HostEndpoint>) {
    if let Ok(mut guard) = PTY_HOST.write() {
        *guard = endpoint;
    }
}

pub fn pty_host() -> Option<HostEndpoint> {
    PTY_HOST.read().ok().and_then(|g| g.clone())
}

/// What the host knows about one of its PTYs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostedPtyInfo {
    pub pty_id: uuid::Uuid,
    pub shell_id: String,
    pub shell_name: String,
    pub cwd: PathBuf,
    pub cols: u16,
    pub rows: u16,
    pub command: Option<String>,
    /// Opaque label from the window that started it (which tab / pane, or an agent).
    pub owner: Option<String>,
    pub exited: bool,
}

/// An event from the window to a hosted PTY.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientEvent {
    Input(Vec<u8>),
    Resize(u16, u16),
    Kill,
}

impl ClientEvent {
    fn to_json(&self) -> Value {
        match self {
            ClientEvent::Input(bytes) => json!({ "in": crate::shell_integration::base64_encode(bytes) }),
            ClientEvent::Resize(c, r) => json!({ "resize": [c, r] }),
            ClientEvent::Kill => json!({ "kill": true }),
        }
    }

    fn from_json(value: &Value) -> Option<Self> {
        if let Some(data) = value["in"].as_str() {
            return crate::vt::decode_base64(data.as_bytes()).map(ClientEvent::Input);
        }
        if let Some(size) = value["resize"].as_array() {
            return Some(ClientEvent::Resize(size.first()?.as_u64()? as u16, size.get(1)?.as_u64()? as u16));
        }
        value["kill"].as_bool().filter(|k| *k).map(|_| ClientEvent::Kill)
    }
}

/// The window side of a hosted PTY.
pub struct HostedPty {
    writer: Mutex<TcpStream>,
    exit: Arc<Mutex<Option<u32>>>,
    pub process_id: Option<u32>,
}

impl HostedPty {
    pub fn send(&self, event: &ClientEvent) -> Result<()> {
        let mut line = event.to_json().to_string();
        line.push('\n');
        let mut writer = self.writer.lock().map_err(|_| anyhow!("PTY stream lock"))?;
        writer.write_all(line.as_bytes())?;
        writer.flush()?;
        Ok(())
    }

    pub fn exit_code(&self) -> Option<u32> {
        self.exit.lock().ok().and_then(|e| *e)
    }
}

fn connect(endpoint: &HostEndpoint, request: &Value) -> Result<(TcpStream, BufReader<TcpStream>, Value)> {
    let stream = TcpStream::connect_timeout(&([127, 0, 0, 1], endpoint.port).into(), Duration::from_secs(2))?;
    stream.set_nodelay(true)?;
    let mut writer = stream.try_clone()?;
    let mut payload = json!({ "mrt_host_auth": endpoint.token }).to_string();
    payload.push('\n');
    payload.push_str(&request.to_string());
    payload.push('\n');
    writer.write_all(payload.as_bytes())?;
    let mut reader = BufReader::new(stream.try_clone()?);
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    stream.set_read_timeout(None)?;
    let reply: Value = serde_json::from_str(&line)?;
    if let Some(error) = reply["error"].as_str() {
        bail!("{error}");
    }
    Ok((writer, reader, reply["ok"].clone()))
}

/// One request/response to the host.
pub fn request(endpoint: &HostEndpoint, request: &Value) -> Result<Value> {
    connect(endpoint, request).map(|(_, _, value)| value)
}

/// Starts a PTY in the host and attaches to it.
pub fn spawn_in_host(
    endpoint: &HostEndpoint,
    shell: &ShellProfile,
    cwd: &Path,
    cols: u16,
    rows: u16,
    command: Option<&str>,
    owner: Option<&str>,
) -> Result<InteractivePty> {
    let reply = request(
        endpoint,
        &json!({ "op": "pty_spawn", "shell": shell, "cwd": cwd, "cols": cols, "rows": rows, "command": command, "owner": owner }),
    )?;
    let info: HostedPtyInfo = serde_json::from_value(reply)?;
    attach(endpoint, &info)
}

/// Attaches to a PTY the host already runs, replaying its whole session.
pub fn attach(endpoint: &HostEndpoint, info: &HostedPtyInfo) -> Result<InteractivePty> {
    let (writer, mut reader, hello) = connect(endpoint, &json!({ "op": "pty_stream", "pty_id": info.pty_id, "from": 0 }))?;
    let output = Arc::new(Mutex::new(String::new()));
    let transcript = Arc::new(Mutex::new(String::new()));
    let exit = Arc::new(Mutex::new(None));
    {
        let (output, transcript, exit) = (output.clone(), transcript.clone(), exit.clone());
        std::thread::Builder::new().name("mrt-hosted-pty".into()).spawn(move || {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let Ok(event) = serde_json::from_str::<Value>(&line) else { continue };
                if let Some(text) = event["out"].as_str() {
                    if let Ok(mut o) = output.lock() {
                        o.push_str(text);
                    }
                    if let Ok(mut t) = transcript.lock() {
                        t.push_str(text);
                    }
                } else if let Some(code) = event["exit"].as_u64() {
                    if let Ok(mut e) = exit.lock() {
                        *e = Some(code as u32);
                    }
                }
            }
            // The host went away without an exit status: report the program as ended.
            if let Ok(mut e) = exit.lock() {
                e.get_or_insert(255);
            }
        })?;
    }
    let hosted = HostedPty {
        writer: Mutex::new(writer),
        exit,
        process_id: hello["pid"].as_u64().map(|p| p as u32),
    };
    let cols = hello["cols"].as_u64().map(|c| c as u16).unwrap_or(info.cols);
    let rows = hello["rows"].as_u64().map(|r| r as u16).unwrap_or(info.rows);
    Ok(InteractivePty::from_hosted(
        hosted,
        (info.pty_id, info.shell_id.clone(), info.shell_name.clone(), info.cwd.clone(), cols, rows),
        output,
        transcript,
    ))
}

/// The host's PTYs.
pub fn list(endpoint: &HostEndpoint) -> Result<Vec<HostedPtyInfo>> {
    Ok(serde_json::from_value(request(endpoint, &json!({ "op": "pty_list" }))?)?)
}

/// Host side: streams a PTY to one client until the program exits or the client
/// leaves. `writer` is the connection, `reader` its read half (after the request).
pub fn serve_stream(mut writer: TcpStream, mut reader: impl BufRead + Send + 'static, pty: Arc<InteractivePty>, from: usize) -> Result<()> {
    let (cols, rows) = pty.size();
    writeln!(writer, "{}", json!({ "ok": { "pid": pty.process_id(), "cols": cols, "rows": rows } }))?;
    let gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let (pty, gone) = (pty.clone(), gone.clone());
        std::thread::spawn(move || {
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
                match ClientEvent::from_json(&value) {
                    Some(ClientEvent::Input(bytes)) => {
                        let _ = pty.send_bytes(&bytes);
                    }
                    Some(ClientEvent::Resize(c, r)) => {
                        let _ = pty.resize(c, r);
                    }
                    Some(ClientEvent::Kill) => {
                        let _ = pty.kill();
                    }
                    None => {}
                }
            }
            gone.store(true, std::sync::atomic::Ordering::Relaxed);
        });
    }
    let mut sent = from;
    loop {
        let (text, end) = pty.transcript_since(sent);
        if !text.is_empty() {
            writeln!(writer, "{}", json!({ "out": text }))?;
            sent = end;
        }
        if let Ok(Some(code)) = pty.try_wait_exit() {
            // Let the last output arrive before reporting the exit.
            std::thread::sleep(Duration::from_millis(50));
            let (text, _) = pty.transcript_since(sent);
            if !text.is_empty() {
                writeln!(writer, "{}", json!({ "out": text }))?;
            }
            writeln!(writer, "{}", json!({ "exit": code }))?;
            return Ok(());
        }
        if gone.load(std::sync::atomic::Ordering::Relaxed) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_events_round_trip() {
        for event in [ClientEvent::Input(vec![0, 27, 200, 10]), ClientEvent::Resize(120, 40), ClientEvent::Kill] {
            assert_eq!(ClientEvent::from_json(&event.to_json()), Some(event));
        }
        assert_eq!(ClientEvent::from_json(&json!({ "other": 1 })), None);
    }
}
