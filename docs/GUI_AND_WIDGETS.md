# In-Depth GUI & Widgets Guide (`mrt-app`)

[`crates/mrt-app`](../crates/mrt-app) is the binary crate (`mrt`) that renders the GPU-accelerated desktop interface using `eframe` / `egui` (`0.29`) and handles CLI flags (`--mcp-server`, `--version`, `--help`).

---

## 1. Non-Blocking Async Architecture & Live MCP Synchronization

Terminal commands, HTTP requests, web fetches, port scans, MCP tool calls, and AI queries must **never** block the 60fps GPU render thread.

`MrtGuiApp` uses a channel-driven architecture:
1. `MrtGuiApp` owns a multi-threaded `tokio::runtime::Runtime`, an `std::sync::mpsc::{Sender<AsyncWorkerEvent>, Receiver<AsyncWorkerEvent>}` for background worker events, and a `Receiver<GuiUiCommand>` for live commands sent by external MCP agents.
2. On startup, `MrtGuiApp` starts the **Live GUI Bridge** (`start_gui_bridge_listener`) on `127.0.0.1` so external `mrt --mcp-server` clients can connect to the running window.
3. On every frame, `MrtGuiApp::update()` calls:
   - `self.process_async_events()`: Handles streaming stdout/stderr chunks, completed `CommandBlock`s (persisted to SQLite history and mirrored to MCP state), AI command proposals, Explorer directory listings, and external MCP server registrations.
   - `self.process_mcp_ui_commands()`: Applies live MCP agent actions (`SyncFromMcp`, `SetInputDraft`, `SetMosaic`, `SetExplorer`, `SetComparison`) directly to the GUI state.
   - `self.sync_mcp_from_gui()`: Keeps `MrtMcpServerState` tabs, active tab ID, CWD, and blocks synchronized with user interactions in the GUI.

---

## 2. Key UI Components

### 2.1 Top Bar (`app.rs`)
- **Layout Toggles**: Toggle **Mosaic** (multi-terminal split grid with configurable columns and drag-and-drop reordering) and **Explorer** (left-hand directory browser synced to the active tab's CWD).
- **Multi-Tab Manager**: Switch between tabs, close tabs (`×`), or click **+ New tab** to open a new tab directly in any discovered shell (`Windows PowerShell`, `Command Prompt / Batch`, `Git Bash`, `WSL`, etc.).
- **Active Shell Switcher**: A live dropdown that changes the active tab's `ShellProfile` on the fly.
- **Right-Side Inspector Toggles**: Toggle buttons for **History** (SQLite-backed command/output search with `+ Runbook` quick-add), **Runbooks** (saved command sequences), **MCP Hub**, **Internet Studio**, and **Palette (`Ctrl+K`)**.

### 2.2 Command Block Renderer (`widgets/block_view.rs`)
Each `CommandBlock` is rendered inside a status-colored card frame (green border for `exit 0`, red border for non-zero exit codes):
- **Header Bar**: Shell badge (`pwsh`, `cmd`, `git-bash`, `wsl`, `mrt-http`, `mrt-net`, `mcp-tool`), command text, exit status & duration pill (`0 (42ms)`), **Compare with previous run**, **Rerun**, **Copy**, **Structured / Raw ANSI Toggle**, and **Diagnose Error**.
- **Specialized Viewers**:
  - `render_json_tree`: Recursive collapsible `egui::CollapsingHeader` tree with syntax-colored keys, strings, numbers, booleans, and nulls.
  - `render_structured_table`: Striped `egui::Grid` with a real-time row substring filter box.
  - `render_http_exchange`: Method/URL header, color-coded HTTP status & latency badge, collapsible response headers, and embedded JSON tree.
  - `render_markdown_block`: Styled `#` / `##` / `###` headings and bullet lists.
  - `render_ansi_spans`: Converts `Vec<AnsiSpan>` into an `egui::text::LayoutJob` with per-span foreground/background RGB colors, italics, and underlines.
  - `render_comparison`: Line-by-line unified diff (`similar::TextDiff`) comparing two `CommandBlock`s.

### 2.3 Side Panels (`widgets/side_panels.rs` & `widgets/runbooks.rs`)
- **MCP Hub Panel (`render_mcp_side_panel`)**: Displays the **Live GUI Bridge** status (`127.0.0.1:<port>`) with a one-click **Copy MCP Config** button, lists connected MCP servers (`mrt-native-mcp` + external `stdio` servers), expandable tool descriptions, custom JSON argument editor, one-click **Invoke Tool** buttons, and a form to connect new external `stdio` MCP servers.
- **Internet Studio Panel (`render_internet_side_panel`)**: Interactive GUI forms for the HTTP/REST Inspector, Readable Webpage Extractor, and Local Port Scanner.
- **Runbooks Panel (`render_runbooks_panel`)**: Offline SQLite-backed manager for saved command sequences (`Runbook` / `RunbookStep` in `history.rs`). Supports creating and editing runbooks, picking steps from command history (or adding manual steps), reordering steps (`^` / `v` / `x`), selecting per-step shell and working directory, previewing steps before execution, and replaying step-by-step via `ActiveRunbookReplay` (routing each step through the Safety Approval Gate, rendering each output as a `CommandBlock`, and stopping on the first non-zero exit code).

### 2.4 Modals (`app.rs`)
- **MRT Safety Approval Gate**: Centered modal window that intercepts `RiskLevel::Critical` commands, displays the risk classification, reason, target shell, and working directory, and requires explicit **Approve & Execute** or **Cancel Command**.
- **Universal Command Palette (`Ctrl+K` / `Ctrl+P`)**: Fuzzy-searchable modal to switch shell profiles or run built-in internet/AI actions from the keyboard.
