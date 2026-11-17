# MRT User Guide

How to install and use **Modern Rust Terminal (MRT)** day to day: the prompt, every built-in command, keyboard shortcuts, where MRT keeps its files, and what to do when something goes wrong. For how the code works, see the [Architecture Overview](ARCHITECTURE.md).

---

## 1. Install

### Prebuilt binaries
Every `v*` tag publishes archives on the [GitHub Releases](https://github.com/SieteCuatro/MRT/releases) page:

| Platform | Archive |
| :--- | :--- |
| Windows (x86_64) | `mrt-<tag>-x86_64-pc-windows-msvc.zip` |
| Linux (x86_64) | `mrt-<tag>-x86_64-unknown-linux-gnu.tar.gz` |
| macOS (Intel) | `mrt-<tag>-x86_64-apple-darwin.tar.gz` |
| macOS (Apple Silicon) | `mrt-<tag>-aarch64-apple-darwin.tar.gz` |

Each archive contains only the `mrt` (or `mrt.exe`) binary. Put it somewhere on your `PATH`.

### Build from source
You need a stable Rust toolchain ([rustup](https://rustup.rs)). On Debian/Ubuntu, install the GUI libraries first:

```bash
sudo apt-get install -y libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev libx11-dev libasound2-dev libudev-dev
```

Then:

```bash
git clone https://github.com/SieteCuatro/MRT.git
cd MRT
cargo build --release -p mrt-app
```

The binary is `target/release/mrt` (`mrt.exe` on Windows).

### Command-line flags

| Flag | What it does |
| :--- | :--- |
| *(none)* | Opens the GUI. |
| `--mcp-server` | Runs the MCP server over stdio for AI clients. Forwards to a running GUI window when one is open, otherwise runs headless. See the [MCP Guide](MCP_GUIDE.md). |
| `--version`, `-V` | Prints the version. |
| `--help`, `-h` | Prints usage. |
| `agent setup [--agent claude\|codex\|gemini\|all] [--user\|--project] [--dry-run]` | Registers MRT's MCP server and an `mrt` skill with Claude Code, Codex CLI and Gemini CLI. See [Connect coding agents](MCP_GUIDE.md#21-one-command-setup-for-coding-agents-mrt-agent-setup). |
| `agent show` | Prints the MCP command line and the agent instructions, for agents `setup` does not know. |
| `agent remove [same flags]` | Removes exactly what `agent setup` added. |

On macOS and Linux, MRT loads your login shell's `PATH` at startup, so tools installed through Homebrew, `nvm`, `cargo`, etc. are found even when MRT is launched from the Dock or a desktop launcher.

---

## 2. The Prompt

Type in the composer at the bottom of the window and press `Enter`.

| Input | Result |
| :--- | :--- |
| `git status` | Runs in the tab's shell (pick it with the shell switcher next to the input). The output becomes a command block. |
| `# find files larger than 100 MB` or `? …` | Asks the AI to translate the request into a command for the current shell. The proposal appears above the composer with **Run** / **Dismiss**; nothing runs until you click **Run**. See [AI Providers](AI_PROVIDERS_AND_AGENTS.md). |
| `vim notes.txt`, `htop`, `:tui <cmd>` | Opens the full-screen interactive mode (see [§4](#4-interactive-tui-mode)). |
| `:command …` | A built-in MRT command (below). |

Each command runs in a fresh shell process in the tab's working directory. MRT tracks `cd`, `cd /d`, `Set-Location`, and `cd ~` so the next command starts in the new folder. Other shell state (variables, aliases, functions) does not carry over between commands.

Commands that may be destructive open the **Safety Approval Gate** first (see [Core & Shells](CORE_AND_SHELLS.md#5-cross-shell-safety-approval-gate-safetyrs)).

### Built-in commands

**Layout**

| Command | What it does |
| :--- | :--- |
| `:split` / `:split-h` | Splits the tab horizontally. |
| `:split-v` | Splits the tab vertically. |
| `:unsplit` | Closes the split. |
| `:clear` (or `cls`) | Removes all blocks from the tab (history in SQLite is kept). |
| `:toolbox [network\|task-manager\|close]` | Shows the Toolbox panel, opens it on a tab, or hides it. With no argument it toggles. |

**Network and diagnostics** — details in the [Toolbox guide](TOOLBOX.md)

| Command | What it does |
| :--- | :--- |
| `:http [METHOD] <url> [json]` / `:curl …` | HTTP request with status, latency, headers, and a JSON tree. |
| `:web <url>` | Fetches a page and shows it as readable Markdown. |
| `:ports` | Lists local listening TCP/UDP ports with their PID. |
| `:dns <host[:port] \| url>` | Resolves A/AAAA records and measures TCP handshake latency. |
| `:ws [--timeout ms] <url> [message]` | Connects to a WebSocket, logs frames, optionally sends a probe. |
| `:diff [block_id block_id]` | Compares two blocks (by default the last two). |
| `:report` / `:bug` | Markdown bug report for the last failed block, with secrets redacted. |

**Processes** — details in the [Toolbox guide](TOOLBOX.md#6-task-manager--process-tools-process_inspectorrs)

| Command | What it does |
| :--- | :--- |
| `:ps [filter] [--sort cpu\|mem\|pid\|name] [--limit N] [--tree] [--mrt]` | Process table snapshot. |
| `:top [same options]` | Live process table, refreshed every 2 s. `:top stop` ends it. |
| `:proc <pid>` | Details, children, ancestors, and listening ports of one process. |
| `:kill <pid\|name>... [--force] [--tree]` | Stops processes after Safety Gate approval. With no arguments, opens the Task Manager. |
| `:watch-proc <pid\|name> [--mem 2GB] [--cpu 90]` | Posts a notice when a process crosses a threshold, starts, or exits. |
| `:watch-proc list \| clear \| stop <id>`, `:unwatch-proc <id\|all>` | Manages watches. |

---

## 3. Keyboard Shortcuts

| Keys | Where | Action |
| :--- | :--- | :--- |
| `Enter` | Composer | Runs the command. |
| `↑` / `↓` | Composer | Previous / next command in this tab. |
| `Ctrl+K` or `Ctrl+P` | Anywhere | Opens the Command Palette (layout, tools, processes, AI, shells). |
| `Enter` | Command Palette | Runs the first match, or the typed text as a command when nothing matches. |
| `Esc` | Command Palette / Safety Gate | Closes the palette / cancels the pending command. |
| `Ctrl+Q` | Interactive TUI mode | Ends the session and saves its transcript as a block. |

The Command Palette lists everything you can also reach with the mouse, so it is the quickest way to discover features.

---

## 4. Interactive TUI Mode

Full-screen programs need a real terminal. MRT opens them in a full-screen PTY overlay when the command starts with `vim`, `vi`, `nvim`, `nano`, `htop`, `top`, `lazygit`, `lazydocker`, `less`, `man`, or `fzf`, or when you type `:tui <command>` for anything else (for example `:tui ssh myserver` or `:tui python`).

- Text, `Enter`, `Backspace`, `Tab`, `Esc`, arrows, `Home` / `End`, `PageUp` / `PageDown`, `Delete`, and `Ctrl+A/C/D/E/L/U/W/Z` are sent to the program.
- The overlay closes when the program exits, or when you press `Ctrl+Q`.
- The session transcript is saved as a normal command block in the tab.

---

## 5. Data Files and Locations

MRT stores its files in the per-user local data directory:

| OS | Folder |
| :--- | :--- |
| Windows | `%LOCALAPPDATA%\MRT\data` |
| macOS | `~/Library/Application Support/MRT` |
| Linux | `$XDG_DATA_HOME/mrt` (usually `~/.local/share/mrt`) |

| File | Contents |
| :--- | :--- |
| `history.sqlite` | Every completed command block (command, output, shell, folder, exit code) and your saved runbooks. **Search history** searches it; queries of 3+ characters use the FTS5 trigram index, shorter ones a plain substring match. Output can contain secrets that commands printed; delete the file to wipe history. |
| `mcp-bridge.json` | Port and per-session token of the running GUI's MCP bridge. Written at startup (`0600` on macOS/Linux) and removed on exit. A stale file left after a crash is ignored when no GUI answers. |

`mrt agent setup` writes to the coding agents' own files (`~/.claude.json`, `~/.codex/config.toml`, `~/.gemini/settings.json` and `skills/mrt/SKILL.md` folders; see the [MCP Guide](MCP_GUIDE.md#21-one-command-setup-for-coding-agents-mrt-agent-setup)). Before changing an existing file it saves a copy next to it as `<file>.mrt-backup-<date>-<time>`.

See [§6 Configuration File](#6-configuration-file) for where MRT reads its settings.

---

## 6. Configuration File

MRT reads two optional [TOML](https://toml.io) files. Every setting is optional: a missing, empty, or partial file is fine, and anything you leave out keeps its default. Start from [`mrt.example.toml`](../mrt.example.toml).

| File | Where |
| :--- | :--- |
| User config | `~/.config/mrt/config.toml` on Linux and macOS (`$XDG_CONFIG_HOME/mrt/config.toml` if set), `%APPDATA%\mrt\config.toml` on Windows. Set `MRT_CONFIG` to use another file. |
| Project config | `.mrt.toml` in the folder you start MRT from. It is merged on top of the user config, so its values win. |

**Merging.** Tables merge key by key. `[[mcp.servers]]` entries merge by `name` (a project entry replaces the user entry with the same name; others are added). `safety.blocked_patterns` is the union of both lists, so a project file cannot remove your blocked patterns.

**Live reload.** MRT checks both files about once a second and reloads 300 ms after the last change. If a file has a syntax or type error, MRT keeps the last good settings (the defaults, at startup) and shows the error, with the file name, in a banner under the header. Fixing the file clears the banner. Unknown values (theme, provider, out-of-range font size) show a warning in the same banner.

| Setting | Default | Effect |
| :--- | :--- | :--- |
| `terminal.default_shell` | `"auto"` | Shell of the first tab at startup: `auto` (first discovered shell) or a label from **+ New tab** (`pwsh`, `powershell`, `cmd`, `bash`, `git-bash`, `wsl`, `zsh`, `fish`, `nu`, `sh`). An unknown or missing shell falls back to `auto`. |
| `terminal.font_size` | `13.0` | Terminal (monospace) font size, clamped to 8–40. Applied live. |
| `terminal.theme` | built-in | `graphite-calm` (aliases `dark`, `default`) is the only theme today. |
| `terminal.scrollback_blocks`, `terminal.auto_detect_structured_output` | `500`, `true` | Reserved; parsed but not applied yet. |
| `safety.enabled` | `true` | `false` turns off approval prompts for commands you type and runbook steps. Blocked patterns still apply, and `:kill` still confirms. |
| `safety.require_approval_for_moderate` | `false` | `true` asks before every Moderate command, not only process-control ones. |
| `safety.require_approval_for_critical` | `true` | `false` runs Critical commands without asking. |
| `safety.blocked_patterns` | `[]` | Commands containing any of these strings (case-insensitive) are refused with an error block, and stop a runbook. |
| `ai.provider` | `"auto"` | `auto` tries providers in the [default order](#7-environment-variables). A name (`gemini`, `anthropic`, `openai`, `xai`/`grok`, `deepseek`, `openrouter`, `ollama`) tries that provider first; `offline` always uses the built-in engine. A provider is only used if its key is set. |
| `ai.model` | provider default | Model for the provider named in `ai.provider`. |
| `ai.ollama_endpoint` | `http://localhost:11434` | Ollama base URL. |
| `[[mcp.servers]]` | none | MCP servers to connect at startup (`name`, `command`, `args`, `enabled`, `transport = "stdio"`). Servers added on reload are connected; removing one takes effect after a restart. |
| `notifications.enabled`, `notifications.threshold_secs` | `true`, `10` | Reserved for finished-command notifications; parsed but not used yet. |
| `[keybindings]` | none | `"binding" = "action"` strings, reserved for configurable shortcuts; parsed but not used yet. |

**Environment variables win over the config file.** API keys are read only from the environment (never put them in the file). `OLLAMA_MODEL` and `OLLAMA_HOST` override `ai.model` and `ai.ollama_endpoint` for Ollama; with `ai.provider = "ollama"` and `ai.model` set, Ollama works without `OLLAMA_MODEL`.

Safety settings apply to the GUI. Commands sent by MCP agents (`mrt_run_command`, `mrt_pty_*`) keep their fixed rule: Critical commands are always refused.

---

## 7. Environment Variables

| Variable | Purpose |
| :--- | :--- |
| `GEMINI_API_KEY`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `XAI_API_KEY`, `DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY` | AI provider keys, checked in this order (`ai.provider` in the config can move one to the front). See [AI Providers](AI_PROVIDERS_AND_AGENTS.md). |
| `MRT_CONFIG` | Path of the user configuration file, instead of the default location. |
| `OLLAMA_MODEL`, `OLLAMA_HOST` | Use a local Ollama model (default host `http://localhost:11434`). They override `ai.model` and `ai.ollama_endpoint`. |
| `MRT_MCP_BRIDGE_PORT`, `MRT_MCP_BRIDGE_TOKEN` | Point `mrt --mcp-server` at a specific GUI bridge instead of reading `mcp-bridge.json`. |
| `CODEX_HOME` | Where `mrt agent setup` finds Codex CLI's `config.toml` (default `~/.codex`). |
| `RUST_LOG` | Log level for the GUI's stderr logs (default `info`), e.g. `RUST_LOG=debug`. |

With no AI variables set, MRT uses its offline heuristic engine, so `#` prompts and **Diagnose** always work.

---

## 8. Troubleshooting

| Problem | What to check |
| :--- | :--- |
| A shell is missing from **+ New tab** | MRT lists only shells it finds on `PATH` (Git Bash also in `C:\Program Files\Git\bin` or `C:\Program Files (x86)\Git\bin`). Install it or add it to `PATH`, then restart MRT. |
| `command not found` for tools that work in your normal terminal (macOS/Linux) | MRT loads the login shell's `PATH` at startup. Make sure the tool's folder is added in your login profile (`~/.zprofile`, `~/.bash_profile`, `~/.profile`), not only in an interactive-only file. |
| A command waits forever | It is probably waiting for input. Press **■ Stop** on the block and rerun it with `:tui <command>`. |
| `export VAR=…` has no effect on the next command | Each command runs in a new shell process. Combine them on one line (`export X=1 && ./run.sh`) or use a script. |
| A banner says "Config not applied" | Your `config.toml` or `.mrt.toml` has a TOML error; the banner names the file and line. MRT keeps the last good settings until you fix it. |
| The AI answer looks generic | No provider key is set, or the provider failed and MRT fell back to the offline engine. Check the provider name in the proposal card. |
| An MCP client cannot see the open GUI | Make sure the GUI is running and that the client starts the same `mrt` binary with `--mcp-server`. Use **Copy MCP Config** in the MCP Hub for the exact path. If you moved or rebuilt `mrt` elsewhere, run `mrt agent setup` again to update the path. |
| `mrt agent setup` says *skipped* | The agent already has an `mrt` MCP server or `mrt` skill that MRT did not write. MRT leaves it alone; rename or delete it, then run setup again. |
| Linux build fails with missing `xcb` / `X11` / `udev` | Install the packages listed in [§1](#1-install). |

Bug reports are welcome: run `:report` on the failed block and paste the result (secrets are redacted, but review it first) into a GitHub issue.
