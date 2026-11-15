//! Cross-platform process inspector and task manager backend (`:ps`, `:top`, `:proc`,
//! `:kill`, `:watch-proc`).
//!
//! Process data comes from the `sysinfo` crate, so Windows, macOS and Linux behave the same
//! without spawning `ps` / `tasklist`. The only external command is the graceful
//! `taskkill /PID <pid>` on Windows, spawned with the `CREATE_NO_WINDOW` guard.

use crate::net_inspector::{list_listening_ports, PortEntry};
use anyhow::{anyhow, bail, Context, Result};
use mrt_core::{is_protected_process_name, RiskLevel, SafetyAssessment, StructuredOutput};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use sysinfo::{
    Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System, UpdateKind, Users,
};

/// Shell name stamped on every block produced by the process tools.
pub const PROCESS_BLOCK_SHELL: &str = "mrt-proc";

/// Headers of the `:ps` / `:top` table. `entries_from_table` relies on these names.
pub const PROCESS_TABLE_HEADERS: [&str; 9] = [
    "PID", "PPID", "Name", "CPU %", "Memory", "User", "Status", "MRT", "Command",
];

const TREE_MARKER: &str = "└ ";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessEntry {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub name: String,
    pub exe: Option<String>,
    pub command_line: String,
    pub user: Option<String>,
    /// CPU usage as a share of the whole machine (0–100), not per core.
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    /// Seconds since the Unix epoch.
    pub start_time: u64,
    pub run_time_secs: u64,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSnapshot {
    pub taken_at_ms: u64,
    /// PID of the process that took the snapshot (MRT itself).
    pub current_pid: u32,
    pub current_user: Option<String>,
    pub logical_cpus: usize,
    pub total_memory_bytes: u64,
    pub entries: Vec<ProcessEntry>,
}

impl ProcessSnapshot {
    /// Looks up a PID. Snapshots from the sampler are sorted by PID, so this is a binary
    /// search, with a linear fallback for hand-built (unsorted) snapshots.
    pub fn get(&self, pid: u32) -> Option<&ProcessEntry> {
        match self.entries.binary_search_by_key(&pid, |e| e.pid) {
            Ok(idx) => Some(&self.entries[idx]),
            Err(_) => self.entries.iter().find(|e| e.pid == pid),
        }
    }

    pub fn children_of(&self, pid: u32) -> Vec<&ProcessEntry> {
        self.entries
            .iter()
            .filter(|e| e.parent_pid == Some(pid) && e.pid != pid)
            .collect()
    }

    /// All descendants of `pid`, deepest first (the order to stop a process tree in),
    /// each with its depth below `pid` (children are depth 1).
    pub fn descendants(&self, pid: u32) -> Vec<(usize, &ProcessEntry)> {
        let mut out = Vec::new();
        let mut visited = HashSet::from([pid]);
        let mut frontier = vec![pid];
        let mut depth = 0;
        while !frontier.is_empty() {
            depth += 1;
            let mut next = Vec::new();
            for parent in frontier {
                for child in self.children_of(parent) {
                    if visited.insert(child.pid) {
                        out.push((depth, child));
                        next.push(child.pid);
                    }
                }
            }
            frontier = next;
        }
        out.sort_by(|a, b| b.0.cmp(&a.0));
        out
    }

    /// Parent chain of `pid`, nearest first. Cycle-safe (PIDs are reused on Windows).
    pub fn ancestors(&self, pid: u32) -> Vec<&ProcessEntry> {
        let mut out = Vec::new();
        let mut visited = HashSet::from([pid]);
        let mut current = self.get(pid).and_then(|e| e.parent_pid);
        while let Some(parent_pid) = current {
            if !visited.insert(parent_pid) {
                break;
            }
            let Some(parent) = self.get(parent_pid) else {
                break;
            };
            out.push(parent);
            current = parent.parent_pid;
        }
        out
    }

    pub fn is_descendant_of(&self, pid: u32, ancestor: u32) -> bool {
        self.ancestors(pid).iter().any(|e| e.pid == ancestor)
    }

    pub fn total_cpu_percent(&self) -> f32 {
        self.entries.iter().map(|e| e.cpu_percent).sum::<f32>().min(100.0)
    }

    pub fn used_memory_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.memory_bytes).sum()
    }
}

// ---------------------------------------------------------------------------
// MRT-owned processes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnedProcessKind {
    /// A command block running in a tab.
    Command,
    /// A full-screen TUI overlay (`:tui`, `vim`, `htop`, ...).
    Tui,
    /// An interactive PTY session started through MCP (`mrt_pty_spawn`).
    Pty,
}

/// A process MRT started, with the tab/pane it belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedProcess {
    pub pid: u32,
    pub label: String,
    pub tab_id: Option<String>,
    pub kind: OwnedProcessKind,
}

pub type OwnedProcessMap = HashMap<u32, OwnedProcess>;

/// Describes which part of MRT owns `pid`: a registered command/TUI/PTY, one of their
/// descendants, MRT itself, or another child of the MRT process.
pub fn owner_label(snapshot: &ProcessSnapshot, pid: u32, owned: &OwnedProcessMap) -> Option<String> {
    if let Some(own) = owned.get(&pid) {
        return Some(own.label.clone());
    }
    if pid == snapshot.current_pid {
        return Some("MRT (this app)".into());
    }
    let ancestors = snapshot.ancestors(pid);
    for ancestor in &ancestors {
        if let Some(own) = owned.get(&ancestor.pid) {
            return Some(format!("{} › child", own.label));
        }
    }
    if ancestors.iter().any(|a| a.pid == snapshot.current_pid) {
        return Some("MRT child process".into());
    }
    None
}

// ---------------------------------------------------------------------------
// Sampling
// ---------------------------------------------------------------------------

/// Keeps a `sysinfo::System` alive between refreshes so CPU usage can be computed
/// (CPU % needs two samples at least `MINIMUM_CPU_UPDATE_INTERVAL` apart).
pub struct ProcessSampler {
    system: System,
    users: Option<Users>,
    users_refreshed: Option<Instant>,
    last_refresh: Option<Instant>,
    current_pid: u32,
}

impl ProcessSampler {
    pub fn new() -> Result<Self> {
        let system = System::new().map_err(|e| anyhow!("Process inspection unavailable: {e}"))?;
        let current_pid = sysinfo::get_current_pid()
            .map(|p| p.as_u32())
            .unwrap_or_else(|_| std::process::id());
        Ok(Self {
            system,
            users: None,
            users_refreshed: None,
            last_refresh: None,
            current_pid,
        })
    }

    fn refresh_kind() -> ProcessRefreshKind {
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_user(UpdateKind::OnlyIfNotSet)
            .without_tasks()
    }

    fn refresh(&mut self) {
        self.system
            .refresh_processes_specifics(ProcessesToUpdate::All, true, Self::refresh_kind());
        self.last_refresh = Some(Instant::now());
    }

