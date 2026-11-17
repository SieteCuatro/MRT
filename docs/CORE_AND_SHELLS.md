# In-Depth Core & Polyglot Shells Guide (`mrt-core`)

[`crates/mrt-core`](../crates/mrt-core) is the foundational library of **Modern Rust Terminal (MRT)**. It has zero GUI dependencies so it can power both the GPU desktop application (`mrt-app`) and the headless MCP server (`mrt --mcp-server`).

---

## 1. Module Overview

| Module | File | Responsibility |
| :--- | :--- | :--- |
| **`pty`** | [`src/pty.rs`](../crates/mrt-core/src/pty.rs) | Multi-shell discovery (`ShellProfile`), per-command & streaming shell execution (`execute_command_in_shell`, `execute_command_streaming`, `execute_command_streaming_with_pid`), and streaming `portable-pty` sessions (`InteractivePty`). |
| **`vt`** | [`src/vt.rs`](../crates/mrt-core/src/vt.rs) | ANSI / VT100 escape sequence state machine powered by `vte`, producing styled `AnsiSpan` vectors and stripped plain text, plus OSC 7 / OSC 8 / OSC 52 handling. |
| **`term`** | [`src/term.rs`](../crates/mrt-core/src/term.rs) | Terminal grid emulator (`TerminalEmulator`) wrapping `alacritty_terminal`: turns PTY bytes into a screen of cells with cursor, scrollback, alternate screen, title, input modes, and replies for the PTY. Library only; not wired into the GUI yet. |
| **`links`** | [`src/links.rs`](../crates/mrt-core/src/links.rs) | Detects plain-text URLs and `path:line[:col]` locations in output, classifies OSC 8 targets, and lists the URL schemes MRT will open. |
| **`block`** | [`src/block.rs`](../crates/mrt-core/src/block.rs) | `CommandBlock` data model and automatic `StructuredOutput` detector (JSON, CSV/TSV Tables, Markdown, HTTP results). |
| **`safety`** | [`src/safety.rs`](../crates/mrt-core/src/safety.rs) | Cross-shell destructive command classifier (`RiskLevel::Safe`, `Moderate`, `Critical`) powering the MRT Safety Approval Gate. |
| **`activity`** | [`src/activity.rs`](../crates/mrt-core/src/activity.rs) | Per-tab `ActivityTracker` (idle, running, progress, waiting for input, succeeded, failed): parses `OSC 9;4` progress and MRT's `OSC 678;status=…` sequence, recognises agent CLIs (`claude`, `codex`, `gemini`, `aider`), and applies conservative "waiting for input" heuristics. See the [User Guide](USER_GUIDE.md#tab-activity-status). |
| **`shell_integration`** | [`src/shell_integration.rs`](../crates/mrt-core/src/shell_integration.rs) | OSC 133 / OSC 7 parsers and shell integration scripts for Bash, Git Bash, sh, Zsh, Fish, PowerShell, and CMD. |
| **`shell_session`** | [`src/shell_session.rs`](../crates/mrt-core/src/shell_session.rs) | Persistent shell per tab (`ShellSession`): one long-lived PTY shell with the integration script injected, `run(command)` handles that stream output and resolve with exit code, duration and cwd, and a fallback signal when markers never arrive. |
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

