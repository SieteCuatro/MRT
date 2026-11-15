# MRT roadmap and project ideas

MRT already saves command blocks in SQLite, searches commands and output across restarts, compares a command’s output with its previous run, and displays several terminals in Mosaic view. The proposals below build on those features.

## Suggested first project — saved runbooks *(implemented)*

Let a developer select commands from history, arrange them in order, and save the sequence under a name. A Rust contributor could save “Check this project” with `cargo fmt --check`, `cargo clippy`, and `cargo test`, then open it again after the next edit.

The first version is implemented in the **Runbooks** panel and SQLite `HistoryStore`: it shows the commands, shell, and working directory before replay, runs each step through the existing Safety Approval Gate, shows its output in a command block, stops on the first failure so the user can inspect it, and works offline.

## Everyday terminal use

- **Interactive terminal mode.** *(implemented)* Launch full-screen interactive TUI applications (`vim`, `vi`, `nvim`, `nano`, `htop`, `lazygit`, `less`, etc., or via `:tui <cmd>`) inside an alt-screen PTY overlay modal. Forwards text and control keystrokes, provides rapid repaint streaming, supports graceful exit via `Ctrl+Q` or normal process termination, and preserves the full session transcript as a standard completed CommandBlock.
- **Split panes within a tab.** *(implemented)* Split any tab horizontally or vertically into independent panes (`:split`, `:split-h`, `:split-v`, `:unsplit`, and UI buttons), each with its own block stream, shell, and focus management.
- **Shell integration.** *(library implemented, GUI pending)* `mrt-core::shell_integration` parses OSC 133 semantic prompt markers and generates integration scripts for Bash, Zsh, Fish, PowerShell, and CMD. Next step: run each tab as a persistent PTY shell with the script injected, and use the markers to split output into command blocks with exact exit codes, so `cd`, environment variables, and shell functions persist naturally between commands.
- **Configuration file.** Load `~/.config/mrt/config.toml` / `.mrt.toml` following the schema in [`mrt.example.toml`](mrt.example.toml) (default shell, font size, Safety Gate options, AI provider, MCP servers to connect at startup). Today MRT reads environment variables and GUI controls only.

## Remote connections between MRT instances

Attach to an existing MRT tab from a second computer running Windows, macOS, or Linux. A developer who starts a build on a Linux workstation can open that same tab on a Windows laptop, see earlier command blocks, follow live output, and enter the next command. Show the host name, shell, and working directory in the attached tab.

- **Local network.** Pair two devices on the same LAN with host approval and share a selected tab over an encrypted connection. The host owns the shell, processes, and command blocks and can revoke access.
- **Reconnect milestone.** Start a build on the host, attach from the laptop, and disconnect the laptop mid-build. When it reconnects to the same tab, the build should have continued and the laptop should show the missed output exactly once, including the exit status if the build finished.
- **Over the internet.** Extend tab attachment and reconnection to paired devices on different networks. Choose the internet transport after validating the LAN milestone.

## Debugging and sharing results

- **Markdown bug reports.** *(implemented)* Select a failed block and its preceding history via the Report button, the `:report` / `:bug` commands, or the `mrt_generate_bug_report` MCP tool. Previews the commands, shell, exit codes, and output with automatic secret token redaction (API keys, GitHub tokens, bearer tokens, private keys) ready for GitHub issues.
- **Structured output comparison.** *(implemented)* Compare two selected blocks via the GUI Diff button, the `:diff` command, or the `mrt_compare_blocks` MCP tool. Compares JSON structures canonically so reordering keys produces zero diff, reports added/removed/modified JSON paths, and tracks added or removed socket listeners between `:ports` snapshots.
- **WebSocket inspector.** *(implemented)* Open a connection with `:ws` or the `mrt_inspect_ws` MCP tool, inspect connection handshake latency, stream incoming frames in real-time, and send probe frames with interactive tabular breakdown and text logs.
- **DNS and latency tools.** *(implemented)* Use `:dns` to inspect a hostname or URL, resolve its IPv4 (A) and IPv6 (AAAA) addresses, measure DNS lookup duration, trace concurrent TCP handshake latency to all endpoints, and inspect the results via an interactive table block, the Toolbox Network tab, the Command Palette, or the `mrt_inspect_dns` MCP tool.

## Task manager and process tools *(implemented)*

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
- **Out of scope for now.** Suspend/resume (needs `NtSuspendProcess` on Windows), priority changes, and services/daemons management.

## Rename the "Internet Studio" panel to "Toolbox" *(implemented)*

The panel held the HTTP inspector, web reader, port scanner, DNS/latency tracer, and WebSocket inspector. Only some of these reach the internet (`:ports` inspects the local machine), and more small tools were coming, so it is now the **Toolbox**: one sidebar entry and one panel with a tab per tool family.

- **Tabs.** **Network** (`:http`, `:ws`, `:dns`, `:web`, `:ports`, plus the last port scan with PID links) and **Task Manager**. The panel remembers the last tab; `:toolbox [network|task-manager|close]`, the Command Palette, and `mrt_control_gui`'s `toolbox` argument open it on a given tab.
- **Structure.** `ToolboxTab` in `widgets/toolbox.rs`, a thin `render_toolbox_side_panel` that draws the tab strip and dispatches to `render_network_tab` (`widgets/side_panels.rs`) and `render_task_manager_tab` (`widgets/task_manager.rs`). A future tool family is a new `ToolboxTab` variant plus a widget file.
- **Renamed.** `show_internet_panel` → `show_toolbox_panel`, `render_internet_side_panel` → `render_network_tab`, the egui panel id, sidebar label, Command Palette entries, `--help` banner, Linux desktop entry, and `docs/INTERNET_TOOLS.md` → `docs/TOOLBOX.md` with references updated across the docs. `ToolboxTab::parse` still accepts `"internet"` for the Network tab.
- **Crate name.** `mrt-tools` stays as is; `process_inspector.rs` lives there alongside the network modules.

## AI and MCP

- **Streaming AI responses.** *(implemented)* Display tokens of an AI error explanation in the requesting terminal tab progressively as they are generated, eliminating UI latency.
- **Remote MCP connections.** Add HTTP transport with support for streamed server events alongside the existing stdio client.
- **MCP server presets.** *(implemented)* Pre-configured server profiles in the MCP Hub (Filesystem, Git, Memory Graph, SQLite, Fetch) with one-click settings population for review before connecting.
- **Multi-step MCP assistance.** Let the assistant use results from one tool call to choose the next. For example, it could inspect a failed command block, retrieve the previous run, and explain the differences. Route any proposed shell execution through the existing Safety Approval Gate.

## Distribution *(implemented)*

Automated multi-platform CI/CD release workflow via `.github/workflows/release.yml`. On tagged releases (`v*`), compiles optimized release binaries and generates downloadable release packages across Windows (MSVC ZIP), Linux (Ubuntu tar.gz), and macOS (Intel x86_64 and Apple Silicon aarch64 tar.gz) and uploads them directly to GitHub Releases.
