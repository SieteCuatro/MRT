# In-Depth Core & Polyglot Shells Guide (`mrt-core`)

[`crates/mrt-core`](../crates/mrt-core) is the foundational library of **Modern Rust Terminal (MRT)**. It has zero GUI dependencies so it can power both the GPU desktop application (`mrt-app`) and the headless MCP server (`mrt --mcp-server`).

---

## 1. Module Overview

| Module | File | Responsibility |
| :--- | :--- | :--- |
| **`pty`** | [`src/pty.rs`](../crates/mrt-core/src/pty.rs) | Multi-shell discovery (`ShellProfile`), per-command & streaming shell execution (`execute_command_in_shell`, `execute_command_streaming`), and streaming `portable-pty` sessions (`InteractivePty`). |
| **`vt`** | [`src/vt.rs`](../crates/mrt-core/src/vt.rs) | ANSI / VT100 escape sequence state machine powered by `vte`, producing styled `AnsiSpan` vectors and stripped plain text. |
| **`block`** | [`src/block.rs`](../crates/mrt-core/src/block.rs) | `CommandBlock` data model and automatic `StructuredOutput` detector (JSON, CSV/TSV Tables, Markdown, HTTP results). |
| **`safety`** | [`src/safety.rs`](../crates/mrt-core/src/safety.rs) | Cross-shell destructive command classifier (`RiskLevel::Safe`, `Moderate`, `Critical`) powering the MRT Safety Approval Gate. |
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

Every command executed in MRT produces a `CommandBlock`. When a block completes, `classify_output(&plain_output)` inspects the output in order:
1. **JSON Detection (`StructuredOutput::Json`)**: If the trimmed output starts with `{...}` or `[...]` and parses via `serde_json::from_str`, MRT defaults the block to an interactive collapsible **JSON Tree** view.
2. **CSV / TSV Table Detection (`StructuredOutput::Table`)**: If the output has 2+ non-empty lines with a consistent `,` or `\t` column count (at least 2 columns), MRT parses the header row and data rows into an interactive filterable **Table** view.
3. **Markdown Detection (`StructuredOutput::Markdown`)**: If the output contains Markdown headings (`# `, `## `), fenced code blocks (```` ``` ````), and bullet lists, MRT renders it with formatted typography.
4. **Fallback (`StructuredOutput::RawText`)**: Renders the styled `Vec<AnsiSpan>` in monospace.

Users can toggle any structured block between **Structured View** and **Raw ANSI View** at any time with a single click in the block header.

---

## 5. Cross-Shell Safety Approval Gate (`safety.rs`)

`assess_command(command: &str) -> SafetyAssessment` protects users and AI agents from accidental data loss across all shells:
- **`RiskLevel::Critical` (`requires_approval = true`)**:
  - Unix/Bash: `rm -rf`, `rm -fr`, `mkfs`, `dd if=`, `shutdown`, `reboot`
  - PowerShell: `Remove-Item -Recurse -Force`, `ri -r -fo`
  - Windows CMD/Batch: `del /s /q`, `rd /s /q`, `rmdir /s /q`, `format `
  - Git: `git reset --hard`, `git push --force`, `git push -f`, `git clean -fd`
  - SQL & Cloud: `DROP TABLE`, `DROP DATABASE`, `TRUNCATE TABLE`, `kubectl delete`, `terraform destroy`
- **`RiskLevel::Moderate`**:
  - `sudo `, `rm `, `del `, `Remove-Item `, `kill -9`, `Stop-Process`, `taskkill /f`, `chmod 777`
- **`RiskLevel::Safe`**:
  - Read-only and standard developer commands (`git status`, `cargo test`, `dir`, `ls`, etc.).
