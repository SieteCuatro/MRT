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
    let start = Instant::now();
    let trimmed = command.trim();

    // Handle pure directory change commands (`cd <path>` or `Set-Location <path>`)
    if let Some(target_dir) = parse_cd_command(trimmed) {
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd_builder.creation_flags(CREATE_NO_WINDOW);
    }

    let output = cmd_builder
        .output()
        .await
        .with_context(|| format!("Failed to spawn shell '{}' with args {:?}", shell.program, args))?;

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

    let exit_code = output.status.code().unwrap_or(if output.status.success() { 0 } else { 1 });

    Ok(CommandExecutionResult {
        stdout_raw,
        stderr_raw,
        combined_output: combined,
        exit_code,
        duration_ms: start.elapsed().as_millis() as u64,
        updated_cwd: None,
    })
}

fn parse_cd_command(cmd: &str) -> Option<String> {
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
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    output_buffer: Arc<Mutex<String>>,
}

impl InteractivePty {
    pub fn spawn(shell: &ShellProfile, cwd: &Path, cols: u16, rows: u16) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new(&shell.program);
        for arg in &shell.interactive_args {
            cmd.arg(arg);
        }
        cmd.cwd(cwd);

        let _child = pair.slave.spawn_command(cmd)?;
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let output_buffer = Arc::new(Mutex::new(String::new()));
        let buf_clone = Arc::clone(&output_buffer);

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
            }
        });

        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
            output_buffer,
        })
    }

    pub fn send_line(&self, input: &str) -> Result<()> {
        let mut guard = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY writer mutex poisoned"))?;
        writeln!(guard, "{input}")?;
        guard.flush()?;
        Ok(())
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
}