    fn refresh_users_if_stale(&mut self) {
        let stale = self
            .users_refreshed
            .map(|t| t.elapsed() > Duration::from_secs(60))
            .unwrap_or(true);
        if stale {
            self.users = Users::new_with_refreshed_list().ok();
            self.users_refreshed = Some(Instant::now());
        }
    }

    /// Refreshes all processes and returns a snapshot. The first call (or a call long after
    /// the previous one) samples twice so CPU percentages are meaningful.
    pub fn snapshot(&mut self) -> ProcessSnapshot {
        let min = sysinfo::MINIMUM_CPU_UPDATE_INTERVAL;
        match self.last_refresh.map(|t| t.elapsed()) {
            None => {
                self.refresh();
                std::thread::sleep(min);
            }
            Some(elapsed) if elapsed > Duration::from_secs(30) => {
                self.refresh();
                std::thread::sleep(min);
            }
            Some(elapsed) if elapsed < min => std::thread::sleep(min - elapsed),
            Some(_) => {}
        }
        self.refresh();
        self.refresh_users_if_stale();
        self.build_snapshot()
    }

    fn build_snapshot(&self) -> ProcessSnapshot {
        let logical_cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .max(1);
        let mut entries: Vec<ProcessEntry> = self
            .system
            .processes()
            .values()
            .filter(|p| p.thread_kind().is_none())
            .map(|p| {
                let name = p.name().to_string_lossy().to_string();
                let exe = p.exe().map(|e| e.display().to_string());
                let mut command_line = p
                    .cmd()
                    .iter()
                    .map(|a| a.to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                if command_line.trim().is_empty() {
                    command_line = exe.clone().unwrap_or_else(|| name.clone());
                }
                let user = p.user_id().and_then(|uid| {
                    self.users
                        .as_ref()
                        .and_then(|users| users.get_user_by_id(uid))
                        .map(|u| u.name().to_string())
                });
                ProcessEntry {
                    pid: p.pid().as_u32(),
                    parent_pid: p.parent().map(|pp| pp.as_u32()),
                    name,
                    exe,
                    command_line,
                    user,
                    cpu_percent: (p.cpu_usage() / logical_cpus as f32).clamp(0.0, 100.0),
                    memory_bytes: p.memory(),
                    start_time: p.start_time(),
                    run_time_secs: p.run_time(),
                    status: format!("{:?}", p.status()),
                }
            })
            .collect();
        entries.sort_by_key(|e| e.pid);
        let current_user = entries
            .iter()
            .find(|e| e.pid == self.current_pid)
            .and_then(|e| e.user.clone());
        ProcessSnapshot {
            taken_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
            current_pid: self.current_pid,
            current_user,
            logical_cpus,
            total_memory_bytes: 0,
            entries,
        }
    }

    /// Working directory of a process, where the OS allows reading it.
    pub fn working_directory(&mut self, pid: u32) -> Option<String> {
        let pids = [Pid::from_u32(pid)];
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&pids),
            false,
            ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always).without_tasks(),
        );
        self.system
            .process(Pid::from_u32(pid))
            .and_then(|p| p.cwd())
            .map(|c| c.display().to_string())
            .filter(|c| !c.is_empty())
    }

    fn is_alive(&mut self, pid: u32) -> bool {
        let pids = [Pid::from_u32(pid)];
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&pids),
            true,
            ProcessRefreshKind::nothing().without_tasks(),
        );
        // A killed child that its parent has not reaped yet lingers as a zombie on Unix:
        // it no longer runs, so count it as stopped.
        self.system
            .process(Pid::from_u32(pid))
            .is_some_and(|p| !matches!(p.status(), ProcessStatus::Zombie | ProcessStatus::Dead))
    }

    /// Stops the given processes (and their descendants when `tree`), deepest first.
    /// Graceful stop sends SIGTERM (Unix) or `taskkill /PID` (Windows); `force` sends
    /// SIGKILL / `TerminateProcess`.
    pub fn terminate(&mut self, pids: &[u32], force: bool, tree: bool) -> Vec<KillOutcome> {
        let mut outcomes = self.terminate_once(pids, force, tree, &HashSet::new());
        if tree {
            // A parent can spawn a child while it is being stopped. Children keep their
            // parent PID after it exits, so sweep for such orphans a few times.
            for _ in 0..3 {
                let stopped: HashMap<u32, u64> = outcomes
                    .iter()
                    .filter(|o| o.status == KillStatus::Stopped)
                    .map(|o| (o.pid, o.start_time))
                    .collect();
                self.refresh();
                let orphans: Vec<u32> = self
                    .build_snapshot()
                    .entries
                    .iter()
                    .filter(|e| {
                        e.parent_pid
                            .and_then(|pp| stopped.get(&pp))
                            .is_some_and(|parent_start| e.start_time >= *parent_start)
                    })
                    .map(|e| e.pid)
                    .collect();
                if orphans.is_empty() {
                    break;
                }
                let already: HashSet<u32> = outcomes.iter().map(|o| o.pid).collect();
                let more = self.terminate_once(&orphans, force, true, &already);
                if more.is_empty() {
                    break;
                }
                outcomes.extend(more);
            }
        }
        outcomes
    }

    fn terminate_once(&mut self, pids: &[u32], force: bool, tree: bool, skip: &HashSet<u32>) -> Vec<KillOutcome> {
        self.refresh();
        let snapshot = self.build_snapshot();
        let mut order: Vec<(u32, String)> = Vec::new();
        let mut seen: HashSet<u32> = skip.clone();
        for &pid in pids {
            if tree {
                for (_, child) in snapshot.descendants(pid) {
                    if seen.insert(child.pid) {
                        order.push((child.pid, child.name.clone()));
                    }
                }
            }
            if seen.insert(pid) {
                let name = snapshot.get(pid).map(|e| e.name.clone()).unwrap_or_default();
                order.push((pid, name));
            }
        }

        let mut outcomes = Vec::new();
        for (pid, name) in order {
            let start_time = snapshot.get(pid).map(|e| e.start_time).unwrap_or(0);
            if pid == self.current_pid {
                outcomes.push(KillOutcome {
                    pid,
                    name,
                    status: KillStatus::Failed("Refusing to stop MRT itself".into()),
                    method: String::new(),
                    start_time,
                });
                continue;
            }
            let Some(process) = self.system.process(Pid::from_u32(pid)) else {
                outcomes.push(KillOutcome {
                    pid,
                    name,
                    status: KillStatus::NotFound,
                    method: String::new(),
                    start_time,
                });
                continue;
            };
            let (sent, method) = if force {
                (Ok(process.kill()), "force (SIGKILL / TerminateProcess)".to_string())
            } else {
                graceful_stop(process, pid)
            };
            let status = match sent {
                Err(e) => KillStatus::Failed(e),
                Ok(false) => KillStatus::Failed("The OS refused the signal (access denied?)".into()),
                Ok(true) => {
                    let deadline = Instant::now() + Duration::from_millis(2000);
                    let mut alive = self.is_alive(pid);
                    while alive && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(100));
                        alive = self.is_alive(pid);
                    }
                    if alive {
                        KillStatus::StillRunning
                    } else {
                        KillStatus::Stopped
                    }
                }
            };
            outcomes.push(KillOutcome { pid, name, status, method, start_time });
        }
        outcomes
    }
}

