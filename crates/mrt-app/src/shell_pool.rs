//! Persistent shells for the GUI: one `mrt_core::ShellSession` per tab and shell.
//!
//! A command runs in its tab's session when the session is idle, so `cd`, variables,
//! aliases, functions and activated environments carry over to the next command. The
//! command runs in its own shell process instead (`execute_command_streaming_with_pid`,
//! the behaviour before sessions existed) when:
//!
//! * persistent shells are turned off (`terminal.persistent_shell = false`, or unit tests
//!   that do not opt in),
//! * the shell has no session support (WSL, Nushell) or its integration did not start
//!   (`ShellSessionError::should_fall_back`); MRT remembers this and does not retry,
//! * the tab's session is still starting or is busy with another command.
//!
//! **Stop** sends Ctrl+C to the session ([`ShellPool::interrupt`]) rather than killing the
//! shell, so the session survives. A session whose shell exits is dropped and the next
//! command starts a new one.

use mrt_core::{CommandEvent, ShellKind, ShellProfile, ShellSession, ShellSessionOptions};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// How long a new shell may take to print its first prompt before MRT falls back.
const SESSION_READY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a command waits for the tab's shell to return to its prompt before it runs
/// in its own process instead.
const IDLE_WAIT: Duration = Duration::from_secs(3);

type Key = (Uuid, String);

