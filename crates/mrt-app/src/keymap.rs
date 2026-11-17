//! Configurable keybindings for the MRT GUI.
//!
//! A keymap maps *binding strings* to [`Action`]s:
//!
//! - `ctrl+shift+t` — a single chord (modifiers joined with `+`, key last).
//! - `cmd+k` — `cmd` / `super` / `meta` / `win` is the platform's primary
//!   modifier: Command on macOS, Ctrl on Windows and Linux.
//! - `ctrl+a > c` — a chain: press `Ctrl+A`, release, then `C`.
//! - `pane/h` — a binding inside the named key table `pane`. Tables are entered
//!   with the `enter_table:pane` action and stay active until `Escape`, the
//!   `exit_table` action, or a period of inactivity ([`TABLE_TIMEOUT`]).
//!
//! The [`KeyResolver`] is fed one key press at a time and answers
//! [`Resolution::Matched`], [`Resolution::Pending`] (key consumed, waiting for
//! more) or [`Resolution::NoMatch`] (pass the key through to the shell/PTY).

use std::collections::HashMap;
use std::fmt;
use std::time::{Duration, Instant};

use egui::{Key, Modifiers};

/// How long a half-typed chain (`ctrl+a > …`) waits for its next key.
pub const CHAIN_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long a key table stays active without a key press.
pub const TABLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Everything a key binding can trigger.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    ToggleCommandPalette,
    ExitTui,
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    SplitHorizontal,
    SplitVertical,
    Unsplit,
    ClearBlocks,
    ToggleExplorer,
    ToggleHistory,
    ToggleRunbooks,
    ToggleMcpHub,
    ToggleToolbox,
    ToggleMosaic,
    OpenTaskManager,
    /// Enter the named key table.
    EnterTable(String),
    /// Leave the active key table.
    ExitTable,
}

/// The simple (argument-free) actions, in Command Palette order.
pub const PALETTE_ACTIONS: &[Action] = &[
    Action::SplitHorizontal,
    Action::SplitVertical,
    Action::Unsplit,
    Action::ClearBlocks,
    Action::NewTab,
    Action::CloseTab,
    Action::NextTab,
    Action::PreviousTab,
    Action::ToggleExplorer,
    Action::ToggleHistory,
    Action::ToggleRunbooks,
    Action::ToggleMcpHub,
    Action::ToggleToolbox,
    Action::ToggleMosaic,
    Action::OpenTaskManager,
];

const SIMPLE_ACTIONS: &[(Action, &str, &str)] = &[
    (
        Action::ToggleCommandPalette,
        "toggle_command_palette",
        "Toggle the Command Palette",
    ),
    (Action::ExitTui, "exit_tui", "Exit Interactive TUI mode"),
    (Action::NewTab, "new_tab", "New tab"),
    (Action::CloseTab, "close_tab", "Close tab"),
    (Action::NextTab, "next_tab", "Next tab"),
    (Action::PreviousTab, "previous_tab", "Previous tab"),
    (
        Action::SplitHorizontal,
        "split_horizontal",
        "Split pane horizontally",
    ),
    (
        Action::SplitVertical,
        "split_vertical",
        "Split pane vertically",
    ),
    (Action::Unsplit, "unsplit", "Unsplit"),
    (Action::ClearBlocks, "clear_blocks", "Clear terminal blocks"),
    (
        Action::ToggleExplorer,
        "toggle_explorer",
        "Toggle Folder Explorer",
    ),
    (
        Action::ToggleHistory,
        "toggle_history",
        "Toggle History panel",
    ),
    (
        Action::ToggleRunbooks,
        "toggle_runbooks",
        "Toggle Runbooks panel",
    ),
    (Action::ToggleMcpHub, "toggle_mcp_hub", "Toggle MCP Hub"),
    (Action::ToggleToolbox, "toggle_toolbox", "Toggle Toolbox"),
    (Action::ToggleMosaic, "toggle_mosaic", "Toggle Mosaic grid"),
    (
        Action::OpenTaskManager,
        "open_task_manager",
        "Show processes (Task Manager)",
    ),
    (
        Action::ExitTable,
        "exit_table",
        "Leave the active key table",
    ),
];

/// Extra spellings accepted for action names.
const ACTION_ALIASES: &[(&str, &str)] = &[
    ("command_palette", "toggle_command_palette"),
    ("palette", "toggle_command_palette"),
    ("split_h", "split_horizontal"),
    ("split_v", "split_vertical"),
    ("prev_tab", "previous_tab"),
    ("clear", "clear_blocks"),
    ("toggle_mcp", "toggle_mcp_hub"),
    ("task_manager", "open_task_manager"),
    ("activate_key_table", "enter_table"),
    ("deactivate_key_table", "exit_table"),
];

impl Action {
    /// The canonical config name, e.g. `split_horizontal` or `enter_table:pane`.
    pub fn name(&self) -> String {
        match self {
            Action::EnterTable(table) => format!("enter_table:{table}"),
            other => SIMPLE_ACTIONS
                .iter()
                .find(|(a, _, _)| a == other)
                .map(|(_, name, _)| (*name).to_string())
                .unwrap_or_default(),
        }
    }

    /// A human-readable title for the Command Palette.
    pub fn title(&self) -> String {
        match self {
            Action::EnterTable(table) => format!("Enter key table '{table}'"),
            other => SIMPLE_ACTIONS
                .iter()
                .find(|(a, _, _)| a == other)
                .map(|(_, _, title)| (*title).to_string())
                .unwrap_or_default(),
        }
    }

