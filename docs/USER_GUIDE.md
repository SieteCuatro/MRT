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

### Notifications for long commands

When a command (including runbook steps and interactive TUI sessions) takes longer than **10 seconds** and finishes while the MRT window is unfocused or minimized, or while its tab is not on screen, MRT shows a desktop notification with the result (finished, or failed with its exit code), the command, how long it took, and the tab. On Linux, clicking the notification brings MRT to the front on that tab; on Windows and macOS the notification is informational only.

Tabs that are not on screen when one of their commands finishes get a numbered badge in the sidebar (any duration). The badge clears when you open the tab. In Mosaic every tab is on screen, so no badges appear.

Click **🔔 Notifications** in the sidebar to turn desktop notifications off, change the threshold, or send a test notification. These settings are not saved yet: they reset to on / 10 s when MRT restarts.

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

MRT does not read a configuration file yet. [`mrt.example.toml`](../mrt.example.toml) shows the planned schema.

---

## 6. Environment Variables

| Variable | Purpose |
| :--- | :--- |
| `GEMINI_API_KEY`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `XAI_API_KEY`, `DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY` | AI provider keys, checked in this order. See [AI Providers](AI_PROVIDERS_AND_AGENTS.md). |
| `OLLAMA_MODEL`, `OLLAMA_HOST` | Use a local Ollama model (default host `http://localhost:11434`). |
| `MRT_MCP_BRIDGE_PORT`, `MRT_MCP_BRIDGE_TOKEN` | Point `mrt --mcp-server` at a specific GUI bridge instead of reading `mcp-bridge.json`. |
| `RUST_LOG` | Log level for the GUI's stderr logs (default `info`), e.g. `RUST_LOG=debug`. |

With no AI variables set, MRT uses its offline heuristic engine, so `#` prompts and **Diagnose** always work.

---

## 7. Troubleshooting

| Problem | What to check |
| :--- | :--- |
| A shell is missing from **+ New tab** | MRT lists only shells it finds on `PATH` (Git Bash also in `C:\Program Files\Git\bin` or `C:\Program Files (x86)\Git\bin`). Install it or add it to `PATH`, then restart MRT. |
| `command not found` for tools that work in your normal terminal (macOS/Linux) | MRT loads the login shell's `PATH` at startup. Make sure the tool's folder is added in your login profile (`~/.zprofile`, `~/.bash_profile`, `~/.profile`), not only in an interactive-only file. |
| A command waits forever | It is probably waiting for input. Press **■ Stop** on the block and rerun it with `:tui <command>`. |
| `export VAR=…` has no effect on the next command | Each command runs in a new shell process. Combine them on one line (`export X=1 && ./run.sh`) or use a script. |
| The AI answer looks generic | No provider key is set, or the provider failed and MRT fell back to the offline engine. Check the provider name in the proposal card. |
| No desktop notification appears | Check that **🔔 Notifications** is on and the command ran longer than the threshold. Use **Send test notification**. Windows: allow notifications in *Settings → System → Notifications* (they are listed under Windows PowerShell). macOS: they are listed under Finder in *System Settings → Notifications*. Linux: a notification daemon must be running (most desktops have one). |
| An MCP client cannot see the open GUI | Make sure the GUI is running and that the client starts the same `mrt` binary with `--mcp-server`. Use **Copy MCP Config** in the MCP Hub for the exact path. |
| Linux build fails with missing `xcb` / `X11` / `udev` | Install the packages listed in [§1](#1-install). |

Bug reports are welcome: run `:report` on the failed block and paste the result (secrets are redacted, but review it first) into a GitHub issue.
