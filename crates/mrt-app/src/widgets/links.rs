//! Clickable links in command output: OSC 8 hyperlinks, plain-text URLs and
//! `path:line[:col]` locations. Ctrl-click (Cmd-click on macOS) opens URLs in
//! the browser and files in `$VISUAL` / `$EDITOR` (or the OS opener).

use crate::theme::Palette;
use egui::{Color32, Ui};
use mrt_core::{classify_uri, detect_links, is_openable_uri, AnsiSpan, LinkTarget};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Visual style for a run of ANSI spans.
pub struct SpanStyle {
    pub font_size: f32,
    pub backgrounds: bool,
}

/// A link resolved to a range of the laid-out text.
struct LinkRegion {
    /// Byte range into the job text.
    bytes: Range<usize>,
    /// Char range, as used by galley cursors.
    chars: Range<usize>,
    target: LinkTarget,
    /// Raw target text (URL or path) for tooltips.
    label: String,
}

/// What a Ctrl-click on a link asks the app to do.
pub enum LinkRequest {
    /// Run this command (used for terminal editors via `:tui`).
    RunCommand(String),
}

/// Human-readable name of the modifier used to open links.
pub fn open_modifier_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    }
}

/// Renders ANSI spans as one selectable label with clickable links.
/// `cwd` resolves relative file paths. Returns a request when a click needs
/// the app (terminal editors); everything else is handled here.
/// Find-in-output highlights for one block: the query, which of this block's matches
/// is the current one, and whether to scroll it into view this frame.
pub struct FindView<'a> {
    pub query: &'a str,
    pub current: Option<usize>,
    pub scroll: bool,
}

pub fn render_spans_with_links(
    ui: &mut Ui,
    spans: &[AnsiSpan],
    style: &SpanStyle,
    cwd: &str,
    error_id: egui::Id,
    find: Option<&FindView>,
) -> Option<LinkRequest> {
    let text: String = spans.iter().map(|s| s.text.as_str()).collect();
    let links = collect_links(spans, &text);
    let matches = find.map(|f| crate::find::match_ranges(&text, f.query)).unwrap_or_default();
    let current_match = find.and_then(|f| f.current).and_then(|n| matches.get(n).cloned());

    let mut job = egui::text::LayoutJob::default();
    let mut offset = 0;
    for span in spans {
        let span_end = offset + span.text.len();
        // Cut the span at link boundaries so links can be styled separately.
        let mut cuts = vec![offset, span_end];
        let link_bounds = links.iter().flat_map(|l| [l.bytes.start, l.bytes.end]);
        let match_bounds = matches.iter().flat_map(|m| [m.start, m.end]);
        for b in link_bounds.chain(match_bounds) {
            if b > offset && b < span_end {
                cuts.push(b);
            }
        }
        cuts.sort_unstable();
        cuts.dedup();
        for pair in cuts.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let in_link = links.iter().any(|l| l.bytes.start <= a && b <= l.bytes.end);
            let mut format = span_format(span, style, in_link);
            if matches.iter().any(|m| m.start <= a && b <= m.end) {
                let is_current = current_match.as_ref().is_some_and(|m| m.start <= a && b <= m.end);
                if is_current {
                    format.background = Palette::ACCENT_AMBER;
                    format.color = Palette::ON_ACCENT;
                } else {
                    format.background = Palette::ACCENT_AMBER.gamma_multiply(0.35);
                }
            }
            job.append(&text[a..b], 0.0, format);
        }
        offset = span_end;
    }
    job.wrap.max_width = ui.available_width();
    let galley = ui.fonts(|f| f.layout_job(job));
    let response = ui.add(egui::Label::new(galley.clone()));

    // Bring the current find match into view.
    if let (Some(range), Some(true)) = (&current_match, find.map(|f| f.scroll)) {
        let index = text[..range.start].chars().count();
        let cursor = galley.from_ccursor(egui::text::CCursor::new(index));
        let rect = galley.pos_from_cursor(&cursor).translate(response.rect.min.to_vec2());
        // Jump without animation: an animated scroll toward a match far up a long block
        // did not reach it.
        ui.scroll_to_rect_animation(rect.expand(24.0), Some(egui::Align::Center), egui::style::ScrollAnimation::none());
    }

    let mut request = None;
    if let Some(pos) = response.hover_pos() {
        let local = pos - response.rect.left_top();
        let cursor = galley.cursor_from_pos(local);
        let on_text = galley
            .rows
            .get(cursor.rcursor.row)
            .is_some_and(|row| row.rect.expand(1.0).contains(local.to_pos2()));
        let index = cursor.ccursor.index;
        let hovered = on_text
            .then(|| {
                links
                    .iter()
                    .find(|l| l.chars.start <= index && index < l.chars.end.max(l.chars.start + 1))
            })
            .flatten();
        if let Some(link) = hovered {
            let modifier = ui.input(|i| i.modifiers.command);
            let openable = match &link.target {
                LinkTarget::Url(url) => is_openable_uri(url),
                LinkTarget::File { .. } => true,
            };
            let hint = if openable {
                format!("{}+click to open {}", open_modifier_label(), link.label)
            } else {
                format!("{} (scheme not allowed)", link.label)
            };
            if modifier && openable {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let clicked = response.clicked() && modifier && openable;
            response.on_hover_text_at_pointer(hint);
            if clicked {
                match open_target(ui.ctx(), &link.target, cwd) {
                    Ok(Some(cmd)) => request = Some(LinkRequest::RunCommand(cmd)),
                    Ok(None) => {}
                    Err(err) => ui.ctx().data_mut(|d| d.insert_temp(error_id, err)),
                }
            }
        }
    }
    if let Some(err) = ui.ctx().data(|d| d.get_temp::<String>(error_id)) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("Could not open link: {err}"))
                    .size(11.5)
                    .color(Palette::ACCENT_RED),
            );
            if ui.small_button("✕").clicked() {
                ui.ctx().data_mut(|d| d.remove::<String>(error_id));
            }
        });
    }
    request
}

