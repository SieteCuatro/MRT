//! Per-tab activity state: is the program in a tab running, showing progress,
//! waiting for the user, or finished?
//!
//! The [`ActivityTracker`] is fed three kinds of input:
//!
//! * **Block lifecycle events** from the GUI ([`ActivityTracker::command_started`] and
//!   [`ActivityTracker::command_finished`]).
//! * **Raw output bytes** ([`ActivityTracker::feed_output`]). The tracker scans them for
//!   the standard `OSC 9;4` progress sequence (ConEmu / Windows Terminal) and for MRT's own
//!   status sequence `OSC 678` (see [`MRT_STATUS_OSC`]), and remembers the last lines of
//!   plain text for the "waiting for input" heuristic.
//! * **Process names** of the running command's process tree
//!   ([`ActivityTracker::observe_process_names`]), so an agent CLI started through a wrapper
//!   script is still recognised.
//!
//! [`ActivityTracker::snapshot`] combines these into an [`ActivitySnapshot`]. Explicit
//! signals (OSC 678, then OSC 9;4) always win over heuristics. The "waiting for input"
//! heuristic is deliberately conservative: it needs a running command, a quiet period with
//! no output, and a last line that reads like a question or an approval prompt.
//!
//! ## The MRT status sequence
//!
//! ```text
//! ESC ] 678 ; status=<value> [; progress=<0-100>] [; message=<text>] BEL
//! ```
//!
//! `ST` (`ESC \`) is accepted as the terminator too. `<value>` is one of `running`,
//! `waiting`, `success` (alias `done`), `failure` (alias `error`, `failed`), `idle`, or
//! `clear` (drop any status set earlier and go back to automatic detection). Keys may come
//! in any order; unknown keys are ignored so the format can grow.
//!
//! Number 678 spells "MRT" on a phone keypad. It is not assigned by xterm, VTE, iTerm2
//! (1337), kitty (5113, 99, 30001), WezTerm, VS Code (633), Windows Terminal / ConEmu (9),
//! rxvt (777) or FinalTerm (133), so it cannot clash with sequences those terminals
//! interpret, and terminals that do not know it ignore it — scripts can emit it
//! unconditionally.

use crate::vt::parse_ansi_text;
use std::time::{Duration, Instant};

/// OSC number of MRT's own status sequence (`ESC ] 678 ; status=... BEL`).
pub const MRT_STATUS_OSC: &str = "678";

/// How long an agent CLI must stay quiet before a prompt-like screen counts as waiting.
pub const AGENT_QUIET_PERIOD: Duration = Duration::from_millis(1500);
/// How long any other command must stay quiet before a question counts as waiting.
pub const GENERIC_QUIET_PERIOD: Duration = Duration::from_secs(5);

/// Agent CLIs MRT recognises, by executable name.
pub const AGENT_CLIS: &[&str] = &["claude", "codex", "gemini", "aider"];

/// Longest unterminated OSC sequence kept between chunks before it is dropped.
const MAX_OSC_CARRY: usize = 4096;
/// Plain-text tail kept for the prompt heuristic.
const TAIL_BYTES: usize = 2048;
/// How many trailing non-empty lines agent heuristics look at.
const AGENT_TAIL_LINES: usize = 6;

/// What a tab is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityState {
    /// Nothing has run yet.
    Idle,
    /// A command is running.
    Running,
    /// A command is running and reported this percentage (0–100).
    Progress(u8),
    /// A command is running and appears to wait for the user.
    WaitingForInput,
    /// The last command finished with exit code 0.
    Succeeded,
    /// The last command failed.
    Failed,
}

impl ActivityState {
    /// Stable machine-readable name, used in MCP output.
    pub fn as_str(self) -> &'static str {
        match self {
            ActivityState::Idle => "idle",
            ActivityState::Running => "running",
            ActivityState::Progress(_) => "progress",
            ActivityState::WaitingForInput => "waiting_for_input",
            ActivityState::Succeeded => "succeeded",
            ActivityState::Failed => "failed",
        }
    }

    /// Short human-readable label.
    pub fn label(self) -> String {
        match self {
            ActivityState::Idle => "Idle".into(),
            ActivityState::Running => "Running".into(),
            ActivityState::Progress(p) => format!("Running · {p}%"),
            ActivityState::WaitingForInput => "Waiting for input".into(),
            ActivityState::Succeeded => "Finished".into(),
            ActivityState::Failed => "Failed".into(),
        }
    }

    pub fn is_running(self) -> bool {
        matches!(
            self,
            ActivityState::Running | ActivityState::Progress(_) | ActivityState::WaitingForInput
        )
    }
}