    /// Parses an action name. Case, `-` vs `_`, and a few aliases are accepted.
    pub fn parse(raw: &str) -> Result<Action, String> {
        let normalized = raw.trim().to_ascii_lowercase().replace('-', "_");
        let (head, arg) = match normalized.split_once(':') {
            Some((head, arg)) => (head.trim().to_string(), Some(arg.trim().to_string())),
            None => (normalized.clone(), None),
        };
        let head = ACTION_ALIASES
            .iter()
            .find(|(alias, _)| *alias == head)
            .map(|(_, canonical)| (*canonical).to_string())
            .unwrap_or(head);
        if head == "enter_table" {
            return match arg {
                Some(table) if is_table_name(&table) => Ok(Action::EnterTable(table)),
                _ => Err(format!(
                    "`{raw}` needs a table name, e.g. `enter_table:pane` (letters, digits, `_` or `-`)"
                )),
            };
        }
        if arg.is_some() {
            return Err(format!("action `{head}` does not take an argument"));
        }
        SIMPLE_ACTIONS
            .iter()
            .find(|(_, name, _)| *name == head)
            .map(|(action, _, _)| action.clone())
            .ok_or_else(|| format!("unknown action `{}`", raw.trim()))
    }

    fn applies_in(&self, context: KeyContext) -> bool {
        match self {
            Action::ExitTui => context.tui_active,
            _ => true,
        }
    }
}

/// Where keys are being pressed; some actions only apply in some places.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyContext {
    /// The full-screen Interactive TUI overlay is open.
    pub tui_active: bool,
}

/// Which platform's modifier conventions to parse and display with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    Mac,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::Mac
        } else {
            Platform::Other
        }
    }
}

/// One key press with its modifiers, e.g. `Ctrl+Shift+T`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// The macOS Command key. Always false on other platforms.
    pub cmd: bool,
}

impl KeyChord {
    pub fn plain(key: Key) -> Self {
        Self {
            key,
            ctrl: false,
            alt: false,
            shift: false,
            cmd: false,
        }
    }

    pub fn from_egui(key: Key, modifiers: Modifiers) -> Self {
        Self {
            key,
            ctrl: modifiers.ctrl,
            alt: modifiers.alt,
            shift: modifiers.shift,
            cmd: modifiers.mac_cmd,
        }
    }

    /// Like egui's `matches_logically`: Ctrl/Cmd must match, while Shift and
    /// Alt only need to be held if the binding asks for them.
    fn matches_logically(&self, pressed: &KeyChord) -> bool {
        self.key == pressed.key
            && self.ctrl == pressed.ctrl
            && self.cmd == pressed.cmd
            && (!self.shift || pressed.shift)
            && (!self.alt || pressed.alt)
    }

    pub fn display(&self, platform: Platform) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.cmd {
            parts.push(if platform == Platform::Mac {
                "Cmd"
            } else {
                "Super"
            });
        }
        if self.alt {
            parts.push(if platform == Platform::Mac {
                "Option"
            } else {
                "Alt"
            });
        }
        if self.shift {
            parts.push("Shift");
        }
        let key = self.key.symbol_or_name();
        parts.push(key);
        parts.join("+")
    }
}

/// A parsed binding: an optional key table plus a chain of one or more chords.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindingSpec {
    pub table: Option<String>,
    pub sequence: Vec<KeyChord>,
}

impl BindingSpec {
    pub fn display(&self, platform: Platform) -> String {
        let chain = self
            .sequence
            .iter()
            .map(|c| c.display(platform))
            .collect::<Vec<_>>()
            .join(" > ");
        match &self.table {
            Some(table) => format!("[{table}] {chain}"),
            None => chain,
        }
    }

    /// True if one chain equals or starts with the other, so both cannot be bound.
    fn overlaps(&self, other: &BindingSpec) -> bool {
        self.table == other.table
            && self
                .sequence
                .iter()
                .zip(other.sequence.iter())
                .all(|(a, b)| a == b)
    }
}

fn is_table_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Parses a binding string such as `ctrl+shift+t`, `cmd+k`, `ctrl+a > c` or `pane/h`.
pub fn parse_binding(raw: &str, platform: Platform) -> Result<BindingSpec, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err("empty binding".into());
    }
    let (table, chain) = match text.split_once('/') {
        Some((table, rest)) if is_table_name(table.trim()) && !rest.trim().is_empty() => {
            (Some(table.trim().to_ascii_lowercase()), rest)
        }
        _ => (None, text),
    };
    let mut sequence = Vec::new();
    for step in chain.split('>') {
        let step = step.trim();
        if step.is_empty() {
            return Err(format!("`{raw}` has an empty step in its `>` chain"));
        }
        sequence.push(parse_chord(step, platform).map_err(|e| format!("`{raw}`: {e}"))?);
    }
    Ok(BindingSpec { table, sequence })
}

