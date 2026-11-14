# MRT roadmap and project ideas

MRT already saves command blocks in SQLite, searches commands and output across restarts, compares a command’s output with its previous run, and displays several terminals in Mosaic view. The proposals below build on those features.

## Suggested first project — saved runbooks *(implemented)*

Let a developer select commands from history, arrange them in order, and save the sequence under a name. A Rust contributor could save “Check this project” with `cargo fmt --check`, `cargo clippy`, and `cargo test`, then open it again after the next edit.

The first version is implemented in the **Runbooks** panel and SQLite `HistoryStore`: it shows the commands, shell, and working directory before replay, runs each step through the existing Safety Approval Gate, shows its output in a command block, stops on the first failure so the user can inspect it, and works offline.

## Everyday terminal use

- **Interactive terminal mode.** *(implemented)* Launch full-screen interactive TUI applications (`vim`, `vi`, `nvim`, `nano`, `htop`, `lazygit`, `less`, etc., or via `:tui <cmd>`) inside an alt-screen PTY overlay modal. Forwards text and control keystrokes, provides rapid repaint streaming, supports graceful exit via `Ctrl+Q` or normal process termination, and preserves the full session transcript as a standard completed CommandBlock.
- **Split panes within a tab.** *(implemented)* Split any tab horizontally or vertically into independent panes (`:split`, `:split-h`, `:split-v`, `:unsplit`, and UI buttons), each with its own block stream, shell, and focus management.
- **Shell integration.** *(implemented)* Built-in OSC 133 semantic prompt marker parser and integration script generator for Bash, Zsh, Fish, PowerShell, and CMD to identify prompt, command start, execution, and exit codes.

## Remote connections between MRT instances

Attach to an existing MRT tab from a second computer running Windows, macOS, or Linux. A developer who starts a build on a Linux workstation can open that same tab on a Windows laptop, see earlier command blocks, follow live output, and enter the next command. Show the host name, shell, and working directory in the attached tab.

- **Local network.** Pair two devices on the same LAN with host approval and share a selected tab over an encrypted connection. The host owns the shell, processes, and command blocks and can revoke access.
- **Reconnect milestone.** Start a build on the host, attach from the laptop, and disconnect the laptop mid-build. When it reconnects to the same tab, the build should have continued and the laptop should show the missed output exactly once, including the exit status if the build finished.
- **Over the internet.** Extend tab attachment and reconnection to paired devices on different networks. Choose the internet transport after validating the LAN milestone.

## Debugging and sharing results

- **Markdown bug reports.** *(implemented)* Select a failed block and its preceding history via the Report button, the `:report` / `:bug` commands, or the `mrt_generate_bug_report` MCP tool. Previews the commands, shell, exit codes, and output with automatic secret token redaction (API keys, GitHub tokens, bearer tokens, private keys) ready for GitHub issues.
- **Structured output comparison.** *(implemented)* Compare two selected blocks via the GUI Diff button, the `:diff` command, or the `mrt_compare_blocks` MCP tool. Compares JSON structures canonically so reordering keys produces zero diff, reports added/removed/modified JSON paths, and tracks added or removed socket listeners between `:ports` snapshots.
- **WebSocket inspector.** *(implemented)* Open a connection with `:ws` or the `mrt_inspect_ws` MCP tool, inspect connection handshake latency, stream incoming frames in real-time, and send probe frames with interactive tabular breakdown and text logs.
- **DNS and latency tools.** *(implemented)* Use `:dns` to inspect a hostname or URL, resolve its IPv4 (A) and IPv6 (AAAA) addresses, measure DNS lookup duration, trace concurrent TCP handshake latency to all endpoints, and inspect the results via an interactive table block, the Internet Studio panel, the Command Palette, or the `mrt_inspect_dns` MCP tool.

## AI and MCP

- **Streaming AI responses.** *(implemented)* Display tokens of an AI error explanation in the requesting terminal tab progressively as they are generated, eliminating UI latency.
- **Remote MCP connections.** Add HTTP transport with support for streamed server events alongside the existing stdio client.
- **MCP server presets.** *(implemented)* Pre-configured server profiles in the MCP Hub (Filesystem, Git, Memory Graph, SQLite, Fetch) with one-click settings population for review before connecting.
- **Multi-step MCP assistance.** Let the assistant use results from one tool call to choose the next. For example, it could inspect a failed command block, retrieve the previous run, and explain the differences. Route any proposed shell execution through the existing Safety Approval Gate.

## Distribution *(implemented)*

Automated multi-platform CI/CD release workflow via `.github/workflows/release.yml`. On tagged releases (`v*`), compiles optimized release binaries and generates downloadable release packages across Windows (MSVC ZIP), Linux (Ubuntu tar.gz), and macOS (Intel x86_64 and Apple Silicon aarch64 tar.gz) and uploads them directly to GitHub Releases.
