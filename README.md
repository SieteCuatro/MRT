# Modern Rust Terminal (MRT)

A cross-platform (Windows, macOS, Linux) GPU-accelerated terminal emulator written in Rust (`eframe`/`egui` + `wgpu`). MRT combines native shell execution with structured command blocks, built-in network inspection tools, multi-provider AI translation/triage, and dual-mode Model Context Protocol (MCP) support.

Licensed under the [MIT License](LICENSE).

## Features

- **Polyglot Shell Support & Split Panes**: Auto-discovers and runs PowerShell (`pwsh`, `powershell`), Windows Batch (`cmd.exe`), Bash, Git Bash, WSL, Zsh, Fish, Nushell, and `sh` in multi-tab, horizontal/vertical split panes (`:split-h`, `:split-v`, `:unsplit`), or Mosaic split-grid layouts, with a synchronized Folder Explorer.
- **Interactive Terminal Mode (TUI)**: Run full-screen interactive console programs (`vim`, `nano`, `htop`, `lazygit`, `less`, etc., or via `:tui <cmd>`) inside an alt-screen PTY overlay with full keyboard forwarding, rapid streaming repaints, and graceful exit (`Ctrl+Q`), automatically saving the session transcript as a command block.
- **Shell Integration (OSC 133)**: Built-in semantic prompt marker parser and integration script generator for Bash, Zsh, Fish, PowerShell, and CMD for precise prompt, command execution, and exit code detection.
- **Structured Command Blocks**: Automatically classifies command output into collapsible JSON trees, filterable CSV/TSV tables, Markdown, or HTTP exchanges while preserving full ANSI/VT100 styling, structured JSON/port diffing, and line-by-line block comparisons.
- **Persistent History & Saved Runbooks**: Stores executed command blocks in SQLite with FTS5 trigram indexing for fast substring search across sessions, and lets you assemble, reorder, and save offline command sequences as **Runbooks**—previewing commands, shells, and working directories before replaying each step through the Safety Approval Gate and stopping on the first failure.
- **Built-in Network & Diagnostic Tools**:
  - `:http` / `:curl` — Inline REST API client with latency timing, response headers, and JSON tree rendering.
  - `:web` — Fetches URLs and converts HTML pages into readable Markdown.
  - `:ports` — Cross-platform local listening TCP/UDP port scanner (`netstat`, `lsof`, `ss`).
  - `:dns` — Resolves IPv4 (A) and IPv6 (AAAA) records, measures DNS lookup duration, and traces concurrent TCP handshake latency to all endpoints.
  - `:ws` — WebSocket client inspecting handshake latency, logging real-time frames with timing offsets, and sending probe messages.
  - `:report` / `:bug` — Generates GitHub-flavored Markdown bug reports with automatic redaction of sensitive credentials (API keys, tokens, private keys).
  - `:diff` — Compares two command blocks with canonical JSON key ordering, listening port snapshot deltas, and process snapshot deltas.