fn span_format(span: &AnsiSpan, style: &SpanStyle, in_link: bool) -> egui::TextFormat {
    let mut fg = span
        .fg
        .map(|c| Color32::from_rgb(c.r, c.g, c.b))
        .unwrap_or(Palette::TEXT_BODY);
    if in_link && span.fg.is_none() {
        fg = Palette::ACCENT_TEXT;
    }
    let background = match span.bg {
        Some(c) if style.backgrounds => Color32::from_rgb(c.r, c.g, c.b),
        _ => Color32::TRANSPARENT,
    };
    let mut format = egui::TextFormat {
        font_id: egui::FontId::monospace(style.font_size),
        color: fg,
        background,
        italics: span.italic,
        ..Default::default()
    };
    if span.underline || in_link {
        format.underline = egui::Stroke::new(1.0_f32, fg);
    }
    format
}

/// OSC 8 hyperlinks come from the spans; plain-text URLs and file locations
/// are detected in the remaining text.
fn collect_links(spans: &[AnsiSpan], text: &str) -> Vec<LinkRegion> {
    let mut regions: Vec<LinkRegion> = Vec::new();
    let mut offset = 0;
    for span in spans {
        let end = offset + span.text.len();
        if let Some(uri) = &span.link {
            match regions.last_mut() {
                // Adjacent spans of one hyperlink (different colors) merge.
                Some(last) if last.bytes.end == offset && &last.label == uri => {
                    last.bytes.end = end
                }
                _ => regions.push(LinkRegion {
                    bytes: offset..end,
                    chars: 0..0,
                    target: classify_uri(uri),
                    label: uri.clone(),
                }),
            }
        }
        offset = end;
    }
    for found in detect_links(text) {
        if regions
            .iter()
            .any(|r| r.bytes.start < found.range.end && found.range.start < r.bytes.end)
        {
            continue;
        }
        regions.push(LinkRegion {
            label: text[found.range.clone()].to_string(),
            bytes: found.range,
            chars: 0..0,
            target: found.target,
        });
    }
    for r in &mut regions {
        let start = text[..r.bytes.start].chars().count();
        r.chars = start..start + text[r.bytes.clone()].chars().count();
    }
    regions
}

/// Opens a link. Returns `Ok(Some(cmd))` when the app must run `cmd`
/// (a terminal editor inside the `:tui` overlay).
pub fn open_target(
    ctx: &egui::Context,
    target: &LinkTarget,
    cwd: &str,
) -> Result<Option<String>, String> {
    match target {
        LinkTarget::Url(url) => {
            if !is_openable_uri(url) {
                return Err(format!("refusing to open {url}: scheme not allowed"));
            }
            ctx.open_url(egui::OpenUrl::new_tab(url));
            Ok(None)
        }
        LinkTarget::File { path, line, column } => {
            let full = resolve_path(path, cwd);
            open_in_editor(&full, *line, *column)
        }
    }
}

