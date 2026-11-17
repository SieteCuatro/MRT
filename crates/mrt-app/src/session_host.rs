//! The session host: a separate MRT process that owns the tabs' shells.
//!
//! With `terminal.session_server = true` the window starts `mrt session-host`
//! (detached, no console) and runs every local tab command through it. The host keeps
//! one persistent shell per tab and shell (the same `ShellPool` the window would use),
//! collects each command's output, and keeps running when the window closes or
//! crashes. A window that starts again reattaches: it asks for the jobs of its tabs
//! (layouts keep tab ids) and shows them, running or finished, with all their output.
//! Shell state (`cd`, variables, activated environments) carries over between
//! windows, too.
//!
//! Protocol: localhost TCP, one request per connection. The client sends
//! `{"mrt_host_auth": "<token>"}` and one JSON request on two lines and gets one
//! JSON line back. The port and token are in `session-host.json` in the data folder
//! (`0600` on Unix). The host exits after 30 idle minutes with nothing running.

use crate::shell_pool::ShellPool;
use anyhow::{anyhow, bail, Context, Result};
use mrt_core::ShellProfile;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

const AUTH_KEY: &str = "mrt_host_auth";
const IDLE_EXIT: Duration = Duration::from_secs(30 * 60);
/// Finished jobs nobody collected are dropped after this long.
const FINISHED_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostRequest {
    Run { tab_id: Uuid, block_id: Uuid, shell: ShellProfile, command: String, cwd: PathBuf },
    /// Output from byte offset `from`, and the result once the job is done.
    Poll { block_id: Uuid, from: usize },
    Stop { block_id: Uuid },
    Jobs,
    /// The window has the finished block; the host may drop it.
    Forget { block_id: Uuid },
    CloseTab { tab_id: Uuid },
    Shutdown,
    /// Starts a PTY owned by the host (full-screen programs, agents' PTYs).
    PtySpawn { shell: ShellProfile, cwd: PathBuf, cols: u16, rows: u16, command: Option<String>, owner: Option<String> },
    PtyList,
    /// Drops an exited PTY.
    PtyForget { pty_id: Uuid },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JobDone {
    pub exit_code: i32,
    pub duration_ms: u64,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JobInfo {
    pub block_id: Uuid,
    pub tab_id: Uuid,
    pub command: String,
    pub shell_name: String,
    pub cwd: PathBuf,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PollReply {
    pub output: String,
    pub next: usize,
    pub done: Option<JobDone>,
}

struct Job {
    info: JobInfo,
    output: String,
    done: Option<JobDone>,
    finished_at: Option<Instant>,
}

/// The host's state.
pub struct HostState {
    pool: ShellPool,
    jobs: Mutex<HashMap<Uuid, Job>>,
    pids: Arc<Mutex<HashMap<Uuid, u32>>>,
    sampler: mrt_tools::SharedProcessSampler,
    last_request: Mutex<Instant>,
    shutdown: tokio::sync::Notify,
    ptys: Mutex<HashMap<Uuid, (Arc<mrt_core::InteractivePty>, mrt_core::pty_host::HostedPtyInfo)>>,
}

impl HostState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            pool: ShellPool::new(true),
            jobs: Mutex::new(HashMap::new()),
            pids: Arc::new(Mutex::new(HashMap::new())),
            sampler: mrt_tools::SharedProcessSampler::new(),
            last_request: Mutex::new(Instant::now()),
            shutdown: tokio::sync::Notify::new(),
            ptys: Mutex::new(HashMap::new()),
        })
    }

    fn running(&self) -> usize {
        let jobs = self.jobs.lock().map(|j| j.values().filter(|j| j.done.is_none()).count()).unwrap_or(0);
        let ptys = self
            .ptys
            .lock()
            .map(|p| p.values().filter(|(pty, _)| !matches!(pty.try_wait_exit(), Ok(Some(_)))).count())
            .unwrap_or(0);
        jobs + ptys
    }

    fn pty(&self, id: Uuid) -> Option<Arc<mrt_core::InteractivePty>> {
        self.ptys.lock().ok()?.get(&id).map(|(pty, _)| pty.clone())
    }

    async fn handle(self: &Arc<Self>, request: HostRequest) -> Result<Value> {
        if let Ok(mut last) = self.last_request.lock() {
            *last = Instant::now();
        }
        match request {
            HostRequest::Run { tab_id, block_id, shell, command, cwd } => {
                let info = JobInfo { block_id, tab_id, command: command.clone(), shell_name: shell.kind.short_label().to_string(), cwd: cwd.clone(), done: false };
                {
                    let mut jobs = self.jobs.lock().map_err(|_| anyhow!("lock"))?;
                    if jobs.contains_key(&block_id) {
                        return Ok(json!({ "ok": true, "already": true }));
                    }
                    jobs.insert(block_id, Job { info, output: String::new(), done: None, finished_at: None });
                }
                let state = self.clone();
                tokio::spawn(async move { state.run_job(tab_id, block_id, shell, command, cwd).await });
                Ok(json!({ "ok": true }))
            }
            HostRequest::Poll { block_id, from } => {
                let jobs = self.jobs.lock().map_err(|_| anyhow!("lock"))?;
                let job = jobs.get(&block_id).ok_or_else(|| anyhow!("unknown job"))?;
                let mut start = from.min(job.output.len());
                while !job.output.is_char_boundary(start) {
                    start -= 1;
                }
                let reply = PollReply { output: job.output[start..].to_string(), next: job.output.len(), done: job.done.clone() };
                Ok(serde_json::to_value(reply)?)
            }
            HostRequest::Stop { block_id } => {
                self.stop(block_id).await;
                Ok(json!({ "ok": true }))
            }
            HostRequest::Jobs => {
                let jobs = self.jobs.lock().map_err(|_| anyhow!("lock"))?;
                let list: Vec<JobInfo> = jobs.values().map(|j| JobInfo { done: j.done.is_some(), ..j.info.clone() }).collect();
                Ok(serde_json::to_value(list)?)
            }
            HostRequest::Forget { block_id } => {
                if let Ok(mut jobs) = self.jobs.lock() {
                    if jobs.get(&block_id).is_some_and(|j| j.done.is_some()) {
                        jobs.remove(&block_id);
                    }
                }
                Ok(json!({ "ok": true }))
            }
            HostRequest::CloseTab { tab_id } => {
                self.pool.close_tab(tab_id);
                Ok(json!({ "ok": true }))
            }
            HostRequest::Shutdown => {
                if let Ok(ptys) = self.ptys.lock() {
                    for (pty, _) in ptys.values() {
                        let _ = pty.kill();
                    }
                }
                self.shutdown.notify_waiters();
                Ok(json!({ "ok": true }))
            }
            HostRequest::PtySpawn { shell, cwd, cols, rows, command, owner } => {
                let pty = mrt_core::InteractivePty::spawn_local(&shell, &cwd, cols, rows, command.as_deref())?;
                let info = mrt_core::pty_host::HostedPtyInfo {
                    pty_id: pty.id,
                    shell_id: shell.id.clone(),
                    shell_name: shell.name.clone(),
                    cwd,
                    cols,
                    rows,
                    command,
                    owner,
                    exited: false,
                };
                self.ptys.lock().map_err(|_| anyhow!("lock"))?.insert(pty.id, (Arc::new(pty), info.clone()));
                Ok(serde_json::to_value(info)?)
            }
            HostRequest::PtyList => {
                let ptys = self.ptys.lock().map_err(|_| anyhow!("lock"))?;
                let list: Vec<mrt_core::pty_host::HostedPtyInfo> = ptys
                    .values()
                    .map(|(pty, info)| mrt_core::pty_host::HostedPtyInfo {
                        exited: matches!(pty.try_wait_exit(), Ok(Some(_))),
                        cols: pty.size().0,
                        rows: pty.size().1,
                        ..info.clone()
                    })
                    .collect();
                Ok(serde_json::to_value(list)?)
            }
            HostRequest::PtyForget { pty_id } => {
                if let Ok(mut ptys) = self.ptys.lock() {
                    if ptys.get(&pty_id).is_some_and(|(pty, _)| matches!(pty.try_wait_exit(), Ok(Some(_)))) {
                        ptys.remove(&pty_id);
                    }
                }
                Ok(json!({ "ok": true }))
            }
        }
    }

    async fn run_job(self: Arc<Self>, tab_id: Uuid, block_id: Uuid, shell: ShellProfile, command: String, cwd: PathBuf) {
        let output = Arc::new(Mutex::new(String::new()));
        // Copy streamed output into the job as it arrives.
        let pump = {
            let (state, output) = (self.clone(), output.clone());
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    let chunk = output.lock().map(|mut o| std::mem::take(&mut *o)).unwrap_or_default();
                    if !chunk.is_empty() {
                        if let Ok(mut jobs) = state.jobs.lock() {
                            if let Some(job) = jobs.get_mut(&block_id) {
                                job.output.push_str(&chunk);
                            }
                        }
                    }
                }
            })
        };
        let started = Instant::now();
        let result = match crate::shell_pool::run_with_pool(&self.pool, tab_id, block_id, &shell, &command, &cwd, &output, &self.pids).await {
            Some(Ok(run)) => (run.output, run.exit_code, run.duration_ms, run.cwd),
            Some(Err(e)) => (format!("Error: {e}"), 1, 0, None),
            None => {
                let pids = self.pids.clone();
                let on_spawn = move |pid: u32| {
                    if let Ok(mut p) = pids.lock() {
                        p.insert(block_id, pid);
                    }
                };
                let streamed = output.clone();
                match mrt_core::execute_command_streaming_with_pid(&shell, &command, &cwd, on_spawn, move |chunk| {
                    if let Ok(mut o) = streamed.lock() {
                        o.push_str(chunk);
                    }
                })
                .await
                {
                    Ok(res) => (res.combined_output, res.exit_code, res.duration_ms, res.updated_cwd),
                    Err(e) => (format!("Error: {e}"), 1, started.elapsed().as_millis() as u64, None),
                }
            }
        };
        pump.abort();
        if let Ok(mut pids) = self.pids.lock() {
            pids.remove(&block_id);
        }
        let (text, exit_code, duration_ms, new_cwd) = result;
        if let Ok(mut jobs) = self.jobs.lock() {
            if let Some(job) = jobs.get_mut(&block_id) {
                // The final text is authoritative (the shell's own capture of the output).
                job.output = text;
                job.done = Some(JobDone { exit_code, duration_ms, cwd: new_cwd });
                job.finished_at = Some(Instant::now());
            }
            jobs.retain(|_, j| j.finished_at.is_none_or(|t| t.elapsed() < FINISHED_TTL));
        }
    }

    /// Ctrl+C in a persistent shell, then its child processes; or the command's own
    /// process tree.
    async fn stop(&self, block_id: Uuid) {
        if let Some(session) = self.pool.interrupt(block_id) {
            if let Some(shell_pid) = session.process_id() {
                tokio::time::sleep(Duration::from_millis(1000)).await;
                if self.pool.is_running(block_id) {
                    if let Ok(snapshot) = self.sampler.snapshot().await {
                        let children: Vec<u32> = snapshot.children_of(shell_pid).iter().map(|e| e.pid).collect();
                        if !children.is_empty() {
                            let _ = self.sampler.terminate(children, true, true).await;
                        }
                    }
                }
            }
            return;
        }
        let pid = self.pids.lock().ok().and_then(|p| p.get(&block_id).copied());
        if let Some(pid) = pid {
            let _ = self.sampler.terminate(vec![pid], true, true).await;
        }
    }
}