- **Task Manager & Process Tools**: A cross-platform task manager (`sysinfo`) in the **Toolbox** panel and at the prompt:
  - `:ps` — Sortable, filterable process table (tree view, `--mrt` for processes started by MRT tabs, TUI overlays and PTYs).
  - `:top` — Live process table refreshed in place every 2 seconds.
  - `:proc <pid>` — Details, child tree, ancestors and listening ports of one process.
  - `:kill <pid|name> [--force] [--tree]` — Stops processes through the Safety Approval Gate (OS-critical processes, MRT itself and other users' processes are Critical).
  - `:watch-proc <pid|name> [--mem 2GB] [--cpu 90]` — Notifies the tab when a process crosses a threshold, starts, or exits.
  - Running command blocks get a **■ Stop** button that stops the command's whole process tree; PIDs in `:ports` / `:ps` tables open the process in the Task Manager.
- **Toolbox Panel**: One side panel with a **Network** tab (HTTP, web reader, ports, DNS, WebSocket) and a **Task Manager** tab, linked by PID.
- **MCP Host & Server (31 Native Tools)**:
  - **MCP Server (`mrt --mcp-server`)**: Exposes 31 tools and 5 resources over JSON-RPC 2.0 (`stdio`), transparently proxying to a running GUI window via a local TCP bridge (`mcp-bridge.json`) or running standalone headless.
  - **MCP Host**: Connects to external `stdio` MCP servers from the GUI's MCP Hub panel, with one-click presets for common servers (Filesystem, Git, Memory Graph, SQLite, Fetch).
- **Safety Approval Gate**: Classifies commands (`Safe`, `Moderate`, `Critical`) across Bash, PowerShell, CMD/Batch, Git, SQL, Kubernetes, and process control (`kill`, `pkill`, `taskkill`, `Stop-Process`, …); prompts for confirmation in the GUI and blocks unapproved `Critical` commands over MCP.
- **Provider-Agnostic AI with Streaming**: Translates natural-language prompts (`#` or `?`) into shell-specific commands and streams AI error diagnoses token-by-token directly into failed blocks, with an offline heuristic fallback when no API key is configured.
- **Automated CI/CD Distribution**: Multi-platform release matrix in GitHub Actions publishing ready-to-use binaries for Windows (x86_64 ZIP), Linux (Ubuntu tar.gz), and macOS (Intel & Apple Silicon tar.gz).

## Quick Start

```bash
git clone https://github.com/SieteCuatro/MRT.git
cd MRT

# Launch the native GPU terminal GUI
cargo run -p mrt-app

# Run in headless MCP Server mode (JSON-RPC 2.0 over stdio)
cargo run -p mrt-app -- --mcp-server

# Run the workspace test suite
cargo test --workspace
```

## AI Providers

MRT checks configured environment variables in the following order and falls back to the built-in offline heuristic engine if none are set or reachable:

| Provider | Environment Variable | Default Model |
| :--- | :--- | :--- |
| Google Gemini | `GEMINI_API_KEY` | `gemini-2.5-flash` |
| Anthropic Claude | `ANTHROPIC_API_KEY` | `claude-3-7-sonnet-latest` |
| OpenAI | `OPENAI_API_KEY` | `gpt-4o-mini` |
| xAI Grok | `XAI_API_KEY` | `grok-2-latest` |
| DeepSeek | `DEEPSEEK_API_KEY` | `deepseek-chat` |
| OpenRouter | `OPENROUTER_API_KEY` | `openrouter/auto` |
| Local Ollama | `OLLAMA_MODEL` (`OLLAMA_HOST`) | User-configured (e.g. `qwen2.5-coder`) |
| Offline Engine | *(none required)* | Native Rust heuristics |

## Documentation

- [Architecture Overview](docs/ARCHITECTURE.md) — 5-crate workspace design and data flow.
- [MCP Guide](docs/MCP_GUIDE.md) — Tool schemas, resources, Live GUI Bridge, and client setup.
- [Core & Polyglot Shells](docs/CORE_AND_SHELLS.md) — Shell discovery, PTY engine, VT parser, blocks, and Safety Gate.
- [Toolbox: Network Tools & Task Manager](docs/TOOLBOX.md) — `:http`, `:web`, `:ports`, `:dns`, `:ws`, `:ps`, `:top`, `:proc`, `:kill`, and `:watch-proc` internals.
- [AI Providers & Error Triage](docs/AI_PROVIDERS_AND_AGENTS.md) — Provider resolution and command diagnosis.
- [GUI & Widgets](docs/GUI_AND_WIDGETS.md) — Async architecture, block viewers, and side panels.
- [Roadmap](ROADMAP.md) — Planned milestones and project ideas.
- [Contributing](CONTRIBUTING.md) — Development workflow and PR checklist.
- [AI Agent Instructions](AGENTS.md) — Workspace context for coding agents.
- [Configuration Example](mrt.example.toml) — Reference configuration schema.
