use anyhow::{Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::process::Command as TokioCommand;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellKind {
    PowerShellCore,
    WindowsPowerShell,
    CmdBatch,
    Bash,
    GitBash,
    Wsl,
    Zsh,
    Fish,
    Nushell,
    Sh,
}

impl ShellKind {
    pub fn short_label(&self) -> &'static str {
        match self {
            ShellKind::PowerShellCore => "pwsh",
            ShellKind::WindowsPowerShell => "powershell",
            ShellKind::CmdBatch => "cmd (batch)",
            ShellKind::Bash => "bash",
            ShellKind::GitBash => "git-bash",
            ShellKind::Wsl => "wsl",
            ShellKind::Zsh => "zsh",
            ShellKind::Fish => "fish",
            ShellKind::Nushell => "nu",
            ShellKind::Sh => "sh",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellProfile {
    pub id: String,
    pub name: String,
    pub kind: ShellKind,
    pub program: String,
    pub interactive_args: Vec<String>,
    pub exec_flag: Vec<String>,
}

impl ShellProfile {
    /// Builds the argument list to execute a single command string in this shell.
    pub fn build_exec_args(&self, command: &str) -> Vec<String> {
        let mut args = self.exec_flag.clone();
        args.push(command.to_string());
        args
    }
}

/// Discovers all available shells on the current operating system.
pub fn discover_shells() -> Vec<ShellProfile> {
    let mut profiles = Vec::new();

    let candidates: Vec<ShellProfile> = if cfg!(target_os = "windows") {
        let mut win_candidates = vec![
            ShellProfile {
                id: "pwsh".into(),
                name: "PowerShell 7 (pwsh)".into(),
                kind: ShellKind::PowerShellCore,
                program: "pwsh.exe".into(),
                interactive_args: vec!["-NoLogo".into()],
                exec_flag: vec!["-NoLogo".into(), "-NoProfile".into(), "-Command".into()],
            },
            ShellProfile {
                id: "powershell".into(),
                name: "Windows PowerShell".into(),
                kind: ShellKind::WindowsPowerShell,
                program: "powershell.exe".into(),
                interactive_args: vec!["-NoLogo".into()],
                exec_flag: vec!["-NoLogo".into(), "-NoProfile".into(), "-Command".into()],
            },
            ShellProfile {
                id: "cmd".into(),
                name: "Command Prompt / Batch (cmd.exe)".into(),
                kind: ShellKind::CmdBatch,
                program: "cmd.exe".into(),
                interactive_args: vec![],
                exec_flag: vec!["/C".into()],
            },
        ];

        // Check standard Git Bash installation paths on Windows
        let git_bash_paths = [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ];
        let mut added_git_bash = false;
        for path in git_bash_paths {
            if Path::new(path).exists() {
                win_candidates.push(ShellProfile {
                    id: "git-bash".into(),
                    name: "Git Bash (MINGW64)".into(),
                    kind: ShellKind::GitBash,
                    program: path.into(),
                    interactive_args: vec!["--login".into(), "-i".into()],
                    exec_flag: vec!["-c".into()],
                });
                added_git_bash = true;
                break;
            }
        }
        if !added_git_bash {
            win_candidates.push(ShellProfile {
                id: "bash".into(),
                name: "Bash (bash.exe)".into(),
                kind: ShellKind::Bash,
                program: "bash.exe".into(),
                interactive_args: vec!["-i".into()],
                exec_flag: vec!["-c".into()],
            });
        }

        win_candidates.push(ShellProfile {
            id: "wsl".into(),
            name: "WSL (Linux Subsystem)".into(),
            kind: ShellKind::Wsl,
            program: "wsl.exe".into(),
            interactive_args: vec![],
            exec_flag: vec!["--".into(), "sh".into(), "-c".into()],
        });

        win_candidates.push(ShellProfile {
            id: "nu".into(),
            name: "Nushell (nu)".into(),
            kind: ShellKind::Nushell,
            program: "nu.exe".into(),
            interactive_args: vec![],
            exec_flag: vec!["-c".into()],
        });

        win_candidates
    } else {
        vec![
            ShellProfile {
                id: "zsh".into(),
                name: "Zsh".into(),
                kind: ShellKind::Zsh,
                program: "zsh".into(),
                interactive_args: vec!["-i".into()],
                exec_flag: vec!["-c".into()],
            },
            ShellProfile {
                id: "bash".into(),
                name: "Bash".into(),
                kind: ShellKind::Bash,
                program: "bash".into(),
                interactive_args: vec!["-i".into()],
                exec_flag: vec!["-c".into()],
            },
            ShellProfile {
                id: "fish".into(),
                name: "Fish".into(),
                kind: ShellKind::Fish,
                program: "fish".into(),
                interactive_args: vec!["-i".into()],
                exec_flag: vec!["-c".into()],
            },
            ShellProfile {
                id: "nu".into(),
                name: "Nushell".into(),
                kind: ShellKind::Nushell,
                program: "nu".into(),
                interactive_args: vec![],
                exec_flag: vec!["-c".into()],
            },
            ShellProfile {
                id: "pwsh".into(),
                name: "PowerShell Core (pwsh)".into(),
                kind: ShellKind::PowerShellCore,
                program: "pwsh".into(),
                interactive_args: vec!["-NoLogo".into()],
                exec_flag: vec!["-NoLogo".into(), "-NoProfile".into(), "-Command".into()],
            },
            ShellProfile {
                id: "sh".into(),
                name: "POSIX sh".into(),
                kind: ShellKind::Sh,
                program: "sh".into(),
                interactive_args: vec!["-i".into()],
                exec_flag: vec!["-c".into()],
            },
        ]
    };

    for candidate in candidates {
        if is_program_available(&candidate.program) {
            profiles.push(candidate);
        }
    }

    if profiles.is_empty() {
        // Fallback guarantee
        if cfg!(target_os = "windows") {
            profiles.push(ShellProfile {
                id: "cmd".into(),
                name: "Command Prompt (cmd.exe)".into(),
                kind: ShellKind::CmdBatch,
                program: "cmd.exe".into(),
                interactive_args: vec![],
                exec_flag: vec!["/C".into()],
            });
        } else {
            profiles.push(ShellProfile {
                id: "sh".into(),
                name: "POSIX sh".into(),
                kind: ShellKind::Sh,
                program: "sh".into(),
                interactive_args: vec![],
                exec_flag: vec!["-c".into()],
            });
        }
    }

    profiles
}

fn is_program_available(program: &str) -> bool {
    let path = Path::new(program);
    if path.is_absolute() {
        return path.exists();
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let full = dir.join(program);
            if full.is_file() {
                return true;
            }
            if cfg!(target_os = "windows") && !program.ends_with(".exe") {
                let with_exe = dir.join(format!("{program}.exe"));
                if with_exe.is_file() {
                    return true;
                }
            }
        }
    }
    false
}

#[derive(Debug, Clone)]
pub struct CommandExecutionResult {
    pub stdout_raw: String,
    pub stderr_raw: String,
    pub combined_output: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    pub updated_cwd: Option<PathBuf>,
}

/// Executes a command in the chosen shell profile, handling built-in `cd` navigation
/// and capturing stdout, stderr, exit code, and timing.
pub async fn execute_command_in_shell(
    shell: &ShellProfile,
    command: &str,
    cwd: &Path,
) -> Result<CommandExecutionResult> {
    execute_command_impl(shell, command, cwd, None, None).await
}

/// Streams decoded stdout and stderr as they become available. Programs control
/// their own flushing when writing to pipes. The callback must return promptly.
pub async fn execute_command_streaming(
    shell: &ShellProfile,
    command: &str,
    cwd: &Path,
    mut on_output: impl FnMut(&str) + Send,
) -> Result<CommandExecutionResult> {
    execute_command_impl(shell, command, cwd, Some(&mut on_output), None).await
}

/// Like [`execute_command_streaming`], but reports the OS process id of the spawned shell
/// through `on_spawn` so callers (the MRT Task Manager, the block "Stop" button) can track
/// and stop the whole process tree. `on_spawn` is not called for built-in `cd` handling.
pub async fn execute_command_streaming_with_pid(
    shell: &ShellProfile,
    command: &str,
    cwd: &Path,
    mut on_spawn: impl FnMut(u32) + Send,
    mut on_output: impl FnMut(&str) + Send,
) -> Result<CommandExecutionResult> {
    execute_command_impl(shell, command, cwd, Some(&mut on_output), Some(&mut on_spawn)).await
}

async fn execute_command_impl(
    shell: &ShellProfile,
    command: &str,
    cwd: &Path,
    on_output: Option<&mut (dyn FnMut(&str) + Send)>,
    on_spawn: Option<&mut (dyn FnMut(u32) + Send)>,
) -> Result<CommandExecutionResult> {
    let start = Instant::now();
    let trimmed = command.trim();

    // Handle pure directory change commands (`cd <path>` or `Set-Location <path>`)
    if let Some(target_dir) = parse_cd_command(trimmed, &shell.kind) {
        let resolved = if target_dir == "~" {
            dirs_home().unwrap_or_else(|| cwd.to_path_buf())
        } else {
            let candidate = cwd.join(&target_dir);
            candidate
        };
        match std::fs::canonicalize(&resolved) {
            Ok(canon) if canon.is_dir() => {
                let clean = strip_windows_unc_prefix(canon);
                let msg = format!("Changed directory to {}", clean.display());
                return Ok(CommandExecutionResult {
                    stdout_raw: msg.clone(),
                    stderr_raw: String::new(),
                    combined_output: msg,
                    exit_code: 0,
                    duration_ms: start.elapsed().as_millis() as u64,
                    updated_cwd: Some(clean),
                });
            }
            _ => {
                let err_msg = format!("cd: directory not found: {}", resolved.display());
                return Ok(CommandExecutionResult {
                    stdout_raw: String::new(),
                    stderr_raw: err_msg.clone(),
                    combined_output: err_msg,
                    exit_code: 1,
                    duration_ms: start.elapsed().as_millis() as u64,
                    updated_cwd: None,
                });
            }
        }
    }

    let args = shell.build_exec_args(trimmed);
    let mut cmd_builder = TokioCommand::new(&shell.program);
    cmd_builder
        .args(&args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd_builder.creation_flags(CREATE_NO_WINDOW);
    }

    let (output, streamed_output) = if let Some(callback) = on_output {
        let mut child = cmd_builder
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("Failed to spawn shell '{}'", shell.program))?;
        if let (Some(report), Some(pid)) = (on_spawn, child.id()) {
            report(pid);
        }
        let mut stdout = child.stdout.take().context("Missing stdout pipe")?;
        let mut stderr = child.stderr.take().context("Missing stderr pipe")?;
        let mut stdout_raw = Vec::new();
        let mut stderr_raw = Vec::new();
        let mut stdout_pending = Vec::new();
        let mut stderr_pending = Vec::new();
        let mut combined = String::new();
        let mut out_buffer = [0u8; 8192];
        let mut err_buffer = [0u8; 8192];
        let mut out_open = true;
        let mut err_open = true;
        use tokio::io::AsyncReadExt;
        while out_open || err_open {
            let (bytes, raw, pending) = tokio::select! {
                result = stdout.read(&mut out_buffer), if out_open => {
                    let count = result.context("Failed to read stdout")?;
                    out_open = count != 0;
                    (&out_buffer[..count], &mut stdout_raw, &mut stdout_pending)
                }
                result = stderr.read(&mut err_buffer), if err_open => {
                    let count = result.context("Failed to read stderr")?;
                    err_open = count != 0;
                    (&err_buffer[..count], &mut stderr_raw, &mut stderr_pending)
                }
            };
            raw.extend_from_slice(bytes);
            pending.extend_from_slice(bytes);
            let text = decode_output(pending, bytes.is_empty());
            if !text.is_empty() {
                callback(&text);
                combined.push_str(&text);
            }
        }
        let status = child.wait().await?;
        (
            std::process::Output {
                status,
                stdout: stdout_raw,
                stderr: stderr_raw,
            },
            Some(combined),
        )
    } else {
        (
            cmd_builder.output().await.with_context(|| {
                format!(
                    "Failed to spawn shell '{}' with args {:?}",
                    shell.program, args
                )
            })?,
            None,
        )
    };

    let stdout_raw = String::from_utf8_lossy(&output.stdout).replace("\r\n", "\n");
    let stderr_raw = String::from_utf8_lossy(&output.stderr).replace("\r\n", "\n");

    let mut combined = String::new();
    if !stdout_raw.trim().is_empty() {
        combined.push_str(stdout_raw.trim_end());
    }
    if !stderr_raw.trim().is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(stderr_raw.trim_end());
    }

    let exit_code = output
        .status
        .code()
        .unwrap_or(if output.status.success() { 0 } else { 1 });

    Ok(CommandExecutionResult {
        stdout_raw,
        stderr_raw,
        combined_output: streamed_output.unwrap_or(combined).replace("\r\n", "\n"),
        exit_code,
        duration_ms: start.elapsed().as_millis() as u64,
        updated_cwd: None,
    })
}

