# MRT roadmap and project ideas

MRT already saves command blocks in SQLite, searches commands and output across restarts, compares a command’s output with its previous run, and displays several terminals in Mosaic view. The proposals below build on those features.

## Suggested first project — saved runbooks

Let a developer select commands from history, arrange them in order, and save the sequence under a name. A Rust contributor could save “Check this project” with `cargo fmt --check`, `cargo clippy`, and `cargo test`, then open it again after the next edit.

A first version should show the commands, shell, and working directory before replay. Run each step through the existing Safety Approval Gate, show its output in a command block, and stop on the first failure so the user can inspect it. Saving and replaying a runbook should work offline.

## Everyday terminal use

- **Interactive terminal mode.** Let a user launch `vim`, `htop`, or `lazygit` from a block, interact with the full-screen program, and return to the block view when it exits. A first milestone is editing and saving a file in Vim, including resizing the window during the edit.
- **Split panes within a tab.** Let users split a pane horizontally or vertically so a server log can stay visible beside a shell in the same project. Each pane should keep its own terminal session and receive input when focused.
- **Shell integration.** Use OSC 133 prompt markers to identify where prompts, commands, and output begin and end. Explore integration for Bash, Zsh, Fish, PowerShell, and CMD, accounting for how each shell emits its prompt.

## Remote connections between MRT instances

Open a terminal tab on another computer running MRT, across Windows, macOS, and Linux. For example, a developer on a Windows laptop could open a shell on a Linux workstation, run a build there, and watch its output appear in command blocks on the laptop. The tab should show the remote computer’s name, shell, and working directory so the user can see where each command will run.

- **Local network.** Start with two computers on the same LAN. Pair the devices with approval on the host, open an encrypted session, and let the host disconnect it or revoke access.
- **Over the internet.** Extend the same paired-device experience to computers on different networks. A milestone is reconnecting to the workstation from another network, verifying its identity, and opening a remote tab.

## Debugging and sharing results

- **Markdown bug reports.** Select a failed block and the steps leading up to it. Preview the commands, shell, exit codes, and output, redact private values, then copy the report into a GitHub issue.
- **Structured output comparison.** Extend the current line diff with a `:diff` command for two selected blocks. Show changed JSON fields side by side and identify listeners added or removed between two `:ports` snapshots. Reordering JSON object keys should leave the comparison empty.
- **WebSocket inspector.** Open a connection with `:ws`, display incoming messages as they arrive, and send a test message while the server log is visible in another terminal.
- **DNS and latency tools.** Use `:dns` to inspect a hostname’s resolved addresses. Add a latency trace to help investigate slow connections.

## AI and MCP

- **Streaming AI responses.** Display the first tokens of an explanation in the requesting tab while the provider continues generating.
- **Remote MCP connections.** Add HTTP transport with support for streamed server events alongside the existing stdio client.
- **MCP server presets.** Let a user select a known server configuration in the MCP Hub and review its settings before connecting. A searchable server catalog could follow.
- **Multi-step MCP assistance.** Let the assistant use results from one tool call to choose the next. For example, it could inspect a failed command block, retrieve the previous run, and explain the differences. Route any proposed shell execution through the existing Safety Approval Gate.

## Distribution

Publish downloadable releases through GitHub Actions so testers can launch MRT without a Rust toolchain. Candidate formats are an executable or MSI for Windows, an app bundle or DMG for macOS, and an AppImage or Debian package for Linux. Verify each published package by launching MRT and running a command on its target OS.
