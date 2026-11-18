# Changelog

## Unreleased

### New
- **Icon**: `mrt.exe` has MRT's icon (the sidebar's `›_` mark) and version information in its file properties, the window and taskbar show it on Windows and Linux, and the Linux archive includes `mrt.desktop` and `mrt.png` for a menu entry. The icon is drawn by `scripts/icons/make_icon.py`.

### Notes
- The User Guide explains the Windows SmartScreen warning ("Windows protected your PC") that unsigned downloads show, and how to run MRT anyway.

## v0.3.0 — 2026-11-18

Agents working in parallel (Milestone 7 of the [ROADMAP](ROADMAP.md)): each agent in its own git worktree tab, a review step before its work lands, work queued to idle tabs, and the cost of AI requests.

### New
- **AI token use and cost**: every `#` suggestion and `:assist` answer shows the tokens it used and its estimated cost, the sidebar shows the session total, and `:ai-usage` lists them per tab. Prices come from a new `[ai.prices]` table in the config (dollars per million tokens, by model); local Ollama models are free, and models without a price still count their tokens. `mrt_ai_translate` returns the usage too.
- **Send work to an idle tab**: `mrt send --idle COMMAND` and `mrt_run_command` with `idle: true` run the command in a tab of the same git checkout that is not running anything (preferring the active tab), or open a new tab there when all are busy, so scripts and agents can queue work while another tab builds. Tabs where another agent's `mrt_run_command` is running count as busy.
- **Review before merge**: a worktree tab has a bar under the header with Review, Merge, Open PR and Discard (`:worktree review|merge|pr|discard`). Review shows the commits, files and colored diff against the branch it was created from, plus files not committed yet. Merge merges it in the main checkout, Open PR pushes it and opens a pull request with `gh`, and Discard deletes the folder and the branch; all three always ask in the Safety Gate first.
- **Safety Gate rules for git**: `git push`, `git merge` and `gh pr create` are Moderate (also as `git -C <folder> push`), `git branch -D` and `git worktree remove --force` are Critical.
- **Worktree tabs**: `:worktree <branch> [base]` opens a tab in a git worktree for the branch (in `<repo>.worktrees/<branch>`, next to the repository), so several agents can change the same repository at once without touching each other's files; `:worktree list` lists them. Agents get the same with `mrt_create_tab` and `"worktree": { "branch": … }`. Closing a worktree tab asks whether to remove the folder when it has no uncommitted changes; the branch and its commits always stay.

### Fixed
- A command an agent submits with `mrt_control_gui` (`input_text` + `submit_input`) no longer stays in the input box.

### Notes
- The CI and release workflows are still disabled; the Windows and Linux archives were built locally (Linux with `scripts/release/build-linux.sh`, glibc 2.17 or newer) and tested on Windows and Linux. There are no macOS archives.
- Open PR (`:worktree pr`) and the token counts of the cloud AI providers were not tried against the real services; their command lines and response formats are covered by tests.

## v0.2.2 — 2026-11-17

### Fixed
- **Linux binary on older distributions**: the Linux archive is now linked against glibc 2.17 (`scripts/release/build-linux.sh`, with cargo-zigbuild), so it runs on RHEL/CentOS 7, Ubuntu 14.04, Debian 8 and every newer distribution. v0.2.0 and v0.2.1 needed glibc 2.43. Tested headless on Ubuntu 14.04 (glibc 2.19) and with the window on Ubuntu 20.04 (glibc 2.31) and 26.04 (glibc 2.43).
- Leftover `mrt-session-*` folders in the temp directory (from sessions that ended in a crash or a killed process) are removed when they are more than 12 hours old.

### Notes
- The CI and release workflows are still disabled; the Windows and Linux archives were built locally (Linux with `scripts/release/build-linux.sh`) and tested on Windows and Linux. There are no macOS archives.
- The roadmap has a second list (Milestones 7–11) from a 2026 comparison with Warp, Ghostty, Kitty, Windows Terminal and Wave.

## v0.2.1 — 2026-11-17

### New
- **Kitty keyboard protocol**: programs that turn it on (Neovim, Helix, recent fish…) get kitty key codes for all five flags: disambiguated Esc and modified keys, repeat and release events, alternate keys, all keys as escape codes, and associated text.
- **Search a running program's scrollback**: in grid copy mode (`Ctrl+Shift+Space` inside a full-screen program), `/` searches the screen and its history, `n` / `N` step through matches, and moving past the edges or `PgUp` / `PgDn` scroll back through the history.
- **`mrt_pty_read` scrollback**: with `view: "screen"`, `scrollback_lines` also returns the lines above the screen.
- **`:agents`**: a window that shows agents' PTYs live, drawn like a terminal, read-only.

### Notes
- The CI and release workflows are disabled while there are no GitHub Actions minutes; this release was tested locally on Windows and Linux and has Windows and Linux archives built locally (the Linux binary needs glibc 2.43 or newer). There are no macOS archives.

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
- The Windows and Linux archives of this release were built locally (Windows 11, Ubuntu); the Linux binary needs glibc 2.43 or newer. There are no macOS archives: the release workflow is disabled while there are no GitHub Actions minutes; build from source on macOS.
