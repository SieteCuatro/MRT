# Getting Started with MRT

A step-by-step tour for your first hour with **Modern Rust Terminal (MRT)**: install it, run commands, ask the AI for help, and find your way around. Each section links to the [User Guide](USER_GUIDE.md), the full reference, for the details. *¿Prefieres español? Lee la [Guía de inicio](GUIA_DE_INICIO.md).*

---

## 1. Install

Download the archive for your system from the [latest release](https://github.com/SieteCuatro/MRT/releases/latest).

**Windows (x64).** Unzip `mrt-<version>-x86_64-pc-windows-msvc.zip` and run `mrt.exe`. MRT is not code-signed yet, so the first start may show **"Windows protected your PC"**. Choose **More info → Run anyway**, or unblock the file once in PowerShell:

```powershell
Unblock-File .\mrt.exe
```

Put `mrt.exe` in a folder on your `PATH` if you want to type `mrt` in any terminal.

**Linux (x86_64).** The binary runs on any distribution with glibc 2.17 or newer (CentOS 7, Ubuntu 14.04 and later):

```bash
tar xzf mrt-<version>-x86_64-unknown-linux-gnu.tar.gz
./mrt
# Optional: a menu entry with the icon
cp mrt.desktop ~/.local/share/applications/
mkdir -p ~/.local/share/icons/hicolor/512x512/apps && cp mrt.png ~/.local/share/icons/hicolor/512x512/apps/
```

**macOS.** There are no prebuilt archives yet; [build from source](USER_GUIDE.md#build-from-source) with `cargo build --release -p mrt-app`.

## 2. The window

- **Sidebar (left)**: **+ New tab** (pick a shell), your tabs with a status dot (idle, running, waiting for input, failed), and the panels: Explorer, Runbooks, History, MCP Hub, Toolbox, Mosaic, Notifications and **Settings**.
- **Header (top)**: the tab's folder, a search box that opens the **Command Palette**, and the split buttons.
- **Output (middle)**: every command you run becomes a **block**.
- **Prompt (bottom)**: where you type. The selector under it switches the tab's shell (PowerShell, CMD, Bash, Zsh, WSL…).

## 3. Run your first commands

Type a command and press **Enter**:

```
git status
```

The output appears in a block with a ✔ or ✖, the exit code and the time it took. Each block has buttons:

- **Copy** copies the output, **■ Stop** stops a running command (the shell stays), and failed blocks get **Diagnose** (an explanation from the AI) and **Report**.
- Output that looks like JSON, CSV or Markdown is shown as a tree or a table you can filter.

Each tab keeps **one shell alive**, so `cd`, variables and activated environments (`venv`, `nvm`) carry over to the next command. **↑ / ↓** go through the tab's earlier commands.

Commands that start with `:` are MRT's own. Try a few:

```
:ports              listening ports and the process behind each one
:ps --sort mem      processes by memory
:http GET https://httpbin.org/json
:diff               compare the last two blocks
```

The [full list](USER_GUIDE.md#built-in-commands) is in the User Guide.

## 4. Find anything: the Command Palette

Press **Ctrl+K** (or Ctrl+P). The palette lists everything MRT can do: actions with their shortcuts, tools, process views, AI examples, the hosts in your `~/.ssh/config`, and your shells. Type a few letters and press **Enter** to run the first match. If nothing matches, Enter runs what you typed as a command.

Other searches:

- **Ctrl+F** finds text in every block of the tab.
- **Search history** (sidebar) searches every command you have run, with its output, across sessions.

## 5. Ask the AI

Start a line with **`#`** and describe what you want:

```
# find the 10 largest files in this folder
```

MRT writes a command for your current shell and shows it in a card: **Run** runs it (through the Safety Gate), **Dismiss** drops it. Nothing runs on its own.

- **Without any setup** MRT uses its offline engine, which knows common tasks.
- **With a provider** the answers are much better. Open **Settings** (**Ctrl+,**) → **API keys** and paste a key for Gemini, Anthropic, OpenAI, xAI, DeepSeek or OpenRouter; it is stored in your system's keyring, not in a file. Or run a local model with [Ollama](https://ollama.com) and set **AI provider** to `ollama`.
- On a failed block, **Diagnose** explains the error and suggests a fix.
- `:assist why did the build fail?` investigates with MRT's tools (it finds the failed block, its last good run, and compares them).
- Each answer shows the tokens it used and an estimated cost; `:ai-usage` sums them up per tab.

More in [AI Providers](AI_PROVIDERS_AND_AGENTS.md).

## 6. The Safety Gate

Before a risky command runs (`rm -rf`, `git push --force`, `DROP TABLE`, `kill`…), MRT shows what it does and why it is risky, and waits for **Run** or **Cancel**. Critical commands ask by default. You can tune it in **Settings → Safety Gate**, and `safety.blocked_patterns` in the config file refuses some commands outright.

## 7. Full-screen programs

`vim`, `nano`, `htop`, `less` and `lazygit` open in a full terminal screen over the window (any other program with `:tui <command>`). The mouse and the wheel work, a scrollbar on the right goes back through what scrolled off, and **Ctrl+Q** ends the session, which is kept as a block. In a split tab the program runs inside its pane instead.

## 8. Tabs, splits and windows

- **New tab**: **+ New tab** in the sidebar, or the `new_tab` action. Drag tabs in the sidebar to reorder them.
- **Split**: `:split-h` (side by side) or `:split-v` (stacked); drag the line between the panes to resize them, `:unsplit` to close the split.
- **Another window**: right-click a tab → **Move to new window**, or type `:window new`. The tab keeps its shell and whatever it is running. `:window main` brings it back; closing a window moves its tabs back to the main one.
- **Mosaic** (sidebar): every tab of the window on screen at once, in a grid.

MRT reopens your tabs, splits and windows at the next start.

## 9. Make it yours

**Settings** (**Ctrl+,**, or `:settings`) has a search box and covers the font, theme, default shell, Safety Gate, AI provider and model, notifications and session options. **Save** writes only what you changed into your config file, keeping your comments.

- **Themes**: `graphite-calm` (default), `high-contrast`, or a theme file from Windows Terminal, Alacritty, Ghostty or iTerm2.
- **Keyboard shortcuts**: the Settings window lists every shortcut and adds new ones (for example `ctrl+shift+t` → New tab), and warns when two clash.
- **The config file** is `%APPDATA%\mrt\config.toml` on Windows and `~/.config/mrt/config.toml` on Linux and macOS. A `.mrt.toml` in a project folder overrides it there. [`mrt.example.toml`](../mrt.example.toml) lists every setting; changes apply as soon as you save.
- **Update check**: turn on **Settings → Updates** to see a notice when a new release is out (nothing is downloaded).

## 10. Network, processes and the Toolbox

The **Toolbox** (sidebar) has a **Network** tab (HTTP requests, web pages as text, ports, DNS, WebSocket) and a **Task Manager**. The same tools work at the prompt:

```
:top                live process table
:proc 1234          details of one process
:kill node --tree   stop a process and its children (asks first)
:dns github.com     DNS records and connection times
```

See [Toolbox](TOOLBOX.md).

## 11. Remote machines

- `:ssh user@host` opens a tab whose commands run on that machine, with the same blocks and exit codes. It needs key or agent login; nothing is installed on the remote side. Your `~/.ssh/config` hosts are in the Command Palette under **SSH hosts**.
- `mrt share` lets another computer or your phone follow and drive your tabs over an encrypted connection; see [Remote connections](USER_GUIDE.md#remote-connections-mrt-share-mrt-pair-mrt-attach).

## 12. Coding agents

MRT works as an MCP server, so agents such as Claude Code, Codex CLI and Gemini CLI can open tabs, run commands (through the Safety Gate) and read the results:

```
mrt agent setup
```

registers MRT with the agents it finds (`--dry-run` shows what it would change). For agents working side by side, `:worktree feature/login` gives each one its own git worktree tab with Review, Merge and Open PR buttons. See the [MCP Guide](MCP_GUIDE.md).

## 13. Closing MRT

Closing the window while commands are running keeps them going in the background: run `mrt` again to get the same window back, or `mrt quit` (or `:quit`) to stop everything. With nothing running, closing the window just exits.

## Where next

- The [User Guide](USER_GUIDE.md): every command, shortcut, setting and file, plus [troubleshooting](USER_GUIDE.md#8-troubleshooting).
- The [Changelog](../CHANGELOG.md) for what each release added, and the [Roadmap](../ROADMAP.md) for what comes next.
- Found a bug? `:report` drafts an issue from the last failed block, with secrets removed.
