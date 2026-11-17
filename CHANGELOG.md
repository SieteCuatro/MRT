# Changelog

## v0.2.0 — 2026-11-17

The first release with the whole [ROADMAP](ROADMAP.md): a real terminal core, persistent shells, sessions that outlive the window, remote access, and the features suggested by Ghostty, tmux and Rex. Details are in the [User Guide](docs/USER_GUIDE.md).

### Terminal core
- Full-screen programs (`vim`, `htop`, `lazygit`, `:tui …`) run on a real screen emulator (`alacritty_terminal`): xterm keys and mouse, bracketed paste, resizing, selection and copy, inside the overlay or a split pane, and read-only in Mosaic tiles.
- Each tab keeps one persistent shell with OSC 133 / OSC 7 integration (PowerShell, CMD, Bash, Git Bash, Zsh, Fish, sh): `cd`, variables and environments carry over; **Stop** interrupts without losing the shell.
- Inline images (iTerm2, kitty graphics, Sixel) in command blocks and full-screen programs.
- Find in output (`Ctrl+F`), copy mode with the keyboard (`Ctrl+Shift+Space`) in blocks and programs, clickable links and approved OSC 52 clipboard writes.

### Sessions and remote access
- **Session host**: tab shells, running commands, full-screen programs and agents' PTYs live in `mrt session-host` and survive the window closing or crashing; the next window reattaches (event streams, screen rebuilt).
- Closing the window with work running keeps MRT in the background; one instance per user; upgrades never cut running sessions; tabs, splits and folders are restored at start.
- `mrt list | send | capture | split | quick | quit` control the window from scripts; `mrt server` serves the CLI and agents headless.
- `mrt share` / `mrt pair` / `mrt attach` / `--host`: end-to-end encrypted remote access (SPAKE2 pairing code, Noise, paired keys, revocation, reconnect without losing output), and a phone companion web app (`mrt share --web`).
- `:ssh user@host` remote tabs with blocks, exit codes and folder tracking, nothing installed on the host.

### Agents and AI
- Tab activity (running, progress, waiting for input, finished, failed) including agent CLIs, and MRT's `OSC 678` status sequence.
- `mrt agent setup` registers MRT's MCP server and skill with Claude Code, Codex CLI and Gemini CLI.
- `:assist` multi-step investigations over MRT's tools (any provider, or offline), and `:sandbox` tabs where agents run only Safe commands.
- MCP host over Streamable HTTP, MCP screen reads of running programs.

### Keyboard, configuration and look
- Config file (`config.toml` + `.mrt.toml`, live reload), keybindings with chains and key tables, `[[actions]]`, `[[hooks]]`, `:broadcast` to all panes.
- Themes from files (MRT, Windows Terminal, Alacritty, Ghostty, iTerm2), a high-contrast theme, screen-reader text, configurable fonts with fallbacks, `mrt quick` for a quick terminal.

### Task Manager and tools
- `:ps`, `:top`, `:proc`, `:kill`, `:watch-proc`, `:suspend`, `:resume`, `:priority`, `:services`, `:service`, all through the Safety Gate where they change something.

### Performance
- Incremental parsing of streamed output, bounded output per block with tail rendering, 64 KiB reads, lighter MCP mirroring and history, near-zero repaint when idle, and an optional fast path for native programs in Windows PowerShell.

### Fixes
- OSC strings (images, OSC 52) are no longer cut at 1 KiB.
- Links, clipboard requests and images now reach blocks in persistent shells.
- ConPTY resize, Ctrl+C in programs, Alt+letter, find scrolling, pane focus, and more (see the merged pull requests #7–#37).

### Notes
- The Windows and Linux archives of this release were built locally (Windows 11, Ubuntu); the Linux binary needs a recent glibc. macOS builds come from the release workflow when GitHub Actions runs it.