fn tokens_match(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// One connection, on a blocking thread: a request and its reply, or a PTY event
/// stream that lasts until the program exits or the client leaves.
fn serve_connection(stream: std::net::TcpStream, token: &str, state: &Arc<HostState>, runtime: tokio::runtime::Handle) -> Result<()> {
    use std::io::{BufRead, Write};
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut writer = stream.try_clone()?;
    let mut reader = std::io::BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let auth: Value = serde_json::from_str(&line)?;
    if !auth[AUTH_KEY].as_str().is_some_and(|t| tokens_match(t, token)) {
        bail!("bad token");
    }
    line.clear();
    reader.read_line(&mut line)?;
    let request: Value = serde_json::from_str(&line)?;
    if request["op"] == "job_stream" {
        // Event stream for a command: {"out"} lines as output appears, then {"done"}.
        let id: Uuid = serde_json::from_value(request["block_id"].clone())?;
        let mut sent = request["from"].as_u64().unwrap_or(0) as usize;
        stream.set_read_timeout(None)?;
        loop {
            let (chunk, done) = {
                let jobs = state.jobs.lock().map_err(|_| anyhow!("lock"))?;
                let Some(job) = jobs.get(&id) else {
                    writeln!(writer, "{}", json!({ "error": "unknown job" }))?;
                    return Ok(());
                };
                let mut start = sent.min(job.output.len());
                while !job.output.is_char_boundary(start) {
                    start -= 1;
                }
                let chunk = job.output[start..].to_string();
                sent = job.output.len();
                (chunk, job.done.clone().map(|d| (d, job.output.clone())))
            };
            if let Some((done, full)) = done {
                writeln!(writer, "{}", json!({ "done": done, "output": full }))?;
                return Ok(());
            }
            if !chunk.is_empty() {
                writeln!(writer, "{}", json!({ "out": chunk }))?;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    if request["op"] == "pty_stream" {
        if let Ok(mut last) = state.last_request.lock() {
            *last = Instant::now();
        }
        let id: Uuid = serde_json::from_value(request["pty_id"].clone())?;
        let from = request["from"].as_u64().unwrap_or(0) as usize;
        let Some(pty) = state.pty(id) else {
            writeln!(writer, "{}", json!({ "error": "unknown PTY" }))?;
            return Ok(());
        };
        stream.set_read_timeout(None)?;
        return mrt_core::pty_host::serve_stream(writer, reader, pty, from);
    }
    let request: HostRequest = serde_json::from_value(request)?;
    let reply = match runtime.block_on(state.handle(request)) {
        Ok(value) => json!({ "ok": value }),
        Err(e) => json!({ "error": e.to_string() }),
    };
    writeln!(writer, "{reply}")?;
    Ok(())
}

/// Serves `state` on `listener` with `token` until shutdown (or idle exit when `idle_exit`).
pub async fn serve(listener: TcpListener, token: String, state: Arc<HostState>, idle_exit: bool) {
    let token = Arc::new(token);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let (token, state) = (token.clone(), state.clone());
                let Ok(stream) = stream.into_std() else { continue };
                let runtime = tokio::runtime::Handle::current();
                tokio::task::spawn_blocking(move || {
                    let _ = serve_connection(stream, &token, &state, runtime);
                });
            }
            _ = state.shutdown.notified() => return,
            _ = tokio::time::sleep(Duration::from_secs(60)), if idle_exit => {
                let idle = state.last_request.lock().map(|t| t.elapsed()).unwrap_or_default();
                if idle >= IDLE_EXIT && state.running() == 0 {
                    return;
                }
            }
        }
    }
}

/// `session-host.json` in the data folder.
pub fn host_file() -> Option<PathBuf> {
    mrt_core::data_dir().map(|dir| dir.join("session-host.json"))
}

/// 64 hex characters from two random (v4) UUIDs.
fn random_token() -> String {
    let a = Uuid::new_v4().simple().to_string();
    let b = Uuid::new_v4().simple().to_string();
    format!("{a}{b}")
}

/// `mrt session-host`: runs the host until shutdown or idle exit.
pub fn run_host_process() -> Result<()> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let token = random_token();
        let path = host_file().ok_or_else(|| anyhow!("no data folder"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        let mut file = options.open(&path)?;
        file.write_all(json!({ "port": port, "token": token, "pid": std::process::id() }).to_string().as_bytes())?;
        file.sync_all()?;
        drop(file);
        serve(listener, token, HostState::new(), true).await;
        // Only remove the file if it is still ours.
        if std::fs::read_to_string(&path).is_ok_and(|t| t.contains(&format!("\"port\":{port}"))) {
            let _ = std::fs::remove_file(&path);
        }
        Ok(())
    })
}