fn parse_chord(step: &str, platform: Platform) -> Result<KeyChord, String> {
    // `ctrl++` binds the `+` key itself.
    let (mods_part, key_part) = if step == "+" {
        ("", "+")
    } else if let Some(prefix) = step.strip_suffix("++") {
        (prefix, "+")
    } else {
        match step.rsplit_once('+') {
            Some((mods, key)) => (mods, key),
            None => ("", step),
        }
    };
    let mut chord = KeyChord::plain(parse_key(key_part.trim())?);
    if !mods_part.is_empty() {
        for modifier in mods_part.split('+') {
            match modifier.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "control" => chord.ctrl = true,
                "shift" => chord.shift = true,
                "alt" | "option" | "opt" => chord.alt = true,
                "cmd" | "command" | "super" | "meta" | "win" | "primary" => match platform {
                    Platform::Mac => chord.cmd = true,
                    Platform::Other => chord.ctrl = true,
                },
                "" => return Err("stray `+`".into()),
                other => return Err(format!("unknown modifier `{other}`")),
            }
        }
    }
    Ok(chord)
}

fn parse_key(name: &str) -> Result<Key, String> {
    if name.is_empty() {
        return Err("missing key".into());
    }
    if let Some(key) = Key::from_name(name) {
        return Ok(key);
    }
    let lower = name.to_ascii_lowercase();
    let alias = match lower.as_str() {
        "esc" | "escape" => Some(Key::Escape),
        "enter" | "return" => Some(Key::Enter),
        "tab" => Some(Key::Tab),
        "space" => Some(Key::Space),
        "backspace" => Some(Key::Backspace),
        "delete" | "del" => Some(Key::Delete),
        "insert" | "ins" => Some(Key::Insert),
        "home" => Some(Key::Home),
        "end" => Some(Key::End),
        "pageup" | "pgup" => Some(Key::PageUp),
        "pagedown" | "pgdn" => Some(Key::PageDown),
        "up" => Some(Key::ArrowUp),
        "down" => Some(Key::ArrowDown),
        "left" => Some(Key::ArrowLeft),
        "right" => Some(Key::ArrowRight),
        "plus" => Some(Key::Plus),
        "minus" => Some(Key::Minus),
        "equal" | "equals" => Some(Key::Equals),
        "comma" => Some(Key::Comma),
        "period" => Some(Key::Period),
        "slash" => Some(Key::Slash),
        "backslash" => Some(Key::Backslash),
        "semicolon" => Some(Key::Semicolon),
        "backtick" | "grave" => Some(Key::Backtick),
        _ => None,
    };
    if let Some(key) = alias {
        return Ok(key);
    }
    // Letters and function keys: `t` -> `T`, `f5` -> `F5`.
    if let Some(key) = Key::from_name(&lower.to_ascii_uppercase()) {
        return Ok(key);
    }
    Err(format!("unknown key `{name}`"))
}

/// Where a binding came from, so user bindings can replace defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Default,
    User,
}

#[derive(Clone, Debug)]
struct Binding {
    spec: BindingSpec,
    action: Action,
    source: Source,
    /// The binding as the user wrote it (for messages).
    text: String,
}

/// A problem found while loading keybinding overrides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeymapIssue {
    InvalidBinding {
        binding: String,
        reason: String,
    },
    InvalidAction {
        binding: String,
        reason: String,
    },
    Conflict {
        binding: String,
        action: String,
        existing_binding: String,
        existing_action: String,
        /// Both bindings name exactly the same keys (otherwise one chain starts with the other).
        same_keys: bool,
    },
    EmptyTable {
        binding: String,
        table: String,
    },
}

impl fmt::Display for KeymapIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeymapIssue::InvalidBinding { reason, .. } => write!(f, "invalid binding {reason}"),
            KeymapIssue::InvalidAction { binding, reason } => write!(f, "`{binding}`: {reason}"),
            KeymapIssue::Conflict {
                binding,
                action,
                existing_binding,
                existing_action,
                same_keys,
            } => {
                if *same_keys {
                    write!(
                        f,
                        "`{binding}` = `{action}` ignored: the same keys are already bound by \
                         `{existing_binding}` = `{existing_action}`"
                    )
                } else {
                    write!(
                        f,
                        "`{binding}` = `{action}` ignored: it overlaps `{existing_binding}` = `{existing_action}` \
                         (one chain cannot start with another binding's keys)"
                    )
                }
            }
            KeymapIssue::EmptyTable { binding, table } => {
                write!(
                    f,
                    "`{binding}` enters key table `{table}`, but no `{table}/…` bindings exist"
                )
            }
        }
    }
}

/// The full set of bindings.
#[derive(Clone, Debug)]
pub struct Keymap {
    bindings: Vec<Binding>,
    platform: Platform,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::defaults(Platform::current())
    }
}

/// The built-in bindings. They reproduce MRT's historical hard-coded shortcuts.
const DEFAULT_BINDINGS: &[(&str, &str)] = &[
    ("ctrl+k", "toggle_command_palette"),
    ("ctrl+p", "toggle_command_palette"),
    ("ctrl+q", "exit_tui"),
];

impl Keymap {
    pub fn defaults(platform: Platform) -> Self {
        let bindings = DEFAULT_BINDINGS
            .iter()
            .map(|(binding, action)| Binding {
                spec: parse_binding(binding, platform).expect("default binding parses"),
                action: Action::parse(action).expect("default action parses"),
                source: Source::Default,
                text: (*binding).to_string(),
            })
            .collect();
        Self { bindings, platform }
    }

