//! Link detection for command output: plain-text URLs and `path:line[:col]`
//! locations (compiler errors, stack traces, `grep -n`), plus helpers to
//! classify OSC 8 hyperlink targets.

use serde::{Deserialize, Serialize};
use std::ops::Range;

/// What a link points at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkTarget {
    /// A URL to hand to the browser / OS opener.
    Url(String),
    /// A local file, optionally at a line and column (both 1-based).
    File {
        path: String,
        line: Option<u32>,
        column: Option<u32>,
    },
}

/// A link found in plain text. `range` is a byte range into the scanned text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedLink {
    pub range: Range<usize>,
    pub target: LinkTarget,
}

/// URL schemes MRT is willing to open. Anything else (for example
/// `javascript:` or OS-specific handler schemes) is shown but not opened.
pub const OPENABLE_SCHEMES: &[&str] = &["http", "https", "ftp", "mailto", "file"];

/// Schemes recognised when scanning plain text for URLs.
const DETECTED_SCHEMES: &[&str] = &["https://", "http://", "ftp://", "file://"];

/// Returns the lowercase scheme of `uri` (the part before `:`), if it has one.
pub fn uri_scheme(uri: &str) -> Option<String> {
    let (scheme, _) = uri.split_once(':')?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic()
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    Some(scheme.to_ascii_lowercase())
}

/// True when `uri` uses a scheme listed in [`OPENABLE_SCHEMES`].
pub fn is_openable_uri(uri: &str) -> bool {
    uri_scheme(uri).is_some_and(|s| OPENABLE_SCHEMES.contains(&s.as_str()))
}

/// Classifies an OSC 8 hyperlink target: `file://` URIs become [`LinkTarget::File`]
/// (so they open in the editor), everything else stays a URL.
pub fn classify_uri(uri: &str) -> LinkTarget {
    if let Some(rest) = uri.strip_prefix("file://") {
        if let Some(slash) = rest.find('/') {
            let mut path = percent_decode_lossy(&rest[slash..]);
            let b = path.as_bytes();
            if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
                path.remove(0);
            }
            // Some tools append `#L12` or `:12` to file links.
            if let Some((p, line)) = path.rsplit_once("#L").or_else(|| path.rsplit_once('#')) {
                if let Ok(line) = line.parse::<u32>() {
                    return LinkTarget::File { path: p.to_string(), line: Some(line), column: None };
                }
            }
            if let Some((p, line, column)) = split_line_col(&path) {
                return LinkTarget::File { path: p.to_string(), line: Some(line), column };
            }
            return LinkTarget::File { path, line: None, column: None };
        }
    }
    LinkTarget::Url(uri.to_string())
}