/// A connection to a session host.
#[derive(Debug, Clone)]
pub struct HostClient {
    pub port: u16,
    pub token: String,
}

impl HostClient {
    pub async fn request(&self, request: &HostRequest) -> Result<Value> {
        let stream = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(("127.0.0.1", self.port))).await??;
        let (read, mut write) = stream.into_split();
        let mut payload = json!({ AUTH_KEY: self.token }).to_string();
        payload.push('\n');
        payload.push_str(&serde_json::to_string(request)?);
        payload.push('\n');
        write.write_all(payload.as_bytes()).await?;
        let mut lines = BufReader::new(read).lines();
        let line = tokio::time::timeout(Duration::from_secs(30), lines.next_line()).await??.ok_or_else(|| anyhow!("host closed"))?;
        let reply: Value = serde_json::from_str(&line)?;
        if let Some(error) = reply["error"].as_str() {
            bail!("{error}");
        }
        Ok(reply["ok"].clone())
    }

    /// The endpoint for [`mrt_core::pty_host`].
    pub fn endpoint(&self) -> mrt_core::pty_host::HostEndpoint {
        mrt_core::pty_host::HostEndpoint { port: self.port, token: self.token.clone() }
    }

    pub async fn jobs(&self) -> Result<Vec<JobInfo>> {
        Ok(serde_json::from_value(self.request(&HostRequest::Jobs).await?)?)
    }

    /// Follows a job over an event stream (the host pushes output as it appears),
    /// copying new output into `output` until it is done; returns the full output and
    /// the result. A dropped stream is resumed from the last byte received.
    pub async fn follow(&self, block_id: Uuid, output: &Arc<Mutex<String>>) -> Result<(String, JobDone)> {
        let mut received = 0usize;
        let mut failures = 0;
        loop {
            match self.stream_job(block_id, received, output).await {
                Ok(StreamEnd::Done(full, done)) => {
                    let _ = self.request(&HostRequest::Forget { block_id }).await;
                    return Ok((full, done));
                }
                Ok(StreamEnd::Dropped(at)) => received = at,
                Err(e) => {
                    failures += 1;
                    if failures > 20 {
                        return Err(e.context("the session host stopped answering"));
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            }
        }
    }

    async fn stream_job(&self, block_id: Uuid, from: usize, output: &Arc<Mutex<String>>) -> Result<StreamEnd> {
        let stream = tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(("127.0.0.1", self.port))).await??;
        let (read, mut write) = stream.into_split();
        let mut payload = json!({ AUTH_KEY: self.token }).to_string();
        payload.push('\n');
        payload.push_str(&json!({ "op": "job_stream", "block_id": block_id, "from": from }).to_string());
        payload.push('\n');
        write.write_all(payload.as_bytes()).await?;
        let mut lines = BufReader::new(read).lines();
        let mut at = from;
        while let Some(line) = lines.next_line().await? {
            let event: Value = serde_json::from_str(&line)?;
            if let Some(error) = event["error"].as_str() {
                bail!("{error}");
            }
            if let Some(text) = event["out"].as_str() {
                at += text.len();
                if let Ok(mut pending) = output.lock() {
                    pending.push_str(text);
                }
            } else if event.get("done").is_some() {
                let done: JobDone = serde_json::from_value(event["done"].clone())?;
                return Ok(StreamEnd::Done(event["output"].as_str().unwrap_or_default().to_string(), done));
            }
        }
        Ok(StreamEnd::Dropped(at))
    }
}