fn resolve_path(path: &str, cwd: &str) -> PathBuf {
    let p = Path::new(path);
    let mut full = if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\"))
    {
        directories::BaseDirs::new()
            .map(|d| d.home_dir().join(rest))
            .unwrap_or_else(|| p.to_path_buf())
    } else if p.is_absolute() || cwd.is_empty() {
        p.to_path_buf()
    } else {
        Path::new(cwd).join(p)
    };
    if cfg!(windows) {
        full = PathBuf::from(full.to_string_lossy().replace('/', "\\"));
    }
    full
}

/// How an editor expects a line/column to be passed.
#[derive(Debug, PartialEq, Eq)]
enum EditorStyle {
    /// `code --goto path:line:col`
    VsCode,
    /// `subl path:line:col`
    PathColon,
    /// `vim +line path`
    PlusLine,
    /// `nano +line,col path`
    PlusLineComma,
    /// `micro +line:col path`, `emacs +line:col path`
    PlusLineColon,
    /// Just the path.
    Plain,
}

fn editor_style(program: &str) -> (EditorStyle, bool) {
    // Split on both separators: a Windows path (`C:\Tools\subl.exe`) in $EDITOR must be
    // recognised on any OS, and `Path` only splits on `\` on Windows.
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let stem = Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name)
        .to_ascii_lowercase();
    match stem.as_str() {
        "code" | "code-insiders" | "codium" | "vscodium" | "cursor" | "windsurf" => {
            (EditorStyle::VsCode, false)
        }
        "subl" | "sublime_text" | "zed" | "zeditor" | "mate" => (EditorStyle::PathColon, false),
        "hx" | "helix" => (EditorStyle::PathColon, true),
        "gvim" | "mvim" => (EditorStyle::PlusLine, false),
        "vim" | "vi" | "nvim" => (EditorStyle::PlusLine, true),
        "nano" => (EditorStyle::PlusLineComma, true),
        "micro" | "kak" | "kakoune" => (EditorStyle::PlusLineColon, true),
        "emacs" | "emacsclient" => (EditorStyle::PlusLineColon, false),
        _ => (EditorStyle::Plain, false),
    }
}

/// Builds the editor arguments for `path` at `line:col`.
fn editor_args(
    style: &EditorStyle,
    path: &str,
    line: Option<u32>,
    column: Option<u32>,
) -> Vec<String> {
    let loc = match (line, column) {
        (Some(l), Some(c)) => format!("{path}:{l}:{c}"),
        (Some(l), None) => format!("{path}:{l}"),
        _ => path.to_string(),
    };
    match (style, line) {
        (EditorStyle::VsCode, Some(_)) => vec!["--goto".into(), loc],
        (EditorStyle::PathColon, _) => vec![loc],
        (EditorStyle::PlusLine, Some(l)) => vec![format!("+{l}"), path.into()],
        (EditorStyle::PlusLineComma, Some(l)) => {
            vec![format!("+{l},{}", column.unwrap_or(1)), path.into()]
        }
        (EditorStyle::PlusLineColon, Some(l)) => {
            vec![format!("+{l}:{}", column.unwrap_or(1)), path.into()]
        }
        _ => vec![path.into()],
    }
}

/// Splits an `$EDITOR` value into words, honouring double/single quotes.
fn split_command_line(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has_word = false;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                has_word = true;
            }
            None if c.is_whitespace() => {
                if has_word {
                    words.push(std::mem::take(&mut cur));
                    has_word = false;
                }
            }
            None => {
                cur.push(c);
                has_word = true;
            }
        }
    }
    if has_word {
        words.push(cur);
    }
    words
}

fn configured_editor() -> Option<Vec<String>> {
    ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .map(|v| split_command_line(&v))
        .find(|w| !w.is_empty())
}