/// Colour hint carried by `OSC 9;4` progress (error and warning bars).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProgressTone {
    #[default]
    Normal,
    Error,
    Warning,
}

impl ProgressTone {
    pub fn as_str(self) -> &'static str {
        match self {
            ProgressTone::Normal => "normal",
            ProgressTone::Error => "error",
            ProgressTone::Warning => "warning",
        }
    }
}

/// The `st` field of `OSC 9 ; 4 ; st ; pr`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressState {
    /// 0: remove the progress indicator.
    Remove,
    /// 1: normal progress with a percentage.
    Normal,
    /// 2: error state (percentage optional).
    Error,
    /// 3: indeterminate (busy, no percentage).
    Indeterminate,
    /// 4: warning / paused state (percentage optional).
    Warning,
}

/// Values of `status=` in the MRT status sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MrtStatus {
    Clear,
    Idle,
    Running,
    Waiting,
    Succeeded,
    Failed,
}

impl MrtStatus {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "clear" | "auto" | "reset" => Some(MrtStatus::Clear),
            "idle" => Some(MrtStatus::Idle),
            "running" | "busy" | "working" => Some(MrtStatus::Running),
            "waiting" | "waiting_for_input" | "input" | "blocked" => Some(MrtStatus::Waiting),
            "success" | "succeeded" | "done" | "ok" => Some(MrtStatus::Succeeded),
            "failure" | "failed" | "error" => Some(MrtStatus::Failed),
            _ => None,
        }
    }
}

/// One activity-related escape sequence found in output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivitySignal {
    /// `OSC 9 ; 4 ; st [; pr]`.
    Progress {
        state: ProgressState,
        percent: Option<u8>,
    },
    /// `OSC 678 ; status=... [; progress=..] [; message=..]`.
    Status {
        status: MrtStatus,
        progress: Option<u8>,
        message: Option<String>,
    },
}

fn parse_percent(value: &str) -> Option<u8> {
    let n: i64 = value.trim().parse().ok()?;
    Some(n.clamp(0, 100) as u8)
}

/// Parses the body of one OSC sequence (between `ESC ]` and the terminator).
fn parse_osc_body(body: &str) -> Option<ActivitySignal> {
    let (number, rest) = body.split_once(';')?;
    match number {
        "9" => {
            let mut parts = rest.split(';');
            if parts.next()? != "4" {
                return None; // OSC 9;<other> is a ConEmu notification, not progress.
            }
            let state = match parts.next().unwrap_or("0").trim() {
                "" | "0" => ProgressState::Remove,
                "1" => ProgressState::Normal,
                "2" => ProgressState::Error,
                "3" => ProgressState::Indeterminate,
                "4" => ProgressState::Warning,
                _ => return None,
            };
            let percent = parts.next().and_then(parse_percent);
            Some(ActivitySignal::Progress { state, percent })
        }
        MRT_STATUS_OSC => {
            let mut status = None;
            let mut progress = None;
            let mut message = None;
            // `message` is last-wins and may itself contain ';' if it is the final key.
            let mut rest = rest;
            while !rest.is_empty() {
                let (field, tail) = rest.split_once(';').unwrap_or((rest, ""));
                let Some((key, value)) = field.split_once('=') else {
                    rest = tail;
                    continue;
                };
                match key.trim().to_ascii_lowercase().as_str() {
                    "status" => status = MrtStatus::parse(value),
                    "progress" => progress = parse_percent(value),
                    "message" => {
                        // Everything up to the end belongs to the message.
                        let full = if tail.is_empty() {
                            value.to_string()
                        } else {
                            format!("{value};{tail}")
                        };
                        let text: String = full.chars().filter(|c| !c.is_control()).take(200).collect();
                        message = (!text.trim().is_empty()).then(|| text.trim().to_string());
                        break;
                    }
                    _ => {}
                }
                rest = tail;
            }
            Some(ActivitySignal::Status {
                status: status?,
                progress,
                message,
            })
        }
        _ => None,
    }
}

/// Incremental scanner for activity OSC sequences. Keeps an unterminated sequence between
/// chunks so a sequence split across two reads is still recognised.
#[derive(Debug, Default, Clone)]
pub struct OscScanner {
    carry: String,
}

