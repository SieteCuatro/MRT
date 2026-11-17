use crate::vt::{parse_ansi_rich, AnsiSpan, ParsedAnsi, VtEvent, VtStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewMode {
    Structured,
    RawAnsi,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpExchangeSummary {
    pub method: String,
    pub url: String,
    pub status_code: u16,
    pub status_text: String,
    pub duration_ms: u64,
    pub response_headers: BTreeMap<String, String>,
    pub body_raw: String,
    pub body_json: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StructuredOutput {
    RawText,
    Json(serde_json::Value),
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Markdown(String),
    HttpResult(HttpExchangeSummary),
}

impl StructuredOutput {
    pub fn badge_label(&self) -> &'static str {
        match self {
            StructuredOutput::RawText => "ANSI Text",
            StructuredOutput::Json(_) => "JSON Tree",
            StructuredOutput::Table { .. } => "Interactive Table",
            StructuredOutput::Markdown(_) => "Markdown View",
            StructuredOutput::HttpResult(_) => "HTTP Inspector",
        }
    }

    pub fn is_structured(&self) -> bool {
        !matches!(self, StructuredOutput::RawText)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandBlock {
    pub id: Uuid,
    pub command: String,
    pub shell_name: String,
    pub cwd: String,
    pub timestamp: DateTime<Local>,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub raw_output: String,
    pub plain_output: String,
    pub ansi_spans: Vec<AnsiSpan>,
    pub structured: StructuredOutput,
    pub view_mode: ViewMode,
    pub pinned: bool,
    pub ai_explanation: Option<String>,
    pub ai_suggested_fix: Option<String>,
    /// Last working directory the command reported via OSC 7, if any.
    #[serde(default)]
    pub reported_cwd: Option<String>,
    /// OSC 52 clipboard writes requested by the command, in output order.
    /// Never applied without the user's approval.
    #[serde(default)]
    pub clipboard_requests: Vec<ClipboardRequest>,
    /// Bytes of earlier output that were dropped to keep the block under the
    /// output limit ([`set_max_block_output_bytes`]); the block keeps the end.
    #[serde(default)]
    pub truncated_bytes: u64,
    /// Inline images in the output, anchored at byte offsets of `plain_output`.
    /// Not saved in history (they are rebuilt from `raw_output` when needed).
    #[serde(skip)]
    pub images: Vec<crate::images::InlineImage>,
}

/// Default limit for the output one block keeps (32 MiB of text).
pub const DEFAULT_MAX_BLOCK_OUTPUT_BYTES: usize = 32 * 1024 * 1024;

/// Output above this size is not checked for JSON / CSV / Markdown: parsing it
/// costs seconds and a table of millions of rows is not useful to look at.
pub const STRUCTURED_CLASSIFY_LIMIT: usize = 4 * 1024 * 1024;

static MAX_BLOCK_OUTPUT_BYTES: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_BLOCK_OUTPUT_BYTES);

/// Sets how much output each block keeps (`0` keeps everything). Applies to
/// blocks created or appended to afterwards; older blocks keep what they have.
pub fn set_max_block_output_bytes(bytes: usize) {
    MAX_BLOCK_OUTPUT_BYTES.store(bytes, Ordering::Relaxed);
}

/// The current per-block output limit in bytes (`0` means no limit).
pub fn max_block_output_bytes() -> usize {
    MAX_BLOCK_OUTPUT_BYTES.load(Ordering::Relaxed)
}

/// Bytes cut off the front of `text` so that what is left starts at a line
/// boundary near `text.len() - keep`. Returns the number of bytes removed.
pub(crate) fn keep_tail(text: &mut String, keep: usize) -> usize {
    if text.len() <= keep {
        return 0;
    }
    let mut cut = text.len() - keep;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    // Start at a line when one begins soon, so the kept part has no half line.
    if let Some(nl) = text[cut..].bytes().take(4096).position(|b| b == b'\n') {
        cut += nl + 1;
    }
    text.drain(..cut);
    cut
}

/// The user's answer to an OSC 52 clipboard write request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardDecision {
    Pending,
    Approved,
    Denied,
}

/// An OSC 52 clipboard write found in a block's output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardRequest {
    /// Selection targets as sent by the program (`c`, `p`, ...).
    pub selection: String,
    pub text: String,
    pub decision: ClipboardDecision,
}

/// Splits parsed OSC events into the last reported cwd and clipboard requests,
/// keeping the decisions already made for requests in `previous` (matched by
/// position, since output is only ever appended).
fn split_events(
    events: Vec<VtEvent>,
    previous: &[ClipboardRequest],
) -> (Option<String>, Vec<ClipboardRequest>) {
    let mut cwd = None;
    let mut requests = Vec::new();
    for event in events {
        match event {
            VtEvent::WorkingDirectory { path, .. } => cwd = Some(path),
            VtEvent::ClipboardWrite { selection, text } => {
                let decision = previous
                    .get(requests.len())
                    .filter(|p| p.text == text)
                    .map(|p| p.decision)
                    .unwrap_or(ClipboardDecision::Pending);
                requests.push(ClipboardRequest { selection, text, decision });
            }
        }
    }
    (cwd, requests)
}

impl CommandBlock {
    /// Re-parses `raw_output` after new output was appended: refreshes the
    /// spans, plain text, reported cwd and clipboard requests, keeping the
    /// user's earlier clipboard decisions.
    pub fn refresh_output(&mut self) {
        let parsed = parse_ansi_rich(&self.raw_output);
        self.ansi_spans = parsed.spans;
        self.plain_output = parsed.plain_text;
        let (cwd, requests) = split_events(parsed.events, &self.clipboard_requests);
        self.reported_cwd = cwd;
        self.clipboard_requests = requests;
    }

    /// Appends a streamed chunk: only the new text is parsed (`stream` keeps the
    /// parser state between chunks), so streaming costs O(n) instead of the O(n²)
    /// of [`refresh_output`](Self::refresh_output) after every chunk. Output over
    /// the block limit is dropped from the front.
    pub fn append_output(&mut self, chunk: &str, stream: &mut VtStream) {
        self.append_output_with_limit(chunk, stream, max_block_output_bytes());
    }

    fn append_output_with_limit(&mut self, chunk: &str, stream: &mut VtStream, max: usize) {
        self.raw_output.push_str(chunk);
        let delta = stream.feed(chunk);
        let base = self.plain_output.len();
        self.images.extend(delta.images.into_iter().map(|mut image| {
            image.anchor += base;
            image
        }));
        self.plain_output.push_str(&delta.plain_text);
        self.ansi_spans.extend(delta.spans);
        for event in delta.events {
            match event {
                VtEvent::WorkingDirectory { path, .. } => self.reported_cwd = Some(path),
                VtEvent::ClipboardWrite { selection, text } => self.clipboard_requests.push(ClipboardRequest {
                    selection,
                    text,
                    decision: ClipboardDecision::Pending,
                }),
            }
        }
        self.enforce_output_limit(max);
    }

    /// Drops the oldest output once the block holds more than the limit. It trims
    /// to three quarters of the limit, so the work happens once per quarter of the
    /// limit and not on every chunk.
    fn enforce_output_limit(&mut self, max: usize) {
        if max == 0 {
            return;
        }
        let keep = max / 4 * 3;
        if self.raw_output.len() > max {
            keep_tail(&mut self.raw_output, keep);
        }
        if self.plain_output.len() <= max {
            return;
        }
        let removed = keep_tail(&mut self.plain_output, keep);
        // Remove the same bytes from the spans (their texts concatenate to the plain text).
        let mut left = removed;
        let mut whole = 0;
        for span in self.ansi_spans.iter_mut() {
            if span.text.len() <= left {
                left -= span.text.len();
                whole += 1;
                if left == 0 {
                    break;
                }
            } else {
                span.text.drain(..left);
                break;
            }
        }
        self.ansi_spans.drain(..whole);
        self.images.retain_mut(|image| {
            let kept = image.anchor >= removed;
            image.anchor = image.anchor.saturating_sub(removed);
            kept
        });
        self.truncated_bytes += removed as u64;
    }

    /// Clipboard requests still waiting for the user's decision.
    pub fn pending_clipboard_requests(&self) -> usize {
        self.clipboard_requests
            .iter()
            .filter(|r| r.decision == ClipboardDecision::Pending)
            .count()
    }

    pub fn new_completed(
        command: impl Into<String>,
        shell_name: impl Into<String>,
        cwd: impl Into<String>,
        raw_output: impl Into<String>,
        exit_code: i32,
        duration_ms: u64,
    ) -> Self {
        let mut raw_output = raw_output.into();
        let max = max_block_output_bytes();
        let truncated_bytes = if max == 0 { 0 } else { keep_tail(&mut raw_output, max) as u64 };
        let ParsedAnsi { spans: ansi_spans, plain_text: plain_output, events, images } = parse_ansi_rich(&raw_output);
        let (reported_cwd, clipboard_requests) = split_events(events, &[]);
        let structured = if plain_output.len() <= STRUCTURED_CLASSIFY_LIMIT {
            classify_output(&plain_output)
        } else {
            StructuredOutput::RawText
        };
        let view_mode = if structured.is_structured() {
            ViewMode::Structured
        } else {
            ViewMode::RawAnsi
        };

        Self {
            id: Uuid::new_v4(),
            command: command.into(),
            shell_name: shell_name.into(),
            cwd: cwd.into(),
            timestamp: Local::now(),
            duration_ms,
            exit_code: Some(exit_code),
            raw_output,
            plain_output,
            ansi_spans,
            structured,
            view_mode,
            pinned: false,
            ai_explanation: None,
            ai_suggested_fix: None,
            reported_cwd,
            clipboard_requests,
            truncated_bytes,
            images,
        }
    }

    pub fn new_structured(
        command: impl Into<String>,
        shell_name: impl Into<String>,
        cwd: impl Into<String>,
        raw_output: impl Into<String>,
        structured: StructuredOutput,
        exit_code: i32,
        duration_ms: u64,
    ) -> Self {
        let raw_output = raw_output.into();
        let ParsedAnsi { spans: ansi_spans, plain_text: plain_output, events, images } = parse_ansi_rich(&raw_output);
        let (reported_cwd, clipboard_requests) = split_events(events, &[]);
        Self {
            id: Uuid::new_v4(),
            command: command.into(),
            shell_name: shell_name.into(),
            cwd: cwd.into(),
            timestamp: Local::now(),
            duration_ms,
            exit_code: Some(exit_code),
            raw_output,
            plain_output,
            ansi_spans,
            structured,
            view_mode: ViewMode::Structured,
            pinned: false,
            ai_explanation: None,
            ai_suggested_fix: None,
            reported_cwd,
            clipboard_requests,
            truncated_bytes: 0,
            images,
        }
    }
}

/// Automatically inspects plain terminal output to detect JSON, CSV/TSV tables, or Markdown.
pub fn classify_output(plain: &str) -> StructuredOutput {
    let trimmed = plain.trim();
    if trimmed.is_empty() {
        return StructuredOutput::RawText;
    }

    // 1. Check JSON object or array
    if (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
    {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return StructuredOutput::Json(val);
        }
    }

    // 2. Check CSV / TSV table (at least 2 lines with consistent delimiter `,` or `\t`)
    if let Some((headers, rows)) = try_parse_delimited_table(trimmed) {
        return StructuredOutput::Table { headers, rows };
    }

    // 3. Check Markdown content (headers, fenced code blocks, bullet lists)
    if looks_like_markdown(trimmed) {
        return StructuredOutput::Markdown(trimmed.to_string());
    }

    StructuredOutput::RawText
}

fn try_parse_delimited_table(text: &str) -> Option<(Vec<String>, Vec<Vec<String>>)> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.len() < 2 {
        return None;
    }

    for delim in [',', '\t'] {
        let header_cols: Vec<String> = lines[0]
            .split(delim)
            .map(|s| s.trim().trim_matches('"').to_string())
            .collect();
        if header_cols.len() < 2 || header_cols.iter().any(|h| h.is_empty()) {
            continue;
        }
        let mut rows = Vec::with_capacity(lines.len() - 1);
        let mut valid = true;
        for line in &lines[1..] {
            let cols: Vec<String> = line
                .split(delim)
                .map(|s| s.trim().trim_matches('"').to_string())
                .collect();
            if cols.len() != header_cols.len() {
                valid = false;
                break;
            }
            rows.push(cols);
        }
        if valid && !rows.is_empty() {
            return Some((header_cols, rows));
        }
    }

    None
}

