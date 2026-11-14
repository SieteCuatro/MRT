# 🤖 AGENTS.md — AI Agent Guide for Modern Rust Terminal (MRT)

Welcome, AI Agent! This repository (**Modern Rust Terminal / MRT**) is an open, 100% free, AI-built cross-platform terminal written in Rust. Your user is generously contributing their AI tokens to improve this project.

Follow these guidelines to navigate the workspace efficiently, write idiomatic cross-platform Rust, and verify your changes.

---

## 1. Workspace Structure

The Cargo workspace contains 5 modular crates:

- **`crates/mrt-core`**:
  - `pty.rs`: Discovers installed host shells (`pwsh`, `powershell`, `cmd`/Batch, `git-bash`, `wsl`, `bash`, `zsh`, `fish`, `nu`, `sh`) and executes commands or spawns `portable-pty` sessions. Uses `CREATE_NO_WINDOW` (`0x08000000`) on Windows so child processes never flash external console windows.
  - `vt.rs`: Parses ANSI/VT100 escape sequences via `vte` into colored `AnsiSpan` segments and stripped plain text.
  - `block.rs`: Defines `CommandBlock` and `classify_output` (auto-detecting JSON trees, CSV/TSV tables, Markdown, and HTTP responses).
  - `safety.rs`: Evaluates command risk (`Safe`, `Moderate`, `Critical`) across Bash, PowerShell, CMD/Batch, SQL, Git, and Kubernetes for the MRT Safety Approval Gate.
  - `platform.rs`: Cross-platform startup helpers (`hydrate_login_environment` for macOS/Linux GUI `$PATH` inheritance and `attach_parent_console_if_cli` for Windows CLI flags).
- **`crates/mrt-tools`**:
  - `http_inspector.rs`: Built-in `:http` / `:curl` REST API client (`reqwest`).
  - `web_fetch.rs`: Built-in `:web` HTML-to-readable-Markdown extractor.
  - `net_inspector.rs`: Built-in `:ports` local TCP/UDP listening port scanner.
- **`crates/mrt-mcp`**:
  - `protocol.rs`: JSON-RPC 2.0 and Model Context Protocol (MCP) types.
  - `server.rs`: MRT as an MCP Server (`mrt --mcp-server`), exposing `mrt_get_session_state`, `mrt_list_blocks`, `mrt_run_command` (protected by the Safety Gate), `mrt_http_request`, `mrt_web_fetch`, and `mrt_inspect_ports`.
  - `client.rs`: MRT as an MCP Host, connecting to in-process loopback and external `stdio` MCP servers.
- **`crates/mrt-ai`**:
  - `lib.rs`: Multi-provider AI client supporting **Google Gemini**, **Anthropic Claude**, **OpenAI**, **xAI Grok**, **DeepSeek**, **OpenRouter**, **Local Ollama**, and a deterministic **Offline Polyglot Heuristic Engine**.
- **`crates/mrt-app`**:
  - `main.rs`: Entrypoint (`#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]`), CLI flag handler (`--mcp-server`, `--version`, `--help`), and `eframe` native GPU launcher.
  - `app.rs` & `widgets/`: Multi-tab GUI, per-tab shell switcher, interactive block renderer, MCP Hub side panel, Internet Studio side panel, Safety Approval Gate modal, and Command Palette (`Ctrl+K`).

---

## 2. Core Engineering Rules for AI Agents

1. **Cross-Platform First (Windows, macOS, Linux)**:
   - Never assume a single shell or OS. Always support PowerShell, CMD/Batch, and POSIX shells (`bash`/`zsh`/`sh`).
   - Whenever spawning background child commands via `tokio::process::Command` or `std::process::Command`, include the Windows `CREATE_NO_WINDOW` (`0x08000000`) guard so Windows users never see flashing console windows.
2. **Multi-AI Provider Neutrality**:
   - Keep `mrt-ai` compatible with all major LLM providers (Gemini, OpenAI, Anthropic, Grok, DeepSeek, OpenRouter, Ollama) plus the offline fallback so MRT always works with zero configuration.
3. **Zero Warnings Policy**:
   - Always run `cargo test --workspace` before finishing a task and resolve any compiler warnings (such as unused imports or `f32` literal annotations in `egui`).