`parse_ansi_rich(input) -> ParsedAnsi { spans, plain_text, events }` does the same pass and also handles OSC sequences (both `BEL` and `ESC \` (ST) terminators). `parse_ansi_text` is a thin wrapper over it, so existing callers are unchanged.
- **OSC 7 (working directory)**: `OSC 7 ; file://host/path` becomes `VtEvent::WorkingDirectory { host, path }`. The same parsing is public as `vt::parse_osc7_uri(&str) -> Option<PathBuf>` for other modules. The path is percent-decoded and `/C:/...` becomes `C:/...`. Anything that is not a `file://host/path` URI, or has bad percent-escapes, is ignored.
- **OSC 8 (hyperlinks)**: `OSC 8 ; params ; URI` sets `AnsiSpan::link` on every span until an `OSC 8 ;;` with an empty URI. `;` inside the URI is kept.
- **Other OSC sequences** (window title, OSC 9, OSC 133, ...) are stripped from the text and not interpreted here.
- **OSC 52 (clipboard write)**: `OSC 52 ; selection ; base64` becomes `VtEvent::ClipboardWrite { selection, text }`. Read requests (`?`), invalid base64 and payloads over 1 MiB (`MAX_CLIPBOARD_WRITE_BYTES`) are dropped. The parser never touches the clipboard; the GUI asks the user first.

### Link detection (`links.rs`)

`detect_links(text) -> Vec<DetectedLink>` scans whitespace-separated words and returns byte ranges with a `LinkTarget`:
- **URLs**: `http://`, `https://`, `ftp://` and `file://`, anywhere in a word (`url=https://...`, `<https://...>`). Trailing punctuation and unbalanced closing brackets are trimmed.
- **File locations**: `path:line` and `path:line:col` as printed by compilers, stack traces and `grep -n` (`src/main.rs:10:5`, `(/app/x.js:42:7)`, `C:\work\lib.rs:3`, `notes.md:12:match`). A path needs a separator or a file extension starting with a letter, so times (`12:30:45`), hosts (`localhost:8080`) and IPs (`127.0.0.1:8080`) are not links.

`classify_uri` turns OSC 8 `file://` targets into `LinkTarget::File` (with `:line:col` or `#L12` suffixes), and `is_openable_uri` allows only `http`, `https`, `ftp`, `mailto` and `file`.

---

## 4. Command Blocks & Automatic Structured Output (`block.rs`)

Every command executed in MRT produces a `CommandBlock`. When a block completes, `classify_output(&plain_output)` inspects the output in order (`StructuredOutput::HttpResult` is never produced by the classifier; only `:http` / `:curl` and `mrt_http_request` create it):
1. **JSON Detection (`StructuredOutput::Json`)**: If the trimmed output starts with `{...}` or `[...]` and parses via `serde_json::from_str`, MRT defaults the block to an interactive collapsible **JSON Tree** view.
2. **CSV / TSV Table Detection (`StructuredOutput::Table`)**: If the output has 2+ non-empty lines with a consistent `,` or `\t` column count (at least 2 columns), MRT parses the header row and data rows into an interactive filterable **Table** view.
3. **Markdown Detection (`StructuredOutput::Markdown`)**: If the output contains Markdown headings (`# `, `## `), fenced code blocks (```` ``` ````), and bullet lists, MRT renders it with formatted typography.
4. **Fallback (`StructuredOutput::RawText`)**: Renders the styled `Vec<AnsiSpan>` in monospace.

While a command streams, the GUI calls `CommandBlock::append_output(chunk, &mut VtStream)`: a `vt::VtStream` keeps the `vte` parser state between chunks (an escape sequence split across chunks is still recognised) and returns only the spans, plain text and OSC events the chunk added, so streaming costs O(n) in total instead of re-parsing everything after every chunk. It updates `ansi_spans`, `plain_output`, `reported_cwd` (last OSC 7 path) and `clipboard_requests` (OSC 52 writes, each `Pending`, `Approved` or `Denied`). `refresh_output()` still re-parses the whole `raw_output` when needed, keeping decisions already made.

**Inline images (`images.rs`).** `VtParserState` reports images as `ParsedAnsi::images` (`InlineImage`: RGBA pixels in an `Arc`, the byte offset in the plain text where it appears, and any requested width/height). Sources: OSC 1337 `File=` (`parse_iterm_file`, decoded with the `image` crate: PNG, JPEG, GIF), kitty graphics APC strings (`ApcScanner` runs next to `vte`, which skips APC content; `KittyGraphics` joins `m=1` chunks, inflates `o=z`, and keeps up to 16 `a=t` images for `a=p`; only `t=d`), and Sixel DCS (`hook`/`put`/`unhook`, `decode_sixel`). Every decoder is limited to 8192 px per side, 64 MiB of payload and 256 MiB decoded; failures become a visible `[image not shown: …]` line. `CommandBlock::images` is not serialized. `ShellSession`'s `Normalizer` passes OSC 8, OSC 52, OSC 1337, kitty APC and Sixel DCS through unchanged, so links, clipboard requests and images work in persistent shells too (before this, they were dropped there).

**Kitty keyboard protocol and scrollback search (`term.rs`).** The emulator runs with `kitty_keyboard: true`, so programs can push and pop kitty flags (`CSI > flags u`, `CSI < u`) and query them (`CSI ? u`); `TermModes::kitty_keyboard` reports the active flags, and `mrt-app/src/term_input.rs` encodes keys as `CSI code[:alternate];mods[:event];text u` accordingly (disambiguate, event types, alternate keys, all keys as escape codes, associated text). `search` finds text in the history and screen; `set_view_offset` / `reveal_line` scroll the view `snapshot` and the image placements use, and the view stays on the same lines while output arrives.

**Hosted PTYs (`pty_host.rs`).** `InteractivePty` has a `Local` and a `Hosted` backend. With `set_pty_host(Some(endpoint))` (the window does this once it reaches the session host), `spawn_with_owner` asks the host to start the PTY (`pty_spawn`) and attaches over a `pty_stream` connection; a reader thread fills the same output buffer and transcript a local PTY uses, and `send_bytes` / `resize` / `kill` become stream events. `spawn_local` always runs in this process (the host uses it). `serve_stream` is the host side; `transcript_since` streams from an offset. `list` and `attach` let a new window reattach.

**Data folder.** `platform::data_dir()` is `$MRT_DATA_DIR` or the platform's local data folder; history, layout, the bridge file, the session host file and the pairing store all live there.

**Images in the grid (`grid_images.rs`).** `TerminalEmulator::feed` runs PTY output through `GraphicsSplitter`, which takes iTerm2 `OSC 1337 ; File=`, kitty `ESC _ G` and Sixel `ESC P … q` sequences out (across chunk boundaries; other OSC / DCS / APC strings pass through). Each is decoded by a `VtStream` kept for that purpose (kitty chunks and `a=t` images persist), placed at the cursor's line counted from the top of the scrollback (`GridImage::line`) with `image_cells` for its size, and followed by `\r\n` per row so the cursor moves below it. `images_on_screen()` gives the visible placements; `clear_scrollback` shifts or drops them, and switching screens clears them. At most 64 images are kept.

**Service control in the Safety Gate.** `assess_service_control` (in `safety.rs`) matches `sc`, `net`, `systemctl`, `service`, `launchctl` with a changing verb in command position, and the `*-Service` cmdlets: always approval, Critical for `PROTECTED_SERVICES` and for deleting or masking.

**Theme files (`theme_file.rs`).** `parse_theme` reads MRT's TOML (`[ui]`, `[terminal]`), Windows Terminal JSON schemes, Alacritty TOML, Ghostty and iTerm2 `.itermcolors` into a `ThemeSpec`. `set_ansi_palette` replaces the 16 ANSI colors process-wide; `vt.rs` (`standard_color`) and `term.rs` (`xterm_palette` 0–15) read them through `themed_ansi`.

**Output limit.** `block::set_max_block_output_bytes` (from `terminal.max_block_output_mb`, default `DEFAULT_MAX_BLOCK_OUTPUT_BYTES` = 32 MiB, `0` = none) bounds what a block keeps. `append_output` trims to three quarters of the limit once it is exceeded, cutting at a line start, and drops the same bytes from the spans; `new_completed` keeps the tail of the final output; `ShellSession` trims the output it collects for `CommandOutcome` the same way. `CommandBlock::truncated_bytes` records how much was dropped. `new_completed` skips `classify_output` above `STRUCTURED_CLASSIFY_LIMIT` (4 MiB).

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

## 6. Shell Integration & Semantic Prompt Markers (`shell_integration.rs`, `shell_session.rs`)

MRT supports the **OSC 133 semantic prompt specification**, which lets the terminal tell prompts, command input, command output, and exit codes apart, plus **OSC 7** for the working directory:

### Supported Markers
- **`OSC 133 ; A ST`**: Prompt started.
- **`OSC 133 ; B ST`**: Prompt ended / command input started.
- **`OSC 133 ; C ST`**: Command execution initiated (output begins).
- **`OSC 133 ; D [; <code>] ST`**: Command finished with optional numeric exit code.
- **`OSC 7 ; file://<host>/<path> ST`**: Current working directory.

`ST` is `ESC \` or `BEL`; both are accepted.

### Parsers
- `parse_osc133_markers(input: &str) -> Vec<Osc133Event>` finds every marker in a string and returns its `Osc133Kind` (`PromptStart`, `CommandStart`, `CommandExecuted`, `CommandFinished(Option<i32>)`) with its `byte_offset`.
- `parse_osc7_path(payload) -> Option<PathBuf>` turns an OSC 7 payload into a path (percent escapes decoded, `/C:/x` becomes `C:\x`).

### Shell Integration Scripts
`generate_shell_integration_script(kind: ShellKind) -> &'static str` returns an idempotent init snippet. Each one reports `A`, `B`, `D;<exit code>` and OSC 7. Most shells have no reliable pre-execution hook, so `C` comes from a helper that MRT puts in front of each command it types (`__mrt_c`, or `__mrt_sb` in PowerShell):

| Shell | Hooks | Exit code |
| :--- | :--- | :--- |
| **Bash / Git Bash** | `PROMPT_COMMAND` (chains the user's own) and `PS1` markers | `$?` in the prompt hook |
| **Zsh** | `precmd_functions` and `PS1` markers; `PROMPT_SP` is turned off, since zsh prints its `%` partial-line mark before the hooks and it would end up in the command's output | `$?` in `precmd` |
| **Fish** | `fish_postexec` event and a wrapped `fish_prompt` | `$status` |
| **POSIX `sh`** | `PS1` markers only; the command wrapper prints `D` (`__mrt_c; . <file>; __mrt_d $?`) | `$?` |
| **PowerShell** (5.1 and 7+) | Custom `prompt` (keeps the user's prompt text) | `$?`, then `$LASTEXITCODE` for native commands |
| **CMD** | `PROMPT $E]133;A$E\…` for `A`/`B`/OSC 7; helper batch files print `C` and `D;%ERRORLEVEL%` | `%ERRORLEVEL%` |

WSL and Nushell get a comment saying OSC 133 is not supported.

### Remote Sessions over SSH (`ShellSession::spawn_remote`)
`spawn_remote(program, args, cwd, options)` starts a program that ends in an interactive Bash or Zsh (the GUI uses `ssh -tt -o BatchMode=yes -o ConnectTimeout=20 <args>`) and **types** the integration in instead of passing an rcfile: `shell_integration::remote_bootstrap_lines()` assembles the base64 of `remote_integration_script()` (the Bash and Zsh scripts behind a `$BASH_VERSION` / `$ZSH_VERSION` check, plus a `__mrt_decode` helper for GNU `base64 -d` or BSD `base64 -D`) in a variable over lines of at most 900 characters (a terminal in canonical mode keeps only ~4 KB per line before `readline` starts), then `eval`s it. Every typed line starts with a space to stay out of history. Commands are typed as ` __mrt_c; eval "$(printf %s '<base64>' | __mrt_decode)"` (`remote_command_line`), so quotes and multi-line scripts arrive unchanged; commands over 3,000 base64 characters are refused. Nothing is written on the remote machine. Shells that run neither script never print a prompt marker, so `spawn_remote` returns `IntegrationTimeout`. Tests stand a plain `bash --norc --noprofile -i` (Git Bash on Windows) and `zsh -f -i` in for the remote shell.

### Persistent Shell Sessions (`shell_session.rs`)
`ShellSession` keeps **one long-lived shell in a PTY** (`portable-pty`, ConPTY on Windows), so `cd`, exported/`set` variables, aliases, functions, and venv/conda activation persist between commands. It is a library API; the GUI still runs one process per command.

```rust
use mrt_core::{ShellSession, ShellSessionOptions};

let session = ShellSession::spawn(&shell, &cwd, ShellSessionOptions::default())?;
let mut handle = session.run("cd src")?;
let outcome = handle.wait()?;          // exit_code, duration, output, cwd
let mut handle = session.run("cargo build")?;
while let Some(event) = handle.try_next() { /* CommandEvent::Output(chunk) / Finished(outcome) */ }
```

- **Startup.** The integration script is injected per shell: `bash --rcfile <init> -i` (sources `~/.bashrc` first), `ENV=<init> sh -i`, a temporary `ZDOTDIR` whose `.zshenv`/`.zshrc` load the user's own files first, `fish -C 'source <init>'`, `powershell -NoExit -Command ". ([scriptblock]::Create(...))"` (a script block, so the execution policy does not block it; PSReadLine is unloaded because MRT edits the command line itself), and `cmd /K <init>.cmd`. `ShellSessionOptions::load_user_config = false` skips user config (`-NoProfile`, `/D`, `--no-config`). The files live in a per-session temp directory that is removed when the session is dropped.
- **Running a command.** `run(command)` returns a `CommandHandle` at once. Commands queue and are typed one at a time, each after the previous prompt's `B` marker. For POSIX shells, fish and PowerShell the command is written to a file in the session directory and sourced in the current scope (`__mrt_c; . <file>` or `. (__mrt_sb <file>)`), so quotes, tabs and multi-line scripts reach the shell unchanged. CMD cannot source a file without batch-file semantics, so its command is typed on one line (multiple lines are joined with ` & `) between `call begin.cmd` and `call end.cmd`.
- **Output.** Only text between `C` and `D` belongs to the command. The PTY stream goes through a `vte` normalizer that keeps SGR colors, turns ConPTY's cursor positioning back into newlines and spaces, answers cursor-position queries (`CSI 6 n`), and drops the echoed input and prompt. On Windows the session keeps collecting output until the next prompt, because ConPTY passes OSC markers through ahead of text it has not rendered yet. `CommandOutcome::plain_output()` strips the remaining escapes.
- **Result.** `CommandOutcome` has `exit_code` (`None` if the shell printed a new prompt without `D`, e.g. `sh` abandoning a line), `duration` (from `C` to `D`), `output`, `cwd` (from OSC 7; Git Bash `/c/x` paths become `C:\x`), and `shell_exited`. `ShellSession::cwd()` always has the last reported directory.
- **Other controls.** `send_input` (stdin for the running program), `interrupt` (Ctrl+C), `resize`, `kill`, `is_idle`, `process_id`.
- **Fallback.** If no prompt marker arrives within `ready_timeout` (default 20 s), or the shell exits while starting, `spawn` returns `ShellSessionError::IntegrationTimeout` / `StartupExited` (with the startup transcript), and WSL/Nushell return `Unsupported`. For all three `should_fall_back()` is `true`: keep using one process per command (`execute_command_streaming_with_pid`).
- **Limits.** A user config that replaces `PROMPT_COMMAND`, `precmd_functions` or `prompt` after the script runs hides the markers (the session then times out and falls back). In CMD, `ERRORLEVEL` is reset to 0 before each command so commands that never set it (`echo`, `cd`) report 0, as `cmd /C` would. Typing `exit` ends the session; its command finishes with `shell_exited`.

### Status
The parser, scripts, and `ShellSession` have tests that start real shells (PowerShell, CMD and Git Bash on Windows; bash, sh, zsh, fish and pwsh on Unix when installed). The GUI runs tab commands through them with `mrt-app/src/shell_pool.rs`: one session per tab and shell, used when it is idle; a busy or starting session, WSL/Nushell, a failed integration, or `terminal.persistent_shell = false` run the command in its own shell process (`-Command` / `/C` / `-c`) as before. Through Windows ConPTY the Ctrl+C byte from `interrupt()` does not reach the running program, so the GUI's **Stop** also ends the shell's child processes a second later; on Unix Ctrl+C is enough.

---

## 7. Terminal Grid Emulator (`term.rs`)

`vt.rs` only colors a text transcript. Full-screen programs (`vim`, `htop`, `less`, `lazygit`) need a real screen: they move the cursor, erase regions, set scroll regions, and switch to the alternate screen. `TerminalEmulator` keeps that screen. It wraps [`alacritty_terminal`](https://crates.io/crates/alacritty_terminal) behind an MRT-owned API, so no `alacritty_terminal` type appears in a public signature.

The emulator is a pure state machine: it never touches a PTY. The caller feeds it PTY output and writes its replies back.

```rust
use mrt_core::TerminalEmulator;

let mut term = TerminalEmulator::new(120, 35, 10_000); // cols, rows, scrollback lines
term.feed(&bytes_from_pty);
pty.write_all(&term.take_replies())?;                 // DA, DSR/CPR, color and size replies
let screen = term.snapshot();                          // cells, cursor, modes, title
```

### API

| Method | Returns |
| :--- | :--- |
| `new(cols, rows, scrollback)` | Emulator with a `cols` x `rows` screen keeping up to `scrollback` history lines. Sizes below 2x1 are clamped. |
| `feed(&[u8])` | Parses PTY output. Escape sequences and UTF-8 characters may be split across calls. |
| `resize(cols, rows)` | Resizes the grid (lines reflow; shrinking rows pushes the top into scrollback). |
| `take_replies() -> Vec<u8>` | Drains the bytes the terminal must write back to the PTY: primary/secondary device attributes (`CSI c`, `CSI > c`), status and cursor position reports (`CSI 5n`, `CSI 6n`), text area size (`CSI 14t`, `CSI 18t`), and color queries (`OSC 4/10/11 ; ?`). |
| `snapshot() -> TermSnapshot` | Visible cells (`rows` x `cols` `TermCell`s), cursor, modes and title. |
| `row_cells(row)` | One visible row as `TermCell`s. |
| `screen_text()` | Visible screen as plain text, one line per row, trailing spaces trimmed, wide-character spacers skipped. |
| `text_with_colors()` | History and main screen as text with SGR color sequences (no cursor movement), leading/trailing empty lines dropped. Used for a finished TUI session's command block. |
| `scrollback_with_colors()`, `clear_scrollback()` | History as colored lines, and dropping it (`CSI 3 J`). The GUI uses them before resizing on Windows. |
| `scrollback_len()`, `scrollback_lines()`, `scrollback_row_cells(i)` | History that scrolled off the primary screen, oldest first. The alternate screen has no history. |
| `cursor() -> TermCursor` | 0-based `row`/`col`, `visible` (`CSI ?25 h/l`), and `shape` (`Block`, `Underline`, `Beam` from `CSI n SP q`). |
| `modes() -> TermModes` | What the input side needs: `app_cursor` (DECCKM), `app_keypad`, `bracketed_paste` (2004), `mouse` (`MouseMode::None/Click/Drag/Motion` for 1000/1002/1003), `sgr_mouse` (1006), `utf8_mouse` (1005), `focus_reporting` (1004), `alternate_scroll` (1007), `alt_screen`. |
| `is_alt_screen()` | Whether the alternate screen (1049/1047/47) is active. |
| `title()` | Window title set with `OSC 0` / `OSC 2`. |
| `take_bell()` | Whether `BEL` rang since the last call. |
| `sync_deadline()`, `flush_sync()` | Synchronized updates (mode 2026) hold output back until the program ends them; if it never does, call `flush_sync()` once the deadline passes (`feed` also flushes an expired one). |
| `set_cell_size_px(w, h)` | Cell size reported by `CSI 14t` (default 8x16). |

Each `TermCell` has `ch`, `zero_width` (combining marks), `fg`/`bg`, and the flags `bold`, `dim`, `italic`, `underline`, `strikeout`, `inverse`, `hidden`, `wide` (a double-width CJK or emoji character) and `wide_spacer` (the right half of a wide character, which renderers skip).

Colors stay unresolved as `TermColor::DefaultFg`, `DefaultBg`, `Indexed(0..=255)` or `Rgb(AnsiColor)`, so the renderer applies its theme. `TermColor::to_rgb(fg, bg)` and `xterm_palette(index)` resolve them with the standard xterm 256-color palette.

### Security
`OSC 52` clipboard access is disabled: programs inside the terminal can neither read nor set the clipboard.

### Dependency note
`alacritty_terminal` 0.26 depends on `vte` 0.15, while `vt.rs` uses the workspace's `vte` 0.13. Cargo builds both versions side by side; they never meet in a public type. `vt.rs` can move to 0.15 in a later change.

### Status
Library API with unit tests (cursor movement, erase, alternate screen, scroll regions, 16/256/truecolor, wide and combining characters, resize, replies, modes, and vim-like and htop-like byte streams). The GUI's TUI overlay draws from it (`mrt-app/src/widgets/term_view.rs`) and uses `modes()` to encode keys and mouse reports (`mrt-app/src/term_input.rs`). Programs in MRT's PTYs (`InteractivePty` and `ShellSession`) get `COLORTERM=truecolor`, `TERM_PROGRAM=MRT` and `TERM_PROGRAM_VERSION`, plus `TERM=xterm-256color` outside Windows (`pty::apply_terminal_env`).
