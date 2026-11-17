//! Persistent shell sessions: one long-lived PTY shell per tab.
//!
//! [`ShellSession::spawn`] starts the shell in a pseudo terminal with the matching
//! [`shell_integration`](crate::shell_integration) script injected (an rcfile, `ENV`,
//! `ZDOTDIR`, `-Command`, or `/K` batch file depending on the shell). The script makes the
//! shell print OSC 133 prompt markers and an OSC 7 working directory. [`ShellSession::run`]
//! types a command into the shell and returns a [`CommandHandle`] that streams the command's
//! output and resolves with its exit code, duration and the new working directory.
//!
//! Because commands run inside one shell, `cd`, exported variables, aliases, functions and
//! activated virtual environments persist from one command to the next.
//!
//! If the shell never prints a prompt marker within [`ShellSessionOptions::ready_timeout`]
//! (an unsupported shell, a user config that replaces the prompt hooks, a broken install),
//! `spawn` fails with an error whose [`ShellSessionError::should_fall_back`] is `true`, and the
//! caller should keep using one process per command
//! ([`execute_command_streaming_with_pid`](crate::pty::execute_command_streaming_with_pid)).

use crate::pty::{ShellKind, ShellProfile};
use crate::shell_integration::{generate_shell_integration_script, parse_osc7_path, Osc133Kind};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use vte::{Params, Perform};

/// How much startup output is kept for [`ShellSessionError`] diagnostics.
const STARTUP_TRANSCRIPT_LIMIT: usize = 16 * 1024;

/// Options for [`ShellSession::spawn`].
#[derive(Debug, Clone)]
pub struct ShellSessionOptions {
    pub cols: u16,
    pub rows: u16,
    /// How long to wait for the first OSC 133 prompt marker before giving up.
    pub ready_timeout: Duration,
    /// Load the user's shell config (`~/.bashrc`, `.zshrc`, PowerShell profile, CMD
    /// AutoRun, fish config). The integration hooks are installed after it.
    pub load_user_config: bool,
    /// Extra environment variables for the shell.
    pub env: Vec<(String, String)>,
}

