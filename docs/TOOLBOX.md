# Toolbox Guide: Network Tools & Task Manager (`mrt-tools`)

[`crates/mrt-tools`](../crates/mrt-tools) gives **Modern Rust Terminal (MRT)** built-in network and process tools, so you don't have to switch to Postman, a browser, Task Manager, Activity Monitor, or `htop` for common developer workflows.

## The Toolbox panel

Every tool is available from the prompt (`:http`, `:ps`, …), from MCP, and from the **Toolbox** side panel (sidebar → **Toolbox**, `:toolbox`, or the Command Palette). The panel has one tab per tool family and remembers the last tab you used:

| Tab | Tools |
| :--- | :--- |
| **Network** | HTTP inspector (`:http`), web reader (`:web`), listening ports (`:ports`), DNS & latency (`:dns`), WebSocket inspector (`:ws`) |
| **Task Manager** | Live process list with CPU/memory sparklines, tree view, MRT-owned filter, details, Stop / Force stop / Stop tree, watches (`:ps`, `:top`, `:proc`, `:kill`, `:watch-proc`) |

The tabs are linked: a PID in the Network tab's port list (or in any `:ports` / `:ps` table block) opens that process in the Task Manager, and the Task Manager's details list the ports the selected process listens on.

Open a specific tab with `:toolbox network` or `:toolbox task-manager`; `:toolbox close` hides the panel. MCP clients use `mrt_control_gui` with `"toolbox": "network" | "task_manager" | true | false`.

---

## 1. Inline HTTP / REST API Inspector (`http_inspector.rs`)

Invoked from the prompt via `:http` or `:curl`, from the **Toolbox** panel (**Network** tab), or via the `mrt_http_request` MCP tool.

### Syntax
```text
:http [METHOD] <url> [optional_json_body]
:curl [METHOD] <url> [optional_json_body]
```
- If `METHOD` is omitted, `GET` is used by default.
- If `http://` or `https://` is omitted, `https://` is automatically prepended.
- When a body is provided, `Content-Type: application/json` is automatically set.

### Examples
```text
:http GET https://httpbin.org/json
:http POST https://httpbin.org/post {"project":"MRT","free":true}
:http https://api.github.com/zen
```

### What It Captures (`HttpExchangeSummary`)
- `method` & normalized `url`
- `status_code` (`u16`) and canonical `status_text` (color-coded green for `2xx`, amber for `3xx`, red for `4xx/5xx`)
- Round-trip `duration_ms` measured around `reqwest` execution
- Sorted `BTreeMap<String, String>` of response headers (rendered in a collapsible header drawer)
- Automatic `serde_json::Value` parsing (`body_json`) so JSON API responses immediately render as an interactive collapsible tree inside the terminal block

---

## 2. Readable Web Page Extractor (`web_fetch.rs`)

Invoked via `:web <url>`, the **Toolbox** panel (**Network** tab), or the `mrt_web_fetch` MCP tool.

### How It Works
1. Fetches the target URL with `reqwest` using a clean `ModernRustTerminal/0.1` User-Agent.
2. If the response `Content-Type` is `application/json`, formats the JSON into a fenced Markdown block with status metadata.
3. If the response is HTML (`html_to_readable_markdown`):
   - Extracts `<title>` as the top-level `# Heading`.
   - Strips noisy non-content blocks (`<script>`, `<style>`, `<svg>`, `<noscript>`, `<head>`).
   - Converts structural HTML tags (`<h1>`..`<h3>`, `<li>`, `<p>`, `<br>`, `<pre>`) into Markdown syntax.
   - Strips remaining inline tags, decodes HTML entities (`&amp;`, `&lt;`, `&gt;`, `&quot;`, `&#39;`, `&nbsp;`), collapses excessive blank lines, and returns a clean `StructuredOutput::Markdown` block.

---

## 3. Local Network & Listening Port Scanner (`net_inspector.rs`)

Invoked via `:ports`, the **Toolbox** panel (**Network** tab), or the `mrt_inspect_ports` MCP tool.

### Cross-Platform Implementation
- **Windows**: Runs `netstat -ano` (with `CREATE_NO_WINDOW`) and parses active TCP and UDP sockets via `parse_windows_netstat` (including localized listening state names), splitting the local endpoint into `Local Address` and `Port` alongside `Protocol`, `State`, and `PID`.
- **macOS**: Runs `/usr/sbin/lsof -nP -iTCP -sTCP:LISTEN -iUDP -F pPTn` and parses active sockets via `parse_lsof`.
- **Linux / other Unix**: Runs `ss -tulnp` and parses active sockets via `parse_unix_ss`.
- **Output**: Returns both a `StructuredOutput::Table` (rendered as a striped, live-filterable table in the GUI) and a raw CSV string (so you can copy the port list or consume it via MCP).

---

