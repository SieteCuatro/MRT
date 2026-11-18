# AGENTS.md — AI Agent Guide for Modern Rust Terminal (MRT)

Welcome, AI Agent! This repository (**Modern Rust Terminal / MRT**) is an open, 100% free, AI-built cross-platform terminal written in Rust. Your user is generously contributing their AI tokens to improve this project.

Follow these guidelines to navigate the workspace efficiently, write idiomatic cross-platform Rust, and verify your changes.

---

## 1. Workspace Structure

The Cargo workspace contains 5 modular crates:

- **`crates/mrt-core`**:
  - `pty.rs`: Discovers installed host shells (`pwsh`, `powershell`, `cmd`/Batch, `git-bash`, `wsl`, `bash`, `zsh`, `fish`, `nu`, `sh`) and executes commands or spawns `portable-pty` sessions. Uses `CREATE_NO_WINDOW` (`0x08000000`) on Windows so child processes never flash external console windows.
  - `vt.rs`: Parses ANSI/VT100 escape sequences via `vte` into colored `AnsiSpan` segments and stripped plain text; `parse_ansi_rich` also returns OSC 7 (working directory), OSC 8 (hyperlinks on spans), and OSC 52 (clipboard requests, never applied automatically) as `VtEvent`s. `VtStream` parses streamed output chunk by chunk (used by `CommandBlock::append_output`, which also enforces the per-block output limit).
  - `images.rs`: Inline images for blocks: iTerm2 `OSC 1337 File=`, kitty graphics (APC, direct transmission only) and Sixel, decoded to RGBA with size limits (`InlineImage` in `ParsedAnsi::images` / `CommandBlock::images`).
  - `theme_file.rs`: Theme files (MRT TOML, Windows Terminal, Alacritty, Ghostty, iTerm2) and the process-wide 16-color ANSI palette.
  - `grid_images.rs`: Image sequences taken out of the PTY stream (`GraphicsSplitter`) and placed in the `TerminalEmulator` grid.
  - `links.rs`: Detects plain-text URLs and `path:line[:col]` locations in output and classifies OSC 8 targets (which URL schemes MRT will open).
  - `term.rs`: Terminal grid emulator (`TerminalEmulator`) wrapping `alacritty_terminal` behind an MRT-owned API: `feed` PTY bytes, `resize`, `snapshot` of cells (colors, attributes, wide chars), cursor, scrollback, title, alternate screen, input modes (app cursor, bracketed paste, mouse), and `take_replies` for DA/CPR answers to the PTY. Used by the GUI's TUI overlay.
  - `block.rs`: Defines `CommandBlock` and `classify_output` (auto-detecting JSON trees, CSV/TSV tables, Markdown, and HTTP responses).
  - `safety.rs`: Evaluates command risk (`Safe`, `Moderate`, `Critical`) across Bash, PowerShell, CMD/Batch, SQL, Git, Kubernetes, and process control (`kill`, `taskkill`, `Stop-Process`, protected OS process names) for the MRT Safety Approval Gate.
  - `activity.rs`: Per-tab activity state (`ActivityTracker`: idle, running, progress, waiting for input, succeeded, failed) from block lifecycle, `OSC 9;4` progress, MRT's `OSC 678;status=…` sequence, and conservative agent-CLI (`claude`, `codex`, `gemini`, `aider`) prompt heuristics.
  - `shell_integration.rs`: OSC 133 / OSC 7 parsers and shell integration scripts for Bash, Git Bash, sh, Zsh, Fish, PowerShell, and CMD.
  - `shell_session.rs`: `ShellSession`, one long-lived PTY shell per tab with the integration script injected, so `cd`, variables, aliases and venvs persist. `run(command)` returns a `CommandHandle` that streams output and resolves with exit code, duration and cwd from OSC 133/OSC 7 markers; `ShellSessionError::should_fall_back()` tells callers to use one process per command instead. The GUI runs tab commands through it (`mrt-app/src/shell_pool.rs`).
  - `worktree.rs`: Git worktrees for worktree tabs (`:worktree <branch>`, `mrt_create_tab` with `worktree`): `create` (in `<repo>.worktrees/<branch>`, recording the base branch), `list`, `changed_files`, `remove`, `review`, `discard`, and `merge_command` / `pr_command` per shell, all through the `git` CLI with `CREATE_NO_WINDOW`.
  - `pty_host.rs`: PTYs owned by the session host: the hosted `InteractivePty` backend and the `pty_stream` event protocol (`set_pty_host`, `attach`, `serve_stream`).
  - `config.rs`: Configuration file schema (`config.toml` + per-project `.mrt.toml`), layered merge, and debounced `ConfigWatcher`; `[keybindings]` (`HashMap<String, String>`) and `[notifications]` feed the keymap and the command notifier. Applied to the GUI by `mrt-app/src/config_runtime.rs`.
  - `platform.rs`: Cross-platform startup helpers (`hydrate_login_environment` for macOS/Linux GUI `$PATH` inheritance and `attach_parent_console_if_cli` for Windows CLI flags).