fn open_in_editor(
    path: &Path,
    line: Option<u32>,
    column: Option<u32>,
) -> Result<Option<String>, String> {
    let path_str = path.to_string_lossy().to_string();
    let Some(words) = configured_editor() else {
        return open_with_os(&path_str);
    };
    let (program, base_args) = words.split_first().expect("non-empty editor command");
    let (style, terminal) = editor_style(program);
    let terminal = terminal
        || (matches!(style, EditorStyle::PlusLineColon)
            && base_args
                .iter()
                .any(|a| matches!(a.as_str(), "-nw" | "-t" | "-tty" | "--tty")));
    let args = editor_args(&style, &path_str, line, column);
    if terminal {
        // Terminal editors need a TTY: run them in the full-screen TUI overlay.
        let quoted: Vec<String> = std::iter::once(program.clone())
            .chain(base_args.iter().cloned())
            .chain(args)
            .map(|a| {
                if a.contains(char::is_whitespace) {
                    format!("\"{a}\"")
                } else {
                    a
                }
            })
            .collect();
        return Ok(Some(format!(":tui {}", quoted.join(" "))));
    }
    let mut cmd = Command::new(resolve_program(program));
    cmd.args(base_args).args(&args);
    spawn_detached(cmd).map_err(|e| format!("{program}: {e}"))?;
    Ok(None)
}

fn open_with_os(path: &str) -> Result<Option<String>, String> {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = Command::new("explorer.exe");
        c.arg(path);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(path);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(path);
        c
    };
    cmd.current_dir(std::env::temp_dir());
    spawn_detached(cmd).map_err(|e| e.to_string())?;
    Ok(None)
}

/// On Windows, `code`/`subl` are usually `.cmd` shims that `Command` does not
/// find by bare name, so look them up on `PATH` with the usual extensions.
fn resolve_program(program: &str) -> PathBuf {
    let p = PathBuf::from(program);
    if !cfg!(windows) || p.extension().is_some() || p.components().count() > 1 {
        return p;
    }
    let Some(path_var) = std::env::var_os("PATH") else {
        return p;
    };
    for dir in std::env::split_paths(&path_var) {
        for ext in ["exe", "cmd", "bat", "com"] {
            let candidate = dir.join(format!("{program}.{ext}"));
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    p
}

fn spawn_detached(mut cmd: Command) -> std::io::Result<()> {
    use std::process::Stdio;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_arguments_per_style() {
        let p = "/src/main.rs";
        let (vs, t) = editor_style("code");
        assert!(!t);
        assert_eq!(
            editor_args(&vs, p, Some(3), Some(4)),
            ["--goto", "/src/main.rs:3:4"]
        );
        let (vim, t) = editor_style("/usr/bin/nvim");
        assert!(t);
        assert_eq!(editor_args(&vim, p, Some(3), Some(4)), ["+3", p]);
        let (nano, _) = editor_style("nano");
        assert_eq!(editor_args(&nano, p, Some(3), None), ["+3,1", p]);
        let (subl, _) = editor_style("C:\\Tools\\subl.exe");
        assert_eq!(editor_args(&subl, p, Some(3), None), ["/src/main.rs:3"]);
        let (plain, _) = editor_style("notepad");
        assert_eq!(editor_args(&plain, p, Some(3), None), [p]);
        assert_eq!(editor_args(&vs, p, None, None), [p]);
    }

    #[test]
    fn splits_editor_command_lines() {
        assert_eq!(split_command_line("code -w"), ["code", "-w"]);
        assert_eq!(
            split_command_line("\"C:\\Program Files\\Editor\\ed.exe\" --new"),
            ["C:\\Program Files\\Editor\\ed.exe", "--new"]
        );
        assert!(split_command_line("   ").is_empty());
    }

    #[test]
    fn collects_osc8_and_detected_links_without_overlap() {
        let spans = vec![
            AnsiSpan {
                text: "see ".into(),
                ..Default::default()
            },
            AnsiSpan {
                text: "docs".into(),
                link: Some("https://a.dev".into()),
                ..Default::default()
            },
            AnsiSpan {
                text: " or ünï src/lib.rs:4 https://b.dev".into(),
                ..Default::default()
            },
        ];
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        let links = collect_links(&spans, &text);
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].label, "https://a.dev");
        assert_eq!(links[0].chars, 4..8);
        assert_eq!(links[1].label, "src/lib.rs:4");
        let c = &links[1].chars;
        assert_eq!(
            text.chars().skip(c.start).take(c.len()).collect::<String>(),
            "src/lib.rs:4"
        );
        assert_eq!(links[2].target, LinkTarget::Url("https://b.dev".into()));
    }

    #[test]
    fn relative_paths_resolve_against_cwd() {
        let resolved = resolve_path("src/main.rs", "/work/proj");
        assert!(resolved.ends_with(Path::new("main.rs")));
        assert!(resolved.to_string_lossy().contains("proj"));
    }
}
