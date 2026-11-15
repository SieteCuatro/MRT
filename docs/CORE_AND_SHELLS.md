# In-Depth Core & Polyglot Shells Guide (`mrt-core`)

[`crates/mrt-core`](../crates/mrt-core) is the foundational library of **Modern Rust Terminal (MRT)**. It has zero GUI dependencies so it can power both the GPU desktop application (`mrt-app`) and the headless MCP server (`mrt --mcp-server`).

---

## 1. Module Overview

| Module | File | Responsibility |
| :--- | :--- | :--- |
| **`pty`** | [`src/pty.rs`](../crates/mrt-core/src/pty.rs) | Multi-shell discovery (`ShellProfile`), per-command & streaming shell execution (`execute_command_in_shell`, `execute_command_streaming`, `execute_command_streaming_with_pid`), and streaming `portable-pty` sessions (`InteractivePty`). |
| **`vt`** | [`src/vt.rs`](../crates/mrt-core/src/vt.rs) | ANSI / VT100 escape sequence state machine powered by `vte`, producing styled `AnsiSpan` vectors and stripped plain text. |
| **`block`** | [`src/block.rs`](../crates/mrt-core/src/block.rs) | `CommandBlock` data model and automatic `StructuredOutput` detector (JSON, CSV/TSV Tables, Markdown, HTTP results). |
| **`safety`** | [`src/safety.rs`](../crates/mrt-core/src/safety.rs) | Cross-shell destructive command classifier (`RiskLevel::Safe`, `Moderate`, `Critical`) powering the MRT Safety Approval Gate. |
| **`shell_integration`** | [`src/shell_integration.rs`](../crates/mrt-core/src/shell_integration.rs) | OSC 133 semantic prompt marker parser and shell integration generator for Bash, Zsh, Fish, PowerShell, and CMD. |
| **`platform`** | [`src/platform.rs`](../crates/mrt-core/src/platform.rs) | OS-specific environment hydration (macOS/Linux login `$PATH` loader and Windows `AttachConsole` CLI helper). |

---

## 2. Polyglot Multi-Shell Discovery (`pty.rs`)

Unlike terminals that hardcode a single default shell, `discover_shells()` dynamically probes the host OS and `PATH` for all installed shells and returns a prioritized `Vec<ShellProfile>`:

| `ShellKind` | Windows Program | Unix/macOS Program | Execution Flags (`exec_flag`) |
| :--- | :--- | :--- | :--- |
| `PowerShellCore` | `pwsh.exe` | `pwsh` | `["-NoLogo", "-NoProfile", "-Command"]` |
| `WindowsPowerShell` | `powershell.exe` | — | `["-NoLogo", "-NoProfile", "-Command"]` |
| `CmdBatch` | `cmd.exe` | — | `["/C"]` |
| `GitBash` | `C:\Program Files\Git\bin\bash.exe` | — | `["-c"]` |
| `Wsl` | `wsl.exe` | — | `["--", "sh", "-c"]` |
| `Bash` | `bash.exe` | `bash` | `["-c"]` |
| `Zsh` | — | `zsh` | `["-c"]` |
| `Fish` | — | `fish` | `["-c"]` |
| `Nushell` | `nu.exe` | `nu` | `["-c"]` |
| `Sh` | — | `sh` | `["-c"]` |

Each profile has a stable `id` (`pwsh`, `powershell`, `cmd`, `git-bash`, `wsl`, `bash`, `zsh`, `fish`, `nu`, `sh`) used by the shell switcher, runbooks, and the MCP `shell` arguments. Only shells found on the host are returned; if none is found, `cmd` (Windows) or `sh` (Unix) is added as a fallback so the list is never empty.

### Directory Tracking (`cd` / `Set-Location`)
When a user runs `cd <path>`, `cd /d <path>`, `Set-Location <path>`, or `cd ~`, `execute_command_in_shell()` resolves and canonicalizes the target directory relative to the tab's current `cwd`, strips Windows UNC `\\?\` prefixes for clean display, and returns `updated_cwd: Some(new_path)` so subsequent commands in that tab (or MCP session) execute in the new directory.

### Windows `CREATE_NO_WINDOW` Guard
On Windows, `execute_command_in_shell()` applies `.creation_flags(0x08000000)` (`CREATE_NO_WINDOW`) to `tokio::process::Command` so spawning `powershell.exe`, `cmd.exe`, or `wsl.exe` from the GUI never flashes an external console window.

---

## 3. ANSI / VT100 Escape Parser (`vt.rs`)

`parse_ansi_text(input: &str) -> (Vec<AnsiSpan>, String)` uses the `vte` state machine (`Perform` trait) to parse terminal output in a single pass:
- **Standard & Bright SGR Colors**: Codes `30..=37`, `40..=47`, `90..=97`, `100..=107` mapped to a tuned OneDark-inspired palette (`AnsiColor { r, g, b }`).
- **24-Bit Truecolor (`CSI 38;2;r;g;b m` / `48;2;r;g;b m`)**: Full RGB foreground and background color extraction.
- **Text Styles**: Bold (`1`/`22`), Italic (`3`/`23`), Underline (`4`/`24`), and Reset (`0`).
- **Simultaneous Plain Text Stripping**: Produces a clean `plain_text` string free of escape codes so `classify_output` can accurately parse JSON or CSV even if a CLI tool emitted ANSI color codes.