#[cfg(not(target_os = "windows"))]
fn graceful_stop(process: &sysinfo::Process, _pid: u32) -> (Result<bool, String>, String) {
    match process.kill_with(sysinfo::Signal::Term) {
        Some(sent) => (Ok(sent), "graceful (SIGTERM)".into()),
        None => (Ok(process.kill()), "force (SIGTERM unsupported)".into()),
    }
}

#[cfg(target_os = "windows")]
fn graceful_stop(_process: &sysinfo::Process, pid: u32) -> (Result<bool, String>, String) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    let result = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let method = "graceful (taskkill /PID)".to_string();
    match result {
        Ok(out) if out.status.success() => (Ok(true), method),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let detail = if stderr.is_empty() {
                "taskkill could not close the process gracefully".to_string()
            } else {
                stderr
            };
            (Err(format!("{detail} — retry with --force")), method)
        }
        Err(e) => (Err(format!("Failed to run taskkill: {e}")), method),
    }
}

/// Thread-safe, lazily initialised sampler shared by the GUI, the MCP server and the
/// background refresh tasks.
#[derive(Clone, Default)]
pub struct SharedProcessSampler {
    inner: Arc<Mutex<Option<ProcessSampler>>>,
}

impl SharedProcessSampler {
    pub fn new() -> Self {
        Self::default()
    }

    fn with_sampler<T>(&self, f: impl FnOnce(&mut ProcessSampler) -> T) -> Result<T> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| anyhow!("Process sampler lock poisoned"))?;
        if guard.is_none() {
            *guard = Some(ProcessSampler::new()?);
        }
        Ok(f(guard.as_mut().expect("sampler initialised above")))
    }

    pub fn snapshot_blocking(&self) -> Result<ProcessSnapshot> {
        self.with_sampler(|s| s.snapshot())
    }

    pub async fn snapshot(&self) -> Result<ProcessSnapshot> {
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.snapshot_blocking())
            .await
            .context("Process snapshot task failed")?
    }

    pub async fn terminate(&self, pids: Vec<u32>, force: bool, tree: bool) -> Result<Vec<KillOutcome>> {
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.with_sampler(|s| s.terminate(&pids, force, tree)))
            .await
            .context("Process termination task failed")?
    }

    pub async fn working_directory(&self, pid: u32) -> Option<String> {
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.with_sampler(|s| s.working_directory(pid)).ok().flatten())
            .await
            .ok()
            .flatten()
    }
}

// ---------------------------------------------------------------------------
// Queries and tables (`:ps`, `:top`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ProcessSort {
    #[default]
    Cpu,
    Memory,
    Pid,
    Name,
}