    /// Defaults plus user overrides (`binding -> action name`, e.g. from the
    /// config file's `[keybindings]` table). The action `unbind` (or `none`)
    /// removes a binding. Returns the keymap and every problem found; bad
    /// entries are skipped, the rest still apply.
    pub fn with_overrides(
        overrides: &HashMap<String, String>,
        platform: Platform,
    ) -> (Self, Vec<KeymapIssue>) {
        let mut keymap = Self::defaults(platform);
        let issues = keymap.apply_overrides(overrides);
        (keymap, issues)
    }

    pub fn apply_overrides(&mut self, overrides: &HashMap<String, String>) -> Vec<KeymapIssue> {
        let mut issues = Vec::new();
        let mut entries: Vec<_> = overrides.iter().collect();
        entries.sort();
        for (binding, action) in entries {
            let spec = match parse_binding(binding, self.platform) {
                Ok(spec) => spec,
                Err(reason) => {
                    issues.push(KeymapIssue::InvalidBinding {
                        binding: binding.clone(),
                        reason,
                    });
                    continue;
                }
            };
            let action_text = action.trim().to_ascii_lowercase();
            if action_text == "unbind" || action_text == "none" {
                self.bindings.retain(|b| b.spec != spec);
                continue;
            }
            let action = match Action::parse(action) {
                Ok(action) => action,
                Err(reason) => {
                    issues.push(KeymapIssue::InvalidAction {
                        binding: binding.clone(),
                        reason,
                    });
                    continue;
                }
            };
            // A user binding replaces any default it collides with.
            self.bindings
                .retain(|b| !(b.source == Source::Default && b.spec.overlaps(&spec)));
            if let Some(existing) = self.bindings.iter().find(|b| b.spec.overlaps(&spec)) {
                issues.push(KeymapIssue::Conflict {
                    binding: binding.clone(),
                    action: action.name(),
                    existing_binding: existing.text.clone(),
                    existing_action: existing.action.name(),
                    same_keys: existing.spec == spec,
                });
                continue;
            }
            self.bindings.push(Binding {
                spec,
                action,
                source: Source::User,
                text: binding.clone(),
            });
        }
        for binding in &self.bindings {
            if let Action::EnterTable(table) = &binding.action {
                if !self
                    .bindings
                    .iter()
                    .any(|b| b.spec.table.as_deref() == Some(table))
                {
                    issues.push(KeymapIssue::EmptyTable {
                        binding: binding.text.clone(),
                        table: table.clone(),
                    });
                }
            }
        }
        issues
    }

    /// Every binding of `action`, formatted for display (e.g. `Ctrl+K`).
    pub fn bindings_for(&self, action: &Action) -> Vec<String> {
        let mut found: Vec<&Binding> = self
            .bindings
            .iter()
            .filter(|b| &b.action == action)
            .collect();
        found.sort_by_key(|b| (b.spec.table.is_some(), b.spec.sequence.len()));
        found
            .iter()
            .map(|b| b.spec.display(self.platform))
            .collect()
    }
}

/// What the resolver decided about one key press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    /// A binding completed: run this action and swallow the key.
    Matched(Action),
    /// The key was consumed: a chain is half-typed, a chain was cancelled
    /// with Escape, or a key table is swallowing an unbound key.
    Pending,
    /// No binding wants this key: pass it through to the shell/PTY.
    NoMatch,
}

/// Turns key presses into actions, tracking chains and the active key table.
#[derive(Clone, Debug)]
pub struct KeyResolver {
    keymap: Keymap,
    pending: Vec<KeyChord>,
    pending_since: Option<Instant>,
    table: Option<String>,
    table_last_used: Option<Instant>,
    chain_timeout: Duration,
    table_timeout: Duration,
}

impl Default for KeyResolver {
    fn default() -> Self {
        Self::new(Keymap::default())
    }
}

impl KeyResolver {
    pub fn new(keymap: Keymap) -> Self {
        Self {
            keymap,
            pending: Vec::new(),
            pending_since: None,
            table: None,
            table_last_used: None,
            chain_timeout: CHAIN_TIMEOUT,
            table_timeout: TABLE_TIMEOUT,
        }
    }

    pub fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// Drops a half-typed chain or an idle key table whose time is up.
    pub fn tick(&mut self, now: Instant) {
        if self
            .pending_since
            .is_some_and(|t| now.duration_since(t) >= self.chain_timeout)
        {
            self.clear_pending();
        }
        if self
            .table_last_used
            .is_some_and(|t| now.duration_since(t) >= self.table_timeout)
        {
            self.table = None;
            self.table_last_used = None;
        }
    }

