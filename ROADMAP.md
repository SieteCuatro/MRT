# MRT roadmap

MRT's strengths are the features built on top of the terminal: command blocks with structured output, the Safety Approval Gate, the MCP server and host, AI diagnosis, runbooks, and the Toolbox. Two gaps sit underneath them. MRT has no terminal screen emulator, and each command runs in a new shell process. Until those are fixed, full-screen programs do not display correctly and shell state is lost between commands.

The milestones below are in priority order. Each item says what to build and how to tell it is done. Status markers: *(implemented)*, *(in progress)*, *(library only)*. Unmarked items are open. When you finish an item, mark it here and update the documents listed in [CONTRIBUTING.md](CONTRIBUTING.md#which-documents-to-update).

## What other terminals do better

These comparisons shaped the order of the milestones.

- **Ghostty.** A correct, fast terminal core (screen grid, keyboard protocols, GPU rendering) ships as a separate library, `libghostty-vt`. Scrollback search, command-finished notifications, key tables and chained keybinds, and shell integration that keeps working over SSH. *Lesson:* build the features on a correct core. Use a proven emulator crate instead of writing one.
- **tmux.** A server owns the shells, and windows attach and detach. Every pane can be scripted (`send-keys`, `capture-pane`, targets, hooks). Keyboard copy mode, input sent to several panes at once, and a prefix key. *Lesson:* a session should survive closing the window.
- **Rex (Superlogical).** tmux rebuilt as a native app. A server owns the PTYs and sends raw bytes to clients, which parse them themselves. Tabs show what each program is doing (building, blocked, done, failed). One command installs a skill and a hook for coding agents. A CLI with `--json` output, Lua actions that appear in the command palette, and phone access over Tailscale. *Lesson:* make AI agents a normal part of the terminal, not an add-on. Rex's gaps are the desktop client running only on macOS, shells that do not survive a reboot, manual config reload, and remote login that trusts the network. MRT can do better on all four.

## Milestone 1 — A real terminal core

The TUI overlay now draws from a real screen emulator and follows the window size. Keys, Alt+letter and the mouse reach the program, and text can be selected and copied. Interactive programs also run inside split panes and show in Mosaic tiles, and MCP agents can read the screen. Milestone 1 is done.

- **Terminal grid emulator.** *(implemented, #9; the TUI overlay uses it)* Wrap `alacritty_terminal` (it uses the same `vte` parser MRT already depends on) or `libghostty-vt` behind a `mrt-core::term` module that exposes a cell grid, cursor, scrollback, modes, and title. Keep `vt::parse_ansi_text` for finished command blocks. *Done when:* `vim`, `htop`, `less`, `lazygit`, and `top` display and redraw correctly, and a unit test feeds recorded escape-sequence fixtures and checks the resulting grid.
- **Grid renderer widget.** *(implemented in the TUI overlay: `widgets/term_view.rs`, with drag selection and copy; the MCP PTY tools do not use it)* An egui widget that paints the grid cell by cell: colors, bold, italic, underline, inverse, wide CJK characters and emoji, the cursor, and selection. Use it in the TUI overlay and in the MCP PTY tools.
- **Resize.** *(implemented for the TUI overlay)* Compute columns and rows from the widget size and font metrics, and call `InteractivePty::resize` when the window or pane changes size. *Done when:* resizing the window while `htop` runs reflows it.
- **Complete keyboard and mouse input.** *(implemented in `term_input.rs`: xterm keys including Alt+letter, bracketed paste, and mouse reports in SGR 1006 or X10 for clicks, drags, motion and the wheel; the kitty keyboard protocol is not done)* Function keys, Alt/Meta, Home/End/PgUp/PgDn, and application cursor mode. Mouse reporting (SGR 1006) for TUIs that ask for it. Bracketed paste. Later, the kitty keyboard protocol.
- **Inline interactive panes.** *(implemented: in a split tab, `:tui` and full-screen programs run inside the active pane; one session per pane; Mosaic tiles show the program's screen, read-only)* Show a running PTY directly inside a split pane instead of a modal overlay, so `htop` can run next to a shell.
- **Screen-accurate MCP reads.** *(implemented: `mrt_pty_read` with `view: "screen"`; scrollback is not returned)* `mrt_pty_read` can return the visible screen as text, the cursor position, and the scrollback, not only the raw byte transcript, so agents can drive TUIs reliably.

## Milestone 2 — Everyday terminal use

These items make MRT usable as a person's main terminal.

- **Persistent shell per tab.** *(implemented: `ShellSession` #16, used by the GUI through `shell_pool.rs`; tested on Windows (PowerShell, CMD, Git Bash) and Linux (bash, sh, zsh, fish under WSL Ubuntu); macOS untested. Stop sends Ctrl+C, then ends the shell's child processes. Programs that wait for keyboard input still need `:tui`)* Run each tab or pane as one long-lived PTY shell with the `mrt-core::shell_integration` script injected. Use the OSC 133 markers to split output into command blocks with exact exit codes. *Done when:* `cd`, `export`, aliases, shell functions, `source venv/bin/activate`, and `conda activate` persist between blocks in PowerShell, CMD, Bash, Zsh, and Fish. Keep the one-process-per-command mode as a fallback for shells where injection fails.
- **Working-directory tracking.** *(implemented: the persistent shell reports OSC 7 after every command and updates the tab's folder, which the Explorer and new splits follow)* Read OSC 7 (and the OSC 133 prompt) to keep the tab's working directory, the folder explorer, and new splits in sync with the shell.
- **Configuration file.** *(implemented, #8)* Load `~/.config/mrt/config.toml` and per-project `.mrt.toml` using the schema in [`mrt.example.toml`](mrt.example.toml): default shell, font and size, theme, Safety Gate options, AI provider, keybindings, and MCP servers to connect at startup. Reload automatically when the file changes.
- **Scrollback search.** *(implemented for command blocks: Ctrl+F find bar with highlights and next/previous; searching a running program's scrollback is not done)* Search the live terminal buffer as well as saved history, with match highlighting and next/previous navigation. This is separate from the FTS5 history search.
- **Command-finished notifications.** *(implemented, #7)* Send a desktop notification when a command that ran longer than a set time finishes while MRT is unfocused or the tab is hidden. Include the exit code and duration.
- **Hyperlinks and clipboard.** *(implemented, #15)* OSC 8 hyperlinks, Ctrl/Cmd-click on detected URLs and `file:line` paths (open in the user's editor), and OSC 52 clipboard writes behind a permission prompt.
- **Shell integration over SSH.** *(implemented as opt-in remote tabs: `:ssh user@host` types the integration into the remote Bash/Zsh without writing files; key/agent authentication required, `:tui ssh` for passwords)* When the user runs `ssh`, offer to inject the integration script for that session (Ghostty's approach), so blocks and exit codes keep working on remote machines.

## Milestone 3 — Rendering and performance

- **Throughput benchmark.** *(implemented, #13; the workflow runs only when started by hand while Actions minutes are unavailable. First finding: Windows PowerShell streams output about 20× slower than `cmd`, see [docs/PERFORMANCE.md](docs/PERFORMANCE.md))* Add a benchmark that streams a large file (for example `cat` of 100 MB, and `yes | head -n 5000000`) and records time to finish and frames dropped. Run it in CI to catch regressions.
- **Large output.** *(implemented: incremental parsing while streaming, a per-block output limit (`terminal.max_block_output_mb`), and blocks that draw their last 2,000 lines with "Show more"; see [docs/PERFORMANCE.md](docs/PERFORMANCE.md))* Store very long block output out of memory or truncate it with "show more", virtualize block rendering, and cap the repaint rate while output is streaming. *Done when:* a 1 GB log does not freeze the UI.
- **Font handling.** *(implemented: `terminal.font_family` and `terminal.font_fallback` accept family names or font files, with platform symbol fonts after them; ligatures are not drawn because egui does not shape text)* Configurable font, fallback chain for missing glyphs, and optional ligatures. Get cell metrics right for Nerd Font icons.
- **Images.** The kitty graphics protocol, then Sixel. Show images inline in blocks and in the grid.
- **Idle cost.** *(implemented: worker and MCP events wake the UI, so it repaints once a second when idle instead of every 250 ms, and every 50 ms only while commands run)* Repaint only when something changes; today the app requests a repaint every 30 ms while busy. Measure CPU use when idle and set a target.

## Milestone 4 — Sessions that outlive the window

Inspired by tmux and Rex. This also builds the base for the remote-connection plan below.

- **Session server.** Move PTYs, shells, and running blocks into a background `mrt --server` process. The GUI becomes a client that attaches over the existing token-authenticated local bridge (`mrt-mcp::bridge`). *Done when:* closing the window mid-build and reopening it shows the same tabs, finished blocks, and live output.
- **Layout restore.** Save tabs, splits, shells, and working directories, and restore them after the server itself restarts (fresh shells, same layout).
- **Survive upgrades.** Hand running sessions from the old server to the new one when MRT updates, or keep the old server running until its sessions end.
- **Command-line control.** An `mrt` CLI that talks to the server with tmux-style targets: `mrt send`, `mrt capture`, `mrt split`, `mrt list`, all with `--json` and documented exit codes. These are thin wrappers around the existing MCP tools.

### Remote connections between MRT instances

Attach to an existing MRT tab from a second computer running Windows, macOS, or Linux. A developer who starts a build on a Linux workstation can open that same tab on a Windows laptop, see earlier command blocks, follow live output, and enter the next command. Show the host name, shell, and working directory in the attached tab.

- **Local network.** Pair two devices on the same LAN with host approval and share a selected tab over an encrypted connection. The host owns the shell, processes, and command blocks and can revoke access. Authenticate with a paired key, not only network identity.
- **Reconnect milestone.** Start a build on the host, attach from the laptop, and disconnect the laptop mid-build. When it reconnects to the same tab, the build should have continued and the laptop should show the missed output exactly once, including the exit status if the build finished.
- **Over the internet.** Extend tab attachment and reconnection to paired devices on different networks. Choose the internet transport after validating the LAN milestone.

## Milestone 5 — A terminal built for AI agents

MRT already has a 31-tool MCP server. These items make that easy to use and visible.

- **Agent status on tabs.** *(implemented, #10)* Show what a tab's program is doing (running, waiting for input, finished, failed) on the tab and in the Mosaic grid. Read standard OSC 9;4 progress and OSC 133 states, plus a documented MRT escape code that agents can send. Recognise common agent CLIs (`claude`, `codex`, `gemini`, `aider`) and flag when one is waiting for approval.
- **One-step agent setup.** *(implemented, #14)* `mrt agent setup` writes the MCP server entry and a short skill/instructions file for Claude Code, Codex, Gemini CLI, and other agents, and `mrt agent show` prints the instructions for any other tool. Undo with `mrt agent remove`.
- **Multi-step MCP assistance.** Let the assistant use results from one tool call to choose the next. For example, it could inspect a failed command block, retrieve the previous run, and explain the differences. Route any proposed shell execution through the existing Safety Approval Gate.
- **Remote MCP connections.** *(implemented, #11)* Add HTTP transport with support for streamed server events alongside the existing stdio client.
- **Agent sandbox tab.** A tab type whose commands always go through the Safety Gate and whose working directory is fixed, for letting an agent work unattended.

## Milestone 6 — Keyboard and scripting

- **Configurable keybindings.** *(implemented, #12; read from the config file's `[keybindings]` table)* All actions bindable from the config file, including chained keys (`ctrl+a > c`) and key tables / modes (Ghostty 1.3, tmux prefix key).
- **Named actions.** User-defined actions in the config (a command, a runbook, or a sequence of MRT actions) that appear in the Command Palette and can be bound to keys.
- **Copy mode.** Move through scrollback with the keyboard, select, and copy, without the mouse.
- **Input to several panes.** Optionally send typed input to every pane in a tab at once, with a clear on-screen indicator and the Safety Gate still checking each pane.
- **Event hooks.** Run an action when a command fails, a tab opens, or a long command finishes.

## Ideas not yet scheduled

- Quick terminal: a drop-down window on a global hotkey.
- Themes from a file, including importing common theme formats.
- Accessibility: screen-reader output for blocks and the grid, and high-contrast themes.
- A mobile companion app that attaches to a session server (after Milestone 4).
- Suspend/resume, priority changes, and service management in the Task Manager (needs `NtSuspendProcess` on Windows).

## Shipped

### Saved runbooks *(implemented)*

Let a developer select commands from history, arrange them in order, and save the sequence under a name. A Rust contributor could save “Check this project” with `cargo fmt --check`, `cargo clippy`, and `cargo test`, then open it again after the next edit.

The first version is implemented in the **Runbooks** panel and SQLite `HistoryStore`: it shows the commands, shell, and working directory before replay, runs each step through the existing Safety Approval Gate, shows its output in a command block, stops on the first failure so the user can inspect it, and works offline.

### Terminal basics

- **Interactive TUI overlay.** *(implemented)* `vim`, `nano`, `htop`, `lazygit`, `less`, and `:tui <cmd>` open a PTY in a full-screen overlay drawn from the terminal grid emulator, sized to the window. Keys and the mouse are encoded the xterm way, `Ctrl+Q` or the program exiting closes it, and the session is saved as a command block.
- **Split panes within a tab.** *(implemented)* Split any tab horizontally or vertically into independent panes (`:split`, `:split-h`, `:split-v`, `:unsplit`, and UI buttons), each with its own block stream, shell, and focus management.
- **Shell integration.** *(implemented)* `mrt-core::shell_integration` parses OSC 133 and OSC 7 and generates integration scripts for Bash, Git Bash, sh, Zsh, Fish, PowerShell, and CMD, and `mrt-core::shell_session` runs a persistent shell with them, which each GUI tab uses (see [persistent shell per tab](#milestone-2--everyday-terminal-use)).

### Debugging and sharing results

- **Markdown bug reports.** *(implemented)* Select a failed block and its preceding history via the Report button, the `:report` / `:bug` commands, or the `mrt_generate_bug_report` MCP tool. Previews the commands, shell, exit codes, and output with automatic secret token redaction (API keys, GitHub tokens, bearer tokens, private keys) ready for GitHub issues.
- **Structured output comparison.** *(implemented)* Compare two selected blocks via the GUI Diff button, the `:diff` command, or the `mrt_compare_blocks` MCP tool. Compares JSON structures canonically so reordering keys produces zero diff, reports added/removed/modified JSON paths, and tracks added or removed socket listeners between `:ports` snapshots.
- **WebSocket inspector.** *(implemented)* Open a connection with `:ws` or the `mrt_inspect_ws` MCP tool, inspect connection handshake latency, stream incoming frames in real-time, and send probe frames with interactive tabular breakdown and text logs.
- **DNS and latency tools.** *(implemented)* Use `:dns` to inspect a hostname or URL, resolve its IPv4 (A) and IPv6 (AAAA) addresses, measure DNS lookup duration, trace concurrent TCP handshake latency to all endpoints, and inspect the results via an interactive table block, the Toolbox Network tab, the Command Palette, or the `mrt_inspect_dns` MCP tool.

### Task manager and process tools *(implemented)*

MRT starts shells, PTY sessions, TUI apps, and background commands, and `:ports` reports the PID that owns each listener. The built-in task manager answers questions such as "what is holding port 3000?", "which tab started this runaway `node` process?", and "why is the fan spinning?" without leaving the terminal for Task Manager, Activity Monitor, or `htop`. Details: [Toolbox guide](docs/TOOLBOX.md#6-task-manager--process-tools-process_inspectorrs).

- **Process inspector (`mrt-tools/src/process_inspector.rs`).** *(implemented)* Cross-platform snapshots via the `sysinfo` crate (PID, parent PID, name, command line, user, CPU %, memory, start time, status) with one sampler shared by the GUI and the MCP server. The only external command is the graceful `taskkill /PID` on Windows, spawned with `CREATE_NO_WINDOW`.
- **Prompt commands.** *(implemented)* `:ps [filter] [--sort cpu|mem|pid|name] [--limit N] [--tree] [--mrt]`, `:top` (a live table block refreshed in place; `:top stop` or the block's Stop button ends it), `:proc <pid>` (details, child tree, ancestors, listening ports, working directory; the environment is not shown because it often holds secrets), and `:kill <pid|name>... [--force] [--tree]`.
- **Safety Approval Gate for process control.** *(implemented)* Every `:kill`, Task Manager Stop button, and `mrt_kill_process` request is planned against a fresh snapshot and needs approval. MRT-started and other same-user processes are `Moderate`; OS-critical processes, PIDs ≤ 4, MRT itself or its parents, other users' processes, and names matching several PIDs are `Critical`. Shell commands (`kill`, `pkill`, `killall`, `taskkill`, `Stop-Process`, …) in command position now always open the gate, and target protected names / PID 1 / `-1` as `Critical`. Graceful stop is `SIGTERM` / `taskkill /PID`; `--force` is `SIGKILL` / `TerminateProcess`.
- **MRT-aware view.** *(implemented)* The shell PID of every running command block (including runbook steps), the TUI overlay, and MCP PTYs are recorded and shown with their tab and command in the `MRT` column and the Task Manager. Running blocks have a **■ Stop** button that stops the whole process tree, sweeping for children spawned during the stop.
- **Ports ↔ processes.** *(implemented)* PID / PPID cells in any table block (`:ports`, `:ps`, `:top`) and in the Network tab's last-scan list open the process in the Task Manager, whose details list the ports it listens on. The DNS and WebSocket tools have no PID column, so they are not linked.
- **Snapshots and diffs.** *(implemented)* `structured_diff.rs` compares two `:ps` snapshots (started, exited, memory grown ≥ 10 MB and 20%) through the block Diff button, `:diff`, the Task Manager's "Compare with last snapshot", and `mrt_compare_process_snapshots`.
- **Alerts and watches.** *(implemented)* `:watch-proc <pid|name> [--mem 2GB] [--cpu 90]` posts a notice in the tab when a process crosses a threshold (once per crossing), when a matching process starts, or when one exits; `:watch-proc list|clear|stop <id>` manage them. Exit codes are only available for processes MRT started, where the command block already shows them.
- **MCP tools.** *(implemented)* `mrt_list_processes`, `mrt_inspect_process`, `mrt_kill_process` (Critical always blocked; with the GUI attached it opens the Safety Gate and returns `pending_approval`; headless it can only stop processes MRT started), `mrt_compare_process_snapshots`, and the `mrt://processes` resource.
- **GUI: Task Manager tab in the Toolbox.** *(implemented)* `widgets/task_manager.rs`: filter, sort, Tree / MRT only / Live toggles, per-row CPU sparklines, CPU and memory sparklines for the selected process, Stop / Force stop / Stop tree, Details, Watch, Snapshot to tab, and Command Palette entries in a new **Processes** section.

### Toolbox panel *(implemented)*

The "Internet Studio" panel was renamed **Toolbox**, because not all of its tools reach the internet (`:ports` inspects the local machine) and more tool families were coming. It has one sidebar entry and a tab per tool family.

- **Tabs.** **Network** (`:http`, `:ws`, `:dns`, `:web`, `:ports`, plus the last port scan with PID links) and **Task Manager**. The panel remembers the last tab; `:toolbox [network|task-manager|close]`, the Command Palette, and `mrt_control_gui`'s `toolbox` argument open it on a given tab.
- **Structure.** `ToolboxTab` in `widgets/toolbox.rs`, a thin `render_toolbox_side_panel` that draws the tab strip and dispatches to `render_network_tab` (`widgets/side_panels.rs`) and `render_task_manager_tab` (`widgets/task_manager.rs`). A future tool family is a new `ToolboxTab` variant plus a widget file.
- **Renamed.** `show_internet_panel` → `show_toolbox_panel`, `render_internet_side_panel` → `render_network_tab`, the egui panel id, sidebar label, Command Palette entries, `--help` banner, Linux desktop entry, and `docs/INTERNET_TOOLS.md` → `docs/TOOLBOX.md` with references updated across the docs. `ToolboxTab::parse` still accepts `"internet"` for the Network tab.

### AI and MCP

- **Streaming AI responses.** *(implemented)* Display tokens of an AI error explanation in the requesting terminal tab progressively as they are generated, eliminating UI latency.
- **MCP server presets.** *(implemented)* Pre-configured server profiles in the MCP Hub (Filesystem, Git, Memory Graph, SQLite, Fetch) with one-click settings population for review before connecting.

### Distribution *(implemented)*

Automated multi-platform CI/CD release workflow via `.github/workflows/release.yml`. On tagged releases (`v*`), compiles optimized release binaries and generates downloadable release packages across Windows (MSVC ZIP), Linux (Ubuntu tar.gz), and macOS (Intel x86_64 and Apple Silicon aarch64 tar.gz) and uploads them directly to GitHub Releases.