- **`crates/mrt-tools`**:
  - `http_inspector.rs`: Built-in `:http` / `:curl` REST API client (`reqwest`).
  - `web_fetch.rs`: Built-in `:web` HTML-to-readable-Markdown extractor.
  - `net_inspector.rs`: Built-in `:ports` local TCP/UDP listening port scanner.
  - `dns_inspector.rs`: Built-in `:dns` DNS resolution and TCP handshake latency tracer.
  - `ws_inspector.rs`: Built-in `:ws` WebSocket inspector with live frame logging and test probe sender.
  - `bug_reporter.rs`: Built-in `:report` / `:bug` Markdown bug report generator with automatic secret token redaction.
  - `process_control.rs`: `:suspend`, `:resume`, `:priority` (NtSuspendProcess / SetPriorityClass on Windows, SIGSTOP / setpriority on Unix), with Critical targets refused.
  - `service_inspector.rs`: `:services` listing (sc.exe / systemctl / launchctl) and `:service` actions as platform commands for the Safety Gate.
  - `process_inspector.rs`: Built-in Task Manager backend (`sysinfo`): `:ps` / `:top` process tables, `:proc` details, `:kill` planning with Safety Gate classification and tree termination, `:watch-proc` watches, MRT-owned process labels, and process snapshot diffs.
  - `structured_diff.rs`: Canonical JSON diffs (key order invariant), listening port snapshot delta detection, and process snapshot deltas.
- **`crates/mrt-mcp`**:
  - `protocol.rs`: JSON-RPC 2.0 and Model Context Protocol (MCP) types.
  - `bridge.rs`: Token-authenticated localhost TCP bridge (`mcp-bridge.json`) so external `mrt --mcp-server` processes transparently control a running MRT GUI window.
  - `remote.rs` & `companion.rs`: Remote connections (`mrt share`: SPAKE2 pairing, Noise-encrypted relay to the bridge, paired keys only) and the HTTPS phone companion (`mrt share --web`).
  - `server.rs`: MRT as an MCP Server (`mrt --mcp-server` or live GUI bridge), exposing 31 tools across session & multi-tab control (`mrt_get_session_state`, `mrt_list_tabs`, `mrt_create_tab`, `mrt_switch_tab`, `mrt_close_tab`, `mrt_configure_tab`), command execution & block inspection/diffing/triage (`mrt_run_command`, `mrt_list_blocks`, `mrt_get_block`, `mrt_search_blocks`, `mrt_compare_blocks`, `mrt_diagnose_block`, `mrt_clear_blocks`), interactive PTYs (`mrt_pty_spawn`, `mrt_pty_write`, `mrt_pty_read`, `mrt_pty_resize`, `mrt_pty_list`, `mrt_pty_kill`), live GUI control (`mrt_control_gui`), AI translation (`mrt_ai_translate`), and internet & diagnostic tools (`mrt_http_request`, `mrt_web_fetch`, `mrt_inspect_ports`, `mrt_inspect_dns`, `mrt_generate_bug_report`, `mrt_inspect_ws`), and the Task Manager (`mrt_list_processes`, `mrt_inspect_process`, `mrt_kill_process`, `mrt_compare_process_snapshots`), plus 5 MCP resources.
  - `client.rs`: MRT as an MCP Host, connecting to in-process loopback and external `stdio` or Streamable HTTP MCP servers through one shared session layer (with one-click presets for Filesystem, Git, Memory, SQLite, Fetch).
  - `transport.rs`: `stdio` and Streamable HTTP transports (JSON/SSE responses, `Mcp-Session-Id`, `MCP-Protocol-Version`, GET listen stream, `Last-Event-ID` resume, redacted custom headers).
- **`crates/mrt-ai`**:
  - `usage.rs`: Token use and estimated cost of AI requests (`AiUsage`, `usage_from_response` for every provider, prices from `[ai.prices]` via `set_ai_prices`), shown on suggestions, `:assist` blocks, the sidebar and `:ai-usage`.
  - `assist.rs`: Multi-step `:assist` investigations over MRT's read-only MCP tools (model-driven JSON tool calls, or a built-in offline plan); suggestions are never executed directly.
  - `lib.rs`: Multi-provider AI client supporting **Google Gemini**, **Anthropic Claude**, **OpenAI**, **xAI Grok**, **DeepSeek**, **OpenRouter**, **Local Ollama**, and a deterministic **Offline Polyglot Heuristic Engine**, with streaming token delivery (`diagnose_command_error_streaming`).