impl ProcessSort {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "cpu" => Some(Self::Cpu),
            "mem" | "memory" | "ram" => Some(Self::Memory),
            "pid" => Some(Self::Pid),
            "name" => Some(Self::Name),
            _ => None,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Memory => "mem",
            Self::Pid => "pid",
            Self::Name => "name",
        }
    }

    fn compare(&self, a: &ProcessEntry, b: &ProcessEntry) -> std::cmp::Ordering {
        match self {
            Self::Cpu => b
                .cpu_percent
                .partial_cmp(&a.cpu_percent)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.memory_bytes.cmp(&a.memory_bytes)),
            Self::Memory => b.memory_bytes.cmp(&a.memory_bytes),
            Self::Pid => a.pid.cmp(&b.pid),
            Self::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.pid.cmp(&b.pid)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ProcessQuery {
    pub filter: Option<String>,
    pub sort: ProcessSort,
    pub limit: Option<usize>,
    pub tree: bool,
    pub only_mrt: bool,
}

/// Parses `:ps [filter] [--sort cpu|mem|pid|name] [--limit N] [--tree] [--mrt]`
/// (also used for `:top`).
pub fn parse_ps_command(input: &str) -> Result<ProcessQuery> {
    let trimmed = input.trim();
    let rest = [":ps", ":top"]
        .iter()
        .find_map(|p| trimmed.strip_prefix(p))
        .unwrap_or(trimmed);
    let mut query = ProcessQuery::default();
    let mut filter_terms = Vec::new();
    let mut tokens = rest.split_whitespace();
    while let Some(tok) = tokens.next() {
        match tok {
            "--sort" | "-s" => {
                let value = tokens.next().context("--sort needs cpu, mem, pid or name")?;
                query.sort = ProcessSort::parse(value)
                    .with_context(|| format!("Unknown sort '{value}' (use cpu, mem, pid or name)"))?;
            }
            "--limit" | "-n" => {
                let value = tokens.next().context("--limit needs a number")?;
                query.limit = Some(value.parse().with_context(|| format!("Invalid --limit '{value}'"))?);
            }
            "--tree" | "-t" => query.tree = true,
            "--mrt" | "--mine" => query.only_mrt = true,
            other if other.starts_with("--") => bail!("Unknown option '{other}'"),
            other => filter_terms.push(other),
        }
    }
    if !filter_terms.is_empty() {
        query.filter = Some(filter_terms.join(" "));
    }
    Ok(query)
}

/// Case-insensitive match on PID (exact), name, or command line.
pub fn matches_filter(entry: &ProcessEntry, filter: &str) -> bool {
    let f = filter.trim().to_lowercase();
    if f.is_empty() {
        return true;
    }
    if let Ok(pid) = f.parse::<u32>() {
        if entry.pid == pid {
            return true;
        }
    }
    entry.name.to_lowercase().contains(&f) || entry.command_line.to_lowercase().contains(&f)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessRow {
    pub entry: ProcessEntry,
    pub depth: usize,
    pub owner: Option<String>,
}

/// Filters, sorts and optionally arranges processes as a tree.
pub fn query_processes(
    snapshot: &ProcessSnapshot,
    query: &ProcessQuery,
    owned: &OwnedProcessMap,
) -> Vec<ProcessRow> {
    let matched: Vec<ProcessRow> = snapshot
        .entries
        .iter()
        .filter(|e| query.filter.as_deref().map(|f| matches_filter(e, f)).unwrap_or(true))
        .map(|e| ProcessRow {
            entry: e.clone(),
            depth: 0,
            owner: owner_label(snapshot, e.pid, owned),
        })
        .filter(|r| !query.only_mrt || r.owner.is_some())
        .collect();

    let mut rows = if query.tree {
        let pids: HashSet<u32> = matched.iter().map(|r| r.entry.pid).collect();
        let mut by_parent: HashMap<Option<u32>, Vec<&ProcessRow>> = HashMap::new();
        for row in &matched {
            let parent = row
                .entry
                .parent_pid
                .filter(|pp| pids.contains(pp) && *pp != row.entry.pid);
            by_parent.entry(parent).or_default().push(row);
        }
        for children in by_parent.values_mut() {
            children.sort_by(|a, b| query.sort.compare(&a.entry, &b.entry));
        }
        let mut out = Vec::new();
        let mut visited = HashSet::new();
        fn walk(
            parent: Option<u32>,
            depth: usize,
            by_parent: &HashMap<Option<u32>, Vec<&ProcessRow>>,
            visited: &mut HashSet<u32>,
            out: &mut Vec<ProcessRow>,
        ) {
            if let Some(children) = by_parent.get(&parent) {
                for child in children {
                    if visited.insert(child.entry.pid) {
                        let mut row = (*child).clone();
                        row.depth = depth;
                        out.push(row);
                        walk(Some(child.entry.pid), depth + 1, by_parent, visited, out);
                    }
                }
            }
        }
        walk(None, 0, &by_parent, &mut visited, &mut out);
        // Rows caught in parent cycles are appended at the root level.
        for row in &matched {
            if visited.insert(row.entry.pid) {
                out.push(row.clone());
            }
        }
        out
    } else {
        let mut sorted = matched;
        sorted.sort_by(|a, b| query.sort.compare(&a.entry, &b.entry));
        sorted
    };
    if let Some(limit) = query.limit {
        rows.truncate(limit);
    }
    rows
}

/// Renders process rows as an interactive table block plus CSV plain text.
pub fn processes_to_table(rows: &[ProcessRow]) -> (StructuredOutput, String) {
    let headers: Vec<String> = PROCESS_TABLE_HEADERS.iter().map(|h| h.to_string()).collect();
    let mut csv = headers.join(",");
    csv.push('\n');
    let mut table_rows = Vec::with_capacity(rows.len());
    for row in rows {
        let e = &row.entry;
        let name = if row.depth > 0 {
            format!("{}{TREE_MARKER}{}", "  ".repeat(row.depth - 1), e.name)
        } else {
            e.name.clone()
        };
        let cells = vec![
            e.pid.to_string(),
            e.parent_pid.map(|p| p.to_string()).unwrap_or_default(),
            name,
            format!("{:.1}", e.cpu_percent),
            format_bytes(e.memory_bytes),
            e.user.clone().unwrap_or_default(),
            e.status.clone(),
            row.owner.clone().unwrap_or_default(),
            truncate_chars(&e.command_line, 160),
        ];
        csv.push_str(&cells.iter().map(|c| csv_escape(c)).collect::<Vec<_>>().join(","));
        csv.push('\n');
        table_rows.push(cells);
    }
    (StructuredOutput::Table { headers, rows: table_rows }, csv)
}

/// Rebuilds process entries from a `:ps` / `:top` table block (used to diff snapshots).
pub fn entries_from_table(structured: &StructuredOutput) -> Option<Vec<ProcessEntry>> {
    let StructuredOutput::Table { headers, rows } = structured else {
        return None;
    };
    let col = |name: &str| headers.iter().position(|h| h == name);
    let (pid_col, name_col) = (col("PID")?, col("Name")?);
    let ppid_col = col("PPID");
    let cpu_col = col("CPU %");
    let mem_col = col("Memory");
    let user_col = col("User");
    let status_col = col("Status");
    let cmd_col = col("Command");
    let cell = |row: &Vec<String>, idx: Option<usize>| idx.and_then(|i| row.get(i)).cloned().unwrap_or_default();
    let mut out = Vec::new();
    for row in rows {
        let Some(pid) = row.get(pid_col).and_then(|p| p.trim().parse::<u32>().ok()) else {
            continue;
        };
        let raw_name = row.get(name_col).cloned().unwrap_or_default();
        let name = raw_name
            .rsplit(TREE_MARKER)
            .next()
            .unwrap_or(&raw_name)
            .trim()
            .to_string();
        let user = cell(row, user_col);
        out.push(ProcessEntry {
            pid,
            parent_pid: cell(row, ppid_col).trim().parse().ok(),
            name,
            exe: None,
            command_line: cell(row, cmd_col),
            user: if user.is_empty() { None } else { Some(user) },
            cpu_percent: cell(row, cpu_col).trim().parse().unwrap_or(0.0),
            memory_bytes: parse_size(&cell(row, mem_col), false).unwrap_or(0),
            start_time: 0,
            run_time_secs: 0,
            status: cell(row, status_col),
        });
    }
    Some(out)
}

pub fn is_process_block(command: &str, shell_name: &str) -> bool {
    shell_name == PROCESS_BLOCK_SHELL && (command.starts_with(":ps") || command.starts_with(":top"))
}

/// One-line summary used in block footers and the Task Manager header.
pub fn snapshot_summary(snapshot: &ProcessSnapshot, shown: usize) -> String {
    format!(
        "{shown} of {} processes · CPU {:.1}% · {} in use",
        snapshot.entries.len(),
        snapshot.total_cpu_percent(),
        format_bytes(snapshot.used_memory_bytes())
    )
}

// ---------------------------------------------------------------------------
// Details (`:proc <pid>`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessDetails {
    pub entry: ProcessEntry,
    pub owner: Option<String>,
    pub cwd: Option<String>,
    pub protected: bool,
    pub ancestors: Vec<ProcessEntry>,
    /// Descendants with their depth (1 = direct child), in tree order.
    pub descendants: Vec<(usize, ProcessEntry)>,
    pub ports: Vec<PortEntry>,
    pub ports_error: Option<String>,
}

/// Collects details, the child tree and the listening ports of one process.
pub async fn inspect_process(
    sampler: &SharedProcessSampler,
    pid: u32,
    owned: &OwnedProcessMap,
) -> Result<ProcessDetails> {
    let snapshot = sampler.snapshot().await?;
    let entry = snapshot
        .get(pid)
        .cloned()
        .with_context(|| format!("No running process with PID {pid}"))?;
    let cwd = sampler.working_directory(pid).await;
    let (ports, ports_error) = match list_listening_ports().await {
        Ok(all) => (all.into_iter().filter(|p| p.pid.trim() == pid.to_string()).collect(), None),
        Err(e) => (Vec::new(), Some(e.to_string())),
    };
    let mut descendants: Vec<(usize, ProcessEntry)> = Vec::new();
    fn collect(snapshot: &ProcessSnapshot, pid: u32, depth: usize, seen: &mut HashSet<u32>, out: &mut Vec<(usize, ProcessEntry)>) {
        for child in snapshot.children_of(pid) {
            if seen.insert(child.pid) {
                out.push((depth, child.clone()));
                collect(snapshot, child.pid, depth + 1, seen, out);
            }
        }
    }
    collect(&snapshot, pid, 1, &mut HashSet::from([pid]), &mut descendants);
    Ok(ProcessDetails {
        owner: owner_label(&snapshot, pid, owned),
        protected: is_protected_process_name(&entry.name) || pid <= 4,
        ancestors: snapshot.ancestors(pid).into_iter().cloned().collect(),
        descendants,
        cwd,
        ports,
        ports_error,
        entry,
    })
}

pub fn process_details_markdown(d: &ProcessDetails) -> String {
    let e = &d.entry;
    let mut md = format!("# {} (PID {})\n\n", e.name, e.pid);
    if d.protected {
        md.push_str("> ⚠ OS-critical process: stopping it is blocked as Critical by the Safety Gate.\n\n");
    }
    md.push_str(&format!("- **Command:** `{}`\n", e.command_line.replace('`', "'")));
    if let Some(exe) = &e.exe {
        md.push_str(&format!("- **Executable:** `{exe}`\n"));
    }
    if let Some(cwd) = &d.cwd {
        md.push_str(&format!("- **Working directory:** `{cwd}`\n"));
    }
    md.push_str(&format!(
        "- **Parent:** {}\n",
        e.parent_pid
            .map(|p| {
                let parent_name = d.ancestors.first().map(|a| a.name.as_str()).unwrap_or("?");
                format!("{parent_name} (PID {p})")
            })
            .unwrap_or_else(|| "none".into())
    ));
    md.push_str(&format!("- **User:** {}\n", e.user.as_deref().unwrap_or("unknown")));
    md.push_str(&format!("- **Status:** {}\n", e.status));
    md.push_str(&format!("- **CPU:** {:.1}% · **Memory:** {}\n", e.cpu_percent, format_bytes(e.memory_bytes)));
    md.push_str(&format!("- **Running for:** {}\n", format_duration(e.run_time_secs)));
    if let Some(owner) = &d.owner {
        md.push_str(&format!("- **Started by MRT:** {owner}\n"));
    }

    md.push_str("\n## Listening ports\n\n");
    if let Some(err) = &d.ports_error {
        md.push_str(&format!("Port scan failed: {err}\n"));
    } else if d.ports.is_empty() {
        md.push_str("None.\n");
    } else {
        for p in &d.ports {
            md.push_str(&format!("- {} {}:{} ({})\n", p.protocol, p.local_address, p.port, p.state));
        }
    }

    md.push_str(&format!("\n## Child processes ({})\n\n", d.descendants.len()));
    if d.descendants.is_empty() {
        md.push_str("None.\n");
    } else {
        for (depth, child) in d.descendants.iter().take(50) {
            md.push_str(&format!(
                "{}- {} (PID {}) · {:.1}% · {}\n",
                "  ".repeat(depth.saturating_sub(1)),
                child.name,
                child.pid,
                child.cpu_percent,
                format_bytes(child.memory_bytes)
            ));
        }
    }
    if !d.ancestors.is_empty() {
        md.push_str("\n## Ancestors\n\n");
        let chain: Vec<String> = d.ancestors.iter().map(|a| format!("{} ({})", a.name, a.pid)).collect();
        md.push_str(&chain.join(" ← "));
        md.push('\n');
    }
    md.push_str(&format!(
        "\nStop it with `:kill {pid}` (graceful) or `:kill {pid} --force --tree`.\n",
        pid = e.pid
    ));
    md
}

// ---------------------------------------------------------------------------
// Stopping processes (`:kill`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProcessSelector {
    Pid(u32),
    Name(String),
}