## 4. DNS Resolution & Handshake Latency Tracer (`dns_inspector.rs`)

Invoked via `:dns <host_or_url>`, the **Toolbox** panel (**Network** tab), or the `mrt_inspect_dns` MCP tool.

### Syntax
```text
:dns <hostname>
:dns <hostname>:<port>
:dns https://example.com/path
```

### How It Works
1. Normalizes the input, stripping URL schemes (`http://`, `https://`) and paths, extracting hostname and target port (default `80` for http, `443` for https, or custom).
2. Measures DNS resolution latency using non-blocking asynchronous address lookup (`tokio::net::lookup_host`).
3. Classifies resolved addresses into IPv4 (A) and IPv6 (AAAA) records.
4. Concurrently probes TCP connection latency (`TcpStream::connect`) to all resolved IP endpoints.
5. Emits a `StructuredOutput::Table` containing Host, IP Address, Record Type, TCP Handshake Latency (ms), and Reachability Status, along with summary metrics.

---

## 5. WebSocket Inspector (`ws_inspector.rs`)

Invoked via `:ws <url> [optional_initial_message]`, the **Toolbox** panel (**Network** tab), or the `mrt_inspect_ws` MCP tool.

### Syntax
```text
:ws ws://localhost:8080/events
:ws wss://echo.websocket.events "Hello MRT"
:ws --timeout 5000 wss://stream.example.com
```

### What It Measures & Captures
- HTTP/101 Upgrade Handshake round-trip latency.
- Real-time incoming frames stream: Frame Type (`Text`, `Binary`, `Ping`, `Pong`, `Close`), Payload size, Timestamp, and elapsed offset (`+Xms`).
- Emits structured JSON summary (`WsInspectionResult`) and Markdown transcript with message payloads and connection status.

---

## 6. Task Manager & Process Tools (`process_inspector.rs`)

Available from the **Toolbox** panel (**Task Manager** tab), the prompt commands below, and the `mrt_list_processes`, `mrt_inspect_process`, `mrt_kill_process`, and `mrt_compare_process_snapshots` MCP tools.