impl OscScanner {
    pub fn feed(&mut self, chunk: &str) -> Vec<ActivitySignal> {
        let mut text = std::mem::take(&mut self.carry);
        text.push_str(chunk);
        let mut signals = Vec::new();
        let mut search_from = 0;
        while let Some(rel) = text[search_from..].find("\x1b]") {
            let start = search_from + rel;
            let body_start = start + 2;
            let tail = &text[body_start..];
            // Terminator: BEL, or ST (ESC \). A bare ESC starts something else and aborts.
            let mut end = None;
            for (i, b) in tail.bytes().enumerate() {
                match b {
                    0x07 => {
                        end = Some((i, 1));
                        break;
                    }
                    0x1b => {
                        if tail.as_bytes().get(i + 1) == Some(&b'\\') {
                            end = Some((i, 2));
                        } else if i + 1 < tail.len() {
                            end = Some((i, 0)); // aborted sequence
                        }
                        break;
                    }
                    _ => {}
                }
            }
            match end {
                Some((len, term)) => {
                    if term > 0 {
                        if let Some(sig) = parse_osc_body(&tail[..len]) {
                            signals.push(sig);
                        }
                    }
                    // An aborted sequence resumes scanning at its ESC, which may open the next one.
                    search_from = body_start + len + term;
                }
                None => {
                    if text.len() - start <= MAX_OSC_CARRY {
                        self.carry = text[start..].to_string();
                    }
                    return signals;
                }
            }
        }
        // A lone ESC at the very end may be the start of the next OSC.
        if text.ends_with('\x1b') {
            self.carry = "\x1b".into();
        }
        signals
    }
}

/// Parses every complete activity sequence in `text` (stateless convenience wrapper).
pub fn parse_activity_signals(text: &str) -> Vec<ActivitySignal> {
    OscScanner::default().feed(text)
}

fn agent_from_basename(name: &str) -> Option<&'static str> {
    let lower = name.trim().to_ascii_lowercase();
    let stem = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    let stem = ["exe", "cmd", "bat", "ps1", "js", "mjs", "py"]
        .iter()
        .find_map(|ext| stem.strip_suffix(&format!(".{ext}")))
        .unwrap_or(stem);
    // npm packages: @anthropic-ai/claude-code, @openai/codex, @google/gemini-cli, aider-chat
    let stem = stem
        .strip_suffix("-code")
        .or_else(|| stem.strip_suffix("-cli"))
        .or_else(|| stem.strip_suffix("-chat"))
        .unwrap_or(stem);
    AGENT_CLIS.iter().copied().find(|agent| *agent == stem)
}

/// Recognises an agent CLI from a command line: `claude`, `codex --full-auto`,
/// `npx @openai/codex`, `C:\tools\aider.exe`. Only the program word (or the package after
/// `npx`/`bunx`/`pnpx`/`uvx`/`pipx run`) is checked, so `echo claude` is not an agent.
pub fn detect_agent_cli(command: &str) -> Option<&'static str> {
    let mut words = command
        .split_whitespace()
        .map(|w| w.trim_matches(|c| c == '"' || c == '\''))
        .skip_while(|w| w.contains('=') && !w.starts_with('-')); // FOO=1 claude
    let mut program = words.next()?;
    let launcher = program.to_ascii_lowercase();
    let launcher = launcher.strip_suffix(".cmd").or_else(|| launcher.strip_suffix(".exe")).unwrap_or(&launcher);
    if matches!(launcher, "npx" | "bunx" | "pnpx" | "uvx" | "pipx") {
        let next = words.find(|w| !w.starts_with('-') && *w != "run")?;
        // `npx @anthropic-ai/claude-code@latest`
        let without_version = match next.strip_prefix('@') {
            Some(scoped) => scoped.split_once('@').map(|(p, _)| p).unwrap_or(scoped),
            None => next.split('@').next().unwrap_or(next),
        };
        program = without_version;
    }
    agent_from_basename(program)
}

/// Recognises an agent CLI from an OS process name (`claude`, `codex.exe`, `aider`).
/// Generic runtimes (`node`, `python`) are never treated as agents.
pub fn agent_from_process_name(name: &str) -> Option<&'static str> {
    agent_from_basename(name)
}