---

## 4. Command Blocks & Automatic Structured Output (`block.rs`)

Every command executed in MRT produces a `CommandBlock`. When a block completes, `classify_output(&plain_output)` inspects the output in order (`StructuredOutput::HttpResult` is never produced by the classifier; only `:http` / `:curl` and `mrt_http_request` create it):
1. **JSON Detection (`StructuredOutput::Json`)**: If the trimmed output starts with `{...}` or `[...]` and parses via `serde_json::from_str`, MRT defaults the block to an interactive collapsible **JSON Tree** view.
2. **CSV / TSV Table Detection (`StructuredOutput::Table`)**: If the output has 2+ non-empty lines with a consistent `,` or `\t` column count (at least 2 columns), MRT parses the header row and data rows into an interactive filterable **Table** view.
3. **Markdown Detection (`StructuredOutput::Markdown`)**: If the output contains Markdown headings (`# `, `## `), fenced code blocks (```` ``` ````), and bullet lists, MRT renders it with formatted typography.
4. **Fallback (`StructuredOutput::RawText`)**: Renders the styled `Vec<AnsiSpan>` in monospace.

Users can toggle any structured block between **Structured View** and **Raw ANSI View** at any time with a single click in the block header.

---

## 5. Cross-Shell Safety Approval Gate (`safety.rs`)

Rules are case-insensitive substring matches checked in order: Critical patterns first, then process control, then Moderate. `assess_command(command: &str) -> SafetyAssessment` protects users and AI agents from accidental data loss across all shells:
- **`RiskLevel::Critical` (`requires_approval = true`)**:
  - Unix/Bash: `rm -rf`, `rm -fr`, `mkfs`, `dd if=`, `shutdown`, `reboot`
  - PowerShell: `Remove-Item -Recurse -Force`, `ri -r -fo`
  - Windows CMD/Batch: `del /s /q`, `rd /s /q`, `rmdir /s /q`, `format `
  - Git: `git reset --hard`, `git push --force`, `git push -f`, `git clean -fd`
  - SQL & Cloud: `DROP TABLE`, `DROP DATABASE`, `TRUNCATE TABLE`, `kubectl delete`, `terraform destroy`
  - Process control aimed at OS-critical processes (`PROTECTED_PROCESS_NAMES`, e.g. `taskkill /F /IM lsass.exe`, `pkill launchd`), PID 1 (`kill -9 1`) or every process (`kill -1`)
- **Process control (`requires_approval = true`)**: `kill`, `pkill`, `killall`, `taskkill`, `tskill`, `Stop-Process` / `spps`, and `xkill` in command position of any pipeline segment (after optional `sudo` / `doas`) are always sent to the Safety Gate. Ordinary targets are `Moderate`; by-name kills are flagged as possibly stopping several processes. Mentions of these words elsewhere (for example inside a commit message) are not matched. `is_protected_process_name` / `normalize_process_name` are shared with the Task Manager (`mrt-tools::process_inspector`).
- **`RiskLevel::Moderate`** (shown in the composer's Safety indicator, but runs without a prompt):
  - `sudo `, `rm `, `del `, `Remove-Item `, `chmod 777`, `curl `, `Invoke-WebRequest`
- **`RiskLevel::Safe`**:
  - Read-only and standard developer commands (`git status`, `cargo test`, `dir`, `ls`, etc.).

---

## 6. Shell Integration & Semantic Prompt Markers (`shell_integration.rs`)

MRT integrates native support for the **OSC 133 semantic prompt specification**, allowing the terminal to unambiguously separate user prompts, command input, command execution, and exit codes:

### Supported Markers
- **`OSC 133 ; A ST`**: Prompt started.
- **`OSC 133 ; B ST`**: Prompt ended / command input started.
- **`OSC 133 ; C ST`**: Command execution initiated (output begins).
- **`OSC 133 ; D [; <code>] ST`**: Command finished with optional numeric exit code.

### Parser
`parse_osc133_markers(input: &str) -> Vec<Osc133Event>` finds every marker in a stream (terminated by `BEL` or `ESC \`) and returns its `Osc133Kind` (`PromptStart`, `CommandStart`, `CommandExecuted`, `CommandFinished(Option<i32>)`) with its `byte_offset`.

### Shell Integration Generator
`generate_shell_integration_script(kind: ShellKind) -> &'static str` returns an initialization snippet that configures hooks in:
- **Bash**: `PROMPT_COMMAND` wrapper.
- **Zsh**: `precmd` and `preexec` hook functions.
- **Fish**: `fish_prompt` and `fish_postexec` event handlers.
- **PowerShell** (Core and Windows): Custom `prompt` function with `$LASTEXITCODE` forwarding.
- **Windows CMD**: A `PROMPT` string with `$E]133;…$E\` escapes (no Clink required).
- Other shells (`sh`, Nushell, WSL, Git Bash kinds) get a comment saying OSC 133 is not supported.

### Status
The parser and scripts are a library API with unit tests. The GUI does not inject the scripts or read markers yet: each command block runs in its own shell process (`-Command` / `/C` / `-c`), so the exit code comes from the process itself. Wiring OSC 133 into persistent PTY tabs is tracked in the [Roadmap](../ROADMAP.md#everyday-terminal-use).