fn percent_decode_lossy(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(v) = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Finds URLs and `path:line[:col]` locations in `text`.
pub fn detect_links(text: &str) -> Vec<DetectedLink> {
    let mut links = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                if let Some(link) = scan_token(&text[s..i], s) {
                    links.push(link);
                }
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    links
}

fn scan_token(token: &str, offset: usize) -> Option<DetectedLink> {
    // URL anywhere in the token (`url=https://...`, `<https://...>`).
    let url_start = DETECTED_SCHEMES
        .iter()
        .filter_map(|scheme| find_ascii_case_insensitive(token, scheme))
        .min();
    if let Some(at) = url_start {
        let url = trim_url_end(&token[at..]);
        let scheme_len = url.find("://").map(|p| p + 3).unwrap_or(0);
        if url.len() > scheme_len {
            return Some(DetectedLink {
                range: offset + at..offset + at + url.len(),
                target: LinkTarget::Url(url.to_string()),
            });
        }
        return None;
    }

    // `path:line[:col]`, possibly wrapped in quotes or parentheses.
    let lead = token
        .find(|c: char| !matches!(c, '(' | '[' | '{' | '<' | '"' | '\'' | '`'))
        .unwrap_or(token.len());
    let candidate = &token[lead..];
    // Try each `:` left to right so `file.rs:12:text` (grep -n) works and the
    // drive colon in `C:\x.rs:3` is skipped.
    for (colon, _) in candidate.match_indices(':') {
        let path = &candidate[..colon];
        let after = &candidate[colon + 1..];
        let Some((line, line_len)) = leading_number(after) else { continue };
        if line == 0 || !looks_like_path(path) {
            continue;
        }
        let mut end = colon + 1 + line_len;
        let mut column = None;
        if let Some(rest) = candidate[end..].strip_prefix(':') {
            if let Some((col, col_len)) = leading_number(rest) {
                column = Some(col);
                end += 1 + col_len;
            }
        }
        return Some(DetectedLink {
            range: offset + lead..offset + lead + end,
            target: LinkTarget::File { path: path.to_string(), line: Some(line), column },
        });
    }
    None
}

/// Parses the digits at the start of `s` when they end the token or are
/// followed by a separator (`:`, `,`, `)`, ...), returning `(value, len)`.
fn leading_number(s: &str) -> Option<(u32, usize)> {
    let len = s.bytes().take_while(u8::is_ascii_digit).count();
    if len == 0 || len > 9 {
        return None;
    }
    let next = s[len..].chars().next();
    if next.is_some_and(|c| c.is_alphanumeric() || matches!(c, '.' | '/' | '\\' | '_' | '-')) {
        return None;
    }
    Some((s[..len].parse().ok()?, len))
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.len() > h.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// Drops trailing punctuation and unbalanced closing brackets from a URL.
fn trim_url_end(url: &str) -> &str {
    let mut end = url
        .find(|c: char| matches!(c, '"' | '\'' | '<' | '>' | '`') || c.is_control())
        .unwrap_or(url.len());
    loop {
        let s = &url[..end];
        let Some(last) = s.chars().last() else { break };
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' => true,
            ')' => s.matches('(').count() < s.matches(')').count(),
            ']' => s.matches('[').count() < s.matches(']').count(),
            '}' => s.matches('{').count() < s.matches('}').count(),
            _ => false,
        };
        if !drop {
            break;
        }
        end -= last.len_utf8();
    }
    &url[..end]
}

/// Splits `path:line` or `path:line:col` (line ≥ 1).
fn split_line_col(s: &str) -> Option<(&str, u32, Option<u32>)> {
    fn number(s: &str) -> Option<u32> {
        if s.is_empty() || s.len() > 9 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    }
    let (head, last) = s.rsplit_once(':')?;
    let last = number(last)?;
    if let Some((path, mid)) = head.rsplit_once(':') {
        if let Some(line) = number(mid) {
            if line >= 1 && !path.is_empty() {
                return Some((path, line, Some(last)));
            }
        }
    }
    if last >= 1 && !head.is_empty() {
        Some((head, last, None))
    } else {
        None
    }
}

/// Heuristic: does `path` look like a file path rather than a host, time or ratio?
fn looks_like_path(path: &str) -> bool {
    if path.contains("://") || path.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | '|' | '*' | '?' | '"')) {
        return false;
    }
    let file_name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    if file_name.is_empty() || file_name.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let has_separator = path.contains('/') || path.contains('\\');
    // A file extension that starts with a letter (`main.rs`, `app.tsx`), which
    // rules out IPs and version numbers like `127.0.0.1` or `1.2`.
    let has_extension = file_name
        .rsplit_once('.')
        .is_some_and(|(stem, ext)| {
            !stem.is_empty()
                && ext.len() <= 12
                && ext.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
        });
    has_extension || (has_separator && file_name.chars().any(|c| c.is_alphabetic()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(text: &str) -> Vec<(&str, LinkTarget)> {
        detect_links(text)
            .into_iter()
            .map(|l| (&text[l.range.clone()], l.target))
            .collect()
    }

    fn file(path: &str, line: u32, column: Option<u32>) -> LinkTarget {
        LinkTarget::File { path: path.into(), line: Some(line), column }
    }

    #[test]
    fn detects_urls_and_trims_punctuation() {
        let text = "see https://example.com/docs. Or (http://x.dev/a_(b)) and <https://y.io/q?a=1&b=2>, url=ftp://f.org/x";
        assert_eq!(
            found(text),
            vec![
                ("https://example.com/docs", LinkTarget::Url("https://example.com/docs".into())),
                ("http://x.dev/a_(b)", LinkTarget::Url("http://x.dev/a_(b)".into())),
                ("https://y.io/q?a=1&b=2", LinkTarget::Url("https://y.io/q?a=1&b=2".into())),
                ("ftp://f.org/x", LinkTarget::Url("ftp://f.org/x".into())),
            ]
        );
    }

    #[test]
    fn bare_scheme_is_not_a_link() {
        assert!(found("https:// nothing").is_empty());
    }

    #[test]
    fn detects_file_locations() {
        let text = "error --> src/main.rs:10:5\n  at foo (/home/me/app.js:42:7)\nC:\\work\\lib.rs:3: warning\ngrep: notes.md:12:hello";
        assert_eq!(
            found(text),
            vec![
                ("src/main.rs:10:5", file("src/main.rs", 10, Some(5))),
                ("/home/me/app.js:42:7", file("/home/me/app.js", 42, Some(7))),
                ("C:\\work\\lib.rs:3", file("C:\\work\\lib.rs", 3, None)),
                ("notes.md:12", file("notes.md", 12, None)),
            ]
        );
    }

    #[test]
    fn ignores_non_paths() {
        for text in [
            "12:30:45",
            "localhost:8080",
            "127.0.0.1:8080",
            "ratio 3:2",
            "path.rs:0",
            "plain words",
            "file.rs:",
            ":12",
        ] {
            assert!(found(text).is_empty(), "{text:?} -> {:?}", found(text));
        }
    }

    #[test]
    fn byte_ranges_survive_unicode() {
        let text = "ünïcode → https://ex.com/ö done";
        let links = detect_links(text);
        assert_eq!(&text[links[0].range.clone()], "https://ex.com/ö");
    }

    #[test]
    fn classifies_osc8_targets() {
        assert_eq!(classify_uri("https://a.b"), LinkTarget::Url("https://a.b".into()));
        assert_eq!(
            classify_uri("file://host/home/me/my%20file.rs"),
            LinkTarget::File { path: "/home/me/my file.rs".into(), line: None, column: None }
        );
        assert_eq!(classify_uri("file:///C:/x/y.rs:7:2"), file("C:/x/y.rs", 7, Some(2)));
        assert_eq!(classify_uri("file:///x/y.rs#L9"), file("/x/y.rs", 9, None));
    }

    #[test]
    fn only_safe_schemes_open() {
        assert!(is_openable_uri("https://a.b"));
        assert!(is_openable_uri("MAILTO:me@x.y"));
        assert!(!is_openable_uri("javascript:alert(1)"));
        assert!(!is_openable_uri("ms-settings:privacy"));
        assert!(!is_openable_uri("no scheme"));
    }
}
