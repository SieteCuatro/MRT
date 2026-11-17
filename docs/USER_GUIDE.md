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

Agent CLIs such as `claude`, `codex`, `gemini` and `aider` are interactive, so start them with `:tui claude` (and so on). The tab then shows when the agent waits for your approval (see [Tab activity status](#tab-activity-status)).

### Tab activity status

Every sidebar tab row and every Mosaic tile has a small activity indicator. Hover it for details (state, command, detected agent, message).

| Indicator | State | Meaning |
| :--- | :--- | :--- |
| Grey dot | Idle | Nothing has run in this tab yet. |
| Blue spinner | Running | A command is running. |
| Blue ring + `42%` | Progress | The command reported progress (`OSC 9;4` or the MRT sequence below). The ring turns red or amber for error/warning progress. |
| Pulsing amber dot + `input` | Waiting for input | The command appears to wait for you: an approval prompt, a `[y/N]` question, a password prompt. |
| Green dot | Finished | The last command exited with code 0. |
| Red dot | Failed | The last command exited with a non-zero code. |

How MRT decides, in order:

1. **The MRT status sequence** (below), if the program sent one.
2. **Standard progress**: `ESC ] 9 ; 4 ; <state> ; <percent> BEL`, the sequence Windows Terminal and ConEmu understand. States: `0` remove, `1` normal, `2` error, `3` indeterminate (spinner), `4` warning. Tools such as `winget`, `cargo` (with `term.progress`) and many installers already emit it.
3. **"Waiting for input" heuristics** (deliberately conservative):
   - For a recognised agent CLI (`claude`, `codex`, `gemini`, `aider`, also via `npx`/`bunx`/`uvx`, or found in the process tree when the Task Manager is sampling): 1.5 s without output and one of the last lines reads like an approval prompt (`Do you want to proceed?`, `[y/n]`, `Esc to cancel`, a `>` input prompt).
   - For any other command: 5 s without output and the last line is a question (`…?`, `[y/N]`, `(yes/no)`, `password:`, `Press Enter`).
   - Any new output switches the tab back to Running.
4. Otherwise the tab shows Running, and after exit, Finished or Failed.

#### The MRT status sequence (for scripts and agents)

Programs can set the tab state explicitly by printing:

```text
ESC ] 678 ; status=<value> [; progress=<0-100>] [; message=<text>] BEL
```

- `<value>`: `running`, `waiting`, `success` (or `done`), `failure` (or `error`), `idle`, or `clear` to drop an earlier status and return to automatic detection.
- `progress=` shows a percentage (with `status=running`).
- `message=` appears in the tooltip and in MCP output; put it last, since it may contain `;`.
- `ST` (`ESC \`) works as the terminator too. Keys can come in any order; unknown keys are ignored.
- The status lasts until the program sends another one or `status=clear`, or the command exits (then the exit code decides).

Why 678: it spells "MRT" on a phone keypad, and no major terminal assigns it (xterm, VTE, iTerm2 use 1337, kitty 99/5113, VS Code 633, Windows Terminal/ConEmu 9, rxvt 777, FinalTerm 133). Terminals that do not know an OSC number ignore it, so a script can emit it unconditionally.

Examples:

```bash
printf '\033]678;status=waiting;message=Approve the deploy in the browser\007'
printf '\033]678;status=running;progress=40\007'
printf '\033]678;status=clear\007'
printf '\033]9;4;1;75\007'   # standard progress, 75%
```

```powershell
$e = [char]27; $b = [char]7
Write-Host -NoNewline "$e]678;status=waiting;message=Review the plan$b"
```

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
| An MCP client cannot see the open GUI | Make sure the GUI is running and that the client starts the same `mrt` binary with `--mcp-server`. Use **Copy MCP Config** in the MCP Hub for the exact path. |
| Linux build fails with missing `xcb` / `X11` / `udev` | Install the packages listed in [§1](#1-install). |

Bug reports are welcome: run `:report` on the failed block and paste the result (secrets are redacted, but review it first) into a GitHub issue.