    /// The time until the next timeout, so the UI can repaint to clear the indicator.
    pub fn next_deadline(&self, now: Instant) -> Option<Duration> {
        let chain = self
            .pending_since
            .map(|t| (t + self.chain_timeout).saturating_duration_since(now));
        let table = self
            .table_last_used
            .map(|t| (t + self.table_timeout).saturating_duration_since(now));
        match (chain, table) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// A short label for the "waiting for more keys" indicator, if any.
    pub fn indicator(&self) -> Option<String> {
        let chain = (!self.pending.is_empty()).then(|| {
            let keys: Vec<String> = self
                .pending
                .iter()
                .map(|c| c.display(self.keymap.platform))
                .collect();
            format!("{} > …", keys.join(" > "))
        });
        match (&self.table, chain) {
            (Some(table), Some(chain)) => Some(format!("[{table}] {chain}")),
            (Some(table), None) => Some(format!("[{table}]  Esc to leave")),
            (None, chain) => chain,
        }
    }

    #[cfg(test)]
    pub fn active_table(&self) -> Option<&str> {
        self.table.as_deref()
    }

    fn clear_pending(&mut self) {
        self.pending.clear();
        self.pending_since = None;
    }

    /// Feeds one key press. `repeat` marks auto-repeat events from a held key.
    pub fn feed(
        &mut self,
        chord: KeyChord,
        repeat: bool,
        now: Instant,
        context: KeyContext,
    ) -> Resolution {
        self.tick(now);
        if self.table.is_some() {
            self.table_last_used = Some(now);
        }
        // A held key auto-repeating mid-chain should not abort the chain.
        if repeat && self.pending.last() == Some(&chord) {
            return Resolution::Pending;
        }
        if chord == KeyChord::plain(Key::Escape) {
            if !self.pending.is_empty() {
                self.clear_pending();
                return Resolution::Pending;
            }
            if self.table.is_some() {
                self.table = None;
                self.table_last_used = None;
                return Resolution::Matched(Action::ExitTable);
            }
        }

        let mut candidate = self.pending.clone();
        candidate.push(chord);
        let table = self.table.clone();
        let mut outcome = self.lookup(table.as_deref(), &candidate, context);
        // Inside a table, root bindings still work when the table has no match.
        if outcome.is_none() && table.is_some() {
            outcome = self.lookup(None, &candidate, context);
        }
        match outcome {
            Some(Lookup::Complete(action)) => {
                self.clear_pending();
                match &action {
                    Action::EnterTable(name) => {
                        self.table = Some(name.clone());
                        self.table_last_used = Some(now);
                    }
                    Action::ExitTable => {
                        self.table = None;
                        self.table_last_used = None;
                    }
                    _ => {}
                }
                Resolution::Matched(action)
            }
            Some(Lookup::Prefix) => {
                self.pending = candidate;
                self.pending_since = Some(now);
                Resolution::Pending
            }
            None => {
                let was_pending = !self.pending.is_empty();
                self.clear_pending();
                if was_pending {
                    // The chain is abandoned; retry this key as a fresh press.
                    return self.feed(chord, repeat, now, context);
                }
                if self.table.is_some() {
                    Resolution::Pending
                } else {
                    Resolution::NoMatch
                }
            }
        }
    }

    fn lookup(
        &self,
        table: Option<&str>,
        candidate: &[KeyChord],
        context: KeyContext,
    ) -> Option<Lookup> {
        let scope: Vec<&Binding> = self
            .keymap
            .bindings
            .iter()
            .filter(|b| b.spec.table.as_deref() == table && b.action.applies_in(context))
            .filter(|b| b.spec.sequence.len() >= candidate.len())
            .collect();
        // Exact modifier matches win; otherwise extra Shift/Alt are tolerated,
        // exactly like egui's `consume_key` that MRT used before.
        let exact: Vec<&&Binding> = scope
            .iter()
            .filter(|b| {
                b.spec
                    .sequence
                    .iter()
                    .zip(candidate)
                    .all(|(want, got)| want == got)
            })
            .collect();
        let matches = if exact.is_empty() {
            scope
                .iter()
                .filter(|b| {
                    b.spec
                        .sequence
                        .iter()
                        .zip(candidate)
                        .all(|(want, got)| want.matches_logically(got))
                })
                .collect()
        } else {
            exact
        };
        if let Some(full) = matches
            .iter()
            .find(|b| b.spec.sequence.len() == candidate.len())
        {
            return Some(Lookup::Complete(full.action.clone()));
        }
        (!matches.is_empty()).then_some(Lookup::Prefix)
    }
}

enum Lookup {
    Complete(Action),
    Prefix,
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: KeyContext = KeyContext { tui_active: false };
    const TUI: KeyContext = KeyContext { tui_active: true };

    fn ctrl(key: Key) -> KeyChord {
        KeyChord {
            ctrl: true,
            ..KeyChord::plain(key)
        }
    }

    fn overrides(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(b, a)| (b.to_string(), a.to_string()))
            .collect()
    }

    fn resolver(pairs: &[(&str, &str)]) -> KeyResolver {
        let (keymap, issues) = Keymap::with_overrides(&overrides(pairs), Platform::Other);
        assert!(issues.is_empty(), "unexpected issues: {issues:?}");
        KeyResolver::new(keymap)
    }

    #[test]
    fn parses_single_chords_and_aliases() {
        let spec = parse_binding("ctrl+shift+t", Platform::Other).unwrap();
        assert_eq!(spec.table, None);
        assert_eq!(
            spec.sequence,
            vec![KeyChord {
                shift: true,
                ..ctrl(Key::T)
            }]
        );
        assert_eq!(
            parse_binding("Alt+F5", Platform::Other).unwrap().sequence[0].key,
            Key::F5
        );
        assert_eq!(
            parse_binding("ctrl+esc", Platform::Other).unwrap().sequence[0].key,
            Key::Escape
        );
        assert_eq!(
            parse_binding("ctrl+pgdn", Platform::Other)
                .unwrap()
                .sequence[0]
                .key,
            Key::PageDown
        );
        assert_eq!(
            parse_binding("ctrl++", Platform::Other).unwrap().sequence[0],
            ctrl(Key::Plus)
        );
        assert_eq!(
            parse_binding("ctrl+/", Platform::Other).unwrap().sequence[0],
            ctrl(Key::Slash)
        );
    }