const YES_NO_MARKERS: &[&str] = &[
    "[y/n]", "(y/n)", "[yes/no]", "(yes/no)", "y/n?", "[y/n/a]", "(y)es/(n)o",
];

/// True when `line` reads like a question for the user. `agent` enables the extra
/// approval phrases agent CLIs print ("Do you want to proceed?", "Allow command?").
pub fn looks_like_input_prompt(line: &str, agent: bool) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if YES_NO_MARKERS.iter().any(|m| lower.contains(m)) {
        return true;
    }
    if lower.ends_with("password:") || lower.ends_with("passphrase:") || lower.contains("press enter") || lower.contains("press any key") {
        return true;
    }
    if trimmed.ends_with('?') && trimmed.chars().filter(|c| c.is_alphabetic()).count() >= 3 {
        return true;
    }
    if agent {
        const AGENT_PHRASES: &[&str] = &[
            "do you want",
            "approve",
            "esc to cancel",
            "(y)es",
            "waiting for your",
        ];
        if AGENT_PHRASES.iter().any(|p| lower.contains(p)) {
            return true;
        }
        // Idle input prompts: aider's `> ` / `architect> `, Claude/Codex's `>` box.
        if trimmed == "\u{276f}" || (trimmed.ends_with('>') && trimmed.len() <= 24) {
            return true;
        }
    }
    false
}

/// Everything the GUI and MCP need to show a tab's activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivitySnapshot {
    pub state: ActivityState,
    pub tone: ProgressTone,
    /// Recognised agent CLI (`claude`, `codex`, ...), if any.
    pub agent: Option<&'static str>,
    /// Message from the MRT status sequence, or why the state was chosen.
    pub detail: Option<String>,
    /// The running (or last) command.
    pub command: Option<String>,
}

impl ActivitySnapshot {
    /// Best-effort state for a tab that has no live tracker (a headless MCP server's tab):
    /// a block without an exit code is running, otherwise the last exit code decides.
    pub fn from_blocks(blocks: &[crate::block::CommandBlock]) -> Self {
        let running = blocks.iter().rev().find(|b| b.exit_code.is_none());
        let last = running.or_else(|| blocks.last());
        let state = match (running, last.and_then(|b| b.exit_code)) {
            (Some(_), _) => ActivityState::Running,
            (None, None) => ActivityState::Idle,
            (None, Some(0)) => ActivityState::Succeeded,
            (None, Some(_)) => ActivityState::Failed,
        };
        let command = last.map(|b| b.command.clone());
        Self {
            state,
            tone: ProgressTone::Normal,
            agent: command.as_deref().and_then(detect_agent_cli),
            detail: None,
            command,
        }
    }

    /// JSON form used by the MCP server.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "state": self.state.as_str(),
            "label": self.state.label(),
            "progress": match self.state {
                ActivityState::Progress(p) => Some(p),
                _ => None,
            },
            "tone": self.tone.as_str(),
            "agent": self.agent,
            "detail": self.detail,
            "command": self.command,
        })
    }

    /// One-line tooltip text.
    pub fn tooltip(&self) -> String {
        let mut out = self.state.label();
        if let Some(agent) = self.agent {
            out.push_str(&format!(" · {agent}"));
        }
        if self.tone != ProgressTone::Normal {
            out.push_str(&format!(" ({})", self.tone.as_str()));
        }
        if let Some(cmd) = &self.command {
            let short: String = cmd.chars().take(60).collect();
            out.push_str(&format!("\n{short}"));
        }
        if let Some(detail) = &self.detail {
            out.push_str(&format!("\n{detail}"));
        }
        out
    }
}

/// Tracks one tab's activity. See the module docs.
#[derive(Debug, Clone, Default)]
pub struct ActivityTracker {
    running: usize,
    last_exit: Option<i32>,
    command: Option<String>,
    agent: Option<&'static str>,
    progress: Option<(ProgressState, Option<u8>)>,
    explicit: Option<(MrtStatus, Option<u8>)>,
    message: Option<String>,
    last_output_at: Option<Instant>,
    tail: String,
    scanner: OscScanner,
}