// Retain incomplete UTF-8 between reads, separately for each output stream.
fn decode_output(pending: &mut Vec<u8>, eof: bool) -> String {
    let mut text = String::new();
    let mut consumed = 0;
    while consumed < pending.len() {
        match std::str::from_utf8(&pending[consumed..]) {
            Ok(valid) => {
                text.push_str(valid);
                consumed = pending.len();
            }
            Err(error) => {
                let valid_end = consumed + error.valid_up_to();
                text.push_str(std::str::from_utf8(&pending[consumed..valid_end]).unwrap());
                consumed = valid_end;
                match error.error_len() {
                    Some(length) => {
                        text.push('�');
                        consumed += length;
                    }
                    None if eof => {
                        text.push('�');
                        consumed = pending.len();
                    }
                    None => break,
                }
            }
        }
    }
    pending.drain(..consumed);
    text
}

fn parse_cd_command(cmd: &str, kind: &ShellKind) -> Option<String> {
    if *kind == ShellKind::CmdBatch && cmd.eq_ignore_ascii_case("cd") {
        return None;
    }
    if cmd == "cd" {
        return Some("~".to_string());
    }
    for prefix in ["cd /d ", "cd ", "Set-Location ", "sl "] {
        if let Some(rest) = cmd.strip_prefix(prefix) {
            let clean = rest.trim().trim_matches('"').trim_matches('\'');
            if !clean.contains("&&") && !clean.contains(';') && !clean.contains('|') {
                return Some(clean.to_string());
            }
        }
    }
    None
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn strip_windows_unc_prefix(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path
    }
}