impl ProcessSelector {
    pub fn parse(token: &str) -> Self {
        match token.trim().parse::<u32>() {
            Ok(pid) => Self::Pid(pid),
            Err(_) => Self::Name(token.trim().to_string()),
        }
    }

    pub fn matches(&self, entry: &ProcessEntry) -> bool {
        match self {
            Self::Pid(pid) => entry.pid == *pid,
            Self::Name(name) => {
                mrt_core::normalize_process_name(&entry.name) == mrt_core::normalize_process_name(name)
            }
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Pid(pid) => format!("PID {pid}"),
            Self::Name(name) => format!("'{name}'"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillRequest {
    pub selectors: Vec<ProcessSelector>,
    pub force: bool,
    pub tree: bool,
}

/// Parses `:kill <pid|name>... [--force|-f] [--tree|-t]`.
pub fn parse_kill_command(input: &str) -> Result<KillRequest> {
    let rest = input.trim().strip_prefix(":kill").unwrap_or(input.trim());
    let mut request = KillRequest {
        selectors: Vec::new(),
        force: false,
        tree: false,
    };
    for tok in rest.split_whitespace() {
        match tok {
            "--force" | "-f" | "-9" => request.force = true,
            "--tree" | "-t" => request.tree = true,
            other if other.starts_with("--") => bail!("Unknown option '{other}'"),
            other => request.selectors.push(ProcessSelector::parse(other)),
        }
    }
    if request.selectors.is_empty() {
        bail!("Usage: :kill <pid|name>... [--force] [--tree]");
    }
    Ok(request)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillPlan {
    pub request: KillRequest,
    pub targets: Vec<ProcessEntry>,
    /// Number of descendant processes that `--tree` will also stop.
    pub descendant_count: usize,
    pub unresolved: Vec<String>,
    pub assessment: SafetyAssessment,
}

impl KillPlan {
    pub fn pids(&self) -> Vec<u32> {
        self.targets.iter().map(|t| t.pid).collect()
    }
}

/// Resolves a kill request against a snapshot and classifies it for the Safety Gate.
/// Every plan requires approval; OS-critical targets, MRT itself (or its parents),
/// processes of other users, and names matching several PIDs are Critical.
pub fn plan_kill(
    snapshot: &ProcessSnapshot,
    request: &KillRequest,
    owned: &OwnedProcessMap,
) -> Result<KillPlan> {
    let mut targets: Vec<ProcessEntry> = Vec::new();
    let mut unresolved = Vec::new();
    let mut ambiguous_names = Vec::new();
    for selector in &request.selectors {
        let matches: Vec<&ProcessEntry> = snapshot.entries.iter().filter(|e| selector.matches(e)).collect();
        if matches.is_empty() {
            unresolved.push(selector.label());
        }
        if matches!(selector, ProcessSelector::Name(_)) && matches.len() > 1 {
            ambiguous_names.push(format!("{} matches {} processes", selector.label(), matches.len()));
        }
        for m in matches {
            if !targets.iter().any(|t| t.pid == m.pid) {
                targets.push(m.clone());
            }
        }
    }
    if targets.is_empty() {
        bail!("No running process matches {}", unresolved.join(", "));
    }

    let mut critical: Vec<String> = Vec::new();
    for t in &targets {
        if is_protected_process_name(&t.name) || t.pid <= 4 {
            critical.push(format!("{} (PID {}) is an OS-critical process", t.name, t.pid));
        }
        if t.pid == snapshot.current_pid || snapshot.is_descendant_of(snapshot.current_pid, t.pid) {
            critical.push(format!("{} (PID {}) is MRT itself or one of its parents", t.name, t.pid));
        }
        if let (Some(user), Some(me)) = (&t.user, &snapshot.current_user) {
            if user != me {
                critical.push(format!("{} (PID {}) belongs to another user ({user})", t.name, t.pid));
            }
        }
    }
    critical.extend(ambiguous_names);

    let mut seen = HashSet::new();
    let descendant_count = if request.tree {
        targets
            .iter()
            .flat_map(|t| snapshot.descendants(t.pid))
            .filter(|(_, d)| seen.insert(d.pid))
            .count()
    } else {
        0
    };

    let all_owned = targets.iter().all(|t| owner_label(snapshot, t.pid, owned).is_some());
    let (risk, headline) = if !critical.is_empty() {
        (RiskLevel::Critical, critical.join("; "))
    } else if all_owned {
        (RiskLevel::Moderate, "Stops processes started by MRT".to_string())
    } else {
        (RiskLevel::Moderate, "Stops processes that MRT did not start".to_string())
    };

    let target_lines: Vec<String> = targets
        .iter()
        .map(|t| {
            format!(
                "{} (PID {}) — {}",
                t.name,
                t.pid,
                truncate_chars(&t.command_line, 120)
            )
        })
        .collect();
    let mut flags = Vec::new();
    if request.force {
        flags.push("--force");
    }
    if request.tree {
        flags.push("--tree");
    }
    let command = format!(
        ":kill {}{}",
        targets.iter().map(|t| t.pid.to_string()).collect::<Vec<_>>().join(" "),
        if flags.is_empty() { String::new() } else { format!(" {}", flags.join(" ")) }
    );
    let mut reason = format!("{headline}.\nTargets: {}", target_lines.join("; "));
    if request.tree && descendant_count > 0 {
        reason.push_str(&format!("\nAlso stops {descendant_count} child process(es)."));
    }
    reason.push_str(if request.force {
        "\nMethod: forced (SIGKILL / TerminateProcess)."
    } else {
        "\nMethod: graceful (SIGTERM / taskkill), retry with --force if it keeps running."
    });
    if !unresolved.is_empty() {
        reason.push_str(&format!("\nNot found: {}", unresolved.join(", ")));
    }

    Ok(KillPlan {
        request: request.clone(),
        targets,
        descendant_count,
        unresolved,
        assessment: SafetyAssessment {
            command,
            risk,
            reason,
            requires_approval: true,
        },
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KillStatus {
    Stopped,
    StillRunning,
    NotFound,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillOutcome {
    pub pid: u32,
    pub name: String,
    pub status: KillStatus,
    pub method: String,
    /// Process start time (seconds since the epoch), used to tell reused PIDs apart.
    #[serde(default)]
    pub start_time: u64,
}

impl KillOutcome {
    pub fn succeeded(&self) -> bool {
        matches!(self.status, KillStatus::Stopped | KillStatus::NotFound)
    }
}

pub fn kill_outcomes_markdown(outcomes: &[KillOutcome]) -> String {
    let mut md = String::from("| PID | Name | Result | Method |\n|---|---|---|---|\n");
    for o in outcomes {
        let result = match &o.status {
            KillStatus::Stopped => "stopped".to_string(),
            KillStatus::StillRunning => "still running (retry with --force)".to_string(),
            KillStatus::NotFound => "already exited".to_string(),
            KillStatus::Failed(e) => format!("failed: {e}"),
        };
        md.push_str(&format!("| {} | {} | {} | {} |\n", o.pid, o.name, result.replace('|', "/"), o.method));
    }
    md
}

// ---------------------------------------------------------------------------
// Watches (`:watch-proc`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessWatchSpec {
    pub selector: ProcessSelector,
    pub mem_limit_bytes: Option<u64>,
    pub cpu_limit_percent: Option<f32>,
}

/// Parses `:watch-proc <pid|name> [--mem 2GB] [--cpu 90]`.
pub fn parse_watch_command(input: &str) -> Result<ProcessWatchSpec> {
    let rest = input.trim().strip_prefix(":watch-proc").unwrap_or(input.trim());
    let mut selector = None;
    let mut mem_limit_bytes = None;
    let mut cpu_limit_percent = None;
    let mut tokens = rest.split_whitespace();
    while let Some(tok) = tokens.next() {
        match tok {
            "--mem" | "--memory" => {
                let value = tokens.next().context("--mem needs a size such as 2GB or 512MB")?;
                mem_limit_bytes =
                    Some(parse_size(value, true).with_context(|| format!("Invalid memory size '{value}'"))?);
            }
            "--cpu" => {
                let value = tokens.next().context("--cpu needs a percentage such as 90")?;
                let pct: f32 = value
                    .trim_end_matches('%')
                    .parse()
                    .with_context(|| format!("Invalid CPU percentage '{value}'"))?;
                cpu_limit_percent = Some(pct.clamp(0.1, 100.0));
            }
            other if other.starts_with("--") => bail!("Unknown option '{other}'"),
            other if selector.is_none() => selector = Some(ProcessSelector::parse(other)),
            other => bail!("Unexpected argument '{other}'"),
        }
    }
    let selector = selector.context("Usage: :watch-proc <pid|name> [--mem 2GB] [--cpu 90]")?;
    Ok(ProcessWatchSpec {
        selector,
        mem_limit_bytes,
        cpu_limit_percent,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WatchAlert {
    MemoryAbove { pid: u32, name: String, memory_bytes: u64, limit_bytes: u64 },
    CpuAbove { pid: u32, name: String, cpu_percent: f32, limit_percent: f32 },
    Started { pid: u32, name: String },
    Exited { pid: u32, name: String },
}

impl WatchAlert {
    pub fn message(&self) -> String {
        match self {
            Self::MemoryAbove { pid, name, memory_bytes, limit_bytes } => format!(
                "{name} (PID {pid}) is using {} of memory (limit {}).",
                format_bytes(*memory_bytes),
                format_bytes(*limit_bytes)
            ),
            Self::CpuAbove { pid, name, cpu_percent, limit_percent } => {
                format!("{name} (PID {pid}) is at {cpu_percent:.1}% CPU (limit {limit_percent:.0}%).")
            }
            Self::Started { pid, name } => format!("{name} (PID {pid}) started."),
            Self::Exited { pid, name } => format!("{name} (PID {pid}) exited."),
        }
    }

    pub fn pid(&self) -> u32 {
        match self {
            Self::MemoryAbove { pid, .. }
            | Self::CpuAbove { pid, .. }
            | Self::Started { pid, .. }
            | Self::Exited { pid, .. } => *pid,
        }
    }
}

/// A live watch. Threshold alerts fire when a process crosses the limit and re-arm when it
/// drops back below it, so a process hovering over the limit does not flood the tab.
#[derive(Debug, Clone)]
pub struct ProcessWatch {
    pub id: usize,
    pub spec: ProcessWatchSpec,
    seen: HashMap<u32, String>,
    over_mem: HashSet<u32>,
    over_cpu: HashSet<u32>,
    initialized: bool,
}

impl ProcessWatch {
    pub fn new(id: usize, spec: ProcessWatchSpec) -> Self {
        Self {
            id,
            spec,
            seen: HashMap::new(),
            over_mem: HashSet::new(),
            over_cpu: HashSet::new(),
            initialized: false,
        }
    }

    pub fn describe(&self) -> String {
        let mut parts = vec![format!("#{} {}", self.id, self.spec.selector.label())];
        if let Some(mem) = self.spec.mem_limit_bytes {
            parts.push(format!("mem > {}", format_bytes(mem)));
        }
        if let Some(cpu) = self.spec.cpu_limit_percent {
            parts.push(format!("cpu > {cpu:.0}%"));
        }
        parts.push(format!("{} running", self.seen.len()));
        parts.join(" · ")
    }

    pub fn evaluate(&mut self, snapshot: &ProcessSnapshot) -> Vec<WatchAlert> {
        let mut alerts = Vec::new();
        let current: Vec<&ProcessEntry> = snapshot
            .entries
            .iter()
            .filter(|e| self.spec.selector.matches(e))
            .collect();
        let current_pids: HashSet<u32> = current.iter().map(|e| e.pid).collect();

        let mut exited: Vec<(u32, String)> = self
            .seen
            .iter()
            .filter(|(pid, _)| !current_pids.contains(pid))
            .map(|(pid, name)| (*pid, name.clone()))
            .collect();
        exited.sort();
        for (pid, name) in exited {
            self.seen.remove(&pid);
            self.over_mem.remove(&pid);
            self.over_cpu.remove(&pid);
            alerts.push(WatchAlert::Exited { pid, name });
        }

        for e in current {
            if self.seen.insert(e.pid, e.name.clone()).is_none() && self.initialized {
                alerts.push(WatchAlert::Started { pid: e.pid, name: e.name.clone() });
            }
            if let Some(limit) = self.spec.mem_limit_bytes {
                if e.memory_bytes > limit {
                    if self.over_mem.insert(e.pid) {
                        alerts.push(WatchAlert::MemoryAbove {
                            pid: e.pid,
                            name: e.name.clone(),
                            memory_bytes: e.memory_bytes,
                            limit_bytes: limit,
                        });
                    }
                } else {
                    self.over_mem.remove(&e.pid);
                }
            }
            if let Some(limit) = self.spec.cpu_limit_percent {
                if e.cpu_percent > limit {
                    if self.over_cpu.insert(e.pid) {
                        alerts.push(WatchAlert::CpuAbove {
                            pid: e.pid,
                            name: e.name.clone(),
                            cpu_percent: e.cpu_percent,
                            limit_percent: limit,
                        });
                    }
                } else {
                    self.over_cpu.remove(&e.pid);
                }
            }
        }
        self.initialized = true;
        alerts
    }
}

// ---------------------------------------------------------------------------
// Snapshot comparison
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessMemoryChange {
    pub pid: u32,
    pub name: String,
    pub before_bytes: u64,
    pub after_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ProcessDiffReport {
    pub started: Vec<ProcessEntry>,
    pub exited: Vec<ProcessEntry>,
    pub memory_grown: Vec<ProcessMemoryChange>,
    pub unchanged_count: usize,
}

impl ProcessDiffReport {
    pub fn is_empty(&self) -> bool {
        self.started.is_empty() && self.exited.is_empty() && self.memory_grown.is_empty()
    }

    pub fn started_lines(&self) -> Vec<String> {
        self.started.iter().map(describe_entry).collect()
    }

    pub fn exited_lines(&self) -> Vec<String> {
        self.exited.iter().map(describe_entry).collect()
    }

    pub fn memory_lines(&self) -> Vec<String> {
        self.memory_grown
            .iter()
            .map(|m| {
                format!(
                    "{} (PID {}): {} → {} (+{})",
                    m.name,
                    m.pid,
                    format_bytes(m.before_bytes),
                    format_bytes(m.after_bytes),
                    format_bytes(m.after_bytes.saturating_sub(m.before_bytes))
                )
            })
            .collect()
    }
}

fn describe_entry(e: &ProcessEntry) -> String {
    format!("{} (PID {}) — {}", e.name, e.pid, truncate_chars(&e.command_line, 100))
}

/// Compares two process lists. Processes are matched by PID and name (PIDs get reused).
/// Memory growth is reported when a process grew by at least 10 MB and 20%.
pub fn compare_process_snapshots(previous: &[ProcessEntry], current: &[ProcessEntry]) -> ProcessDiffReport {
    let key = |e: &ProcessEntry| (e.pid, mrt_core::normalize_process_name(&e.name));
    let prev: HashMap<_, &ProcessEntry> = previous.iter().map(|e| (key(e), e)).collect();
    let curr: HashMap<_, &ProcessEntry> = current.iter().map(|e| (key(e), e)).collect();
    let mut report = ProcessDiffReport::default();
    for (k, e) in &curr {
        match prev.get(k) {
            None => report.started.push((*e).clone()),
            Some(before) => {
                let grown = e.memory_bytes.saturating_sub(before.memory_bytes);
                if grown >= 10 * 1024 * 1024 && grown * 5 >= before.memory_bytes {
                    report.memory_grown.push(ProcessMemoryChange {
                        pid: e.pid,
                        name: e.name.clone(),
                        before_bytes: before.memory_bytes,
                        after_bytes: e.memory_bytes,
                    });
                } else {
                    report.unchanged_count += 1;
                }
            }
        }
    }
    for (k, e) in &prev {
        if !curr.contains_key(k) {
            report.exited.push((*e).clone());
        }
    }
    report.started.sort_by_key(|e| e.pid);
    report.exited.sort_by_key(|e| e.pid);
    report
        .memory_grown
        .sort_by_key(|m| std::cmp::Reverse(m.after_bytes.saturating_sub(m.before_bytes)));
    report
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Parses sizes like `2GB`, `512 MB`, `1.5g`, `300k`. A bare number means megabytes when
/// `bare_is_mb`, bytes otherwise.
pub fn parse_size(input: &str, bare_is_mb: bool) -> Option<u64> {
    let s = input.trim().to_lowercase().replace(' ', "");
    if s.is_empty() {
        return None;
    }
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let value: f64 = num.parse().ok()?;
    let multiplier: f64 = match unit {
        "" => {
            if bare_is_mb {
                1024.0 * 1024.0
            } else {
                1.0
            }
        }
        "b" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0 * 1024.0,
        "g" | "gb" | "gib" => 1024.0 * 1024.0 * 1024.0,
        "t" | "tb" | "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((value * multiplier).round() as u64)
}

pub fn format_duration(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60, secs % 60);
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(pid: u32, parent: Option<u32>, name: &str, mem_mb: u64, cpu: f32) -> ProcessEntry {
        ProcessEntry {
            pid,
            parent_pid: parent,
            name: name.into(),
            exe: None,
            command_line: format!("{name} --flag"),
            user: Some("dev".into()),
            cpu_percent: cpu,
            memory_bytes: mem_mb * 1024 * 1024,
            start_time: 0,
            run_time_secs: 42,
            status: "Run".into(),
        }
    }

    fn snapshot() -> ProcessSnapshot {
        ProcessSnapshot {
            taken_at_ms: 0,
            current_pid: 100,
            current_user: Some("dev".into()),
            logical_cpus: 8,
            total_memory_bytes: 0,
            entries: vec![
                entry(1, None, "init", 5, 0.0),
                entry(50, Some(1), "explorer.exe", 80, 0.5),
                entry(100, Some(50), "mrt.exe", 120, 2.0),
                entry(200, Some(100), "pwsh.exe", 60, 0.1),
                entry(201, Some(200), "node.exe", 300, 25.0),
                entry(202, Some(201), "node.exe", 90, 3.0),
                entry(300, Some(1), "Code.exe", 500, 4.0),
                ProcessEntry { user: Some("root".into()), ..entry(400, Some(1), "postgres", 200, 1.0) },
            ],
        }
    }

    fn owned() -> OwnedProcessMap {
        HashMap::from([(
            200,
            OwnedProcess {
                pid: 200,
                label: "Tab 1 · npm run dev".into(),
                tab_id: None,
                kind: OwnedProcessKind::Command,
            },
        )])
    }

    #[test]
    fn parses_ps_and_kill_commands() {
        let q = parse_ps_command(":ps node --sort mem --limit 5 --tree").unwrap();
        assert_eq!(q.filter.as_deref(), Some("node"));
        assert_eq!(q.sort, ProcessSort::Memory);
        assert_eq!(q.limit, Some(5));
        assert!(q.tree);
        assert!(parse_ps_command(":ps --sort bogus").is_err());

        let k = parse_kill_command(":kill 201 node --force -t").unwrap();
        assert_eq!(k.selectors, vec![ProcessSelector::Pid(201), ProcessSelector::Name("node".into())]);
        assert!(k.force && k.tree);
        assert!(parse_kill_command(":kill").is_err());
    }

    #[test]
    fn queries_sort_filter_and_build_trees() {
        let snap = snapshot();
        let rows = query_processes(&snap, &ProcessQuery::default(), &owned());
        assert_eq!(rows[0].entry.pid, 201, "highest CPU first");

        let q = ProcessQuery { filter: Some("node".into()), tree: true, ..Default::default() };
        let rows = query_processes(&snap, &q, &owned());
        assert_eq!(rows.iter().map(|r| (r.entry.pid, r.depth)).collect::<Vec<_>>(), vec![(201, 0), (202, 1)]);

        let mrt_only = ProcessQuery { only_mrt: true, ..Default::default() };
        let pids: HashSet<u32> = query_processes(&snap, &mrt_only, &owned()).iter().map(|r| r.entry.pid).collect();
        assert_eq!(pids, HashSet::from([100, 200, 201, 202]));
        assert_eq!(owner_label(&snap, 202, &owned()).as_deref(), Some("Tab 1 · npm run dev › child"));
    }

    #[test]
    fn table_round_trips_for_diffs() {
        let snap = snapshot();
        let q = ProcessQuery { tree: true, ..Default::default() };
        let rows = query_processes(&snap, &q, &owned());
        let (table, csv) = processes_to_table(&rows);
        assert!(csv.starts_with("PID,PPID,Name"));
        let back = entries_from_table(&table).unwrap();
        assert_eq!(back.len(), snap.entries.len());
        let node_child = back.iter().find(|e| e.pid == 202).unwrap();
        assert_eq!(node_child.name, "node.exe");
        assert_eq!(node_child.memory_bytes, 90 * 1024 * 1024);
    }

    #[test]
    fn kill_plans_are_classified_for_the_safety_gate() {
        let snap = snapshot();
        let own = owned();

        let plan = plan_kill(&snap, &parse_kill_command(":kill 201 --tree").unwrap(), &own).unwrap();
        assert_eq!(plan.assessment.risk, RiskLevel::Moderate);
        assert!(plan.assessment.requires_approval);
        assert_eq!(plan.descendant_count, 1);
        assert!(plan.assessment.reason.contains("started by MRT"));

        let other = plan_kill(&snap, &parse_kill_command(":kill 300").unwrap(), &own).unwrap();
        assert_eq!(other.assessment.risk, RiskLevel::Moderate);
        assert!(other.assessment.reason.contains("did not start"));

        for cmd in [":kill node", ":kill 1", ":kill 50", ":kill 400"] {
            let p = plan_kill(&snap, &parse_kill_command(cmd).unwrap(), &own).unwrap();
            assert_eq!(p.assessment.risk, RiskLevel::Critical, "{cmd} should be Critical");
        }
        assert!(plan_kill(&snap, &parse_kill_command(":kill 999").unwrap(), &own).is_err());
    }

    #[test]
    fn watches_alert_once_per_crossing_and_on_exit() {
        let mut snap = snapshot();
        let spec = parse_watch_command(":watch-proc node --mem 200MB --cpu 20").unwrap();
        assert_eq!(spec.mem_limit_bytes, Some(200 * 1024 * 1024));
        let mut watch = ProcessWatch::new(1, spec);

        let first = watch.evaluate(&snap);
        assert_eq!(first.len(), 2, "pid 201 crosses both limits: {first:?}");
        assert!(watch.evaluate(&snap).is_empty(), "no repeat while still above");

        snap.entries.retain(|e| e.pid != 202);
        snap.entries.push(entry(203, Some(201), "node.exe", 10, 0.0));
        let alerts = watch.evaluate(&snap);
        assert!(alerts.contains(&WatchAlert::Exited { pid: 202, name: "node.exe".into() }));
        assert!(alerts.contains(&WatchAlert::Started { pid: 203, name: "node.exe".into() }));
    }

    #[test]
    fn compares_snapshots() {
        let before = snapshot().entries;
        let mut after = before.clone();
        after.retain(|e| e.pid != 300);
        after.push(entry(500, Some(200), "cargo.exe", 40, 9.0));
        after.iter_mut().find(|e| e.pid == 201).unwrap().memory_bytes = 900 * 1024 * 1024;
        let report = compare_process_snapshots(&before, &after);
        assert_eq!(report.started.iter().map(|e| e.pid).collect::<Vec<_>>(), vec![500]);
        assert_eq!(report.exited.iter().map(|e| e.pid).collect::<Vec<_>>(), vec![300]);
        assert_eq!(report.memory_grown.len(), 1);
        assert!(report.memory_lines()[0].contains("+600.0 MB"));
    }

    #[test]
    fn size_and_duration_helpers() {
        assert_eq!(parse_size("2GB", true), Some(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_size("512", true), Some(512 * 1024 * 1024));
        assert_eq!(parse_size("1.5 MB", false), Some(1_572_864));
        assert_eq!(parse_size("12 parsecs", false), None);
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_duration(3_725), "1h 2m");
    }

    #[test]
    fn live_snapshot_contains_this_process() {
        let sampler = SharedProcessSampler::new();
        let snap = sampler.snapshot_blocking().expect("process snapshot");
        assert!(snap.get(std::process::id()).is_some());
        assert!(snap.entries.len() > 1);
    }
}