enum StreamEnd {
    Done(String, JobDone),
    Dropped(usize),
}

/// Reads the host file and checks the host answers.
pub async fn connect_existing() -> Option<HostClient> {
    let text = std::fs::read_to_string(host_file()?).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let client = HostClient { port: value["port"].as_u64()? as u16, token: value["token"].as_str()?.to_string() };
    client.jobs().await.ok()?;
    Some(client)
}

/// Connects to the session host, starting `mrt session-host` (detached, outliving
/// this window) when none runs.
pub async fn ensure_host() -> Result<HostClient> {
    if let Some(client) = connect_existing().await {
        return Ok(client);
    }
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("session-host").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
        cmd.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0800_0000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().context("cannot start the session host")?;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Some(client) = connect_existing().await {
            return Ok(client);
        }
    }
    bail!("the session host did not start")
}

#[cfg(test)]
pub(crate) async fn start_in_process() -> HostClient {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let token = random_token();
    tokio::spawn(serve(listener, token.clone(), HostState::new(), false));
    HostClient { port, token }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_shell() -> ShellProfile {
        let shells = mrt_core::discover_shells();
        shells
            .iter()
            .find(|s| matches!(s.kind, mrt_core::ShellKind::CmdBatch | mrt_core::ShellKind::Bash | mrt_core::ShellKind::Sh))
            .cloned()
            .unwrap_or_else(|| shells[0].clone())
    }

    #[test]
    fn jobs_keep_running_when_the_client_goes_away() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let host = start_in_process().await;
            let (tab_id, block_id) = (Uuid::new_v4(), Uuid::new_v4());
            let command = if cfg!(windows) { "ping -n 2 127.0.0.1 >nul & echo host-survived" } else { "sleep 1; echo host-survived" };
            host.request(&HostRequest::Run { tab_id, block_id, shell: test_shell(), command: command.into(), cwd: std::env::temp_dir() })
                .await
                .unwrap();
            // The window "crashes" here: nobody follows the job for a while.
            tokio::time::sleep(Duration::from_millis(300)).await;
            let reattached = HostClient { port: host.port, token: host.token.clone() };
            let jobs = reattached.jobs().await.unwrap();
            assert!(jobs.iter().any(|j| j.block_id == block_id && j.tab_id == tab_id), "{jobs:?}");
            let output = Arc::new(Mutex::new(String::new()));
            let (text, done) = tokio::time::timeout(Duration::from_secs(60), reattached.follow(block_id, &output)).await.unwrap().unwrap();
            assert_eq!(done.exit_code, 0, "{text}");
            assert!(text.contains("host-survived"), "{text}");
            assert!(reattached.jobs().await.unwrap().is_empty(), "collected jobs are forgotten");
            assert!(HostClient { port: host.port, token: "wrong".into() }.jobs().await.is_err());
        });
    }

    fn wait_for(pty: &mrt_core::InteractivePty, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if pty.read_transcript().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn hosted_ptys_outlive_their_window_and_replay_on_reattach() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let host = rt.block_on(start_in_process());
        let endpoint = host.endpoint();
        let shell = test_shell();
        let owner = r#"{"kind":"tui","command":"shell"}"#;
        let pty = mrt_core::pty_host::spawn_in_host(&endpoint, &shell, &std::env::temp_dir(), 80, 24, None, Some(owner)).unwrap();
        assert!(pty.is_hosted());
        pty.send_line("echo hosted-one").unwrap();
        assert!(wait_for(&pty, "hosted-one"), "{}", pty.read_transcript());
        let id = pty.id;
        drop(pty); // the window goes away; the program keeps running in the host

        let list = mrt_core::pty_host::list(&endpoint).unwrap();
        let info = list.iter().find(|i| i.pty_id == id).expect("still in the host").clone();
        assert!(!info.exited);
        assert_eq!(info.owner.as_deref(), Some(owner));

        let again = mrt_core::pty_host::attach(&endpoint, &info).unwrap();
        assert!(wait_for(&again, "hosted-one"), "the whole session is replayed: {}", again.read_transcript());
        again.resize(100, 30).unwrap();
        again.send_line("echo hosted-two").unwrap();
        assert!(wait_for(&again, "hosted-two"));
        again.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while again.try_wait_exit().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(again.try_wait_exit().unwrap().is_some(), "the exit reaches the window");
        mrt_core::pty_host::request(&endpoint, &serde_json::json!({ "op": "pty_forget", "pty_id": id })).unwrap();
        assert!(mrt_core::pty_host::list(&endpoint).unwrap().iter().all(|i| i.pty_id != id));
    }

    #[test]
    fn shell_state_carries_over_between_windows() {
        let shell = mrt_core::discover_shells().into_iter().find(|s| matches!(s.kind, mrt_core::ShellKind::Bash | mrt_core::ShellKind::Sh));
        let Some(shell) = shell else { return };
        if cfg!(windows) {
            return; // Windows `bash` may be WSL's, with its own paths.
        }
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let host = start_in_process().await;
            let tab_id = Uuid::new_v4();
            let run = |cmd: &str| HostRequest::Run { tab_id, block_id: Uuid::new_v4(), shell: shell.clone(), command: cmd.into(), cwd: std::env::temp_dir() };
            let first = run("export MRT_HOST_VAR=kept");
            let HostRequest::Run { block_id: b1, .. } = first.clone() else { unreachable!() };
            host.request(&first).await.unwrap();
            host.follow(b1, &Arc::new(Mutex::new(String::new()))).await.unwrap();
            let second = run("echo value=$MRT_HOST_VAR");
            let HostRequest::Run { block_id: b2, .. } = second.clone() else { unreachable!() };
            host.request(&second).await.unwrap();
            let (text, _) = host.follow(b2, &Arc::new(Mutex::new(String::new()))).await.unwrap();
            assert!(text.contains("value=kept"), "{text}");
        });
    }
}