fn looks_like_markdown(text: &str) -> bool {
    let mut md_signals = 0;
    for line in text.lines() {
        let l = line.trim_start();
        if l.starts_with("# ") || l.starts_with("## ") || l.starts_with("### ") {
            md_signals += 2;
        } else if l.starts_with("```") {
            md_signals += 2;
        } else if l.starts_with("- ") || l.starts_with("* ") {
            md_signals += 1;
        }
    }
    md_signals >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    fn streaming_block() -> CommandBlock {
        let mut block = CommandBlock::new_completed("cmd", "sh", "/", "", 0, 0);
        block.exit_code = None;
        block
    }

    #[test]
    fn appended_chunks_match_a_whole_parse() {
        let chunks = ["one \x1b[3", "2mgreen\x1b[0m\n\x1b]7;file:///srv/a", "pp\x07two\n", "\x1b]52;c;aGk=\x07"];
        let mut block = streaming_block();
        let mut stream = VtStream::new();
        for chunk in chunks {
            block.append_output_with_limit(chunk, &mut stream, 0);
        }
        let whole = CommandBlock::new_completed("cmd", "sh", "/", chunks.concat(), 0, 0);
        assert_eq!(block.plain_output, whole.plain_output);
        assert_eq!(block.raw_output, whole.raw_output);
        let text: String = block.ansi_spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, whole.plain_output);
        assert_eq!(block.reported_cwd.as_deref(), Some("/srv/app"));
        assert_eq!(block.clipboard_requests.len(), 1);
        assert_eq!(block.clipboard_requests[0].text, "hi");
        assert_eq!(block.truncated_bytes, 0);
    }

    #[test]
    fn streamed_output_keeps_the_tail_under_the_limit() {
        let mut block = streaming_block();
        let mut stream = VtStream::new();
        for i in 0..2000 {
            block.append_output_with_limit(&format!("\x1b[31mline {i:05}\x1b[0m\n"), &mut stream, 4096);
        }
        assert!(block.plain_output.len() <= 4096, "{}", block.plain_output.len());
        assert!(block.raw_output.len() <= 4096);
        assert!(block.plain_output.ends_with("line 01999\n"));
        assert!(block.plain_output.starts_with("line "), "starts at a line: {:?}", &block.plain_output[..20]);
        let text: String = block.ansi_spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, block.plain_output);
        let total: u64 = (0..2000).map(|i| format!("line {i:05}\n").len() as u64).sum();
        assert_eq!(block.truncated_bytes + block.plain_output.len() as u64, total);
    }

    #[test]
    fn keep_tail_cuts_at_a_line_and_a_char_boundary() {
        let mut text = "ééé\nabc\ndef\n".to_string();
        let removed = keep_tail(&mut text, 9);
        assert_eq!(text, "abc\ndef\n");
        assert_eq!(removed, "ééé\n".len());
        let mut short = "ok".to_string();
        assert_eq!(keep_tail(&mut short, 10), 0);
        assert_eq!(short, "ok");
    }

    #[test]
    fn block_collects_osc_events_and_keeps_decisions() {
        let mut block = CommandBlock::new_completed(
            "x",
            "bash",
            "/",
            "\x1b]7;file://h/srv/app\x07\x1b]52;c;b25l\x07",
            0,
            1,
        );
        assert_eq!(block.reported_cwd.as_deref(), Some("/srv/app"));
        assert_eq!(block.clipboard_requests.len(), 1);
        assert_eq!(block.clipboard_requests[0].text, "one");
        assert_eq!(block.pending_clipboard_requests(), 1);

        block.clipboard_requests[0].decision = ClipboardDecision::Denied;
        block.raw_output.push_str("\x1b]52;c;dHdv\x07\x1b]7;file:///tmp\x07");
        block.refresh_output();
        assert_eq!(block.reported_cwd.as_deref(), Some("/tmp"));
        assert_eq!(block.clipboard_requests[0].decision, ClipboardDecision::Denied);
        assert_eq!(block.clipboard_requests[1].text, "two");
        assert_eq!(block.pending_clipboard_requests(), 1);
    }
}
