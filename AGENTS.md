# AGENTS.md — AI Agent Guide for Modern Rust Terminal (MRT)

Welcome, AI Agent! This repository (**Modern Rust Terminal / MRT**) is an open, 100% free, AI-built cross-platform terminal written in Rust. Your user is generously contributing their AI tokens to improve this project.

Follow these guidelines to navigate the workspace efficiently, write idiomatic cross-platform Rust, and verify your changes.

---

## 1. Workspace Structure

The Cargo workspace contains 5 modular crates:

- **`crates/mrt-core`**:
  - `pty.rs`: Discovers installed host shells (`pwsh`, `powershell`, `cmd`/Batch, `git-bash`, `wsl`, `bash`, `zsh`, `fish`, `nu`, `sh`) and executes commands or spawns `portable-pty` sessions. Uses `CREATE_NO_WINDOW` (`0x08000000`) on Windows so child processes never flash external console windows.
  - `vt.rs`: Parses ANSI/VT100 escape sequences via `vte` into colored `AnsiSpan` segments and stripped plain text.
  - `block.rs`: Defines `CommandBlock` and `classify_output` (auto-detecting JSON trees, CSV/TSV tables, Markdown, and HTTP responses).
  - `safety.rs`: Evaluates command risk (`Safe`, `Moderate`, `Critical`) across Bash, PowerShell, CMD/Batch, SQL, Git, Kubernetes, and process control (`kill`, `taskkill`, `Stop-Process`, protected OS process names) for the MRT Safety Approval Gate.
  - `shell_integration.rs`: OSC 133 semantic prompt markers parser and shell integration generator for Bash, Zsh, Fish, PowerShell, and CMD.
  - `platform.rs`: Cross-platform startup helpers (`hydrate_login_environment` for macOS/Linux GUI `$PATH` inheritance and `attach_parent_console_if_cli` for Windows CLI flags).
- **`crates/mrt-tools`**:
  - `http_inspector.rs`: Built-in `:http` / `:curl` REST API client (`reqwest`).
  - `web_fetch.rs`: Built-in `:web` HTML-to-readable-Markdown extractor.
  - `net_inspector.rs`: Built-in `:ports` local TCP/UDP listening port scanner.
  - `dns_inspector.rs`: Built-in `:dns` DNS resolution and TCP handshake latency tracer.
  - `ws_inspector.rs`: Built-in `:ws` WebSocket inspector with live frame logging and test probe sender.
  - `bug_reporter.rs`: Built-in `:report` / `:bug` Markdown bug report generator with automatic secret token redaction.
  - `process_inspector.rs`: Built-in Task Manager backend (`sysinfo`): `:ps` / `:top` process tables, `:proc` details, `:kill` planning with Safety Gate classification and tree termination, `:watch-proc` watches, MRT-owned process labels, and process snapshot diffs.
  - `structured_diff.rs`: Canonical JSON diffs (key order invariant), listening port snapshot delta detection, and process snapshot deltas.
- **`crates/mrt-mcp`**:
  - `protocol.rs`: JSON-RPC 2.0 and Model Context Protocol (MCP) types.
  - `bridge.rs`: Token-authenticated localhost TCP bridge (`mcp-bridge.json`) so external `mrt --mcp-server` processes transparently control a running MRT GUI window.
  - `server.rs`: MRT as an MCP Server (`mrt --mcp-server` or live GUI bridge), exposing 31 tools across session & multi-tab control (`mrt_get_session_state`, `mrt_list_tabs`, `mrt_create_tab`, `mrt_switch_tab`, `mrt_close_tab`, `mrt_configure_tab`), command execution & block inspection/diffing/triage (`mrt_run_command`, `mrt_list_blocks`, `mrt_get_block`, `mrt_search_blocks`, `mrt_compare_blocks`, `mrt_diagnose_block`, `mrt_clear_blocks`), interactive PTYs (`mrt_pty_spawn`, `mrt_pty_write`, `mrt_pty_read`, `mrt_pty_resize`, `mrt_pty_list`, `mrt_pty_kill`), live GUI control (`mrt_control_gui`), AI translation (`mrt_ai_translate`), and internet & diagnostic tools (`mrt_http_request`, `mrt_web_fetch`, `mrt_inspect_ports`, `mrt_inspect_dns`, `mrt_generate_bug_report`, `mrt_inspect_ws`), and the Task Manager (`mrt_list_processes`, `mrt_inspect_process`, `mrt_kill_process`, `mrt_compare_process_snapshots`), plus 5 MCP resources.
  - `client.rs`: MRT as an MCP Host, connecting to in-process loopback and external `stdio` MCP servers (with one-click presets for Filesystem, Git, Memory, SQLite, Fetch).
- **`crates/mrt-ai`**:
  - `lib.rs`: Multi-provider AI client supporting **Google Gemini**, **Anthropic Claude**, **OpenAI**, **xAI Grok**, **DeepSeek**, **OpenRouter**, **Local Ollama**, and a deterministic **Offline Polyglot Heuristic Engine**, with streaming token delivery (`diagnose_command_error_streaming`).
- **`crates/mrt-app`**:
  - `main.rs`: Entrypoint (`#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]`), CLI flag handler (`--mcp-server`, `--version`, `--help`), and `eframe` native GPU launcher.
  - `app.rs`, `history.rs`, `theme.rs` & `widgets/`: Multi-tab GUI, Split Panes within tabs (`:split-h`, `:split-v`, `:unsplit`), full-screen Interactive TUI Mode overlay (`:tui <cmd>`, `vim`, `nano`, `htop`, `lazygit`), Mosaic split grid, Folder Explorer, SQLite + FTS5 trigram command history, dark theme & system font loader, per-tab shell switcher, interactive block renderer (with structured diffs), MCP Hub side panel, Toolbox side panel (`widgets/toolbox.rs` with Network and Task Manager tabs, `widgets/task_manager.rs`), Safety Approval Gate modal, and Command Palette (`Ctrl+K` / `Ctrl+P`).

---

## 2. Core Engineering Rules for AI Agents

1. **Cross-Platform First (Windows, macOS, Linux)**:
   - Never assume a single shell or OS. Always support PowerShell, CMD/Batch, and POSIX shells (`bash`/`zsh`/`sh`).
   - Whenever spawning background child commands via `tokio::process::Command` or `std::process::Command`, include the Windows `CREATE_NO_WINDOW` (`0x08000000`) guard so Windows users never see flashing console windows.
2. **Multi-AI Provider Neutrality**:
   - Keep `mrt-ai` compatible with all major LLM providers (Gemini, OpenAI, Anthropic, Grok, DeepSeek, OpenRouter, Ollama) plus the offline fallback so MRT always works with zero configuration.
3. **Zero Warnings Policy**:
   - Always run `cargo test --workspace` before finishing a task and resolve any compiler warnings (such as unused imports or `f32` literal annotations in `egui`).