### Cross-Platform Implementation
- Process data comes from the [`sysinfo`](https://crates.io/crates/sysinfo) crate, so Windows, macOS and Linux behave the same without spawning `ps` or `tasklist`. One `ProcessSampler` is shared by the GUI and the MCP server (`SharedProcessSampler`), because CPU % needs two samples at least 200 ms apart; the first sample waits for that interval.
- CPU % is a share of the whole machine (0–100), not per core.
- Stopping a process: graceful stop sends `SIGTERM` (Unix) or runs `taskkill /PID <pid>` (Windows, spawned with `CREATE_NO_WINDOW`); `--force` sends `SIGKILL` / `TerminateProcess`. Console programs on Windows often ignore the graceful request — the result says "still running (retry with --force)".
- Tree stops go deepest-first, then sweep up to three times for children that a parent spawned while it was being stopped (children keep their parent PID after it exits; start times guard against reused PIDs).

### Prompt Commands
```text
:ps [filter] [--sort cpu|mem|pid|name] [--limit N] [--tree] [--mrt]
:top [same options]          # live table block, refreshed every 2 s; :top stop or the block's Stop button ends it
:proc <pid>                  # details, child tree, ancestors, listening ports, working directory
:kill <pid|name>... [--force] [--tree]
:kill                        # with no arguments, opens the Task Manager
:watch-proc <pid|name> [--mem 2GB] [--cpu 90]
:watch-proc list | clear | stop <id>      (also :unwatch-proc <id|all>)
```
- `filter` matches the PID exactly, or the name / command line case-insensitively.
- `--mrt` keeps only processes MRT started (see below) and their children; combine with `--tree` to see what each tab launched.
- `:ps` and `:top` blocks are tables with the columns `PID, PPID, Name, CPU %, Memory, User, Status, MRT, Command`. PID and PPID cells are links to the Task Manager. The block's **Diff** button (or `:diff`) compares two snapshots.

### MRT-Aware View
MRT records the OS PID of the shell behind every running command block, of the full-screen TUI overlay, and of every MCP PTY (`mrt_pty_spawn`). The **MRT** column shows the owning tab and command (for example `Tab 1 · npm run dev › child`); MRT itself shows as `MRT (this app)` and its other children as `MRT child process`. Running blocks show a **■ Stop** button that force-stops the command's whole process tree without a prompt (it is your own command); the block then completes with the shell's exit status.

### Safety Approval Gate
Every `:kill`, Task Manager **Stop** button, and MCP `mrt_kill_process` request is resolved against a fresh snapshot (`plan_kill`) and always needs approval in the Safety Gate modal, which lists each target's name, PID and command line, the method, and how many child processes `--tree` adds.
- **Moderate**: processes MRT started, or other processes of the current user.
- **Critical**: OS-critical processes (`PROTECTED_PROCESS_NAMES`: `System`, `csrss`, `lsass`, `svchost`, `launchd`, `kernel_task`, `WindowServer`, `init`, `systemd`, …, and PIDs ≤ 4), MRT itself or one of its parent processes, processes owned by another user (elevated / root), and names that match several PIDs.
- MRT never stops its own process, even if approved.
- The same rules cover shell commands: `kill`, `pkill`, `killall`, `taskkill`, `tskill`, `Stop-Process` / `spps`, and `xkill` in command position of any pipeline segment now always open the Safety Gate. Targeting a protected name, `kill 1`, or `kill -1` is Critical; by-name kills (`pkill`, `killall`, `taskkill /IM`, `Stop-Process -Name`) are flagged as possibly stopping several processes.

### Watches
`:watch-proc` checks the selected processes every 2 seconds and posts a notice block in the tab when a process crosses `--mem` or `--cpu` (once per crossing; it re-arms after dropping below), when a new matching process starts (name watches), and when one exits. PID watches end when the process exits. Exit codes are only known for processes MRT started (their command block shows it).

### MCP Tools
| Tool | Description |
| :--- | :--- |
| `mrt_list_processes` | Filter, sort, tree, `only_mrt`, and `limit` (default 50); returns JSON with an `mrt_owner` per process. |
| `mrt_inspect_process` | Details, ancestors, descendants, listening ports and working directory of one PID. |
| `mrt_kill_process` | `pid` or `name`, `force`, `tree`. Critical plans are always blocked. With the GUI attached the request opens the Safety Gate and returns `pending_approval`; headless, only MRT-started processes can be stopped. |
| `mrt_compare_process_snapshots` | Diffs two `:ps` blocks by id, or the current processes against a baseline saved by the previous call. |

The `mrt://processes` resource returns the top 100 processes by CPU. An agent can diagnose "address already in use" end to end: read the failed block, find the port owner with `mrt_inspect_ports` + `mrt_inspect_process`, and ask the user (through the Safety Gate) to stop it.

---

## 7. Markdown Bug Reporter with Secret Token Redaction (`bug_reporter.rs`)

Invoked via the **Report** button on failed blocks, the `:report` or `:bug` commands, or the `mrt_generate_bug_report` MCP tool.

### Secret Redactor Engine
Before generating any report, `redact_sensitive_tokens()` scans all inputs and outputs using regular expressions and pattern matching to sanitize:
- GitHub Personal Access Tokens (`ghp_`, `gho_`, `github_pat_`)
- OpenAI API Keys (`sk-...`)
- Google AI / Gemini API Keys (`AIzaSy...`)
- AWS Access Key IDs (`AKIA...`)
- Authorization Bearer tokens (`Bearer ...`)
- Password fields in connection strings and JSON keys (`"password": "..."`)
- PEM private keys (`-----BEGIN PRIVATE KEY-----`)

### Output Format
Generates a GitHub-flavored Markdown issue template with:
- System environment (OS, architecture, MRT version)
- Failed command, shell kind, exit code, and execution duration
- Sanitized command stdout/stderr
- Preceding command history context leading up to the failure

---

## 8. Canonical Structured Diffs (`structured_diff.rs`)

Invoked via the **Diff** button on blocks, `:diff [block1] [block2]`, or the `mrt_compare_blocks` MCP tool.

### Features
- **JSON Key Canonicalization**: Parses JSON objects and sorts all nested keys alphabetically using `BTreeMap`. Reordering keys in a response produces zero spurious diffs.
- **JSON Path Deltas**: Pinpoints added, removed, and modified JSON paths between two executions.
- **Port Snapshot Deltas**: When comparing two `:ports` blocks, identifies newly opened sockets and closed sockets.
- **Process Snapshot Deltas**: When comparing two `:ps` / `:top` blocks, lists processes that started, exited, or grew by at least 10 MB and 20% of memory (matched by PID and name, since PIDs are reused).
- **Line-by-Line Unified Diff**: Integrates `similar::TextDiff` for raw textual differences.

---

## 9. How Contributors Can Add a New Built-in `:tool`

Want to use your AI tokens to add `:whois` or `:ping`?
1. Create a new module in [`crates/mrt-tools/src/`](../crates/mrt-tools/src) and export it in `crates/mrt-tools/src/lib.rs`.
2. Return either a `StructuredOutput::Table`, `StructuredOutput::Json`, or `StructuredOutput::Markdown` from your async function.
3. Wire the tool prefix in [`crates/mrt-app/src/app.rs`](../crates/mrt-app/src/app.rs) inside `MrtGuiApp::submit_input` and expose it as an MCP tool in [`crates/mrt-mcp/src/server.rs`](../crates/mrt-mcp/src/server.rs).
4. Add unit tests in `crates/mrt-tools` and run `cargo test --workspace`.