/// Interactive cross-platform PTY wrapper using `portable-pty` for live streaming sessions.
pub struct InteractivePty {
    pub id: uuid::Uuid,
    pub shell_id: String,
    pub shell_name: String,
    pub cwd: PathBuf,
    pub cols: u16,
    pub rows: u16,
    master: Arc<Mutex<Box<dyn portable_pty::MasterPty + Send>>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    output_buffer: Arc<Mutex<String>>,
    full_transcript: Arc<Mutex<String>>,
}

impl InteractivePty {
    pub fn spawn(shell: &ShellProfile, cwd: &Path, cols: u16, rows: u16) -> Result<Self> {
        Self::spawn_with_command(shell, cwd, cols, rows, None)
    }

    pub fn spawn_with_command(
        shell: &ShellProfile,
        cwd: &Path,
        cols: u16,
        rows: u16,
        initial_command: Option<&str>,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(&shell.program);
        if let Some(exec_cmd) = initial_command {
            for arg in shell.build_exec_args(exec_cmd) {
                cmd.arg(arg);
            }
        } else {
            for arg in &shell.interactive_args {
                cmd.arg(arg);
            }
        }
        cmd.cwd(cwd);

        let child = pair.slave.spawn_command(cmd)?;
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let output_buffer = Arc::new(Mutex::new(String::new()));
        let full_transcript = Arc::new(Mutex::new(String::new()));
        let buf_clone = Arc::clone(&output_buffer);
        let transcript_clone = Arc::clone(&full_transcript);

        std::thread::spawn(move || {
            let mut temp = [0u8; 4096];
            while let Ok(n) = reader.read(&mut temp) {
                if n == 0 {
                    break;
                }
                let text = String::from_utf8_lossy(&temp[..n]);
                if let Ok(mut guard) = buf_clone.lock() {
                    guard.push_str(&text);
                }
                if let Ok(mut guard) = transcript_clone.lock() {
                    guard.push_str(&text);
                }
            }
        });

        Ok(Self {
            id: uuid::Uuid::new_v4(),
            shell_id: shell.id.clone(),
            shell_name: shell.name.clone(),
            cwd: cwd.to_path_buf(),
            cols,
            rows,
            master: Arc::new(Mutex::new(pair.master)),
            child: Arc::new(Mutex::new(child)),
            writer: Arc::new(Mutex::new(writer)),
            output_buffer,
            full_transcript,
        })
    }