    #[test]
    fn rejects_bad_bindings_with_reasons() {
        assert!(parse_binding("", Platform::Other).is_err());
        let err = parse_binding("hyper+k", Platform::Other).unwrap_err();
        assert!(err.contains("unknown modifier `hyper`"), "{err}");
        let err = parse_binding("ctrl+notakey", Platform::Other).unwrap_err();
        assert!(err.contains("unknown key `notakey`"), "{err}");
        let err = parse_binding("ctrl+a > > c", Platform::Other).unwrap_err();
        assert!(err.contains("empty step"), "{err}");
    }

    #[test]
    fn cmd_maps_to_ctrl_off_macos_and_command_on_macos() {
        let other = parse_binding("cmd+k", Platform::Other).unwrap().sequence[0];
        assert_eq!(other, ctrl(Key::K));
        let mac = parse_binding("cmd+k", Platform::Mac).unwrap().sequence[0];
        assert_eq!(
            mac,
            KeyChord {
                cmd: true,
                ..KeyChord::plain(Key::K)
            }
        );
        assert_eq!(mac.display(Platform::Mac), "Cmd+K");
        assert_eq!(other.display(Platform::Other), "Ctrl+K");
        // `super` / `meta` / `win` are spellings of the same primary modifier.
        assert_eq!(
            parse_binding("super+k", Platform::Other).unwrap().sequence[0],
            ctrl(Key::K)
        );
        // egui reports Command as `mac_cmd`, so a real macOS key event matches.
        let mut mods = Modifiers::NONE;
        mods.mac_cmd = true;
        mods.command = true;
        assert_eq!(KeyChord::from_egui(Key::K, mods), mac);
    }

    #[test]
    fn parses_chains_and_tables() {
        let spec = parse_binding("ctrl+a > c", Platform::Other).unwrap();
        assert_eq!(spec.sequence, vec![ctrl(Key::A), KeyChord::plain(Key::C)]);
        assert_eq!(spec.display(Platform::Other), "Ctrl+A > C");
        let spec = parse_binding("pane/shift+h", Platform::Other).unwrap();
        assert_eq!(spec.table.as_deref(), Some("pane"));
        assert_eq!(spec.display(Platform::Other), "[pane] Shift+H");
    }

    #[test]
    fn parses_action_names() {
        assert_eq!(Action::parse("split-h"), Ok(Action::SplitHorizontal));
        assert_eq!(Action::parse("New_Tab"), Ok(Action::NewTab));
        assert_eq!(
            Action::parse("enter_table:pane"),
            Ok(Action::EnterTable("pane".into()))
        );
        assert_eq!(
            Action::parse("activate_key_table:resize"),
            Ok(Action::EnterTable("resize".into()))
        );
        assert!(Action::parse("enter_table").is_err());
        assert!(Action::parse("launch_rockets")
            .unwrap_err()
            .contains("unknown action"));
        for (action, name, _) in SIMPLE_ACTIONS {
            assert_eq!(Action::parse(name).as_ref(), Ok(action));
            assert_eq!(&action.name(), name);
        }
    }

    #[test]
    fn defaults_match_historical_shortcuts() {
        let mut r = KeyResolver::new(Keymap::defaults(Platform::Other));
        let now = Instant::now();
        assert_eq!(
            r.feed(ctrl(Key::K), false, now, NONE),
            Resolution::Matched(Action::ToggleCommandPalette)
        );
        assert_eq!(
            r.feed(ctrl(Key::P), false, now, NONE),
            Resolution::Matched(Action::ToggleCommandPalette)
        );
        // Extra Shift was tolerated by egui's consume_key, so it still is.
        let shifted = KeyChord {
            shift: true,
            ..ctrl(Key::K)
        };
        assert_eq!(
            r.feed(shifted, false, now, NONE),
            Resolution::Matched(Action::ToggleCommandPalette)
        );
        // Ctrl+Q only exits TUI mode; elsewhere it passes through.
        assert_eq!(r.feed(ctrl(Key::Q), false, now, NONE), Resolution::NoMatch);
        assert_eq!(
            r.feed(ctrl(Key::Q), false, now, TUI),
            Resolution::Matched(Action::ExitTui)
        );
        // Everything else (Ctrl+C, letters, Escape) goes to the shell/PTY.
        assert_eq!(r.feed(ctrl(Key::C), false, now, TUI), Resolution::NoMatch);
        assert_eq!(
            r.feed(KeyChord::plain(Key::K), false, now, NONE),
            Resolution::NoMatch
        );
        assert_eq!(
            r.feed(KeyChord::plain(Key::Escape), false, now, NONE),
            Resolution::NoMatch
        );
        assert_eq!(
            r.keymap().bindings_for(&Action::ToggleCommandPalette),
            vec!["Ctrl+K", "Ctrl+P"]
        );
        assert!(r.keymap().bindings_for(&Action::NewTab).is_empty());
    }