enum Entry {
    /// A thread is starting the shell.
    Starting,
    /// A running shell; `busy` while a command from MRT is using it.
    Ready { session: Arc<ShellSession>, busy: bool },
    /// Persistent sessions do not work for this shell; the reason is kept for diagnostics
    /// (tests print it).
    Unavailable(#[cfg_attr(not(test), allow(dead_code))] String),
}

#[derive(Default)]
struct Inner {
    entries: HashMap<Key, Entry>,
    /// Session running each block, for Stop.
    running: HashMap<Uuid, Arc<ShellSession>>,
}

/// Where a command should run.
pub enum Acquired {
    /// Run it in this session; call [`ShellPool::release`] when it finishes.
    Session(Arc<ShellSession>),
    /// Run it in its own process.
    Fallback,
}

/// The result of running one command in a session.
pub struct SessionRun {
    pub output: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub cwd: Option<PathBuf>,
}

/// Shared between the GUI thread and the worker tasks that run commands.
#[derive(Clone)]
pub struct ShellPool {
    inner: Arc<Mutex<Inner>>,
    enabled: Arc<std::sync::atomic::AtomicBool>,
}

impl ShellPool {
    pub fn new(enabled: bool) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
            enabled: Arc::new(std::sync::atomic::AtomicBool::new(enabled)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Turns persistent shells on or off. Turning them off ends every session.
    pub fn set_enabled(&self, enabled: bool) {
        let was = self.enabled.swap(enabled, std::sync::atomic::Ordering::Relaxed);
        if was && !enabled {
            let mut inner = self.lock();
            inner.entries.clear();
        }
    }

    /// Finds or starts the session for `tab_id` and `shell`. Blocks while a new shell
    /// starts (up to [`SESSION_READY_TIMEOUT`]); call it from a blocking thread.
    pub fn acquire(&self, tab_id: Uuid, shell: &ShellProfile, cwd: &Path) -> Acquired {
        if !self.is_enabled() || !ShellSession::supports(&shell.kind) {
            return Acquired::Fallback;
        }
        let key = (tab_id, shell.id.clone());
        let idle_deadline = Instant::now() + IDLE_WAIT;
        loop {
            let mut inner = self.lock();
            match inner.entries.get_mut(&key) {
                Some(Entry::Ready { session, busy }) if !session.has_exited() => {
                    if *busy {
                        // Another command from MRT is running in it.
                        return Acquired::Fallback;
                    }
                    if session.is_idle() {
                        *busy = true;
                        return Acquired::Session(session.clone());
                    }
                    // The previous command just finished and the shell is still printing
                    // its prompt, or the user's own input is running: wait a little.
                    if Instant::now() >= idle_deadline {
                        return Acquired::Fallback;
                    }
                }
                Some(Entry::Starting) | Some(Entry::Unavailable(_)) => return Acquired::Fallback,
                // No session yet, or its shell exited: start a new one.
                _ => {
                    inner.entries.insert(key.clone(), Entry::Starting);
                    break;
                }
            }
            drop(inner);
            std::thread::sleep(Duration::from_millis(20));
        }

        let options = ShellSessionOptions {
            ready_timeout: SESSION_READY_TIMEOUT,
            ..ShellSessionOptions::default()
        };
        let result = ShellSession::spawn(shell, cwd, options);
        let mut inner = self.lock();
        match result {
            Ok(session) => {
                let session = Arc::new(session);
                inner.entries.insert(key, Entry::Ready { session: session.clone(), busy: true });
                Acquired::Session(session)
            }
            Err(error) if error.should_fall_back() => {
                inner.entries.insert(key, Entry::Unavailable(error.to_string()));
                Acquired::Fallback
            }
            Err(_) => {
                // A transient failure (e.g. the PTY could not be created): try again next time.
                inner.entries.remove(&key);
                Acquired::Fallback
            }
        }
    }

    /// Marks the session free again after a command; drops it if its shell exited.
    pub fn release(&self, tab_id: Uuid, shell_id: &str, block_id: Uuid) {
        let mut inner = self.lock();
        inner.running.remove(&block_id);
        let key = (tab_id, shell_id.to_string());
        let exited = match inner.entries.get_mut(&key) {
            Some(Entry::Ready { session, busy }) => {
                *busy = false;
                session.has_exited()
            }
            _ => false,
        };
        if exited {
            inner.entries.remove(&key);
        }
    }

    /// Records that `block_id` is running in `session`, so Stop can interrupt it.
    pub fn track(&self, block_id: Uuid, session: Arc<ShellSession>) {
        self.lock().running.insert(block_id, session);
    }

    /// Sends Ctrl+C to the session running `block_id` and returns that session, or `None`
    /// when the block does not run in a session (it runs in its own process, which the
    /// caller stops instead).
    ///
    /// Ctrl+C reaches the command through the terminal on Unix (SIGINT to the foreground
    /// job). Windows ConPTY does not turn the byte into a console Ctrl+C event for the
    /// running program, so the caller should also stop the shell's child processes if the
    /// command is still running shortly after ([`ShellPool::is_running`]).
    pub fn interrupt(&self, block_id: Uuid) -> Option<Arc<ShellSession>> {
        let session = self.lock().running.get(&block_id).cloned()?;
        let _ = session.interrupt();
        Some(session)
    }

    /// `true` while `block_id` is still running in a session.
    pub fn is_running(&self, block_id: Uuid) -> bool {
        self.lock().running.contains_key(&block_id)
    }

    /// Ends the sessions of a closed tab.
    pub fn close_tab(&self, tab_id: Uuid) {
        self.lock().entries.retain(|(tab, _), _| *tab != tab_id);
    }

    /// Why persistent sessions are unavailable for this tab and shell, if they are.
    #[cfg(test)]
    pub fn unavailable_reason(&self, tab_id: Uuid, shell_id: &str) -> Option<String> {
        match self.lock().entries.get(&(tab_id, shell_id.to_string())) {
            Some(Entry::Unavailable(reason)) => Some(reason.clone()),
            _ => None,
        }
    }
}

/// Runs `command` for the GUI in the persistent shell of `tab_id`, streaming output into
/// `output`. Returns `None` when the command should run in its own process instead
/// (see the module docs). The shell's PID is recorded in `running_pids` under
/// `block_id` while it runs, so the Task Manager marks it as MRT's.
#[allow(clippy::too_many_arguments)]
pub async fn run_with_pool(
    pool: &ShellPool,
    tab_id: Uuid,
    block_id: Uuid,
    shell: &ShellProfile,
    command: &str,
    cwd: &Path,
    output: &Arc<Mutex<String>>,
    running_pids: &Arc<Mutex<HashMap<Uuid, u32>>>,
) -> Option<Result<SessionRun, String>> {
    if !pool.is_enabled() {
        return None;
    }
    let acquired = {
        let (pool, shell, cwd) = (pool.clone(), shell.clone(), cwd.to_path_buf());
        tokio::task::spawn_blocking(move || pool.acquire(tab_id, &shell, &cwd)).await.ok()?
    };
    let Acquired::Session(session) = acquired else {
        return None;
    };
    pool.track(block_id, session.clone());
    if let (Some(pid), Ok(mut pids)) = (session.process_id(), running_pids.lock()) {
        pids.insert(block_id, pid);
    }
    let result = {
        let (command, cwd, output) = (command.to_string(), cwd.to_path_buf(), output.clone());
        tokio::task::spawn_blocking(move || {
            run_in_session(&session, &command, &cwd, |chunk| {
                if let Ok(mut pending) = output.lock() {
                    pending.push_str(chunk);
                }
            })
        })
        .await
    };
    pool.release(tab_id, &shell.id, block_id);
    Some(match result {
        Ok(Ok(run)) => Ok(run),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) => Err(e.to_string()),
    })
}

/// Runs `command` in `session`: first moves the shell to `cwd` if it is somewhere else
/// (the user changed folder from the Explorer, for example), then streams the output into
/// `on_output` until the command finishes or the shell exits.
pub fn run_in_session(
    session: &ShellSession,
    command: &str,
    cwd: &Path,
    mut on_output: impl FnMut(&str),
) -> Result<SessionRun, mrt_core::ShellSessionError> {
    if !session.cwd().is_some_and(|current| same_dir(&current, cwd)) {
        if let Some(cd) = cd_command(session.kind(), cwd) {
            let mut handle = session.run(&cd)?;
            while handle.wait_timeout(Duration::from_millis(200)).is_none() {
                if session.has_exited() {
                    return Err(mrt_core::ShellSessionError::Exited);
                }
            }
        }
    }

    let mut handle = session.run(command)?;
    loop {
        match handle.next_timeout(Duration::from_millis(200)) {
            Some(CommandEvent::Output(chunk)) => on_output(&chunk),
            Some(CommandEvent::Finished(outcome)) => {
                let mut output = outcome.output;
                if outcome.shell_exited {
                    if !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str("[MRT: the shell exited; the next command starts a new one]");
                }
                return Ok(SessionRun {
                    output,
                    // No status means the shell abandoned the command; report it as failed.
                    exit_code: outcome.exit_code.unwrap_or(1),
                    duration_ms: outcome.duration.as_millis() as u64,
                    cwd: outcome.cwd,
                });
            }
            None if session.has_exited() => return Err(mrt_core::ShellSessionError::Exited),
            None => {}
        }
    }
}

/// Compares directories the way the OS does: case-insensitively on Windows, ignoring a
/// trailing separator.
fn same_dir(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        let s = p.to_string_lossy().replace('\\', "/");
        let s = s.trim_end_matches('/').to_string();
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s
        }
    };
    norm(a) == norm(b)
}

