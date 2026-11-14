# 🖥️ In-Depth GUI & Widgets Guide (`mrt-app`)

[`crates/mrt-app`](../crates/mrt-app) is the binary crate (`mrt`) that renders the GPU-accelerated desktop interface using `eframe` / `egui` (`0.29`) and handles CLI flags (`--mcp-server`, `--version`, `--help`).

---

## 1. Non-Blocking Async Architecture

Terminal commands, HTTP requests, web fetches, port scans, MCP tool calls, and AI queries must **never** block the 60fps GPU render thread.

`MrtGuiApp` uses a clean channel-driven architecture:
1. `MrtGuiApp` owns a multi-threaded `tokio::runtime::Runtime` and an `std::sync::mpsc::{Sender<AsyncWorkerEvent>, Receiver<AsyncWorkerEvent>}`.
2. When the user submits a command, clicks an MCP tool, or triggers an AI translation, `MrtGuiApp` increments `self.busy_count` and spawns an async task onto `self.runtime`.
3. On every frame, `MrtGuiApp::update()` calls `self.process_async_events()` via non-blocking `self.rx.try_recv()`:
   - `AsyncWorkerEvent::BlockCompleted`: Appends the finished `CommandBlock` to both the target `TerminalTab` and the in-process `MrtMcpServerState` (so MCP clients see live updates), and updates `tab.cwd` if `cd` was executed.
   - `AsyncWorkerEvent::AiCommandReady`: Pre-fills the tab's input prompt with the synthesized command and displays the AI explanation banner with an **▶ Execute Now** button.
   - `AsyncWorkerEvent::ExternalMcpConnected`: Registers a newly connected external `stdio` MCP server and its discovered tools in the **MCP Hub** panel.

---

## 2. Key UI Components

### 2.1 Top Bar (`app.rs`)
- **Multi-Tab Manager**: Switch between tabs, close tabs (`×`), or click **➕ New Shell Tab** to open a new tab directly in any discovered shell (`Windows PowerShell`, `Command Prompt / Batch`, `Git Bash`, `WSL`, etc.).
- **Active Shell Switcher**: A live dropdown that changes the active tab's `ShellProfile` on the fly.
- **Inspector Toggles**: Toggle buttons for **🔌 MCP Hub**, **🌐 Internet Studio**, and **⌘ Palette (`Ctrl+K`)**.

### 2.2 Command Block Renderer (`widgets/block_view.rs`)
Each `CommandBlock` is rendered inside a status-colored card frame (green border for `exit 0`, red border for non-zero exit codes):
- **Header Bar**: Shell badge (`pwsh`, `cmd`, `git-bash`, `wsl`, `mrt-http`, `mrt-net`, `mcp-tool`), command text, exit status & duration pill (`✓ 0 (42ms)`), **↻ Rerun**, **📋 Copy**, **⇄ Structured / Raw ANSI Toggle**, and **✨ Diagnose Error**.
- **Specialized Viewers**:
  - `render_json_tree`: Recursive collapsible `egui::CollapsingHeader` tree with syntax-colored keys, strings, numbers, booleans, and nulls.
  - `render_structured_table`: Striped `egui::Grid` with a real-time row substring filter box.
  - `render_http_exchange`: Method/URL header, color-coded HTTP status & latency badge, collapsible response headers, and embedded JSON tree.
  - `render_markdown_block`: Styled `#` / `##` / `###` headings and bullet lists.
  - `render_ansi_spans`: Converts `Vec<AnsiSpan>` into an `egui::text::LayoutJob` with per-span foreground/background RGB colors, italics, and underlines.

### 2.3 Side Panels (`widgets/side_panels.rs`)
- **🔌 MCP Hub Panel (`render_mcp_side_panel`)**: Lists connected MCP servers, connection status dots, server versions, expandable tool descriptions, custom JSON argument editor, one-click **▶ Invoke Tool** buttons, and a form to connect new external `stdio` MCP servers.
- **🌐 Internet Studio Panel (`render_internet_side_panel`)**: Interactive GUI forms for the HTTP/REST Inspector, Readable Webpage Extractor, and Local Port Scanner.

### 2.4 Modals (`app.rs`)
- **🛡 MRT Safety Approval Gate**: Centered modal window that intercepts `RiskLevel::Critical` commands, displays the risk classification, reason, target shell, and working directory, and requires explicit **✓ Approve & Execute** or **✗ Cancel Command**.
- **⌘ Universal Command Palette (`Ctrl+K` / `Ctrl+P`)**: Fuzzy-searchable modal to switch shell profiles or run built-in internet/AI actions from the keyboard.