    #[test]
    fn exact_modifiers_beat_tolerant_matches() {
        let mut r = resolver(&[("ctrl+t", "new_tab"), ("ctrl+shift+t", "close_tab")]);
        let now = Instant::now();
        assert_eq!(
            r.feed(ctrl(Key::T), false, now, NONE),
            Resolution::Matched(Action::NewTab)
        );
        let shifted = KeyChord {
            shift: true,
            ..ctrl(Key::T)
        };
        assert_eq!(
            r.feed(shifted, false, now, NONE),
            Resolution::Matched(Action::CloseTab)
        );
    }

    #[test]
    fn chains_wait_then_match() {
        let mut r = resolver(&[
            ("ctrl+a > c", "new_tab"),
            ("ctrl+a > shift+backslash", "split_horizontal"),
        ]);
        let now = Instant::now();
        assert_eq!(r.feed(ctrl(Key::A), false, now, NONE), Resolution::Pending);
        assert_eq!(r.indicator().as_deref(), Some("Ctrl+A > …"));
        assert_eq!(
            r.feed(KeyChord::plain(Key::C), false, now, NONE),
            Resolution::Matched(Action::NewTab)
        );
        assert_eq!(r.indicator(), None);
        assert_eq!(r.feed(ctrl(Key::A), false, now, NONE), Resolution::Pending);
        let pipe = KeyChord {
            shift: true,
            ..KeyChord::plain(Key::Backslash)
        };
        assert_eq!(
            r.feed(pipe, false, now, NONE),
            Resolution::Matched(Action::SplitHorizontal)
        );
    }

    #[test]
    fn chain_aborts_on_unbound_key_escape_or_timeout() {
        let mut r = resolver(&[("ctrl+a > c", "new_tab")]);
        let start = Instant::now();
        // Unbound second key: chain dropped, the key itself passes through.
        assert_eq!(
            r.feed(ctrl(Key::A), false, start, NONE),
            Resolution::Pending
        );
        assert_eq!(
            r.feed(KeyChord::plain(Key::X), false, start, NONE),
            Resolution::NoMatch
        );
        assert_eq!(r.indicator(), None);
        // Escape cancels and is swallowed.
        assert_eq!(
            r.feed(ctrl(Key::A), false, start, NONE),
            Resolution::Pending
        );
        assert_eq!(
            r.feed(KeyChord::plain(Key::Escape), false, start, NONE),
            Resolution::Pending
        );
        assert_eq!(
            r.feed(KeyChord::plain(Key::C), false, start, NONE),
            Resolution::NoMatch
        );
        // Timeout: the late `c` is just a `c`.
        assert_eq!(
            r.feed(ctrl(Key::A), false, start, NONE),
            Resolution::Pending
        );
        assert_eq!(r.next_deadline(start), Some(CHAIN_TIMEOUT));
        let late = start + CHAIN_TIMEOUT + Duration::from_millis(1);
        r.tick(late);
        assert_eq!(r.indicator(), None);
        assert_eq!(
            r.feed(KeyChord::plain(Key::C), false, late, NONE),
            Resolution::NoMatch
        );
        // Held prefix auto-repeating does not abort the chain.
        assert_eq!(
            r.feed(ctrl(Key::A), false, start, NONE),
            Resolution::Pending
        );
        assert_eq!(r.feed(ctrl(Key::A), true, start, NONE), Resolution::Pending);
        assert_eq!(
            r.feed(KeyChord::plain(Key::C), false, start, NONE),
            Resolution::Matched(Action::NewTab)
        );
    }

    #[test]
    fn abandoned_chain_retries_key_as_fresh_press() {
        let mut r = resolver(&[("ctrl+a > c", "new_tab"), ("ctrl+t", "next_tab")]);
        let now = Instant::now();
        assert_eq!(r.feed(ctrl(Key::A), false, now, NONE), Resolution::Pending);
        assert_eq!(
            r.feed(ctrl(Key::T), false, now, NONE),
            Resolution::Matched(Action::NextTab)
        );
    }

    #[test]
    fn key_tables_are_sticky_until_escape_or_timeout() {
        let mut r = resolver(&[
            ("ctrl+b", "enter_table:pane"),
            ("pane/h", "split_horizontal"),
            ("pane/v", "split_vertical"),
            ("pane/q", "exit_table"),
        ]);
        let start = Instant::now();
        assert_eq!(
            r.feed(ctrl(Key::B), false, start, NONE),
            Resolution::Matched(Action::EnterTable("pane".into()))
        );
        assert_eq!(r.active_table(), Some("pane"));
        assert_eq!(r.indicator().as_deref(), Some("[pane]  Esc to leave"));
        // Table bindings repeat without re-entering.
        assert_eq!(
            r.feed(KeyChord::plain(Key::H), false, start, NONE),
            Resolution::Matched(Action::SplitHorizontal)
        );
        assert_eq!(
            r.feed(KeyChord::plain(Key::V), false, start, NONE),
            Resolution::Matched(Action::SplitVertical)
        );
        // Unbound keys are swallowed while the table is active.
        assert_eq!(
            r.feed(KeyChord::plain(Key::X), false, start, NONE),
            Resolution::Pending
        );
        // Root bindings still work inside a table.
        assert_eq!(
            r.feed(ctrl(Key::K), false, start, NONE),
            Resolution::Matched(Action::ToggleCommandPalette)
        );
        // Escape leaves.
        assert_eq!(
            r.feed(KeyChord::plain(Key::Escape), false, start, NONE),
            Resolution::Matched(Action::ExitTable)
        );
        assert_eq!(r.active_table(), None);
        assert_eq!(
            r.feed(KeyChord::plain(Key::H), false, start, NONE),
            Resolution::NoMatch
        );
        // An explicit exit_table binding leaves too.
        r.feed(ctrl(Key::B), false, start, NONE);
        assert_eq!(
            r.feed(KeyChord::plain(Key::Q), false, start, NONE),
            Resolution::Matched(Action::ExitTable)
        );
        assert_eq!(r.active_table(), None);
        // Inactivity leaves; each key press restarts the clock.
        r.feed(ctrl(Key::B), false, start, NONE);
        let mid = start + TABLE_TIMEOUT - Duration::from_millis(10);
        assert_eq!(
            r.feed(KeyChord::plain(Key::H), false, mid, NONE),
            Resolution::Matched(Action::SplitHorizontal)
        );
        r.tick(start + TABLE_TIMEOUT + Duration::from_millis(1));
        assert_eq!(r.active_table(), Some("pane"));
        r.tick(mid + TABLE_TIMEOUT);
        assert_eq!(r.active_table(), None);
        assert_eq!(
            r.feed(KeyChord::plain(Key::H), false, mid + TABLE_TIMEOUT, NONE),
            Resolution::NoMatch
        );
    }

