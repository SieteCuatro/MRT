# MRT roadmap and project ideas

MRT already saves command blocks in SQLite, searches commands and output across restarts, compares a command’s output with its previous run, and displays several terminals in Mosaic view. The proposals below build on those features.

## Suggested first project — saved runbooks *(implemented)*

Let a developer select commands from history, arrange them in order, and save the sequence under a name. A Rust contributor could save “Check this project” with `cargo fmt --check`, `cargo clippy`, and `cargo test`, then open it again after the next edit.

The first version is implemented in the **Runbooks** panel and SQLite `HistoryStore`: it shows the commands, shell, and working directory before replay, runs each step through the existing Safety Approval Gate, shows its output in a command block, stops on the first failure so the user can inspect it, and works offline.

## Everyday terminal use

- **Interactive terminal mode.** Let a user launch `vim`, `htop`, or `lazygit` from a block, interact with the full-screen program, and return to the block view when it exits. A first milestone is editing and saving a file in Vim, including resizing the window during the edit.
- **Split panes within a tab.** Let users split a pane horizontally or vertically so a server log can stay visible beside a shell in the same project. Each pane should keep its own terminal session and receive input when focused.
- **Shell integration.** Use OSC 133 prompt markers to identify where prompts, commands, and output begin and end. Explore integration for Bash, Zsh, Fish, PowerShell, and CMD, accounting for how each shell emits its prompt.

## Remote connections between MRT instances

Attach to an existing MRT tab from a second computer running Windows, macOS, or Linux. A developer who starts a build on a Linux workstation can open that same tab on a Windows laptop, see earlier command blocks, follow live output, and enter the next command. Show the host name, shell, and working directory in the attached tab.

- **Local network.** Pair two devices on the same LAN with host approval and share a selected tab over an encrypted connection. The host owns the shell, processes, and command blocks and can revoke access.
- **Reconnect milestone.** Start a build on the host, attach from the laptop, and disconnect the laptop mid-build. When it reconnects to the same tab, the build should have continued and the laptop should show the missed output exactly once, including the exit status if the build finished.
- **Over the internet.** Extend tab attachment and reconnection to paired devices on different networks. Choose the internet transport after validating the LAN milestone.

## Debugging and sharing results

- **Markdown bug reports.** Select a failed block and the steps leading up to it. Preview the commands, shell, exit codes, and output, redact private values, then copy the report into a GitHub issue.
- **Structured output comparison.** Extend the current line diff with a `:diff` command for two selected blocks. Show changed JSON fields side by side and identify listeners added or removed between two `:ports` snapshots. Reordering JSON object keys should leave the comparison empty.
- **WebSocket inspector.** Open a connection with `:ws`, display incoming messages as they arrive, and send a test message while the server log is visible in another terminal.
- **DNS and latency tools.** *(implemented)* Use `:dns` to inspect a hostname or URL, resolve its IPv4 (A) and IPv6 (AAAA) addresses, measure DNS lookup duration, trace concurrent TCP handshake latency to all endpoints, and inspect the results via an interactive table block, the Internet Studio panel, the Command Palette, or the `mrt_inspect_dns` MCP tool.

## AI and MCP

- **Streaming AI responses.** Display the first tokens of an explanation in the requesting tab while the provider continues generating.
- **Remote MCP connections.** Add HTTP transport with support for streamed server events alongside the existing stdio client.
- **MCP server presets.** Let a user select a known server configuration in the MCP Hub and review its settings before connecting. A searchable server catalog could follow.
- **Multi-step MCP assistance.** Let the assistant use results from one tool call to choose the next. For example, it could inspect a failed command block, retrieve the previous run, and explain the differences. Route any proposed shell execution through the existing Safety Approval Gate.

## Distribution

Publish downloadable releases through GitHub Actions so testers can launch MRT without a Rust toolchain. Candidate formats are an executable or MSI for Windows, an app bundle or DMG for macOS, and an AppImage or Debian package for Linux. Verify each published package by launching MRT and running a command on its target OS.