- **`crates/mrt-app`**:
  - `main.rs`: Entrypoint (`#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]`), CLI dispatch (`--mcp-server`, `--version`, `--help`, `mrt agent setup|show|remove` in `agent_setup.rs`, `list|send|capture|split|quick|quit` in `cli.rs`, `share|pair|attach` in `remote_cli.rs`, `server` in `server.rs`, `session-host` in `session_host.rs`), the single-instance handoff, and the `eframe` launcher.
  - `settings.rs`: The settings window: a searchable form over `config.toml` (`SETTINGS`, `current_value`), written key by key with `toml_edit` (`apply_edits` keeps comments) and reloaded, plus the keybinding list (`Keymap::entries`) with conflicts and add / remove.
  - `windows.rs`: Several windows: extra egui viewports (`ExtraWindow`), each drawing its own tabs through `MrtGuiApp::window_frame`; `TerminalTab::window` says where a tab is, and tab moves and closed windows are applied after every window is drawn.
  - `copy_mode.rs` & `hooks.rs`: Keyboard copy mode over a tab's blocks, and `[[hooks]]` event commands run in the background (`CREATE_NO_WINDOW`, `MRT_*` environment). Named `[[actions]]` (`run:<name>`) and pane broadcast (`:broadcast`) live in `app.rs`.
  - `session_host.rs`: `mrt session-host`, the separate process that owns the tabs' shells and running commands (`terminal.session_server`); the window runs commands through it and reattaches after closing or crashing.
  - `remote_cli.rs`: `mrt share`, `mrt pair`, `mrt attach` and `--host`.
  - `server.rs`: `mrt server`, a headless session server on the bridge (design for GUI attach, upgrades and remote sessions: `docs/SESSION_SERVER.md`).
  - `cli.rs` & `layout.rs`: `mrt list|send|capture|split` over the MCP bridge (thin wrappers around MCP tools, documented exit codes), and saving / restoring the window layout (`layout.json`, `terminal.restore_layout`).
  - `config_runtime.rs`: Loads the config file at startup, polls it for changes, and applies it to the GUI (font size, default shell, AI preferences, startup MCP servers, keymap, notification settings) with an in-window notice for errors.
  - `fonts.rs`: Font loading from `terminal.font_family` / `terminal.font_fallback` (family names resolved in the system font folders, or font file paths) plus platform symbol fallbacks.
  - `notifications.rs`: `CommandNotifier`: desktop notifications (`notify-rust`) for long commands that finish in the background, and unseen-results badges on tabs.
  - `find.rs`: Find in output (Ctrl+F): match ranges, per-block counts and the highlight the block renderer draws.
  - `shell_pool.rs`: One `ShellSession` per tab and shell. A command runs in its tab's session when the session is idle, otherwise (busy, starting, WSL/Nushell, integration failed, `terminal.persistent_shell = false`) in its own process; Stop sends Ctrl+C and then ends the shell's child processes, so the shell survives.
  - `term_input.rs` & `widgets/term_view.rs`: xterm and kitty-protocol key encoding, mouse encoding for the TUI overlay (application cursor mode, function keys, Ctrl+letter, Alt+letter, bracketed paste, SGR/X10 mouse reports and wheel) and the renderer that paints a `TermSnapshot` cell by cell.
  - `widgets/activity.rs` & `widgets/links.rs`: Tab activity indicator (dot, spinner, progress ring) for sidebar rows and Mosaic tiles, and clickable links (`Ctrl`/`Cmd`+click) plus the OSC 52 clipboard prompt in command blocks.
  - `app.rs`, `history.rs`, `keymap.rs`, `theme.rs` & `widgets/`: Configurable keybindings (`keymap.rs`: actions, `ctrl+a > c` chains, key tables, conflict reporting), Multi-tab GUI, Split Panes within tabs (`:split-h`, `:split-v`, `:unsplit`), full-screen Interactive TUI Mode overlay (`:tui <cmd>`, `vim`, `nano`, `htop`, `lazygit`), Mosaic split grid, Folder Explorer, SQLite + FTS5 trigram command history, dark theme & system font loader, per-tab shell switcher, interactive block renderer (with structured diffs), MCP Hub side panel, Toolbox side panel (`widgets/toolbox.rs` with Network and Task Manager tabs, `widgets/task_manager.rs`), Safety Approval Gate modal, and Command Palette (`Ctrl+K` / `Ctrl+P`).

---

## 2. Core Engineering Rules for AI Agents

1. **Cross-Platform First (Windows, macOS, Linux)**:
   - Never assume a single shell or OS. Always support PowerShell, CMD/Batch, and POSIX shells (`bash`/`zsh`/`sh`).
   - Whenever spawning background child commands via `tokio::process::Command` or `std::process::Command`, include the Windows `CREATE_NO_WINDOW` (`0x08000000`) guard so Windows users never see flashing console windows.
2. **Multi-AI Provider Neutrality**:
   - Keep `mrt-ai` compatible with all major LLM providers (Gemini, OpenAI, Anthropic, Grok, DeepSeek, OpenRouter, Ollama) plus the offline fallback so MRT always works with zero configuration.
3. **Zero Warnings Policy**:
   - Always run `cargo test --workspace` before finishing a task and resolve any compiler warnings (such as unused imports or `f32` literal annotations in `egui`). CI runs with `RUSTFLAGS=-D warnings` and `--locked`, so a warning fails the build. The CI workflow is currently disabled (no Actions minutes): run that command locally on Windows and Linux before merging.
4. **Keep the Docs in Sync**:
   - When you change behavior, update the matching document (see the table in [CONTRIBUTING.md](CONTRIBUTING.md#which-documents-to-update)). User-facing commands, shortcuts, and file locations live in [docs/USER_GUIDE.md](docs/USER_GUIDE.md).
