# ⚡ Modern Rust Terminal (MRT)

A cross-platform (Windows, macOS, Linux), GPU-accelerated terminal built in Rust (`eframe`/`egui` + `wgpu`) with polyglot multi-shell support, interactive command blocks, built-in internet/network tools, an AI command assistant with a Safety Approval Gate, and native **Model Context Protocol (MCP)** Host & Server support.

---

## ✨ Key Features

- **🐚 Polyglot Multi-Shell Engine**:
  - Automatically discovers installed shells across Windows, macOS, and Linux: **PowerShell 7 (`pwsh`)**, **Windows PowerShell (`powershell`)**, **Command Prompt / Batch (`cmd.exe`)**, **Git Bash (`bash.exe`)**, **WSL (`wsl.exe`)**, **Bash**, **Zsh**, **Fish**, and **Nushell (`nu`)**.
  - Switch shells per-tab or open new tabs in different shells simultaneously.
- **🧱 Interactive Command Blocks & Structured Output**:
  - Automatically detects **JSON** (collapsible tree viewer), **CSV/TSV** (filterable table viewer), **Markdown**, and **HTTP exchanges** while preserving raw **ANSI/VT100** output.
- **🌐 Built-in Internet & Network Tools**:
  - `:http [METHOD] <url> [json_body]` — Inline HTTP/REST API inspector with latency, status badges, headers, and JSON body tree.
  - `:web <url>` — Fetches any webpage and strips boilerplate HTML into clean, readable Markdown inside a terminal block.
  - `:ports` — Scans active local listening TCP/UDP sockets and PIDs into an interactive table.
- **🔌 Dual-Mode Model Context Protocol (MCP)**:
  - **MCP Host**: Connect external stdio MCP servers and invoke tools directly from the **MCP Hub** side panel.
  - **MCP Server**: Run `mrt --mcp-server` to expose MRT's session state, command block history, internet tools, and safety-gated command execution over JSON-RPC 2.0 `stdio`.
- **🤖 Shell-Aware AI & Safety Approval Gate**:
  - Prefix prompts with `#` or `?` (e.g., `# list all files`, `# show system info`) to synthesize commands tailored to your active shell (`cmd` vs `pwsh` vs `bash`).
  - Destructive commands (`rm -rf`, `Remove-Item -Recurse -Force`, `rd /s /q`, `DROP TABLE`, `git reset --hard`) are intercepted by the **MRT Safety Approval Gate** before execution.

---

## 🚀 Quick Start

```bash
# Build and launch the native GPU GUI
cargo run -p mrt-app

# Run in headless MCP Server mode (JSON-RPC 2.0 over stdio)
cargo run -p mrt-app -- --mcp-server

# Run workspace tests
cargo test --workspace
```
