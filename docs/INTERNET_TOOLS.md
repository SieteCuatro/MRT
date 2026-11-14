# In-Depth Internet & Network Tools Guide (`mrt-tools`)

[`crates/mrt-tools`](../crates/mrt-tools) gives **Modern Rust Terminal (MRT)** native internet and network awareness so you don't have to switch to Postman, a browser, or external GUI utilities for common developer workflows.

---

## 1. Inline HTTP / REST API Inspector (`http_inspector.rs`)

Invoked from the prompt via `:http` or `:curl`, from the **🌐 Internet Studio** side panel, or via the `mrt_http_request` MCP tool.

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

Invoked via `:web <url>`, the **Internet Studio** side panel, or the `mrt_web_fetch` MCP tool.

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

Invoked via `:ports`, the **Internet Studio** side panel, or the `mrt_inspect_ports` MCP tool.

### Cross-Platform Implementation
- **Windows**: Runs `netstat -ano` (with `CREATE_NO_WINDOW`) and parses active TCP and UDP sockets via `parse_windows_netstat` (including localized listening state names), splitting the local endpoint into `Local Address` and `Port` alongside `Protocol`, `State`, and `PID`.
- **macOS**: Runs `/usr/sbin/lsof -nP -iTCP -sTCP:LISTEN -iUDP -F pPTn` and parses active sockets via `parse_lsof`.
- **Linux / other Unix**: Runs `ss -tulnp` and parses active sockets via `parse_unix_ss`.
- **Output**: Returns both a `StructuredOutput::Table` (rendered as a striped, live-filterable table in the GUI) and a raw CSV string (so you can copy the port list or consume it via MCP).

---

## 4. How Contributors Can Add a New Built-in `:tool`

Want to use your AI tokens to add `:dns`, `:whois`, `:ping`, or `:ws` (WebSocket inspector)?
1. Create a new module in [`crates/mrt-tools/src/`](../crates/mrt-tools/src) (for example, `dns_inspector.rs`) and export it in `crates/mrt-tools/src/lib.rs`.
2. Return either a `StructuredOutput::Table`, `StructuredOutput::Json`, or `StructuredOutput::Markdown` from your async function.
3. Wire the `:dns` prefix in [`crates/mrt-app/src/app.rs`](../crates/mrt-app/src/app.rs) inside `MrtGuiApp::submit_input` and optionally expose it as an MCP tool in [`crates/mrt-mcp/src/server.rs`](../crates/mrt-mcp/src/server.rs).
4. Add unit tests in `crates/mrt-tools` and run `cargo test --workspace`.