    pub fn send_input(&self, input: &str, append_newline: bool) -> Result<()> {
        let mut guard = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY writer mutex poisoned"))?;
        write!(guard, "{input}")?;
        if append_newline {
            #[cfg(target_os = "windows")]
            write!(guard, "\r\n")?;
            #[cfg(not(target_os = "windows"))]
            write!(guard, "\n")?;
        }
        guard.flush()?;
        Ok(())
    }

    pub fn send_line(&self, input: &str) -> Result<()> {
        self.send_input(input, true)
    }

    pub fn drain_output(&self) -> String {
        if let Ok(mut guard) = self.output_buffer.lock() {
            let out = guard.clone();
            guard.clear();
            out
        } else {
            String::new()
        }
    }

    pub fn read_transcript(&self) -> String {
        if let Ok(guard) = self.full_transcript.lock() {
            guard.clone()
        } else {
            String::new()
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let guard = self
            .master
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY master mutex poisoned"))?;
        guard.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        Ok(())
    }

    pub fn try_wait_exit(&self) -> Result<Option<u32>> {
        let mut guard = self
            .child
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY child mutex poisoned"))?;
        if let Some(status) = guard.try_wait()? {
            Ok(Some(status.exit_code()))
        } else {
            Ok(None)
        }
    }

    /// OS process id of the PTY child, when the platform reports one.
    pub fn process_id(&self) -> Option<u32> {
        self.child.lock().ok().and_then(|guard| guard.process_id())
    }

    pub fn kill(&self) -> Result<()> {
        let mut guard = self
            .child
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY child mutex poisoned"))?;
        guard.kill()?;
        Ok(())
    }
}

#[cfg(test)]
mod directory_tests {
    use super::*;