impl ActivityTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// A command started in this tab.
    pub fn command_started(&mut self, command: &str, now: Instant) {
        self.running += 1;
        self.command = Some(command.to_string());
        self.agent = detect_agent_cli(command);
        self.progress = None;
        self.explicit = None;
        self.message = None;
        self.tail.clear();
        self.scanner = OscScanner::default();
        self.last_output_at = Some(now);
    }

    /// A command finished. The tab shows its result once no other command is running.
    pub fn command_finished(&mut self, exit_code: i32, now: Instant) {
        self.running = self.running.saturating_sub(1);
        self.last_exit = Some(exit_code);
        self.last_output_at = Some(now);
        if self.running == 0 {
            self.progress = None;
            self.explicit = None;
            self.message = None;
        }
    }

    /// Raw output (escape sequences included) from the running command.
    pub fn feed_output(&mut self, chunk: &str, now: Instant) {
        if chunk.is_empty() {
            return;
        }
        for signal in self.scanner.feed(chunk) {
            match signal {
                ActivitySignal::Progress { state: ProgressState::Remove, .. } => self.progress = None,
                ActivitySignal::Progress { state, percent } => self.progress = Some((state, percent)),
                ActivitySignal::Status { status: MrtStatus::Clear, .. } => {
                    self.explicit = None;
                    self.message = None;
                }
                ActivitySignal::Status { status, progress, message } => {
                    self.explicit = Some((status, progress));
                    self.message = message;
                }
            }
        }
        // The VT parser drops '\r'; keep a carriage-return rewrite visible as a line break
        // so the heuristic looks at the text that was drawn last.
        let normalized = chunk.replace("\r\n", "\n").replace('\r', "\n");
        let (_, plain) = parse_ansi_text(&normalized);
        if !plain.trim().is_empty() {
            self.last_output_at = Some(now);
        }
        self.tail.push_str(&plain);
        if self.tail.len() > TAIL_BYTES {
            let mut cut = self.tail.len() - TAIL_BYTES;
            while !self.tail.is_char_boundary(cut) {
                cut += 1;
            }
            self.tail.drain(..cut);
        }
    }

    /// Names of processes in the running command's tree; recognises agents run via wrappers.
    pub fn observe_process_names<'a>(&mut self, names: impl IntoIterator<Item = &'a str>) {
        if self.running == 0 || self.agent.is_some() {
            return;
        }
        self.agent = names.into_iter().find_map(agent_from_process_name);
    }

    pub fn running_commands(&self) -> usize {
        self.running
    }

    /// Trailing non-empty lines of plain output, last one last.
    fn tail_lines(&self, n: usize) -> Vec<&str> {
        let mut lines: Vec<&str> = self
            .tail
            .split('\n')
            .filter(|l| !l.trim().is_empty())
            .collect();
        let keep = lines.len().saturating_sub(n);
        lines.drain(..keep);
        lines
    }

    fn looks_waiting(&self, now: Instant) -> bool {
        let quiet = self
            .last_output_at
            .map(|t| now.saturating_duration_since(t))
            .unwrap_or_default();
        if self.agent.is_some() {
            quiet >= AGENT_QUIET_PERIOD
                && self.tail_lines(AGENT_TAIL_LINES).iter().any(|l| looks_like_input_prompt(l, true))
        } else {
            quiet >= GENERIC_QUIET_PERIOD
                && self.tail_lines(1).first().is_some_and(|l| looks_like_input_prompt(l, false))
        }
    }

    /// Current state at time `now`.
    pub fn snapshot(&self, now: Instant) -> ActivitySnapshot {
        let mut tone = ProgressTone::Normal;
        let mut detail = self.message.clone();
        let state = if let Some((status, progress)) = self.explicit {
            match status {
                MrtStatus::Running | MrtStatus::Clear => progress.map_or(ActivityState::Running, ActivityState::Progress),
                MrtStatus::Waiting => ActivityState::WaitingForInput,
                MrtStatus::Succeeded => ActivityState::Succeeded,
                MrtStatus::Failed => ActivityState::Failed,
                MrtStatus::Idle => ActivityState::Idle,
            }
        } else if self.running > 0 {
            if let Some((pstate, percent)) = self.progress {
                tone = match pstate {
                    ProgressState::Error => ProgressTone::Error,
                    ProgressState::Warning => ProgressTone::Warning,
                    _ => ProgressTone::Normal,
                };
                match (pstate, percent) {
                    (ProgressState::Indeterminate, _) | (_, None) => ActivityState::Running,
                    (_, Some(p)) => ActivityState::Progress(p),
                }
            } else if self.looks_waiting(now) {
                if detail.is_none() {
                    detail = self.tail_lines(1).first().map(|l| {
                        let short: String = l.trim().chars().take(80).collect();
                        format!("Last line: {short}")
                    });
                }
                ActivityState::WaitingForInput
            } else {
                ActivityState::Running
            }
        } else {
            match self.last_exit {
                None => ActivityState::Idle,
                Some(0) => ActivityState::Succeeded,
                Some(code) => {
                    detail.get_or_insert_with(|| format!("Exit code {code}"));
                    ActivityState::Failed
                }
            }
        };
        ActivitySnapshot {
            state,
            tone,
            agent: self.agent,
            detail,
            command: self.command.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn parses_osc_9_4_states() {
        let sig = parse_activity_signals("a\x1b]9;4;1;42\x07b\x1b]9;4;3\x1b\\c\x1b]9;4;0\x07");
        assert_eq!(
            sig,
            vec![
                ActivitySignal::Progress { state: ProgressState::Normal, percent: Some(42) },
                ActivitySignal::Progress { state: ProgressState::Indeterminate, percent: None },
                ActivitySignal::Progress { state: ProgressState::Remove, percent: None },
            ]
        );
        let sig = parse_activity_signals("\x1b]9;4;2;150\x07\x1b]9;4;4;7\x07");
        assert_eq!(
            sig,
            vec![
                ActivitySignal::Progress { state: ProgressState::Error, percent: Some(100) },
                ActivitySignal::Progress { state: ProgressState::Warning, percent: Some(7) },
            ]
        );
    }

    #[test]
    fn ignores_unrelated_and_malformed_osc() {
        // Window title, ConEmu notification, unknown progress state, OSC 133, aborted.
        let text = "\x1b]0;title\x07\x1b]9;9;C:\\\x07\x1b]9;4;9;1\x07\x1b]133;A\x07\x1b]9;4;1;5\x1b[0m";
        assert!(parse_activity_signals(text).is_empty());
        assert!(parse_activity_signals("\x1b]678;progress=5\x07").is_empty()); // no status
        assert!(parse_activity_signals("\x1b]678;status=dancing\x07").is_empty());
    }

    #[test]
    fn parses_mrt_status_sequence() {
        let sig = parse_activity_signals("\x1b]678;status=waiting;message=Approve deploy; then wait\x07");
        assert_eq!(
            sig,
            vec![ActivitySignal::Status {
                status: MrtStatus::Waiting,
                progress: None,
                message: Some("Approve deploy; then wait".into()),
            }]
        );
        let sig = parse_activity_signals("\x1b]678;progress=30;STATUS=Running\x1b\\");
        assert_eq!(
            sig,
            vec![ActivitySignal::Status { status: MrtStatus::Running, progress: Some(30), message: None }]
        );
        assert!(matches!(
            parse_activity_signals("\x1b]678;status=done\x07")[0],
            ActivitySignal::Status { status: MrtStatus::Succeeded, .. }
        ));
    }

    #[test]
    fn scanner_handles_sequences_split_across_chunks() {
        let mut s = OscScanner::default();
        assert!(s.feed("output \x1b]9;4;1").is_empty());
        assert!(s.feed(";6").is_empty());
        assert_eq!(
            s.feed("6\x07 more"),
            vec![ActivitySignal::Progress { state: ProgressState::Normal, percent: Some(66) }]
        );
        assert!(s.feed("x\x1b").is_empty());
        assert_eq!(s.feed("]9;4;0\x1b").len(), 0);
        assert_eq!(
            s.feed("\\"),
            vec![ActivitySignal::Progress { state: ProgressState::Remove, percent: None }]
        );
    }

    #[test]
    fn detects_agent_clis_conservatively() {
        assert_eq!(detect_agent_cli("claude"), Some("claude"));
        assert_eq!(detect_agent_cli("codex --full-auto \"fix it\""), Some("codex"));
        assert_eq!(detect_agent_cli("npx @openai/codex"), Some("codex"));
        assert_eq!(detect_agent_cli("npx -y @anthropic-ai/claude-code@latest"), Some("claude"));
        assert_eq!(detect_agent_cli("C:\\tools\\aider.exe --model x"), Some("aider"));
        assert_eq!(detect_agent_cli("ANTHROPIC_LOG=debug claude"), Some("claude"));
        assert_eq!(detect_agent_cli("gemini"), Some("gemini"));
        assert_eq!(detect_agent_cli("echo claude"), None);
        assert_eq!(detect_agent_cli("claudette"), None);
        assert_eq!(detect_agent_cli("cargo build"), None);
        assert_eq!(detect_agent_cli(""), None);
        assert_eq!(agent_from_process_name("codex.exe"), Some("codex"));
        assert_eq!(agent_from_process_name("node"), None);
    }

    #[test]
    fn prompt_heuristic_is_conservative() {
        assert!(looks_like_input_prompt("Overwrite file? [y/N]", false));
        assert!(looks_like_input_prompt("Are you sure you want to continue connecting?", false));
        assert!(looks_like_input_prompt("[sudo] password:", false));
        assert!(!looks_like_input_prompt("Compiling mrt-core v0.1.0", false));
        assert!(!looks_like_input_prompt("Downloading:", false));
        assert!(!looks_like_input_prompt("??", false));
        assert!(!looks_like_input_prompt("Do you want to proceed", false));
        assert!(looks_like_input_prompt("Do you want to proceed", true));
        assert!(looks_like_input_prompt("Approve this edit", true));
        assert!(looks_like_input_prompt("architect>", true));
        assert!(!looks_like_input_prompt("This will allow faster builds", true));
        assert!(looks_like_input_prompt("> ", true));
        assert!(!looks_like_input_prompt("> ", false));
        assert!(!looks_like_input_prompt("Reading src/main.rs", true));
    }

    #[test]
    fn lifecycle_idle_running_finished() {
        let now = t0();
        let mut t = ActivityTracker::new();
        assert_eq!(t.snapshot(now).state, ActivityState::Idle);
        t.command_started("cargo build", now);
        assert_eq!(t.snapshot(now).state, ActivityState::Running);
        t.feed_output("Compiling\n", now);
        t.command_finished(0, now);
        assert_eq!(t.snapshot(now).state, ActivityState::Succeeded);
        t.command_started("false", now);
        t.command_finished(1, now);
        let snap = t.snapshot(now);
        assert_eq!(snap.state, ActivityState::Failed);
        assert_eq!(snap.detail.as_deref(), Some("Exit code 1"));
    }

    #[test]
    fn concurrent_commands_show_running_until_all_finish() {
        let now = t0();
        let mut t = ActivityTracker::new();
        t.command_started("a", now);
        t.command_started("b", now);
        t.command_finished(0, now);
        assert_eq!(t.snapshot(now).state, ActivityState::Running);
        t.command_finished(2, now);
        assert_eq!(t.snapshot(now).state, ActivityState::Failed);
    }

    #[test]
    fn progress_sequences_drive_state_and_tone() {
        let now = t0();
        let mut t = ActivityTracker::new();
        t.command_started("winget upgrade", now);
        t.feed_output("\x1b]9;4;1;25\x07", now);
        assert_eq!(t.snapshot(now).state, ActivityState::Progress(25));
        t.feed_output("\x1b]9;4;2;80\x07", now);
        let snap = t.snapshot(now);
        assert_eq!((snap.state, snap.tone), (ActivityState::Progress(80), ProgressTone::Error));
        t.feed_output("\x1b]9;4;3\x07", now);
        assert_eq!(t.snapshot(now).state, ActivityState::Running);
        t.feed_output("\x1b]9;4;0\x07", now);
        assert_eq!(t.snapshot(now).state, ActivityState::Running);
        t.feed_output("\x1b]9;4;1;90\x07", now);
        t.command_finished(0, now);
        assert_eq!(t.snapshot(now).state, ActivityState::Succeeded, "progress is cleared on exit");
    }

    #[test]
    fn mrt_status_overrides_heuristics_until_cleared() {
        let now = t0();
        let mut t = ActivityTracker::new();
        t.command_started("./deploy.sh", now);
        t.feed_output("\x1b]678;status=waiting;message=Approve in browser\x07", now);
        let snap = t.snapshot(now);
        assert_eq!(snap.state, ActivityState::WaitingForInput);
        assert_eq!(snap.detail.as_deref(), Some("Approve in browser"));
        t.feed_output("\x1b]9;4;1;50\x07", now);
        assert_eq!(t.snapshot(now).state, ActivityState::WaitingForInput, "explicit status wins");
        t.feed_output("\x1b]678;status=clear\x07", now);
        assert_eq!(t.snapshot(now).state, ActivityState::Progress(50));
        t.feed_output("\x1b]678;status=running;progress=70\x07", now);
        assert_eq!(t.snapshot(now).state, ActivityState::Progress(70));
        t.command_finished(3, now);
        assert_eq!(t.snapshot(now).state, ActivityState::Failed, "exit code wins after finish");
    }

    #[test]
    fn generic_command_waits_only_after_quiet_question() {
        let start = t0();
        let mut t = ActivityTracker::new();
        t.command_started("apt install foo", start);
        t.feed_output("Need to get 3 MB.\nDo you want to continue? [Y/n] ", start);
        assert_eq!(t.snapshot(start + Duration::from_secs(1)).state, ActivityState::Running);
        let later = start + GENERIC_QUIET_PERIOD;
        let snap = t.snapshot(later);
        assert_eq!(snap.state, ActivityState::WaitingForInput);
        assert!(snap.detail.unwrap().contains("[Y/n]"));
        // New output resumes running.
        t.feed_output("\nUnpacking...\n", later);
        assert_eq!(t.snapshot(later + Duration::from_secs(1)).state, ActivityState::Running);
        // Quiet but not a question: still running (e.g. a slow build).
        assert_eq!(t.snapshot(later + Duration::from_secs(60)).state, ActivityState::Running);
    }

    #[test]
    fn agent_cli_waiting_for_approval() {
        let start = t0();
        let mut t = ActivityTracker::new();
        t.command_started("claude", start);
        assert_eq!(t.snapshot(start).agent, Some("claude"));
        t.feed_output(
            "\x1b[1mBash command\x1b[0m\n  rm -rf target\nDo you want to proceed?\n\u{276f} 1. Yes\n  2. No\n",
            start,
        );
        assert_eq!(t.snapshot(start + Duration::from_millis(500)).state, ActivityState::Running);
        assert_eq!(t.snapshot(start + AGENT_QUIET_PERIOD).state, ActivityState::WaitingForInput);
    }

    #[test]
    fn agent_detected_from_process_names() {
        let start = t0();
        let mut t = ActivityTracker::new();
        t.command_started("./run-agent.sh", start);
        assert_eq!(t.snapshot(start).agent, None);
        t.observe_process_names(["bash", "node", "codex"]);
        assert_eq!(t.snapshot(start).agent, Some("codex"));
        t.feed_output("Allow command `git push`? ", start);
        assert_eq!(t.snapshot(start + AGENT_QUIET_PERIOD).state, ActivityState::WaitingForInput);
    }

    #[test]
    fn carriage_return_rewrites_use_latest_text() {
        let start = t0();
        let mut t = ActivityTracker::new();
        t.command_started("tool", start);
        t.feed_output("Continue? [y/n]\r  50% done      ", start);
        assert_eq!(t.snapshot(start + GENERIC_QUIET_PERIOD).state, ActivityState::Running);
    }

    #[test]
    fn snapshot_from_blocks_and_json() {
        use crate::block::CommandBlock;
        assert_eq!(ActivitySnapshot::from_blocks(&[]).state, ActivityState::Idle);
        let ok = CommandBlock::new_completed("ls", "sh", ".", "", 0, 1);
        let bad = CommandBlock::new_completed("codex", "sh", ".", "", 2, 1);
        let snap = ActivitySnapshot::from_blocks(&[ok.clone(), bad]);
        assert_eq!((snap.state, snap.agent), (ActivityState::Failed, Some("codex")));
        let mut running = ok.clone();
        running.exit_code = None;
        assert_eq!(ActivitySnapshot::from_blocks(&[running, ok]).state, ActivityState::Running);

        let now = t0();
        let mut t = ActivityTracker::new();
        t.command_started("make", now);
        t.feed_output("\x1b]9;4;4;12\x07", now);
        let json = t.snapshot(now).to_json();
        assert_eq!(json["state"], "progress");
        assert_eq!(json["progress"], 12);
        assert_eq!(json["tone"], "warning");
        assert!(json["agent"].is_null());
    }

    #[test]
    fn tooltip_mentions_state_agent_and_detail() {
        let now = t0();
        let mut t = ActivityTracker::new();
        t.command_started("aider", now);
        t.feed_output("\x1b]678;status=waiting;message=Review diff\x07", now);
        let tip = t.snapshot(now).tooltip();
        assert!(tip.contains("Waiting for input"));
        assert!(tip.contains("aider"));
        assert!(tip.contains("Review diff"));
    }
}
