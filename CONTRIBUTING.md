# 🤝 Contributing to Modern Rust Terminal (MRT)

First off: **thank you for being here!**

**Modern Rust Terminal (MRT)** is **100% free for everyone with zero restrictions**. It was created with AI and is designed to continuously improve thanks to AI and collaborators around the world.

---

## 💜 Donate Your AI Tokens to Improve MRT!

You don't need to be a senior Rust systems programmer to make a huge impact on MRT.
**If you have spare AI tokens** on *any* platform—**Google Antigravity / Gemini, Claude Code, Cursor, OpenAI Codex, Windsurf, Aider, OpenHands, Grok, DeepSeek, or local LLMs**—we warmly invite you to use your tokens to help build and polish this project!

### How to Contribute with Your AI Agent in 3 Steps:
1. **Fork & Clone** the repository:
   ```bash
   git clone https://github.com/SieteCuatro/MRT.git
   cd MRT
   ```
2. **Tell your AI Agent to read [`AGENTS.md`](AGENTS.md)**:
   - Example prompt you can paste directly into your AI assistant:
     > *"Read `AGENTS.md` and `docs/ARCHITECTURE.md`. Pick one of the high-priority roadmap items (or improve a feature in `mrt-core` / `mrt-tools` / `mrt-mcp` / `mrt-ai` / `mrt-app`), implement it cleanly with unit tests, verify `cargo test --workspace` passes with 0 warnings, and prepare a commit."*
3. **Open a Pull Request**:
   - Mention what feature or fix was added (and feel free to share which AI model/agent helped you build it!).

---

## 🗺️ High-Impact Roadmap Areas We Need Help With

Here are areas where human contributors and AI token donors can immediately add value:

1. **Terminal Emulation & Multiplexing (`crates/mrt-core` & `crates/mrt-app`)**:
   - Split panes (horizontal & vertical pane splitting inside a tab).
   - Full interactive TUI screen buffer mode (for apps like `vim`, `htop`, `lazygit`) alongside Command Block mode.
   - Shell integration scripts (OSC 133 prompt markers for Bash, Zsh, Fish, PowerShell, and CMD).
2. **Persistent Storage & Search**:
   - Local SQLite / DuckDB session store for time-travel command block search across restarts.
3. **Internet & Developer Tools (`crates/mrt-tools`)**:
   - Built-in `:dns` lookup & latency trace tool.
   - Built-in `:ws` WebSocket inspector block.
   - Built-in `:diff` side-by-side block output comparator.
4. **MCP Ecosystem (`crates/mrt-mcp`)**:
   - HTTP/SSE MCP client transport (in addition to `stdio`).
   - One-click MCP Server marketplace/presets in the GUI side panel.
5. **Multi-Provider AI (`crates/mrt-ai`)**:
   - Streaming token responses in the AI assistant banner.
   - Multi-step autonomous agent loop using connected MCP tools.
6. **Cross-Platform Packaging**:
   - GitHub Actions CI/CD workflows building release binaries for Windows (`.exe`/`.msi`), macOS (`.dmg`/`.app`), and Linux (`.AppImage`/`.deb`).

---

## ✅ Quality Checklist Before Submitting a PR

Please make sure every contribution satisfies:
```bash
cargo check --workspace
cargo test --workspace
```
- Zero compiler errors and zero warnings.
- Cross-platform compatibility preserved (use `#[cfg(target_os = "...")]` whenever using OS-specific APIs).
- New features include unit tests where applicable.