    #[test]
    fn streaming_decoder_preserves_split_utf8_and_reports_invalid_bytes() {
        let bytes = "á🙂".as_bytes();
        for split in 0..=bytes.len() {
            let mut pending = bytes[..split].to_vec();
            let first = decode_output(&mut pending, false);
            pending.extend_from_slice(&bytes[split..]);
            let last = decode_output(&mut pending, true);
            assert_eq!(format!("{first}{last}"), "á🙂");
            assert!(pending.is_empty());
        }
        let mut pending = vec![b'a', 0xff, 0xf0];
        assert_eq!(decode_output(&mut pending, true), "a\u{fffd}\u{fffd}");
    }

    #[tokio::test]
    async fn streaming_delivers_output_before_exit_and_drains_both_pipes() {
        let shell = discover_shells()
            .into_iter()
            .find(|shell| {
                matches!(
                    shell.kind,
                    ShellKind::PowerShellCore
                        | ShellKind::WindowsPowerShell
                        | ShellKind::Sh
                        | ShellKind::Bash
                )
            })
            .unwrap();
        let command = if matches!(
            shell.kind,
            ShellKind::PowerShellCore | ShellKind::WindowsPowerShell
        ) {
            "[Console]::Out.WriteLine('first'); [Console]::Out.Flush(); Start-Sleep -Seconds 2; [Console]::Error.WriteLine('error'); [Console]::Out.WriteLine('last'); exit 7"
        } else {
            "printf 'first\\n'; sleep 2; printf 'error\\n' >&2; printf 'last\\n'; exit 7"
        };
        let (first_tx, first_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut first_tx = Some(first_tx);
            let mut chunks = String::new();
            let result = execute_command_streaming(
                &shell,
                command,
                &std::env::current_dir().unwrap(),
                |chunk| {
                    chunks.push_str(chunk);
                    if chunks.contains("first") {
                        if let Some(sender) = first_tx.take() {
                            let _ = sender.send(());
                        }
                    }
                },
            )
            .await
            .unwrap();
            (result, chunks)
        });
        tokio::time::timeout(std::time::Duration::from_secs(15), first_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(
            !task.is_finished(),
            "Output was buffered until process exit"
        );
        let (result, chunks) = tokio::time::timeout(std::time::Duration::from_secs(15), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.exit_code, 7);
        assert!(result.stdout_raw.contains("first"));
        assert!(result.stdout_raw.contains("last"));
        assert!(result.stderr_raw.contains("error"));
        assert_eq!(result.combined_output, chunks.replace("\r\n", "\n"));
    }

    #[test]
    fn bare_cd_preserves_cmd_semantics() {
        assert_eq!(parse_cd_command("cd", &ShellKind::CmdBatch), None);
        assert_eq!(parse_cd_command("CD", &ShellKind::CmdBatch), None);
        assert_eq!(parse_cd_command("cd", &ShellKind::Bash), Some("~".into()));
        assert_eq!(
            parse_cd_command("cd /d project", &ShellKind::CmdBatch),
            Some("project".into())
        );
    }
}
