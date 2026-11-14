use crate::vt::{parse_ansi_text, AnsiSpan};
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
}

impl CommandBlock {
    pub fn new_completed(
        command: impl Into<String>,
        shell_name: impl Into<String>,
        cwd: impl Into<String>,
        raw_output: impl Into<String>,
        exit_code: i32,
        duration_ms: u64,
    ) -> Self {
        let raw_output = raw_output.into();
        let (ansi_spans, plain_output) = parse_ansi_text(&raw_output);
        let structured = classify_output(&plain_output);
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
        let (ansi_spans, plain_output) = parse_ansi_text(&raw_output);
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
