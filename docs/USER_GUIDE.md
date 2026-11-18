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

The Linux binary is linked against glibc 2.17, so it runs on any x86_64 distribution with glibc 2.17 or newer: RHEL/CentOS 7 and later, Ubuntu 14.04 and later, Debian 8 and later, and current rolling distributions. The window needs X11 or Wayland and OpenGL (Mesa is enough); `mrt server`, `mrt --mcp-server` and the other command-line modes need neither. Releases up to v0.2.1 had a Linux binary that needed glibc 2.43; build from source or use a newer release on older systems.

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

The binary is `target/release/mrt` (`mrt.exe` on Windows). It needs the glibc version of the machine that built it or newer; to build a Linux binary that runs on older distributions too, use `scripts/release/build-linux.sh` (it links against glibc 2.17 with [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild)).

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
| `list`, `send`, `capture`, `split` | Control a running MRT window from another terminal or a script (see [Controlling MRT from the command line](#controlling-mrt-from-the-command-line)). Add `--host <name>` to run them on a paired computer. |
| `quick [show\|hide\|toggle]`, `quit` | Bring the window to the front or minimize it ([Quick terminal](#quick-terminal)); close MRT and stop what runs in it. |
| `share [--listen ADDR:PORT] [--web] [--no-pairing]`, `share --list`, `share --revoke NAME` | Let paired devices attach to this MRT, with an optional phone companion ([Remote connections](#remote-connections-mrt-share-mrt-pair-mrt-attach)). |
| `pair HOST[:PORT] [--name NAME]`, `attach HOST [-t TAB]` | Pair with a host running `mrt share`; follow and type into one of its tabs. |
| `server` | A headless session server for the CLI and agents, without a window. |
| `session-host` | The background process that keeps the tabs' shells and programs; the window starts it, you do not need to. |

### Closing the window, and upgrades

**One MRT per user.** Starting `mrt` while it already runs brings the open window to the front instead of opening a second one.

**Closing keeps running work alive.** If you close the window while commands, full-screen programs or agents' PTYs are running, MRT keeps running in the background (on Windows the window is hidden, elsewhere minimized) and a notification says so. Run `mrt` (or `mrt quick`) to get the same window back, with its tabs, finished blocks and live output. `mrt quit`, `:quit`, or closing the window when nothing runs, ends MRT. Turn this off with `terminal.keep_running_on_close = false`.

**The shells live in a separate process.** MRT runs tab commands in a **session host**, `mrt session-host`, which the window starts in the background (no console window) and which keeps each tab's persistent shell. If the window closes or crashes, the host's shells and running commands go on; the next window reattaches and shows those commands, running or finished, with all their output, in the same tabs (the saved layout keeps the tab ids). `cd`, variables and activated environments also carry over to the next window. Full-screen programs (`vim`, `htop`, `:tui`, programs in panes) and agents' PTYs run in the host too: after a crash or close, the next window puts each program back in its tab with its screen rebuilt. `mrt quit` stops the host as well; otherwise it exits on its own after 30 minutes with nothing running and no window. Turn the host off with `terminal.session_server = false`.

**Upgrades never cut running sessions.** When a new MRT version starts while an older one runs, it asks the older one to quit if it is idle (the new one then reopens the saved layout), and otherwise brings the older window to the front and leaves it running until its commands finish.

### Remote connections (`mrt share`, `mrt pair`, `mrt attach`)

Attach to an MRT on another computer (Windows, macOS or Linux), follow its tabs and run commands there.

1. On the host, with MRT running: `mrt share` (add `--web` for the phone companion). It listens on `0.0.0.0:7682` (`--listen ADDR:PORT` to change) and prints a **6-digit pairing code**.
2. On your other computer: `mrt pair <host>` and type the code. The host asks *Allow device "laptop"…? [y/N]*; answer `y`.
3. From then on: `mrt attach <host> [-t TAB]` follows the tab live (missed output is shown once when the connection comes back) and sends each line you type to the host tab as if typed there, so the host's Safety Gate still asks. `mrt list --host <host>`, `mrt send --host <host> …`, `mrt capture --host <host>` and `mrt split --host <host>` work as well.

Security: pairing uses the code with SPAKE2, so it is never sent; every connection is end-to-end encrypted with Noise (X25519, ChaCha20-Poly1305, BLAKE2s) and accepted only from paired device keys, never by IP address or name. `mrt share --list` shows paired devices, and `mrt share --revoke <name>` removes one (its open connections end at their next request). `--no-pairing` shares without accepting new devices. Over the internet the same works through any address the guest can reach (a forwarded port, a VPN such as Tailscale or WireGuard); there is no relay service.

**Phone companion.** `mrt share --web` also serves a small web app at `https://<host>:7683/` with a self-signed certificate: `mrt share` prints its SHA-256 fingerprint, which you can compare in the browser's certificate details before accepting it. Pair with the same code (and approval). The page lists the tabs with their activity, shows the selected tab's blocks live, has a command box, and shows the pending Safety Gate prompt with **Approve** / **Deny**. Approving from the phone only works with `safety.remote_approvals = true` (off by default, because any program that can reach MRT's local bridge could otherwise approve its own commands); every remote answer is recorded in the tab. Revoke a phone with `mrt share --revoke "web: <name>"`.

### Controlling MRT from the command line

These subcommands talk to the open MRT window through the same token-authenticated local bridge that MCP agents use. `TAB` is a tab index, UUID or title substring; without `-t` the active tab is used. Every subcommand accepts `--json` to print the raw JSON result.

| Command | What it does |
| :--- | :--- |
| `mrt list` | Lists the tabs: index (`*` marks the active one), title, shell, activity and folder. |
| `mrt send [-t TAB] [--shell S] [--cwd DIR] [--idle] [--] COMMAND...` | Runs `COMMAND` in the tab under the agent safety rule (Critical commands are refused), records a block there, prints its output and exits with the command's exit code. Everything after the first word of the command belongs to the command; use `--` when the command starts with `-`. With `--idle`, MRT picks a tab in the same git checkout as the folder you are in (or `--cwd`) that is not running anything, preferring the active tab, and opens a new tab there when they are all busy; the command runs in that tab's folder. Several scripts or agents can queue work this way without waiting for each other. |
| `mrt capture [-t TAB] [-n LINES] [-b BLOCKS]` | Prints the tab's last block (or the last `BLOCKS` blocks, each after a `$ command` line); `-n` keeps only the last lines of each. |
| `mrt split [-t TAB] h\|v\|off` | Splits the tab side by side (`h`), stacked (`v`), or removes the split. |
| `mrt server` | Runs a headless **session server**: it keeps tabs, command blocks and interactive PTYs (`mrt_pty_*`) alive between CLI and agent calls, without a window, until Ctrl+C. It does not start while a window is open; the window does not attach to it yet ([design](SESSION_SERVER.md)). |
| `mrt quick [show\|hide\|toggle]` | Brings the window to the front or minimizes it (see [Quick terminal](#quick-terminal)). |
| `mrt quit` | Closes MRT, including a window kept running in the background, and stops what still runs in it. |

Exit codes: `0` success, the command's own code for `send`, `2` wrong arguments, `69` no MRT window is running, `70` MRT reported an error (for example an unknown tab).

```bash
mrt send -t build cargo test && mrt capture -t build -n 20
```

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

Each tab keeps **one shell running** for its commands, so `cd`, variables, aliases, functions, and activated environments (`source .venv/bin/activate`, `conda activate`) carry over from one command to the next. **■ Stop** interrupts the running command (Ctrl+C, then its child processes) and keeps the shell. If you change folder another way (the Explorer), MRT moves the shell there before the next command.

A command runs in a fresh shell process instead, without that state, when the tab's shell is busy with another command, when the shell is WSL or Nushell, when its shell integration does not start (for example a profile that replaces the prompt), or when `terminal.persistent_shell = false`. In that mode MRT still tracks `cd`, `cd /d`, `Set-Location`, and `cd ~`.

Commands run in a terminal, so a program that waits for keyboard input (`python` with no script, `read`, a password prompt) waits until you press **■ Stop**. Run interactive programs with `:tui` instead.

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
| `:assist [question]` | Asks the assistant to investigate with MRT's read-only tools (blocks, tabs, ports, processes), choosing each next step from the last result. With no question it explains the latest failed command: it finds an earlier successful run of the same command, compares the two outputs and diagnoses. Each step appears in the block; the answer and any suggested fix appear in its AI panel, and **Apply fix** goes through the Safety Gate. Works offline; with an AI provider configured, the model drives the investigation (at most 6 tool calls). |
| `:worktree <branch> [base]` | Opens a **worktree tab**: a git worktree for the branch of the current repository, in `<repo>.worktrees/<branch>` next to it, with the tab named after the branch. A new branch starts from `base` (default: the current commit); an existing branch is checked out as it is; a branch that already has a worktree reuses it, and a tab already open on it is focused. Closing the tab asks whether to remove the worktree folder: with no uncommitted changes you can remove it or keep it (the branch and its commits always stay); with uncommitted changes the folder is kept. Use it to let several agents change the same repository at once without touching each other's files. |
| `:worktree list` | Lists the repository's worktrees (branch, folder, and whether a tab has it open). |
| `:worktree review` | In a worktree tab: how far the branch is ahead of and behind its base branch (the one it was created from), its commits, the files changed, files not committed yet, and the colored diff. Read-only. Also the **Review** button in the worktree bar under the header. |
| `:worktree merge` | In a worktree tab: merges the branch into its base in the main checkout (`git -C <repo> merge --no-ff --no-edit <branch>`). The main checkout must have the base branch checked out. Always asks in the Safety Gate first, even with the gate turned off. **Merge** button. |
| `:worktree pr` | In a worktree tab: pushes the branch to `origin` and opens a pull request into the base with GitHub's `gh` (title and body from the commits). Needs an `origin` remote and `gh`. Always asks in the Safety Gate first. **Open PR** button. |
| `:worktree discard` | In a worktree tab: closes the tab, deletes the worktree folder with any uncommitted changes, and deletes the branch with any commits not merged elsewhere. Rated Critical in the Safety Gate. **Discard** button. |
| `:sandbox [folder]` | Opens an **agent sandbox tab** rooted at the folder (default: the current one). Its commands always start in that folder, and every command the Safety Gate does not rate Safe asks for approval, even with `safety.enabled = false`. Agents calling `mrt_run_command` on it can only run Safe commands. |
| `:agents` | Opens (or closes) the **Agent terminals** window: the live screen of every PTY agents started with `mrt_pty_spawn`, drawn like a terminal, read-only. |
| `:broadcast [on\|off]` | In a split tab, sends keys typed into one pane's program to the program in the other pane too (both panes show a **broadcast** marker). With no argument it toggles. Commands typed in the prompt are not duplicated. |
| `:ssh <host> [ssh options]` | Opens a **remote tab**: its commands run on the host with blocks, exit codes and the remote folder (see [Remote tabs](#remote-tabs-ssh)). |

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

### Very long output

A block draws its **last 2,000 lines**. When there are more, a line above the output says how much is hidden, with **Show 2,000 more** and **Show all** (laying out millions of lines can take a while). Find in output (`Ctrl+F`) still searches the whole block and reveals a hidden match when you go to it. Copy and the MCP tools also see the whole block.

Each block keeps at most `terminal.max_block_output_mb` (32 MiB by default) of output; beyond that the oldest part is dropped and the block shows how much was not kept. Output over 4 MiB is not checked for JSON, CSV or Markdown and is shown as text. On Windows, Windows PowerShell itself passes native program output through at about 1 MiB/s (see [PERFORMANCE.md](PERFORMANCE.md)); use `cmd`, `pwsh` 7 or a POSIX shell for very large outputs.

### Images in output

Programs can show images in a command block with any of the three common protocols:

| Protocol | Example tools | Notes |
| :--- | :--- | :--- |
| iTerm2 `OSC 1337 ; File=…inline=1` | `imgcat`, `wezterm imgcat`, `chafa -f iterm` | PNG, JPEG and GIF (first frame). `width=` / `height=` in cells, `px` or `%` are honoured. |
| kitty graphics (`ESC _ G …`) | `chafa -f kitty`, `timg -pk` | Direct transmission only (`t=d`); PNG (`f=100`), RGB and RGBA, zlib (`o=z`), chunked data (`m=1`), and `a=t` + `a=p` placement. MRT never reads files or shared memory a program names, and does not answer `a=q` queries, so tools that require a reply (`kitty +kitten icat`) may wait or fall back. |
| Sixel (`ESC P … q`) | `img2sixel`, `chafa -f sixels`, `lsix` | Color registers in RGB or HLS, repeats and transparent backgrounds (`P2=1`). |

Images appear where they were printed, scaled down to the block's width and at most 600 points high. Images larger than 8192 pixels on a side, or payloads over 64 MiB, are refused with a short note in the output. Images are not saved in history. Full-screen programs (the TUI overlay and pane programs) show them too: an image appears at the cursor, takes the cells its size needs, and scrolls with the text; it is dropped when the program switches between the normal and alternate screen. After the scrollback limit is reached, older images may drift.

### Remote tabs (SSH)

`:ssh user@host` (any `ssh` options may follow, e.g. `-p 2222` or `-i ~/.ssh/key`) opens a tab whose commands run **on that host**, with the same command blocks, exit codes, Stop button and folder tracking as a local tab. MRT starts `ssh -tt -o BatchMode=yes` and types the shell integration into the remote Bash or Zsh as base64 `eval` lines; **nothing is written on the remote machine**. The tab runs `hostname` first, so its first block shows which machine it is on.

- Authentication must work without a password prompt (an SSH key or agent): MRT never types passwords into hidden sessions. For password logins, or a remote shell other than Bash or Zsh, run `:tui ssh user@host` for a plain interactive session.
- Full-screen programs typed in a remote tab (`vim`, `htop`, `less`…) open in the TUI overlay through `ssh -tt host 'cd <folder> && vim …'`.
- Commands in a remote tab never run on this computer. If the connection fails, the block explains why; the next command tries to connect again.
- The Explorer shows local folders only.

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
| `Ctrl+F` | Anywhere except a running program | Opens **Find in output** (the `find` action; rebindable). |
| `Ctrl+Shift+Space` | Anywhere except a running program | **Copy mode**: select output with the keyboard (the `copy_mode` action; rebindable). |
| `Enter` / `Shift+Enter` | Find bar | Next / previous match. |
| `Esc` | Find bar | Closes the find bar. |
| `Ctrl+click` (`Cmd+click` on macOS) | Link in block output | Opens a URL in the browser, or a `file:line[:col]` path in your editor. |

The Command Palette lists everything you can also reach with the mouse, so it is the quickest way to discover features. Its **Actions** section shows each action's current key binding, or its action name when it has none.

### Find in output

`Ctrl+F` opens a find bar above the active tab's blocks (both panes of a split tab). Type to search the output of every block: matches are highlighted, the current one in solid amber, and the bar shows "3 of 12". `Enter` and `Shift+Enter` (or the ▲ ▼ buttons) move between matches and scroll each one into view. Letters match regardless of case (ASCII); other characters must match exactly. Blocks shown in a structured view (JSON tree, table) are counted and scrolled to but not highlighted. Inside a running full-screen program, `Ctrl+F` goes to the program.

### Custom keybindings

Every shortcut above (except the fixed composer and palette keys) comes from a keymap you can change with `binding = action` pairs in the `[keybindings]` table of the config file. Changes apply as soon as you save the file:

```toml
[keybindings]
"cmd+t" = "new_tab"            # Cmd+T on macOS, Ctrl+T on Windows/Linux
"ctrl+shift+w" = "close_tab"
"ctrl+pgdn" = "next_tab"
"ctrl+a > c" = "new_tab"       # chain: Ctrl+A, then C
"ctrl+b" = "enter_table:pane"  # key table (mode)
"pane/h" = "split_horizontal"
"pane/v" = "split_vertical"
"ctrl+p" = "unbind"            # remove a default binding
```

**Binding syntax**

| Form | Example | Meaning |
| :--- | :--- | :--- |
| Chord | `ctrl+shift+t`, `alt+f5`, `ctrl+pgdn` | Modifiers (`ctrl`, `shift`, `alt`/`option`) joined with `+`, key last. |
| Primary modifier | `cmd+k` | `cmd` (also `super`, `meta`, `win`) is Command on macOS and Ctrl on Windows/Linux. |
| Chain | `ctrl+a > c` | Press `Ctrl+A`, then `C` within 1.5 seconds. A badge in the bottom-right corner shows the keys typed so far; `Esc` cancels. |
| Key table | `pane/h` | A binding that only works inside the key table `pane`. |

Key tables are modes, like tmux's prefix tables: bind a key to `enter_table:pane`, then the `pane/…` bindings stay active, so you can press several of them in a row. Keys the table does not bind are ignored, and global bindings still work. `Esc`, the `exit_table` action, or 5 seconds without a key press leaves the table.

Bindings are matched exactly first; a binding with no `shift`/`alt` also fires when those are held (so `Ctrl+Shift+K` still opens the palette), unless a more specific binding exists. Set an action to `unbind` to remove a default binding. A chain cannot start with the keys of another binding (`ctrl+a` and `ctrl+a > c` together); MRT keeps the first, skips the other, and lists the conflict under **Keybinding problems** in the Command Palette, along with unknown keys or actions.

**Actions**

| Action | Default | Does |
| :--- | :--- | :--- |
| `toggle_command_palette` | `Ctrl+K`, `Ctrl+P` | Opens or closes the Command Palette. |
| `exit_tui` | `Ctrl+Q` | Ends the Interactive TUI session (only while it is open). |
| `new_tab` / `close_tab` | — | Opens a tab with the current shell / closes the active tab. |
| `next_tab` / `previous_tab` | — | Cycles through tabs. |
| `split_horizontal` / `split_vertical` / `unsplit` | — | Same as `:split-h`, `:split-v`, `:unsplit`. |
| `clear_blocks` | — | Same as `:clear`. |
| `toggle_explorer`, `toggle_history`, `toggle_runbooks`, `toggle_mcp_hub`, `toggle_toolbox`, `toggle_mosaic` | — | Show or hide that panel. |
| `open_task_manager` | — | Opens the Toolbox on the Task Manager tab. |
| `enter_table:<name>` / `exit_table` | — | Enters / leaves a key table. |
| `copy_mode` | `Ctrl+Shift+Space` | Starts [copy mode](#copy-mode). |
| `toggle_broadcast` | — | Same as `:broadcast`: keys typed into one pane's program also go to the other pane. |
| `run:<name>` | — | Runs the `[[actions]]` entry `<name>` (see [Named actions](#named-actions)). |

Keys a binding consumes are not sent to the shell or TUI program; everything else passes through unchanged.

### Copy mode

Inside a full-screen program (`vim`, `htop`, `:tui …`), `Ctrl+Shift+Space` starts **grid copy mode** instead: a cursor over the program's screen that moves with `h` `j` `k` `l` or the arrows, `0` / `$` (line start / end), `g` / `G` and `PgUp` / `PgDn`; `v` starts a selection, `y` or `Enter` copies it (or the cursor's line), `Esc` leaves. While it is on, keys go to MRT, not the program. Moving past the top row (or `PgUp` / `PgDn`) scrolls through the program's scrollback. `/` starts a search: type the text and press `Enter` to jump to the newest match (the view scrolls back to it, and the top-right corner shows `/text (2 of 5)`); `n` goes to the next older match and `N` to the next newer one, wrapping around. Leaving copy mode returns to the live screen.

`Ctrl+Shift+Space` starts copy mode on the last line of the active tab's last block. A panel at the bottom shows the lines around the cursor and what is selected; the prompt does not receive keys while it is open.

| Key | Does |
| :--- | :--- |
| `↑` `↓` or `k` `j` | Previous / next line (moving past a block's first or last line goes to the next block) |
| `PgUp` `PgDn` | 20 lines up / down |
| `Home` `End` or `g` `G` | First / last line of the block |
| `[` `]` or `Shift+↑` `Shift+↓` | Previous / next block |
| `v` or `Space` | Start or end a selection (within one block) |
| `y` or `Enter` | Copy the selection, or the cursor line, and leave |
| `Y` | Copy the whole block output and leave |
| `c` | Copy the block's command and leave |
| `Esc` or `q` | Leave without copying |

### Named actions

`[[actions]]` entries in the config file add your own actions to the Command Palette. Each runs one or more command lines in the active tab: shell commands, or MRT commands such as `:split-h`. Several lines run one after another like a runbook from the Runbooks panel (each shell command waits for the previous one; the Safety Gate checks each). An action can have its own `key`, or be bound in `[keybindings]` as `run:<name>` (names are matched ignoring case, with `-` read as `_`).

```toml
[[actions]]
name = "tests"
title = "Run the test suite"
command = "cargo test --workspace"
key = "ctrl+alt+t"

[[actions]]
name = "dev-layout"
commands = [":split-h", "npm run dev"]
```

A project's `.mrt.toml` can add actions or replace the user file's action of the same name.

### Event hooks

`[[hooks]]` entries run a command in the background (through your default shell, without a window, stopped after 60 seconds) when something happens. Hooks never create blocks.

| `event` | When |
| :--- | :--- |
| `command_finished` | A command block finished with exit code 0. |
| `command_failed` | A command block finished with another exit code. |
| `cwd_changed` | A command changed the tab's folder. |
| `tab_opened` | A new tab was opened. |

`match` limits a hook to commands containing that text (case-insensitive), and `min_duration_secs` to commands that ran at least that long. The hook's environment has `MRT_EVENT`, `MRT_COMMAND`, `MRT_EXIT_CODE`, `MRT_DURATION_MS`, `MRT_CWD` and `MRT_TAB` (the tab title).

```toml
[[hooks]]
event = "command_failed"
match = "cargo"
min_duration_secs = 30
run = "notify-send \"MRT\" \"$MRT_COMMAND failed ($MRT_EXIT_CODE)\""
```

Hooks run whatever you put in your config, with your permissions; a project `.mrt.toml` can add hooks too, so check a project's file before starting MRT in it.

### Links and clipboard requests in output

- URLs, terminal hyperlinks (OSC 8) and locations such as `src/main.rs:10:5` are underlined. Hover to see the target, and `Ctrl+click` (`Cmd+click` on macOS) to open it. Only `http`, `https`, `ftp`, `mailto` and `file` links open.
- Files open in `$VISUAL`, else `$EDITOR`, at the right line. Terminal editors such as `vim` or `nano` open in the Interactive TUI overlay. With neither variable set, the file opens with your OS's default app.
- Relative paths are resolved against the directory the command reported with OSC 7, else the directory it started in.
- When a program asks to copy text to your clipboard (OSC 52, used by tmux, Neovim and others over SSH), the block shows the text and asks you to **Allow** or **Deny**. MRT never writes the clipboard without your approval, and never lets programs read it.

---

## 4. Interactive TUI Mode

Full-screen programs need a real terminal. MRT opens them in a full-screen PTY overlay when the command starts with `vim`, `vi`, `nvim`, `nano`, `htop`, `top`, `lazygit`, `lazydocker`, `less`, `man`, or `fzf`, or when you type `:tui <command>` for anything else (for example `:tui ssh myserver` or `:tui python`).

- The overlay is a real terminal screen: the program can move the cursor, use colors and the alternate screen, and set the window title. The screen is sized to the overlay in whole characters and follows the window when you resize it.
- Keys are sent the way xterm sends them: text, `Enter`, `Backspace`, `Tab` / `Shift+Tab`, `Esc`, arrows (application cursor mode when the program asks), `Home` / `End`, `Insert` / `Delete`, `PageUp` / `PageDown`, `F1`-`F12` (with Shift/Alt/Ctrl), `Ctrl+` any letter, and `Alt+` a letter or digit (sent as `Esc` then the key, as readline and emacs expect). Pasted text is wrapped in bracketed-paste markers when the program enables them.
- **Select and copy:** drag to select text (hold `Shift` while dragging when the program uses the mouse), then press `Ctrl+Shift+C`, or `Ctrl+C` while text is selected (`Cmd+C` on macOS). Without a selection, `Ctrl+C` is sent to the program as an interrupt. Typing clears the selection. `Ctrl+V` pastes.
- When the program turns on mouse support (for example vim with `:set mouse=a`, htop, lazygit), clicks, drags and the wheel are sent to it. Otherwise the wheel scrolls a full-screen program that asks for it (alternate-scroll mode) with arrow keys.
- Programs see `COLORTERM=truecolor` and `TERM_PROGRAM=MRT`, and `TERM=xterm-256color` on Linux and macOS. Git for Windows tools (vim, less) also need `LANG` (for example `LANG=C.UTF-8`) to read UTF-8 files; MRT does not set it.
- The overlay closes when the program exits, or when you press `Ctrl+Q` (the `exit_tui` action; rebindable).
- The session is saved as a normal command block in the tab: what the terminal showed (its history and main screen) with colors. A full-screen program such as vim leaves nothing behind once it exits, so its block is empty.

**In a split tab** (`:split`, `:split-v`), the program runs **inside the active pane** instead of the overlay, so you can keep using the other pane next to it. Click inside a pane's terminal (or its **Focus** button) to give it the keyboard; the other pane's program keeps running and its cursor shows as an outline. Each pane runs one program at a time; a second one in the same pane opens the overlay. When the program exits its block appears in that pane. Removing the split (`:unsplit`) ends the program in the second pane. While the active pane runs a program, the command input does not take keys; focus the other pane to run commands.

In **Mosaic**, a tab that is running a program shows that program's screen in its tile (read-only); open the tab to use it.

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
| `layout.json` | The window's tabs, splits, shells, folders and remote (SSH) tabs, saved while MRT runs and when it closes, and reopened at the next start with fresh shells (`terminal.restore_layout`). Command blocks are not restored; they are in `history.sqlite`. Delete the file to start with one tab. |
| `session-host.json` | Port, token and PID of the running session host (`0600` on macOS/Linux); removed when the host exits. |
| `remote.json` | This device's key pair for remote connections, the devices allowed to attach (`guests`) and the hosts it paired with (`hosts`). Written `0600` on macOS/Linux. Delete it to forget all pairings. |
| `companion-cert.pem`, `companion-key.pem` | The phone companion's self-signed TLS certificate and key. |
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
| `terminal.font_family` | platform default | Monospace font: a family name installed on this computer (`"JetBrains Mono"`, `"CaskaydiaCove Nerd Font"`) or a path to a `.ttf` / `.otf` / `.ttc` file. Family names are matched against the font file names in the system and user font folders, preferring the regular face. A font that is not found is reported in the config banner and the default is used. Applied live. |
| `terminal.font_fallback` | `[]` | Fonts tried, in order, for characters the main font lacks (Nerd Font icons, symbols, CJK), same syntax as `font_family`. Platform symbol fonts (Segoe UI Symbol, Apple Symbols, Noto Sans Symbols 2 / DejaVu Sans) are tried after them. Ligatures are not drawn. Applied live. |
| `terminal.session_server` | `true` | Run tab commands in the session host process, so shells and running commands survive the window closing or crashing. |
| `terminal.keep_running_on_close` | `true` | Closing the window while something runs keeps MRT running in the background; `mrt` brings the window back. |
| `safety.remote_approvals` | `false` | Let the phone companion answer the Safety Gate prompt. |
| `terminal.restore_layout` | `true` | Reopen the last session's tabs, splits, shells and folders at startup (fresh shells; nothing is run). Remote tabs reconnect on their first command. A shell that is no longer installed becomes the default shell, and a folder that no longer exists becomes the start folder. |
| `terminal.max_block_output_mb` | `32` | Output kept per command block, in MiB. Older output is dropped from the front and the block says how much; `0` keeps everything (up to 4096). Applies to commands started after the change. |
| `terminal.theme` | `graphite-calm` | `graphite-calm` (aliases `dark`, `default`), `high-contrast`, or a theme file: a path, or a name looked up next to the config file and in its `themes/` folder (`.toml` optional). See [Themes](#themes). Applied live. |
| `terminal.fast_native_output` | `false` | Windows only. A plain native program typed in a PowerShell tab (for example `cargo build`, with no pipes, variables or quotes, and not a PowerShell alias like `curl`) runs through `cmd.exe`, which passes large output about 20× faster than Windows PowerShell. Variables and functions defined in the PowerShell session do not apply to those commands. |
| `terminal.quick_dropdown`, `terminal.quick_height_percent` | `false`, `45` | With `true`, `mrt quick` shows the window as a drop-down across the top of the screen at this height. |
| `terminal.persistent_shell` | `true` | Run each tab's commands in one long-lived shell so state carries over (see [§2](#2-the-prompt)). `false` runs every command in a new shell process. Applied on reload; turning it off ends the running shells. |
| `terminal.scrollback_blocks`, `terminal.auto_detect_structured_output` | `500`, `true` | Reserved; parsed but not applied yet. |
| `safety.enabled` | `true` | `false` turns off approval prompts for commands you type and runbook steps. Blocked patterns still apply, and `:kill` still confirms. |
| `safety.require_approval_for_moderate` | `false` | `true` asks before every Moderate command, not only process-control ones. |
| `safety.require_approval_for_critical` | `true` | `false` runs Critical commands without asking. |
| `safety.blocked_patterns` | `[]` | Commands containing any of these strings (case-insensitive) are refused with an error block, and stop a runbook. |
| `ai.provider` | `"auto"` | `auto` tries providers in the [default order](#7-environment-variables). A name (`gemini`, `anthropic`, `openai`, `xai`/`grok`, `deepseek`, `openrouter`, `ollama`) tries that provider first; `offline` always uses the built-in engine. A provider is only used if its key is set. |
| `ai.model` | provider default | Model for the provider named in `ai.provider`. |
| `ai.ollama_endpoint` | `http://localhost:11434` | Ollama base URL. |
| `[[mcp.servers]]` | none | MCP servers to connect at startup (`name`, `command`, `args`, `enabled`, `transport = "stdio"`). Servers added on reload are connected; removing one takes effect after a restart. |
| `notifications.enabled`, `notifications.threshold_secs` | `true`, `10` | Desktop notification when a command runs at least this long while MRT or its tab is in the background ([Notifications for long commands](#notifications-for-long-commands)). Applied on load and on every reload; changes made in the Notifications window last until the next reload. |
| `[keybindings]` | none | `"binding" = "action"` overrides for the keymap (see [Custom keybindings](#custom-keybindings)). Applied on load and on every reload; problems are listed in the Command Palette. |

**Environment variables win over the config file.** API keys are read only from the environment (never put them in the file). `OLLAMA_MODEL` and `OLLAMA_HOST` override `ai.model` and `ai.ollama_endpoint` for Ollama; with `ai.provider = "ollama"` and `ai.model` set, Ollama works without `OLLAMA_MODEL`.

Safety settings apply to the GUI. Commands sent by MCP agents (`mrt_run_command`, `mrt_pty_*`) keep their fixed rule: Critical commands are always refused.

---

### Themes

`terminal.theme` restyles the whole window. Built in: `graphite-calm` (default) and `high-contrast` (black surfaces, white text, strong borders; for low vision). A theme file can be MRT's own TOML or an existing terminal theme:

| Format | Example file | What MRT reads |
| :--- | :--- | :--- |
| MRT TOML | `night.toml` | `[ui]` palette colors by name (`bg_base`, `bg_surface`, `text_primary`, `accent`, `accent_red`, …) and `[terminal]` `background`, `foreground`, `ansi = [16 colors]` |
| Windows Terminal | a scheme from `settings.json` (`{"name": .., "background": .., "black": .., "brightWhite": ..}`) | background, foreground, 16 colors |
| Alacritty | `[colors.primary]`, `[colors.normal]`, `[colors.bright]` | background, foreground, 16 colors |
| Ghostty | `palette = 0=#1d1f21` … `background = …` | background, foreground, 16 colors |
| iTerm2 | `.itermcolors` | background, foreground, `Ansi 0–15` |

```toml
# ~/.config/mrt/themes/night.toml   →   terminal.theme = "night"
[ui]
accent = "#7AA2F7"
[terminal]
background = "#1A1B26"
foreground = "#C0CAF5"
```

A theme that only sets terminal colors gets matching UI colors derived from them (surfaces step from the background towards the foreground; accents come from the ANSI blue, green, red and yellow); `[ui]` entries override any of them. The 16 ANSI colors apply to output printed after the change and to full-screen programs. Unknown color names and files that cannot be read are reported in the config banner.

### Accessibility

MRT's window is exposed to screen readers through AccessKit (Windows UI Automation, macOS Accessibility, Linux AT-SPI): buttons, inputs and command block output are labeled, and a full-screen program's grid reports its visible screen as text. `terminal.theme = "high-contrast"` and `terminal.font_size` help with low vision, and copy mode (`Ctrl+Shift+Space`) works without a mouse.

### Quick terminal

`mrt quick` brings the MRT window to the front, or minimizes it if it already is; it starts MRT when none is running. Bind it to a global hotkey in your OS: on Windows, a shortcut to `mrt.exe quick` with a **Shortcut key** (Properties → Shortcut); on macOS, a Shortcuts app action or `skhd`; on GNOME or KDE, a custom keyboard shortcut running `mrt quick`. With `terminal.quick_dropdown = true` the window opens across the top of the screen.

## 7. Environment Variables

| Variable | Purpose |
| :--- | :--- |
| `GEMINI_API_KEY`, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `XAI_API_KEY`, `DEEPSEEK_API_KEY`, `OPENROUTER_API_KEY` | AI provider keys, checked in this order (`ai.provider` in the config can move one to the front). See [AI Providers](AI_PROVIDERS_AND_AGENTS.md). |
| `MRT_CONFIG` | Path of the user configuration file, instead of the default location. |
| `OLLAMA_MODEL`, `OLLAMA_HOST` | Use a local Ollama model (default host `http://localhost:11434`). They override `ai.model` and `ai.ollama_endpoint`. |
| `MRT_MCP_BRIDGE_PORT`, `MRT_MCP_BRIDGE_TOKEN` | Point `mrt --mcp-server` at a specific GUI bridge instead of reading `mcp-bridge.json`. |
| `CODEX_HOME` | Where `mrt agent setup` finds Codex CLI's `config.toml` (default `~/.codex`). |
| `VISUAL`, `EDITOR` | Editor used when you `Ctrl+click` a `file:line` link (`VISUAL` wins). Examples: `code`, `subl`, `nvim`. |
| `RUST_LOG` | Log level for the GUI's stderr logs (default `info`), e.g. `RUST_LOG=debug`. |
| `MRT_DATA_DIR` | Use this folder for MRT's data files (history, layout, bridge, session host, pairing) instead of the platform's data folder: for portable installs, or to keep test runs apart. |

With no AI variables set, MRT uses its offline heuristic engine, so `#` prompts and **Diagnose** always work.

---

## 8. Troubleshooting

| Problem | What to check |
| :--- | :--- |
| A shell is missing from **+ New tab** | MRT lists only shells it finds on `PATH` (Git Bash also in `C:\Program Files\Git\bin` or `C:\Program Files (x86)\Git\bin`). Install it or add it to `PATH`, then restart MRT. |
| `command not found` for tools that work in your normal terminal (macOS/Linux) | MRT loads the login shell's `PATH` at startup. Make sure the tool's folder is added in your login profile (`~/.zprofile`, `~/.bash_profile`, `~/.profile`), not only in an interactive-only file. |
| A command waits forever | It is probably waiting for input. Press **■ Stop** on the block and rerun it with `:tui <command>`. |
| `export VAR=…` has no effect on the next command | The tab's shell is not persistent: WSL and Nushell tabs, a busy shell, or `terminal.persistent_shell = false` run each command in a new process. Combine them on one line (`export X=1 && ./run.sh`) or use a script. |
| Commands or programs from before a crash do not come back | `terminal.session_server` must be on, and the session host must still run (`session-host.json` in the data folder). It exits after 30 idle minutes and with `mrt quit`. |
| `mrt attach` / `--host` says the host does not accept this device | The host revoked it, or its pairing was deleted. Pair again with `mrt pair <host>` while the host runs `mrt share`. |
| The phone companion page shows a certificate warning | Expected: the certificate is self-signed. Compare its SHA-256 fingerprint with the one `mrt share --web` printed, then accept it. |
| A font or theme from the config is not used | The config banner names the font or theme it could not find. Use a family name installed on this computer, or a full path to the file. |
| A banner says "Config not applied" | Your `config.toml` or `.mrt.toml` has a TOML error; the banner names the file and line. MRT keeps the last good settings until you fix it. |
| The AI answer looks generic | No provider key is set, or the provider failed and MRT fell back to the offline engine. Check the provider name in the proposal card. |
| An MCP client cannot see the open GUI | Make sure the GUI is running and that the client starts the same `mrt` binary with `--mcp-server`. Use **Copy MCP Config** in the MCP Hub for the exact path. If you moved or rebuilt `mrt` elsewhere, run `mrt agent setup` again to update the path. |
| `mrt agent setup` says *skipped* | The agent already has an `mrt` MCP server or `mrt` skill that MRT did not write. MRT leaves it alone; rename or delete it, then run setup again. |
| No desktop notification appears | Check that **🔔 Notifications** is on and the command ran longer than the threshold. Use **Send test notification**. Windows: allow notifications in *Settings → System → Notifications* (they are listed under Windows PowerShell). macOS: they are listed under Finder in *System Settings → Notifications*. Linux: a notification daemon must be running (most desktops have one). |
| Linux build fails with missing `xcb` / `X11` / `udev` | Install the packages listed in [§1](#1-install). |

Bug reports are welcome: run `:report` on the failed block and paste the result (secrets are redacted, but review it first) into a GitHub issue.