/// A `cd` to `dir` in the syntax of `kind`, with the path quoted for that shell.
pub fn cd_command(kind: &ShellKind, dir: &Path) -> Option<String> {
    let path = dir.to_string_lossy();
    Some(match kind {
        ShellKind::PowerShellCore | ShellKind::WindowsPowerShell => {
            format!("Set-Location -LiteralPath '{}'", path.replace('\'', "''"))
        }
        ShellKind::CmdBatch => format!("cd /d \"{path}\""),
        ShellKind::Fish => format!("cd '{}'", path.replace('\\', "\\\\").replace('\'', "\\'")),
        ShellKind::Bash | ShellKind::GitBash | ShellKind::Zsh | ShellKind::Sh => {
            format!("cd '{}'", path.replace('\'', "'\\''"))
        }
        ShellKind::Wsl | ShellKind::Nushell => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cd_commands_quote_paths_for_each_shell() {
        let dir = Path::new("/tmp/it's here");
        assert_eq!(cd_command(&ShellKind::Bash, dir).unwrap(), "cd '/tmp/it'\\''s here'");
        assert_eq!(cd_command(&ShellKind::Fish, dir).unwrap(), "cd '/tmp/it\\'s here'");
        assert_eq!(
            cd_command(&ShellKind::WindowsPowerShell, Path::new("C:\\a'b")).unwrap(),
            "Set-Location -LiteralPath 'C:\\a''b'"
        );
        assert_eq!(cd_command(&ShellKind::CmdBatch, Path::new("C:\\a b")).unwrap(), "cd /d \"C:\\a b\"");
        assert!(cd_command(&ShellKind::Wsl, dir).is_none());
    }

    #[test]
    fn same_dir_ignores_trailing_separators() {
        assert!(same_dir(Path::new("/a/b/"), Path::new("/a/b")));
        assert!(!same_dir(Path::new("/a/b"), Path::new("/a/c")));
        if cfg!(windows) {
            assert!(same_dir(Path::new("C:\\Users\\X"), Path::new("c:/users/x")));
        }
    }

    #[test]
    fn disabled_pool_always_falls_back() {
        let pool = ShellPool::new(false);
        let shell = mrt_core::discover_shells().into_iter().next().expect("a shell");
        assert!(matches!(
            pool.acquire(Uuid::new_v4(), &shell, &std::env::temp_dir()),
            Acquired::Fallback
        ));
    }
}
