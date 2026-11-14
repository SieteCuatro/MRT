# ⚡ Modern Rust Terminal (MRT)

> **100% FREE for All. Zero Restrictions. Built by AI, Powered by All AI Providers, Open to Every Collaborator.**

**Modern Rust Terminal (MRT)** is a next-generation, cross-platform (**Windows, macOS, Linux**) GPU-accelerated terminal emulator written in Rust (`eframe`/`egui` + `wgpu`). It combines the rock-solid foundation of traditional shells (**PowerShell, CMD/Batch, Bash, Git Bash, WSL, Zsh, Fish, Nushell**) with **Interactive Command Blocks**, **Built-in Internet & Network Tools**, **Multi-Provider AI**, and native **Model Context Protocol (MCP)** Host & Server capabilities.

---

## 🌍 Manifesto: Who Made This, What It's For, and Who It's For

### 🤖 Who Made This?
MRT was initiated by **[@SieteCuatro](https://github.com/SieteCuatro)** and **built by AI**.
It is a living open-source project designed to **keep evolving and improving thanks to AI** and the global developer community.

> 💜 **A Special Thank You to Token Contributors:**
> This project is built by AI and grows through AI. **We deeply thank you if you can use a portion of your AI tokens** (whether on Gemini, Claude, ChatGPT/Codex, Grok, DeepSeek, OpenRouter, or local GPUs) to build new features, refine the UI, write tests, and improve this project for everyone!

### 🎯 What Is It For?
Traditional terminals have been stuck in the 1970s teletype era—treating everything as a flat grid of characters, isolated from the internet, structured data, and modern AI protocols.
**MRT exists to bring the terminal into the modern era:**
1. **Polyglot Shell Freedom**: Run PowerShell (`pwsh`/`powershell`), Windows Batch (`cmd.exe`), Bash, Git Bash, WSL, Zsh, Fish, and Nushell side-by-side in tabs without friction.
2. **Structured Data by Default**: Automatically turn raw JSON into collapsible trees, CSV/TSV into sortable/filterable tables, and Markdown into clean formatted docs—while keeping raw ANSI/VT100 intact.
3. **Internet-Native Workflow**: Test REST APIs (`:http`), read live web documentation as Markdown (`:web`), and inspect local listening TCP/UDP ports (`:ports`) without leaving your prompt.
4. **First-Class MCP (Model Context Protocol)**: Act simultaneously as an **MCP Host** (connecting external tool servers into your terminal) and an **MCP Server** (`mrt --mcp-server`, allowing external AI agents to collaborate with your terminal sessions safely).
5. **Multi-Provider AI with a Safety Gate**: Translate natural language (`#` or `?`) into shell-specific commands and diagnose non-zero exit codes using **any AI provider**, protected by an interactive **Safety Approval Gate** that intercepts destructive commands (`rm -rf`, `Remove-Item -Recurse -Force`, `DROP TABLE`, etc.).

### 👥 Who Is It For?
- **Developers & Systems Engineers** who work across multiple OSes and shells (Windows PowerShell/CMD + WSL/Bash + macOS/Linux) and want structured JSON/table views and built-in HTTP/port tools.
- **AI Engineers & Agent Builders** who want a native **MCP Host & MCP Server** built right into their terminal.
- **Students, Hobbyists & Learners** who can type `# how do I find large files` in any shell (`cmd`, `pwsh`, `bash`) and learn the exact command and why it works.
- **AI Coding Agents** (Antigravity, Claude Code, Cursor, Windsurf, Codex, Aider, OpenHands) that need a safe, structured terminal runtime via MCP.

---

## 🆓 100% Free Forever — No Restrictions

**Modern Rust Terminal (MRT) is 100% FREE for everyone.**
- **No subscriptions, no paywalls, no login required, no feature gates.**
- **No restrictions**: Free for personal, academic, open-source, and commercial use ([LICENSE](LICENSE)).
- **Bring Your Own AI (or Use Offline Mode)**: MRT never locks you into a proprietary cloud subscription. Use your own API keys from *any* AI provider, run 100% free local models via **Ollama**, or use MRT's built-in offline polyglot engine with zero API keys.

---

## 🧠 All AI Providers Supported

MRT is strictly **provider-agnostic**. We embrace **all AI providers** out of the box—automatically detecting whichever API key or local endpoint you have configured:

| Provider | Environment Variable / Endpoint | Default Model |
| :--- | :--- | :--- |
| **Google Gemini** | `GEMINI_API_KEY` | `gemini-2.5-flash` |
| **Anthropic Claude** | `ANTHROPIC_API_KEY` | `claude-3-7-sonnet-latest` |
| **OpenAI** | `OPENAI_API_KEY` | `gpt-4o-mini` |
| **xAI Grok** | `XAI_API_KEY` | `grok-2-latest` |
| **DeepSeek** | `DEEPSEEK_API_KEY` | `deepseek-chat` |
| **OpenRouter (Any Model)** | `OPENROUTER_API_KEY` | `openrouter/auto` |
| **Ollama (Local / Free)** | `OLLAMA_HOST` (`http://localhost:11434`) | `llama3.2` / `qwen2.5-coder` |
| **Offline Polyglot Engine** | *(Built-in fallback — no key required)* | Native Rust Heuristic Engine |

---

## 🤝 Call for Collaborators (Humans & AI Agents Welcome!)

We are actively looking for collaborators! Whether you are a Rust developer, a UI designer, a documentation writer, or someone with spare AI tokens who wants to point an AI agent at an open issue:

1. **Read [CONTRIBUTING.md](CONTRIBUTING.md)** for how to contribute features, bug fixes, and ideas.
2. **Point Your AI Agent at [AGENTS.md](AGENTS.md)** — We included a dedicated `AGENTS.md` blueprint so AI agents can immediately understand the 5-crate workspace, run tests, and build new features autonomously using your tokens.
3. **Open PRs & Issues** at [https://github.com/SieteCuatro/MRT](https://github.com/SieteCuatro/MRT).

---

## 🚀 Quick Start

```bash
# 1. Clone the repository
git clone https://github.com/SieteCuatro/MRT.git
cd MRT

# 2. Launch the native GPU Terminal GUI
cargo run -p mrt-app

# 3. Or run MRT in headless MCP Server mode (JSON-RPC 2.0 over stdio)
cargo run -p mrt-app -- --mcp-server

# 4. Run the automated workspace test suite
cargo test --workspace
```

---

## 📚 Documentation Index

- **[Architecture & Crate Guide](docs/ARCHITECTURE.md)** — Deep dive into `mrt-core`, `mrt-tools`, `mrt-mcp`, `mrt-ai`, and `mrt-app`.
- **[Contributing & Token Donation Guide](CONTRIBUTING.md)** — How humans and AI agents can collaborate on MRT.
- **[AI Agent Instructions (`AGENTS.md`)](AGENTS.md)** — Context file for autonomous coding agents.
- **[Configuration Example (`mrt.example.toml`)](mrt.example.toml)** — Sample config for shells, safety rules, AI providers, and MCP servers.
