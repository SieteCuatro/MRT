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
        })
    }

    fn running(&self) -> usize {
        self.jobs.lock().map(|j| j.values().filter(|j| j.done.is_none()).count()).unwrap_or(0)
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
                self.shutdown.notify_waiters();
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

async fn serve_connection(stream: TcpStream, token: &str, state: &Arc<HostState>) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let auth: Value = serde_json::from_str(&lines.next_line().await?.unwrap_or_default())?;
    if !auth[AUTH_KEY].as_str().is_some_and(|t| tokens_match(t, token)) {
        bail!("bad token");
    }
    let request: HostRequest = serde_json::from_str(&lines.next_line().await?.unwrap_or_default())?;
    let reply = match state.handle(request).await {
        Ok(value) => json!({ "ok": value }),
        Err(e) => json!({ "error": e.to_string() }),
    };
    let mut line = reply.to_string();
    line.push('\n');
    write.write_all(line.as_bytes()).await?;
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
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(10), serve_connection(stream, &token, &state)).await;
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
    directories::ProjectDirs::from("", "", "MRT").map(|d| d.data_local_dir().join("session-host.json"))
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

    pub async fn jobs(&self) -> Result<Vec<JobInfo>> {
        Ok(serde_json::from_value(self.request(&HostRequest::Jobs).await?)?)
    }

    /// Follows a job: copies new output into `output` until it is done, then returns
    /// the result and the full output.
    pub async fn follow(&self, block_id: Uuid, output: &Arc<Mutex<String>>) -> Result<(String, JobDone)> {
        let mut next = 0usize;
        let mut full = String::new();
        let mut failures = 0;
        loop {
            match self.request(&HostRequest::Poll { block_id, from: next }).await {
                Ok(value) => {
                    failures = 0;
                    let reply: PollReply = serde_json::from_value(value)?;
                    if let Some(done) = reply.done {
                        // The final text replaces what was streamed.
                        let all = self.request(&HostRequest::Poll { block_id, from: 0 }).await?;
                        let all: PollReply = serde_json::from_value(all)?;
                        let _ = self.request(&HostRequest::Forget { block_id }).await;
                        return Ok((all.output, done));
                    }
                    if !reply.output.is_empty() {
                        full.push_str(&reply.output);
                        if let Ok(mut pending) = output.lock() {
                            pending.push_str(&reply.output);
                        }
                    }
                    next = reply.next;
                }
                Err(e) => {
                    failures += 1;
                    if failures > 50 {
                        return Err(e.context("the session host stopped answering"));
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
    }
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