impl Default for ShellSessionOptions {
    fn default() -> Self {
        Self {
            cols: 120,
            rows: 30,
            ready_timeout: Duration::from_secs(20),
            load_user_config: true,
            env: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ShellSessionError {
    #[error("{0:?} has no persistent session support; run one process per command instead")]
    Unsupported(ShellKind),
    #[error(
        "`{program}` printed no OSC 133 prompt marker within {waited:?}; run one process per command instead"
    )]
    IntegrationTimeout {
        program: String,
        waited: Duration,
        /// Raw output the shell printed while starting, for diagnostics.
        transcript: String,
    },
    #[error("`{program}` exited before printing a prompt; run one process per command instead")]
    StartupExited { program: String, transcript: String },
    #[error("the shell session has exited")]
    Exited,
    #[error("failed to start the shell session: {0:#}")]
    Spawn(anyhow::Error),
    #[error("failed to write to the shell: {0}")]
    Io(#[from] std::io::Error),
}

impl ShellSessionError {
    /// `true` when shell integration is unavailable and the caller should run commands in
    /// one process each instead.
    pub fn should_fall_back(&self) -> bool {
        matches!(
            self,
            Self::Unsupported(_) | Self::IntegrationTimeout { .. } | Self::StartupExited { .. }
        )
    }
}

/// What a [`CommandHandle`] reports.
#[derive(Debug, Clone, PartialEq)]
pub enum CommandEvent {
    /// A piece of the command's output: text with SGR color sequences kept, `\n` line
    /// endings, and cursor movement turned into newlines and spaces.
    Output(String),
    Finished(CommandOutcome),
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandOutcome {
    /// The exit code from `OSC 133;D`. `None` when the shell reported none (it printed a new
    /// prompt without finishing the command, or exited without a status).
    pub exit_code: Option<i32>,
    /// Time from `OSC 133;C` (output start) to `OSC 133;D`.
    pub duration: Duration,
    /// All output chunks joined, with trailing newlines removed.
    pub output: String,
    /// The working directory the shell reported last (OSC 7).
    pub cwd: Option<PathBuf>,
    /// The shell exited while (or before) running this command.
    pub shell_exited: bool,
}

impl CommandOutcome {
    /// The output without ANSI escape sequences.
    pub fn plain_output(&self) -> String {
        crate::vt::parse_ansi_text(&self.output).1
    }
}

/// Streams the output of one command run through [`ShellSession::run`].
pub struct CommandHandle {
    rx: Receiver<CommandEvent>,
    outcome: Option<CommandOutcome>,
}

impl CommandHandle {
    /// Returns the next event without blocking (for UI loops that poll every frame).
    pub fn try_next(&mut self) -> Option<CommandEvent> {
        let event = self.rx.try_recv().ok()?;
        self.remember(&event);
        Some(event)
    }

    /// Waits up to `timeout` for the next event.
    pub fn next_timeout(&mut self, timeout: Duration) -> Option<CommandEvent> {
        let event = self.rx.recv_timeout(timeout).ok()?;
        self.remember(&event);
        Some(event)
    }

    /// Waits up to `timeout` for the command to finish, discarding output events.
    pub fn wait_timeout(&mut self, timeout: Duration) -> Option<CommandOutcome> {
        let deadline = Instant::now() + timeout;
        while self.outcome.is_none() {
            let left = deadline.checked_duration_since(Instant::now())?;
            match self.rx.recv_timeout(left) {
                Ok(event) => self.remember(&event),
                Err(RecvTimeoutError::Timeout) => return None,
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
        self.outcome.clone()
    }

    /// Blocks until the command finishes.
    pub fn wait(mut self) -> Result<CommandOutcome, ShellSessionError> {
        while self.outcome.is_none() {
            let event = self.rx.recv().map_err(|_| ShellSessionError::Exited)?;
            self.remember(&event);
        }
        Ok(self.outcome.take().expect("checked above"))
    }

    fn remember(&mut self, event: &CommandEvent) {
        if let CommandEvent::Finished(outcome) = event {
            self.outcome = Some(outcome.clone());
        }
    }
}

/// One long-lived shell running in a PTY with OSC 133 shell integration.
pub struct ShellSession {
    kind: ShellKind,
    program: String,
    shared: Arc<Shared>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pid: Option<u32>,
}

impl ShellSession {
    /// Shells that have a session bootstrap. WSL and Nushell need the one-process mode.
    pub fn supports(kind: &ShellKind) -> bool {
        !matches!(kind, ShellKind::Wsl | ShellKind::Nushell)
    }

    /// Starts `shell` in `cwd` and waits until it prints its first prompt marker.
    /// This blocks for up to `options.ready_timeout`; async callers should use
    /// `tokio::task::spawn_blocking`.
    pub fn spawn(
        shell: &ShellProfile,
        cwd: &Path,
        options: ShellSessionOptions,
    ) -> Result<Self, ShellSessionError> {
        if !Self::supports(&shell.kind) {
            return Err(ShellSessionError::Unsupported(shell.kind.clone()));
        }
        let scratch = Scratch::create(&shell.kind, options.load_user_config)
            .map_err(ShellSessionError::Spawn)?;
        let mut cmd = CommandBuilder::new(&shell.program);
        configure_command(&mut cmd, &shell.kind, &scratch, options.load_user_config);
        cmd.cwd(cwd);
        cmd.env("MRT_SHELL_INTEGRATION", "1");
        crate::pty::apply_terminal_env(&mut cmd);
        for (key, value) in &options.env {
            cmd.env(key, value);
        }
        Self::start(shell.kind.clone(), shell.program.clone(), cmd, scratch, options, None)
    }

    /// Starts a remote shell over SSH (or any program that ends in an interactive Bash
    /// or Zsh, e.g. `ssh -tt host`) and installs the integration by typing it in: see
    /// [`remote_bootstrap_lines`](crate::shell_integration::remote_bootstrap_lines).
    /// Nothing is written on the remote machine. Commands are typed as base64 `eval`
    /// lines. Shells other than Bash and Zsh never report a prompt, so this returns
    /// `IntegrationTimeout`; the caller should then offer a plain interactive session.
    pub fn spawn_remote(
        program: &str,
        args: &[String],
        cwd: &Path,
        options: ShellSessionOptions,
    ) -> Result<Self, ShellSessionError> {
        let scratch = Scratch::create(&ShellKind::Bash, false).map_err(ShellSessionError::Spawn)?;
        let mut cmd = CommandBuilder::new(program);
        for arg in args {
            cmd.arg(arg);
        }
        cmd.cwd(cwd);
        crate::pty::apply_terminal_env(&mut cmd);
        for (key, value) in &options.env {
            cmd.env(key, value);
        }
        let bootstrap = crate::shell_integration::remote_bootstrap_lines();
        Self::start(ShellKind::Bash, program.to_string(), cmd, scratch, options, Some(bootstrap))
    }

    fn start(
        kind: ShellKind,
        program: String,
        cmd: CommandBuilder,
        scratch: Scratch,
        options: ShellSessionOptions,
        remote_bootstrap: Option<Vec<String>>,
    ) -> Result<Self, ShellSessionError> {
        let spawn = || -> anyhow::Result<_> {
            let pair = native_pty_system().openpty(PtySize {
                rows: options.rows,
                cols: options.cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
            let child = pair.slave.spawn_command(cmd)?;
            let reader = pair.master.try_clone_reader()?;
            let writer = pair.master.take_writer()?;
            Ok((pair.master, child, reader, writer))
        };
        let (master, mut child, reader, writer) = spawn().map_err(|error| {
            ShellSessionError::Spawn(error.context(format!("could not start `{program}`")))
        })?;

        let (ready_tx, ready_rx) = mpsc::channel();
        let remote = remote_bootstrap.is_some();
        let shared = Arc::new(Shared {
            kind: kind.clone(),
            remote,
            scratch,
            writer: Mutex::new(writer),
            rows: AtomicU16::new(options.rows.max(1)),
            state: Mutex::new(State {
                phase: Phase::Starting,
                active: None,
                queue: VecDeque::new(),
                cwd: None,
                ready: Some(ready_tx),
                transcript: String::new(),
            }),
        });
        let session = Self {
            kind,
            program: program.clone(),
            pid: child.process_id(),
            killer: Mutex::new(child.clone_killer()),
            master: Mutex::new(master),
            shared: Arc::clone(&shared),
        };

        let reader_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("mrt-session-reader".into())
            .spawn(move || reader_loop(reader_shared, reader))
            .map_err(|error| ShellSessionError::Spawn(error.into()))?;
        let waiter_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("mrt-session-waiter".into())
            .spawn(move || {
                let code = child.wait().ok().map(|status| status.exit_code() as i32);
                // Let the reader deliver the last output before commands are failed.
                std::thread::sleep(Duration::from_millis(200));
                waiter_shared.on_exit(code);
            })
            .map_err(|error| ShellSessionError::Spawn(error.into()))?;

        // Remote shells get the integration typed in. The lines wait in the PTY until
        // the remote shell reads them, so they can be sent right away.
        if let Some(lines) = remote_bootstrap {
            for line in lines {
                shared.write(format!("{line}\r").as_bytes())?;
            }
        }

        match ready_rx.recv_timeout(options.ready_timeout) {
            Ok(Startup::Ready) => Ok(session),
            Ok(Startup::Exited) | Err(RecvTimeoutError::Disconnected) => {
                Err(ShellSessionError::StartupExited {
                    program: program.clone(),
                    transcript: shared.state().transcript.clone(),
                })
            }
            Err(RecvTimeoutError::Timeout) => Err(ShellSessionError::IntegrationTimeout {
                program: program.clone(),
                waited: options.ready_timeout,
                transcript: shared.state().transcript.clone(),
            }),
        }
    }

    /// Types `command` into the shell. Commands run one at a time in the order they were
    /// given; a command given while another runs waits for the next prompt.
    ///
    /// The command is passed to the shell through a file (POSIX shells, fish and PowerShell
    /// source it in the current scope), so quoting, tabs and multi-line scripts are kept
    /// as written. CMD has no way to source a file without batch-file semantics, so it gets
    /// the command typed on one line, with multiple lines joined by ` & `.
    pub fn run(&self, command: &str) -> Result<CommandHandle, ShellSessionError> {
        let (line, file) = if self.shared.remote {
            let line = crate::shell_integration::remote_command_line(command).ok_or_else(|| {
                ShellSessionError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "command too long to send to a remote session",
                ))
            })?;
            (line, None)
        } else {
            command_line(&self.kind, &self.shared.scratch, command)
        };
        let (tx, rx) = mpsc::channel();
        {
            let mut state = self.shared.state();
            if state.phase == Phase::Exited {
                return Err(ShellSessionError::Exited);
            }
            state.queue.push_back(Queued { line, file, tx });
        }
        self.shared.pump();
        Ok(CommandHandle { rx, outcome: None })
    }

    /// The working directory the shell reported last.
    pub fn cwd(&self) -> Option<PathBuf> {
        self.shared.state().cwd.clone()
    }

    /// `true` when the shell is at its prompt with no command running or queued.
    pub fn is_idle(&self) -> bool {
        let state = self.shared.state();
        state.phase == Phase::Idle && state.active.is_none() && state.queue.is_empty()
    }

    pub fn has_exited(&self) -> bool {
        self.shared.state().phase == Phase::Exited
    }

    pub fn kind(&self) -> &ShellKind {
        &self.kind
    }

    pub fn program(&self) -> &str {
        &self.program
    }

    /// OS process id of the shell, when the platform reports one.
    pub fn process_id(&self) -> Option<u32> {
        self.pid
    }

    /// Writes raw input to the shell, e.g. an answer to a prompt from the running command.
    pub fn send_input(&self, input: &str) -> Result<(), ShellSessionError> {
        self.shared.write(input.as_bytes())?;
        Ok(())
    }

    /// Sends Ctrl+C to the running command.
    pub fn interrupt(&self) -> Result<(), ShellSessionError> {
        self.send_input("\x03")
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), ShellSessionError> {
        let master = self
            .master
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(ShellSessionError::Spawn)?;
        self.shared.rows.store(rows.max(1), Ordering::Relaxed);
        Ok(())
    }

    /// Kills the shell. Running and queued commands finish with `shell_exited`.
    pub fn kill(&self) -> Result<(), ShellSessionError> {
        let mut killer = self
            .killer
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        killer.kill()?;
        Ok(())
    }
}

impl Drop for ShellSession {
    fn drop(&mut self) {
        if !self.has_exited() {
            let _ = self.kill();
        }
    }
}

/// Builds the line typed into the shell and the contents of the command file, if any.
fn command_line(kind: &ShellKind, scratch: &Scratch, command: &str) -> (String, Option<String>) {
    let posix_body = || Some(command.replace("\r\n", "\n"));
    let file = shell_path(kind, &scratch.command_file);
    match kind {
        ShellKind::PowerShellCore | ShellKind::WindowsPowerShell => (
            format!(". (__mrt_sb {})", ps_quote(&file)),
            Some(command.to_string()),
        ),
        ShellKind::CmdBatch => {
            let joined = command
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" & ");
            (
                format!(
                    "call \"{}\" & {joined} & call \"{}\"",
                    scratch.cmd_begin.display(),
                    scratch.cmd_end.display()
                ),
                None,
            )
        }
        ShellKind::Fish => (
            format!(" __mrt_c; source {}", fish_quote(&file)),
            posix_body(),
        ),
        ShellKind::Sh => (
            format!(" __mrt_c; . {}; __mrt_d $?", sh_quote(&file)),
            posix_body(),
        ),
        // Bash, Git Bash, Zsh: the prompt hook reports `133;D`.
        _ => (format!(" __mrt_c; . {}", sh_quote(&file)), posix_body()),
    }
}

fn configure_command(
    cmd: &mut CommandBuilder,
    kind: &ShellKind,
    scratch: &Scratch,
    load_user: bool,
) {
    let init = shell_path(kind, &scratch.init_file);
    match kind {
        ShellKind::PowerShellCore | ShellKind::WindowsPowerShell => {
            cmd.arg("-NoLogo");
            if !load_user {
                cmd.arg("-NoProfile");
            }
            cmd.arg("-NoExit");
            cmd.arg("-Command");
            // A script block is not subject to the execution policy, unlike a `.ps1` file.
            cmd.arg(format!(
                ". ([scriptblock]::Create([IO.File]::ReadAllText({})))",
                ps_quote(&init)
            ));
        }
        ShellKind::CmdBatch => {
            if !load_user {
                cmd.arg("/D");
            }
            cmd.arg("/K");
            cmd.arg(&init);
        }
        ShellKind::Sh => {
            cmd.env("ENV", &init);
            cmd.arg("-i");
        }
        ShellKind::Zsh => {
            let user_dir = std::env::var_os("ZDOTDIR")
                .or_else(|| std::env::var_os("HOME"))
                .unwrap_or_default();
            cmd.env("MRT_USER_ZDOTDIR", user_dir);
            cmd.env("MRT_LOAD_USER_CONFIG", if load_user { "1" } else { "0" });
            cmd.env("ZDOTDIR", &scratch.dir);
            cmd.arg("-i");
        }
        ShellKind::Fish => {
            cmd.arg("-i");
            if !load_user {
                cmd.arg("--no-config");
            }
            cmd.arg("-C");
            cmd.arg(format!("source {}", fish_quote(&init)));
        }
        _ => {
            // Bash and Git Bash. Long options must come before `-i`.
            cmd.arg("--rcfile");
            cmd.arg(&init);
            cmd.arg("-i");
        }
    }
}

/// Git Bash reports MSYS paths (`/c/Users/me`); turn drive paths back into `C:\Users\me`.
fn native_cwd(kind: &ShellKind, path: PathBuf) -> PathBuf {
    if !cfg!(target_os = "windows") || !matches!(kind, ShellKind::Bash | ShellKind::GitBash) {
        return path;
    }
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    let is_drive = bytes.len() >= 2
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && (bytes.len() == 2 || bytes[2] == b'/');
    if !is_drive {
        return path;
    }
    let rest = text.get(2..).unwrap_or("").replace('/', "\\");
    let rest = if rest.is_empty() {
        "\\".to_string()
    } else {
        rest
    };
    PathBuf::from(format!(
        "{}:{rest}",
        (bytes[1] as char).to_ascii_uppercase()
    ))
}

/// POSIX shells on Windows (Git Bash) want forward slashes.
fn shell_path(kind: &ShellKind, path: &Path) -> String {
    let text = path.display().to_string();
    let posix = !matches!(
        kind,
        ShellKind::PowerShellCore | ShellKind::WindowsPowerShell | ShellKind::CmdBatch
    );
    if cfg!(target_os = "windows") && posix {
        text.replace('\\', "/")
    } else {
        text
    }
}

fn sh_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

fn fish_quote(text: &str) -> String {
    format!("'{}'", text.replace('\\', r"\\").replace('\'', r"\'"))
}

fn ps_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// A per-session temp directory holding the init script and the command file.
struct Scratch {
    dir: PathBuf,
    init_file: PathBuf,
    command_file: PathBuf,
    cmd_begin: PathBuf,
    cmd_end: PathBuf,
}

impl Scratch {
    fn create(kind: &ShellKind, load_user: bool) -> anyhow::Result<Self> {
        let dir =
            std::env::temp_dir().join(format!("mrt-session-{}", uuid::Uuid::new_v4().simple()));
        static PRUNED: std::sync::Once = std::sync::Once::new();
        PRUNED.call_once(|| {
            prune_stale_scratch_dirs(&std::env::temp_dir(), STALE_SCRATCH_AGE);
        });
        std::fs::create_dir_all(&dir)?;
        let script = generate_shell_integration_script(kind.clone());
        let (init_name, init_body, command_name) = match kind {
            ShellKind::PowerShellCore | ShellKind::WindowsPowerShell => (
                "init.ps1",
                // MRT edits the command line itself; PSReadLine would only re-render it.
                format!("Remove-Module PSReadLine -ErrorAction SilentlyContinue\n{script}"),
                "command.ps1",
            ),
            ShellKind::CmdBatch => (
                "init.cmd",
                format!(
                    "@echo off\r\n{}\r\necho on\r\n",
                    script.lines().collect::<Vec<_>>().join("\r\n")
                ),
                "command.txt",
            ),
            ShellKind::Zsh => {
                // zsh reads `$ZDOTDIR/.zshenv` and `.zshrc`; ours load the user's own files.
                std::fs::write(
                    dir.join(".zshenv"),
                    "__mrt_session_dir=$ZDOTDIR\nZDOTDIR=$MRT_USER_ZDOTDIR\n\
                     [[ $MRT_LOAD_USER_CONFIG == 1 && -f $ZDOTDIR/.zshenv ]] && source $ZDOTDIR/.zshenv\n\
                     MRT_USER_ZDOTDIR=$ZDOTDIR\nZDOTDIR=$__mrt_session_dir\n",
                )?;
                (
                    ".zshrc",
                    format!(
                        "ZDOTDIR=$MRT_USER_ZDOTDIR\nunset __mrt_session_dir\n\
                         [[ $MRT_LOAD_USER_CONFIG == 1 && -f $ZDOTDIR/.zshrc ]] && source $ZDOTDIR/.zshrc\n{script}"
                    ),
                    "command.zsh",
                )
            }
            ShellKind::Fish => ("init.fish", script.to_string(), "command.fish"),
            ShellKind::Sh => ("init.sh", script.to_string(), "command.sh"),
            _ => {
                let user = if load_user {
                    "[ -f ~/.bashrc ] && . ~/.bashrc\n"
                } else {
                    ""
                };
                ("init.bash", format!("{user}{script}"), "command.sh")
            }
        };
        let init_file = dir.join(init_name);
        std::fs::write(&init_file, init_body)?;
        let cmd_begin = dir.join("begin.cmd");
        let cmd_end = dir.join("end.cmd");
        if *kind == ShellKind::CmdBatch {
            // `set /p` prints without a newline. The begin helper resets ERRORLEVEL so that
            // commands which never set it (`echo`, `cd`) report 0, as `cmd /C` would.
            std::fs::write(
                &cmd_begin,
                "@<nul set /p \"=\x1b]133;C\x1b\\\"\r\n@exit /b 0\r\n",
            )?;
            std::fs::write(
                &cmd_end,
                "@set \"__mrt_rc=%ERRORLEVEL%\"\r\n\
                 @<nul set /p \"=\x1b]7;file://localhost/%CD%\x1b\\\"\r\n\
                 @<nul set /p \"=\x1b]133;D;%__mrt_rc%\x1b\\\"\r\n\
                 @set \"__mrt_rc=\" & exit /b %__mrt_rc%\r\n",
            )?;
        }
        Ok(Self {
            command_file: dir.join(command_name),
            dir,
            init_file,
            cmd_begin,
            cmd_end,
        })
    }
}

/// Scratch folders older than this are left over from sessions that ended without
/// cleaning up (a crash, a killed process, or a shell still holding the folder on
/// Windows) and are removed the first time a session starts.
const STALE_SCRATCH_AGE: Duration = Duration::from_secs(12 * 60 * 60);

/// Removes `mrt-session-*` folders under `temp` last modified more than `age` ago.
/// Returns how many were removed.
fn prune_stale_scratch_dirs(temp: &Path, age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(temp) else { return 0 };
    let now = std::time::SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("mrt-session-") {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|elapsed| elapsed >= age);
        if old && entry.path().is_dir() && std::fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Waiting for the first prompt.
    Starting,
    /// At the prompt, ready for input.
    Idle,
    /// A command was typed; its output starts at `133;C`.
    Typed,
    Running,
    /// `133;D` arrived; output that ConPTY delivered late is still collected until the
    /// next prompt (Windows only).
    Draining,
    /// The command finished; waiting for the next prompt.
    Finishing,
    Exited,
}

enum Startup {
    Ready,
    Exited,
}

struct Queued {
    line: String,
    file: Option<String>,
    tx: Sender<CommandEvent>,
}

struct Active {
    tx: Sender<CommandEvent>,
    dispatched: Instant,
    started: Option<Instant>,
    /// Exit code and time of `133;D`, while draining.
    ended: Option<(Option<i32>, Instant)>,
    output: String,
}

/// ConPTY renders text on a timer but passes OSC sequences straight through, so on Windows
/// `133;D` can overtake the last lines of output. Output is collected until the next prompt.
const DRAIN_UNTIL_PROMPT: bool = cfg!(target_os = "windows");

struct State {
    phase: Phase,
    active: Option<Active>,
    queue: VecDeque<Queued>,
    cwd: Option<PathBuf>,
    ready: Option<Sender<Startup>>,
    transcript: String,
}

impl State {
    /// Resolves the active command. A code from `133;D` wins over `exit_code`.
    fn finish(&mut self, exit_code: Option<i32>, shell_exited: bool) {
        let Some(active) = self.active.take() else {
            return;
        };
        let mut output = active.output;
        output.truncate(output.trim_end_matches(['\n', '\r']).len());
        let started = active.started.unwrap_or(active.dispatched);
        let (exit_code, ended) = active.ended.unwrap_or((exit_code, Instant::now()));
        let _ = active.tx.send(CommandEvent::Finished(CommandOutcome {
            exit_code,
            duration: ended.saturating_duration_since(started),
            output,
            cwd: self.cwd.clone(),
            shell_exited,
        }));
    }
}

struct Shared {
    kind: ShellKind,
    /// Commands are typed as base64 `eval` lines (SSH sessions) instead of files.
    remote: bool,
    scratch: Scratch,
    writer: Mutex<Box<dyn Write + Send>>,
    rows: AtomicU16,
    state: Mutex<State>,
}

impl Shared {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn write(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        writer.write_all(bytes)?;
        writer.flush()
    }

    /// Types the next queued command when the shell is at its prompt.
    fn pump(&self) {
        let (line, file) = {
            let mut state = self.state();
            if state.phase != Phase::Idle || state.active.is_some() {
                return;
            }
            let Some(next) = state.queue.pop_front() else {
                return;
            };
            state.phase = Phase::Typed;
            state.active = Some(Active {
                tx: next.tx,
                dispatched: Instant::now(),
                started: None,
                ended: None,
                output: String::new(),
            });
            (next.line, next.file)
        };
        // Written outside the state lock: a blocked PTY write must not stop the reader.
        let result = file
            .map_or(Ok(()), |body| {
                std::fs::write(&self.scratch.command_file, body)
            })
            .and_then(|()| self.write(format!("{line}\r").as_bytes()));
        if let Err(error) = result {
            let mut state = self.state();
            if let Some(active) = state.active.as_mut() {
                active.output =
                    format!("MRT could not send the command to {:?}: {error}", self.kind);
            }
            state.finish(None, false);
            state.phase = Phase::Idle;
        }
    }

    fn apply(&self, pieces: Vec<Piece>, raw: &[u8]) {
        let mut replies = Vec::new();
        {
            let mut state = self.state();
            if state.phase == Phase::Starting && state.transcript.len() < STARTUP_TRANSCRIPT_LIMIT {
                state.transcript.push_str(&String::from_utf8_lossy(raw));
            }
            for piece in pieces {
                match piece {
                    Piece::Text(text) => {
                        if matches!(state.phase, Phase::Running | Phase::Draining) {
                            if let Some(active) = state.active.as_mut() {
                                active.output.push_str(&text);
                                // Keep memory bounded for huge outputs: the block keeps
                                // only the end anyway (trim in steps, not per chunk).
                                let max = crate::block::max_block_output_bytes();
                                if max > 0 && active.output.len() > max + max / 4 {
                                    crate::block::keep_tail(&mut active.output, max);
                                }
                                let _ = active.tx.send(CommandEvent::Output(text));
                            }
                        }
                    }
                    Piece::Cwd(path) => state.cwd = Some(native_cwd(&self.kind, path)),
                    Piece::CursorReport(row, col) => {
                        replies.push(format!("\x1b[{row};{col}R"));
                    }
                    Piece::Mark(Osc133Kind::CommandExecuted) => {
                        if state.phase == Phase::Typed {
                            state.phase = Phase::Running;
                            if let Some(active) = state.active.as_mut() {
                                active.started = Some(Instant::now());
                            }
                        }
                    }
                    Piece::Mark(Osc133Kind::CommandFinished(code)) => {
                        if matches!(state.phase, Phase::Typed | Phase::Running) {
                            if DRAIN_UNTIL_PROMPT {
                                if let Some(active) = state.active.as_mut() {
                                    active.ended = Some((code, Instant::now()));
                                }
                                state.phase = Phase::Draining;
                            } else {
                                state.finish(code, false);
                                state.phase = Phase::Finishing;
                            }
                        }
                    }
                    Piece::Mark(Osc133Kind::PromptStart) => {
                        // Without `133;D` first, the shell abandoned the line (e.g. `sh`).
                        if matches!(state.phase, Phase::Typed | Phase::Running | Phase::Draining) {
                            state.finish(None, false);
                            state.phase = Phase::Finishing;
                        }
                    }
                    Piece::Mark(Osc133Kind::CommandStart) => {
                        if matches!(
                            state.phase,
                            Phase::Starting | Phase::Draining | Phase::Finishing
                        ) {
                            state.finish(None, false);
                            if let Some(ready) = state.ready.take() {
                                let _ = ready.send(Startup::Ready);
                            }
                            state.transcript = String::new();
                            state.phase = Phase::Idle;
                        }
                    }
                }
            }
        }
        for reply in replies {
            let _ = self.write(reply.as_bytes());
        }
        self.pump();
    }

    fn on_exit(&self, code: Option<i32>) {
        let mut state = self.state();
        if state.phase == Phase::Exited {
            return;
        }
        state.phase = Phase::Exited;
        if let Some(ready) = state.ready.take() {
            let _ = ready.send(Startup::Exited);
        }
        state.finish(code, true);
        while let Some(queued) = state.queue.pop_front() {
            state.active = Some(Active {
                tx: queued.tx,
                dispatched: Instant::now(),
                started: None,
                ended: None,
                output: String::new(),
            });
            state.finish(None, true);
        }
    }
}

fn reader_loop(shared: Arc<Shared>, mut reader: Box<dyn Read + Send>) {
    let mut parser = vte::Parser::new();
    let mut normalizer = Normalizer::new(shared.rows.load(Ordering::Relaxed));
    let mut buffer = [0u8; 8192];
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        normalizer.rows = shared.rows.load(Ordering::Relaxed);
        normalizer.advance(&mut parser, &buffer[..count]);
        normalizer.flush_text();
        shared.apply(std::mem::take(&mut normalizer.pieces), &buffer[..count]);
    }
    shared.on_exit(None);
}

#[derive(Debug, Clone, PartialEq)]
enum Piece {
    Text(String),
    Mark(Osc133Kind),
    Cwd(PathBuf),
    /// The shell (or ConPTY) asked for the cursor position (`CSI 6 n`).
    CursorReport(u16, u16),
}

/// Turns PTY output into text pieces and markers. It keeps SGR colors and tracks the cursor
/// row so that ConPTY's cursor positioning (which it uses instead of newlines when it
/// repaints) becomes newlines and spaces again.
struct Normalizer {
    rows: u16,
    row: u16,
    col: u16,
    pending_cr: bool,
    text: String,
    pieces: Vec<Piece>,
    /// Kitty graphics come as APC strings, which `vte` skips; found here and passed on.
    apc: crate::images::ApcScanner,
    /// Data of a Sixel DCS being passed through (`None` outside one).
    sixel: Option<Vec<u8>>,
}

impl Normalizer {
    fn new(rows: u16) -> Self {
        Self {
            rows: rows.max(1),
            row: 1,
            col: 1,
            pending_cr: false,
            text: String::new(),
            pieces: Vec::new(),
            apc: crate::images::ApcScanner::default(),
            sixel: None,
        }
    }

    fn advance(&mut self, parser: &mut vte::Parser, bytes: &[u8]) {
        for &byte in bytes {
            if let Some(apc) = self.apc.advance(byte) {
                // Kitty graphics pass through to the block's parser unchanged.
                if apc.first() == Some(&b'G') {
                    self.text.push_str("\x1b_");
                    self.text.push_str(&String::from_utf8_lossy(&apc));
                    self.text.push_str("\x1b\\");
                }
            }
            parser.advance(self, byte);
        }
    }

    fn flush_text(&mut self) {
        if !self.text.is_empty() {
            self.pieces
                .push(Piece::Text(std::mem::take(&mut self.text)));
        }
    }

    fn push_piece(&mut self, piece: Piece) {
        self.flush_text();
        self.pieces.push(piece);
    }

    fn newlines(&mut self, count: u16) {
        self.pending_cr = false;
        for _ in 0..count {
            self.text.push('\n');
        }
        self.row = self.row.saturating_add(count).min(self.rows);
        self.col = 1;
    }

    fn move_to(&mut self, row: u16, col: u16) {
        let row = row.clamp(1, self.rows);
        if row > self.row {
            self.newlines(row - self.row);
        }
        self.row = row;
        if col > self.col {
            self.pending_cr = false;
            self.text
                .extend(std::iter::repeat_n(' ', usize::from(col - self.col)));
        }
        self.col = col.max(1);
    }
}

fn param(params: &Params, index: usize, default: u16) -> u16 {
    params
        .iter()
        .nth(index)
        .and_then(|values| values.first().copied())
        .filter(|value| *value != 0)
        .unwrap_or(default)
}

impl Perform for Normalizer {
    fn print(&mut self, c: char) {
        if self.pending_cr {
            self.text.push('\r');
            self.pending_cr = false;
        }
        self.text.push(c);
        self.col = self.col.saturating_add(1);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.newlines(1),
            b'\r' => {
                self.pending_cr = true;
                self.col = 1;
            }
            b'\t' => {
                self.print('\t');
                self.col = (self.col.saturating_sub(1) / 8 + 1) * 8 + 1;
            }
            0x08 => self.col = self.col.saturating_sub(1).max(1),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        if !intermediates.is_empty() {
            return; // Private modes such as `?25l` (cursor visibility).
        }
        match action {
            'm' => {
                let codes = params
                    .iter()
                    .map(|values| {
                        values
                            .iter()
                            .map(u16::to_string)
                            .collect::<Vec<_>>()
                            .join(":")
                    })
                    .collect::<Vec<_>>()
                    .join(";");
                self.text.push_str(&format!("\x1b[{codes}m"));
            }
            'H' | 'f' => self.move_to(param(params, 0, 1), param(params, 1, 1)),
            'd' => self.move_to(param(params, 0, 1), self.col),
            'G' | '`' => self.move_to(self.row, param(params, 0, 1)),
            'C' => {
                let col = self.col.saturating_add(param(params, 0, 1));
                self.move_to(self.row, col);
            }
            'B' | 'E' => self.newlines(param(params, 0, 1)),
            'n' if param(params, 0, 0) == 6 => {
                let (row, col) = (self.row, self.col);
                self.push_piece(Piece::CursorReport(row, col));
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        match params.first().copied() {
            Some(b"133") => {
                let kind = match params.get(1).and_then(|value| value.first()) {
                    Some(b'A') => Osc133Kind::PromptStart,
                    Some(b'B') => Osc133Kind::CommandStart,
                    Some(b'C') => Osc133Kind::CommandExecuted,
                    Some(b'D') => Osc133Kind::CommandFinished(
                        params
                            .get(2)
                            .and_then(|code| std::str::from_utf8(code).ok())
                            .and_then(|code| code.trim().parse().ok()),
                    ),
                    _ => return,
                };
                self.push_piece(Piece::Mark(kind));
            }
            Some(b"7") => {
                let payload = params[1..].join(&b';');
                if let Some(path) = parse_osc7_path(&String::from_utf8_lossy(&payload)) {
                    self.push_piece(Piece::Cwd(path));
                }
            }
            // Hyperlinks, clipboard requests and inline images are for the block's own
            // parser (`vt.rs`): pass them on as they came.
            Some(b"8" | b"52" | b"1337") => {
                self.text.push_str("\x1b]");
                self.text.push_str(&String::from_utf8_lossy(&params.join(&b';')));
                self.text.push_str("\x1b\\");
            }
            _ => {}
        }
    }

    fn hook(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        if action == 'q' && intermediates.is_empty() {
            let mut head = String::from("\x1bP");
            let values: Vec<String> = params.iter().map(|p| p.first().copied().unwrap_or(0).to_string()).collect();
            head.push_str(&values.join(";"));
            head.push('q');
            self.sixel = Some(head.into_bytes());
        }
    }

    fn put(&mut self, byte: u8) {
        if let Some(data) = self.sixel.as_mut() {
            if data.len() < crate::images::MAX_IMAGE_PAYLOAD {
                data.push(byte);
            }
        }
    }

    fn unhook(&mut self) {
        if let Some(data) = self.sixel.take() {
            self.text.push_str(&String::from_utf8_lossy(&data));
            self.text.push_str("\x1b\\");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::discover_shells;

    #[test]
    fn stale_scratch_folders_are_pruned() {
        let temp = std::env::temp_dir().join(format!("mrt-prune-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(temp.join("mrt-session-old")).unwrap();
        std::fs::create_dir_all(temp.join("other-folder")).unwrap();
        std::fs::write(temp.join("mrt-session-old").join("init.sh"), b"x").unwrap();
        // Nothing is old enough yet.
        assert_eq!(prune_stale_scratch_dirs(&temp, Duration::from_secs(3600)), 0);
        // With a zero age everything counts as stale, but only mrt-session-* folders go.
        assert_eq!(prune_stale_scratch_dirs(&temp, Duration::ZERO), 1);
        assert!(!temp.join("mrt-session-old").exists());
        assert!(temp.join("other-folder").exists());
        std::fs::remove_dir_all(&temp).unwrap();
    }

    fn normalize(input: &[u8]) -> Vec<Piece> {
        let mut parser = vte::Parser::new();
        let mut normalizer = Normalizer::new(24);
        normalizer.advance(&mut parser, input);
        normalizer.flush_text();
        normalizer.pieces
    }

    #[test]
    fn normalizer_passes_links_clipboard_and_images_through() {
        let stream = b"\x1b]8;;https://a.io\x07L\x1b]8;;\x07 \x1b]52;c;aGk=\x07\x1b]1337;File=inline=1:AAAA\x07\x1b_Ga=T,f=24;AAAA\x1b\\\x1bP0;1;0q#1~-\x1b\\\x1b]0;title\x07";
        let pieces = normalize(stream);
        let text: String = pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        let st = "\x1b\\";
        assert_eq!(
            text,
            format!(
                "\x1b]8;;https://a.io{st}L\x1b]8;;{st} \x1b]52;c;aGk={st}\x1b]1337;File=inline=1:AAAA{st}\x1b_Ga=T,f=24;AAAA{st}\x1bP0;1;0q#1~-{st}"
            )
        );
        let parsed = crate::vt::parse_ansi_rich(&text);
        assert!(parsed.plain_text.starts_with("L "), "{:?}", parsed.plain_text);
        assert_eq!(parsed.plain_text.matches("[image not shown").count(), 2, "{:?}", parsed.plain_text);
        assert!(parsed.spans.iter().any(|s| s.link.as_deref() == Some("https://a.io")));
        assert_eq!(parsed.events.len(), 1, "clipboard request survives");
        assert_eq!(parsed.images.len(), 1, "the sixel decodes; the fake PNG and RGB payloads are reported");
    }

    #[test]
    fn normalizer_turns_conpty_cursor_moves_into_newlines_and_keeps_colors() {
        let stream = b"\x1b[?25l\x1b[2J\x1b[m\x1b[H\x1b]0;title\x07\x1b]133;C\x1b\\\x1b[31mred\x1b[m\r\nnext\x1b[4;1Hlast\x1b[4;8Hx\x1b]133;D;42\x07";
        let pieces = normalize(stream);
        assert_eq!(
            pieces,
            vec![
                Piece::Text("\x1b[0m".into()),
                Piece::Mark(Osc133Kind::CommandExecuted),
                Piece::Text("\x1b[31mred\x1b[0m\nnext\n\nlast   x".into()),
                Piece::Mark(Osc133Kind::CommandFinished(Some(42))),
            ]
        );
    }

    #[test]
    fn normalizer_reports_cwd_and_cursor_queries() {
        let pieces = normalize(b"\x1b]7;file://host/tmp/a;b\x1b\\\x1b[6n");
        assert_eq!(
            pieces,
            vec![
                Piece::Cwd(PathBuf::from("/tmp/a;b")),
                Piece::CursorReport(1, 1)
            ]
        );
    }

    #[test]
    fn quoting_helpers_escape_quotes() {
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
        assert_eq!(fish_quote(r"a\b'c"), r"'a\\b\'c'");
        assert_eq!(ps_quote("O'Brien"), "'O''Brien'");
    }

    #[test]
    fn unsupported_shells_ask_for_fallback() {
        let profile = ShellProfile {
            id: "nu".into(),
            name: "Nushell".into(),
            kind: ShellKind::Nushell,
            program: "nu".into(),
            interactive_args: vec![],
            exec_flag: vec!["-c".into()],
        };
        let error = ShellSession::spawn(&profile, Path::new("."), Default::default())
            .err()
            .expect("Nushell has no session support");
        assert!(error.should_fall_back());
    }

    #[test]
    fn missing_marker_times_out_with_fallback() {
        // An interactive shell whose prompt has no OSC 133 markers: `sh` with an empty
        // `ENV` script, or `cmd.exe` (which ignores `-i` and `ENV`) on Windows.
        let profile = ShellProfile {
            id: "quiet".into(),
            name: "quiet".into(),
            kind: ShellKind::Sh,
            program: if cfg!(target_os = "windows") {
                "cmd.exe"
            } else {
                "sh"
            }
            .into(),
            interactive_args: vec![],
            exec_flag: vec![],
        };
        let options = ShellSessionOptions {
            ready_timeout: Duration::from_secs(3),
            env: vec![("ENV".into(), "/dev/null".into())],
            ..Default::default()
        };
        match ShellSession::spawn(&profile, &std::env::temp_dir(), options) {
            Err(error @ ShellSessionError::IntegrationTimeout { .. }) => {
                assert!(error.should_fall_back())
            }
            Err(error) => panic!("expected a timeout, got {error}"),
            Ok(_) => panic!("a prompt without markers must not produce a session"),
        }
    }

    struct Case {
        cd_and_print: fn(&Path) -> (String, String),
        set_var: &'static str,
        print_var: &'static str,
        exit_codes: [&'static str; 3],
    }

    fn posix_case() -> Case {
        Case {
            cd_and_print: |dir| {
                (
                    format!("cd {}", sh_quote(&shell_path(&ShellKind::Bash, dir))),
                    "pwd".into(),
                )
            },
            set_var: "export MRT_SESSION_TEST=persisted",
            print_var: "echo \"$MRT_SESSION_TEST\"",
            exit_codes: ["true", "false", "(exit 42)"],
        }
    }

    /// Generous: a loaded CI machine (or Git Bash on Windows) can take many seconds to start.
    const TEST_TIMEOUT: Duration = Duration::from_secs(90);

    fn find_shell(kind: ShellKind) -> Option<ShellProfile> {
        let found = discover_shells()
            .into_iter()
            .find(|shell| shell.kind == kind);
        if found.is_none() {
            eprintln!("skipping {kind:?}: not installed");
        }
        found
    }

    fn start(shell: &ShellProfile) -> ShellSession {
        let options = ShellSessionOptions {
            load_user_config: false,
            ready_timeout: TEST_TIMEOUT,
            ..Default::default()
        };
        match ShellSession::spawn(shell, &std::env::temp_dir(), options) {
            Ok(session) => session,
            Err(ShellSessionError::IntegrationTimeout { transcript, .. })
            | Err(ShellSessionError::StartupExited { transcript, .. }) => {
                panic!(
                    "{:?} never became ready. Output:\n{}",
                    shell.kind,
                    transcript.escape_debug()
                )
            }
            Err(error) => panic!("{:?}: {error}", shell.kind),
        }
    }

    fn run(session: &ShellSession, command: &str) -> CommandOutcome {
        let mut handle = session.run(command).unwrap();
        handle
            .wait_timeout(TEST_TIMEOUT)
            .unwrap_or_else(|| panic!("{:?}: `{command}` did not finish", session.kind()))
    }

    fn exercise(shell: &ShellProfile, case: Case) {
        let session = start(shell);
        let kind = shell.kind.clone();

        // `cd` persists, and the shell reports the new directory through OSC 7.
        let target =
            std::env::temp_dir().join(format!("mrt session {}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&target).unwrap();
        let target_name = target.file_name().unwrap().to_string_lossy().to_string();
        let (cd, print) = (case.cd_and_print)(&target);
        let outcome = run(&session, &cd);
        assert_eq!(outcome.exit_code, Some(0), "{kind:?} cd: {outcome:?}");
        let outcome = run(&session, &print);
        assert_eq!(outcome.exit_code, Some(0), "{kind:?} pwd: {outcome:?}");
        assert!(
            outcome.plain_output().trim().ends_with(&target_name),
            "{kind:?} pwd printed {:?}, expected {target_name}",
            outcome.plain_output()
        );
        let cwd = session.cwd().expect("cwd reported");
        assert!(
            cwd.to_string_lossy().ends_with(&target_name),
            "{kind:?} cwd {cwd:?}"
        );

        // A variable set in one command is visible in the next.
        let outcome = run(&session, case.set_var);
        assert_eq!(outcome.exit_code, Some(0), "{kind:?} set: {outcome:?}");
        let outcome = run(&session, case.print_var);
        assert_eq!(
            outcome.plain_output().trim(),
            "persisted",
            "{kind:?}: {outcome:?}"
        );

        // Exact exit codes.
        for (command, expected) in case.exit_codes.iter().zip([0, 1, 42]) {
            let outcome = run(&session, command);
            assert_eq!(
                outcome.exit_code,
                Some(expected),
                "{kind:?} `{command}`: {outcome:?}"
            );
        }

        // Two commands queued back to back produce two separate outputs.
        let mut first = session.run("echo first-output").unwrap();
        let mut second = session.run("echo second-output").unwrap();
        let first = first.wait_timeout(TEST_TIMEOUT).expect("first finished");
        let second = second.wait_timeout(TEST_TIMEOUT).expect("second finished");
        assert_eq!(
            first.plain_output().trim(),
            "first-output",
            "{kind:?}: {first:?}"
        );
        assert_eq!(
            second.plain_output().trim(),
            "second-output",
            "{kind:?}: {second:?}"
        );

        drop(session);
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn bash_session_keeps_state() {
        if cfg!(target_os = "windows") {
            if let Some(shell) = find_shell(ShellKind::GitBash) {
                exercise(&shell, posix_case());
            }
        } else if let Some(shell) = find_shell(ShellKind::Bash) {
            exercise(&shell, posix_case());
        }
    }

    #[test]
    fn sh_session_keeps_state() {
        if cfg!(target_os = "windows") {
            eprintln!("skipping sh: not discovered on Windows");
            return;
        }
        if let Some(shell) = find_shell(ShellKind::Sh) {
            exercise(&shell, posix_case());
        }
    }

    #[test]
    fn zsh_session_keeps_state() {
        if let Some(shell) = find_shell(ShellKind::Zsh) {
            exercise(&shell, posix_case());
        }
    }

    /// Runs the remote-session checks against `program args`, a plain interactive shell
    /// standing in for the shell at the other end of `ssh -tt host`.
    fn check_remote(program: &str, args: &[&str], label: &str) {
        let options = ShellSessionOptions {
            ready_timeout: TEST_TIMEOUT,
            ..Default::default()
        };
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let session = match ShellSession::spawn_remote(program, &args, &std::env::temp_dir(), options) {
            Ok(session) => session,
            Err(ShellSessionError::IntegrationTimeout { transcript, .. })
            | Err(ShellSessionError::StartupExited { transcript, .. }) => {
                panic!("{label}: remote integration did not start. Output:\n{}", transcript.escape_debug())
            }
            Err(error) => panic!("{label}: {error}"),
        };
        let out = run(&session, "export MRT_REMOTE=kept; echo \"it's \\\"quoted\\\" $((40+2))\"");
        assert_eq!(out.exit_code, Some(0), "{label}: {:?}", out.plain_output());
        assert_eq!(out.plain_output().trim(), "it's \"quoted\" 42", "{label}");
        assert_eq!(run(&session, "echo $MRT_REMOTE").plain_output().trim(), "kept", "{label}");
        assert_eq!(run(&session, "false").exit_code, Some(1), "{label}");
        assert_eq!(run(&session, "printf a\nprintf b").plain_output().trim(), "ab", "{label}");
        let out = run(&session, "cd / && pwd");
        assert_eq!(out.exit_code, Some(0), "{label}");
        assert!(out.cwd.is_some(), "{label}: the remote shell should report its directory");
    }

    #[test]
    fn remote_session_bootstraps_bash_without_files() {
        let shell = if cfg!(windows) { find_shell(ShellKind::GitBash) } else { find_shell(ShellKind::Bash) };
        if let Some(shell) = shell {
            check_remote(&shell.program, &["--norc", "--noprofile", "-i"], "bash");
        }
    }

    /// An image printed in a session reaches the block's parser (ConPTY on Windows
    /// passes OSC 1337 through too).
    #[test]
    fn inline_images_pass_through_a_session() {
        let shell = if cfg!(windows) { find_shell(ShellKind::GitBash) } else { find_shell(ShellKind::Bash) };
        let Some(shell) = shell else { return };
        let options = ShellSessionOptions { ready_timeout: TEST_TIMEOUT, ..Default::default() };
        let args: Vec<String> = ["--norc", "--noprofile", "-i"].iter().map(|a| a.to_string()).collect();
        let session = ShellSession::spawn_remote(&shell.program, &args, &std::env::temp_dir(), options).expect("session");
        let mut png = Vec::new();
        image::RgbaImage::from_raw(1, 1, vec![1, 2, 3, 255])
            .unwrap()
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let b64 = crate::shell_integration::base64_encode(&png);
        let out = run(&session, &format!(r"printf 'before\n\033]1337;File=inline=1:{b64}\a\nafter\n'"));
        let parsed = crate::vt::parse_ansi_rich(&out.output);
        assert_eq!(parsed.images.len(), 1, "{:?}", out.output);
        assert_eq!(&parsed.images[0].rgba[..], &[1, 2, 3, 255]);
        assert!(parsed.plain_text.contains("before") && parsed.plain_text.contains("after"));
    }

    #[cfg(unix)]
    #[test]
    fn remote_session_bootstraps_zsh_without_files() {
        if let Some(shell) = find_shell(ShellKind::Zsh) {
            check_remote(&shell.program, &["-f", "-i"], "zsh");
        }
    }

    /// The GUI starts sessions with the user's own config (`load_user_config`), unlike
    /// the tests above. Every POSIX shell must still become ready that way, including
    /// a fresh account with no `.zshrc` / `.bashrc` / fish config at all.
    #[cfg(unix)]
    #[test]
    fn posix_sessions_start_with_user_config() {
        for kind in [ShellKind::Bash, ShellKind::Zsh, ShellKind::Fish, ShellKind::Sh] {
            let Some(shell) = find_shell(kind.clone()) else { continue };
            let options = ShellSessionOptions {
                load_user_config: true,
                ready_timeout: Duration::from_secs(15),
                ..Default::default()
            };
            match ShellSession::spawn(&shell, &std::env::temp_dir(), options) {
                Ok(session) => {
                    let out = run(&session, "echo ready-2");
                    assert_eq!(out.plain_output().trim(), "ready-2", "{kind:?}");
                }
                Err(ShellSessionError::IntegrationTimeout { transcript, .. })
                | Err(ShellSessionError::StartupExited { transcript, .. }) => {
                    panic!("{kind:?} with user config never became ready. Output:\n{}", transcript.escape_debug())
                }
                Err(error) => panic!("{kind:?}: {error}"),
            }
        }
    }

    #[test]
    fn fish_session_keeps_state() {
        if let Some(shell) = find_shell(ShellKind::Fish) {
            exercise(
                &shell,
                Case {
                    cd_and_print: |dir| {
                        (
                            format!("cd {}", fish_quote(&dir.display().to_string())),
                            "pwd".into(),
                        )
                    },
                    set_var: "set -gx MRT_SESSION_TEST persisted",
                    print_var: "echo $MRT_SESSION_TEST",
                    exit_codes: ["true", "false", "sh -c 'exit 42'"],
                },
            );
        }
    }

    fn powershell_case() -> Case {
        Case {
            cd_and_print: |dir| {
                (
                    format!(
                        "Set-Location -LiteralPath {}",
                        ps_quote(&dir.display().to_string())
                    ),
                    "(Get-Location).Path".into(),
                )
            },
            set_var: "$env:MRT_SESSION_TEST = 'persisted'; $mrtLocal = 1",
            print_var: "Write-Output $env:MRT_SESSION_TEST",
            exit_codes: [
                "Write-Output ok",
                if cfg!(target_os = "windows") {
                    "cmd /c exit 1"
                } else {
                    "sh -c 'exit 1'"
                },
                if cfg!(target_os = "windows") {
                    "cmd /c exit 42"
                } else {
                    "sh -c 'exit 42'"
                },
            ],
        }
    }

    #[test]
    fn windows_powershell_session_keeps_state() {
        if let Some(shell) = find_shell(ShellKind::WindowsPowerShell) {
            exercise(&shell, powershell_case());
        }
    }

    #[test]
    fn pwsh_session_keeps_state() {
        if let Some(shell) = find_shell(ShellKind::PowerShellCore) {
            exercise(&shell, powershell_case());
        }
    }

    #[test]
    fn cmd_session_keeps_state() {
        if let Some(shell) = find_shell(ShellKind::CmdBatch) {
            exercise(
                &shell,
                Case {
                    cd_and_print: |dir| (format!("cd /d \"{}\"", dir.display()), "cd".into()),
                    set_var: "set MRT_SESSION_TEST=persisted",
                    print_var: "echo %MRT_SESSION_TEST%",
                    exit_codes: ["echo ok", "cmd /c exit 1", "cmd /c exit 42"],
                },
            );
        }
    }

    #[test]
    fn powershell_functions_and_multiline_commands_persist() {
        let Some(shell) = find_shell(ShellKind::WindowsPowerShell)
            .or_else(|| find_shell(ShellKind::PowerShellCore))
        else {
            return;
        };
        let session = start(&shell);
        let outcome = run(
            &session,
            "function Get-MrtGreeting {\n    'hello from a function'\n}",
        );
        assert_eq!(outcome.exit_code, Some(0), "{outcome:?}");
        let outcome = run(&session, "Get-MrtGreeting");
        assert_eq!(outcome.plain_output().trim(), "hello from a function");
        let outcome = run(
            &session,
            "Get-Item -LiteralPath 'Z:\\definitely\\missing' -ErrorAction SilentlyContinue; $?",
        );
        assert_eq!(outcome.plain_output().trim(), "False");
        assert_eq!(outcome.exit_code, Some(0));
        let outcome = run(&session, "throw 'boom'");
        assert_eq!(outcome.exit_code, Some(1), "{outcome:?}");
    }

    #[test]
    fn posix_aliases_and_functions_persist() {
        let shell = if cfg!(target_os = "windows") {
            find_shell(ShellKind::GitBash)
        } else {
            find_shell(ShellKind::Bash)
        };
        let Some(shell) = shell else { return };
        let session = start(&shell);
        run(
            &session,
            "mrt_fn() { echo \"fn:$1\"; }\nalias mrtalias='echo from-alias'",
        );
        // A tab inside the command must not trigger readline completion.
        let outcome = run(&session, "mrt_fn 'a\tb'");
        assert_eq!(outcome.exit_code, Some(0), "{outcome:?}");
        let words: Vec<_> = outcome
            .plain_output()
            .split_whitespace()
            .map(String::from)
            .collect();
        assert_eq!(words, ["fn:a", "b"], "{outcome:?}");
        assert_eq!(
            run(&session, "mrtalias").plain_output().trim(),
            "from-alias"
        );
    }
    /// Ctrl+C stops the running command and leaves the shell usable.
    #[cfg(unix)]
    fn check_interrupt(shell: &ShellProfile, long: &str, after: &str, expect: &str) {
        let session = start(shell);
        let mut handle = session.run(long).unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        session.interrupt().unwrap();
        let started = Instant::now();
        let outcome = handle.wait_timeout(TEST_TIMEOUT);
        assert!(started.elapsed() < Duration::from_secs(10), "{:?}: `{long}` ran to the end", shell.kind);
        assert!(outcome.is_some(), "{:?}: `{long}` did not stop after Ctrl+C", shell.kind);
        assert!(!session.has_exited(), "{:?}: Ctrl+C ended the shell", shell.kind);
        assert_eq!(run(&session, after).plain_output().trim(), expect);
    }

    /// Unix only: through Windows ConPTY the Ctrl+C byte does not reach the running
    /// program, so the GUI's Stop also ends the shell's child processes there.
    #[cfg(unix)]
    #[test]
    fn interrupt_stops_the_command_and_keeps_the_shell() {
        if let Some(shell) = find_shell(ShellKind::Bash) {
            check_interrupt(&shell, "sleep 30", "echo still-here", "still-here");
        }
    }
}