    #[test]
    fn chains_inside_tables() {
        let mut r = resolver(&[("ctrl+b", "enter_table:pane"), ("pane/g > t", "next_tab")]);
        let now = Instant::now();
        r.feed(ctrl(Key::B), false, now, NONE);
        assert_eq!(
            r.feed(KeyChord::plain(Key::G), false, now, NONE),
            Resolution::Pending
        );
        assert_eq!(r.indicator().as_deref(), Some("[pane] G > …"));
        assert_eq!(
            r.feed(KeyChord::plain(Key::T), false, now, NONE),
            Resolution::Matched(Action::NextTab)
        );
        assert_eq!(r.active_table(), Some("pane"));
    }

    #[test]
    fn user_bindings_replace_and_unbind_defaults() {
        let (keymap, issues) = Keymap::with_overrides(
            &overrides(&[
                ("ctrl+k", "new_tab"),
                ("ctrl+p", "unbind"),
                ("ctrl+q > q", "exit_tui"),
            ]),
            Platform::Other,
        );
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(keymap.bindings_for(&Action::NewTab), vec!["Ctrl+K"]);
        assert!(keymap
            .bindings_for(&Action::ToggleCommandPalette)
            .is_empty());
        // The chain replaced the overlapping default `ctrl+q`.
        assert_eq!(keymap.bindings_for(&Action::ExitTui), vec!["Ctrl+Q > Q"]);
    }

    #[test]
    fn conflicts_between_user_bindings_are_reported_clearly() {
        let (keymap, issues) = Keymap::with_overrides(
            &overrides(&[
                ("ctrl+a", "new_tab"),
                ("ctrl+a > c", "close_tab"),
                ("ctrl+shift+a", "next_tab"),
            ]),
            Platform::Other,
        );
        assert_eq!(issues.len(), 1, "{issues:?}");
        let message = issues[0].to_string();
        assert!(
            message.contains("`ctrl+a > c` = `close_tab` ignored"),
            "{message}"
        );
        assert!(
            message.contains("overlaps `ctrl+a` = `new_tab`"),
            "{message}"
        );
        assert_eq!(keymap.bindings_for(&Action::NewTab), vec!["Ctrl+A"]);
        assert!(keymap.bindings_for(&Action::CloseTab).is_empty());
        // Different modifiers are not a conflict.
        assert_eq!(keymap.bindings_for(&Action::NextTab), vec!["Ctrl+Shift+A"]);

        // Two spellings of the same keys are a duplicate, not an overlap.
        let (_, issues) = Keymap::with_overrides(
            &overrides(&[("ctrl+t", "new_tab"), ("control+T", "close_tab")]),
            Platform::Other,
        );
        assert_eq!(issues.len(), 1);
        let message = issues[0].to_string();
        assert!(
            message.contains("`ctrl+t` = `new_tab` ignored"),
            "{message}"
        );
        assert!(
            message.contains("already bound by `control+T` = `close_tab`"),
            "{message}"
        );
        // Same table name scopes the check: a table binding never conflicts with root.
        let (_, issues) = Keymap::with_overrides(
            &overrides(&[
                ("ctrl+b", "enter_table:pane"),
                ("pane/ctrl+b", "exit_table"),
            ]),
            Platform::Other,
        );
        assert!(issues.is_empty(), "{issues:?}");
    }

    #[test]
    fn invalid_entries_are_reported_and_skipped() {
        let (keymap, issues) = Keymap::with_overrides(
            &overrides(&[
                ("ctrl+t", "new_tab"),
                ("ctrl+zz", "new_tab"),
                ("ctrl+y", "teleport"),
                ("ctrl+g", "enter_table:ghost"),
            ]),
            Platform::Other,
        );
        assert_eq!(keymap.bindings_for(&Action::NewTab), vec!["Ctrl+T"]);
        let messages: Vec<String> = issues.iter().map(|i| i.to_string()).collect();
        assert_eq!(messages.len(), 3, "{messages:?}");
        assert!(
            messages.iter().any(|m| m.contains("unknown key `zz`")),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("unknown action `teleport`")),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.contains("no `ghost/…` bindings exist")),
            "{messages:?}"
        );
    }
}
