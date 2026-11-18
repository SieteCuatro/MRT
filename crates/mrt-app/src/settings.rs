//! The settings window (`:settings`, `Ctrl+,`, the Command Palette, the sidebar): a
//! searchable form over the user's `config.toml`, plus the keybindings with their
//! conflicts. Saving edits only the changed keys with `toml_edit`, so comments and
//! everything else in the file stay, and the config watcher applies the file the same
//! way as a change made by hand.

use crate::app::MrtGuiApp;
use crate::keymap::{Action, Keymap, Platform, PALETTE_ACTIONS};
use crate::theme::Palette;
use egui::RichText;
use mrt_ai::keys::{KeySource, API_KEYS};
use mrt_core::config::{MrtConfig, KNOWN_AI_PROVIDERS};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// How a setting is edited.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FieldKind {
    Bool,
    Float { min: f64, max: f64 },
    Int { min: i64, max: i64 },
    /// Free text; empty removes the key (the default applies).
    Text { hint: &'static str },
    /// One of the listed values.
    Choice(&'static [&'static str]),
    /// `auto` or one of the discovered shells.
    Shell,
}

/// One setting in the window.
#[derive(Debug, Clone, Copy)]
pub struct SettingDef {
    /// Dotted config key, `section.name`.
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: FieldKind,
}

/// The settings the window edits, grouped by section (the key's prefix).
pub const SETTINGS: &[SettingDef] = &[
    SettingDef { key: "terminal.font_size", label: "Font size", help: "Text size of the window and terminal, 8 to 40.", kind: FieldKind::Float { min: 8.0, max: 40.0 } },
    SettingDef { key: "terminal.font_family", label: "Font", help: "A font family name (\"JetBrains Mono\") or a font file path. Empty uses the default.", kind: FieldKind::Text { hint: "default" } },
    SettingDef { key: "terminal.theme", label: "Theme", help: "graphite-calm, high-contrast, or the path of a theme file (MRT, Windows Terminal, Alacritty, Ghostty, iTerm2).", kind: FieldKind::Text { hint: "graphite-calm" } },
    SettingDef { key: "terminal.default_shell", label: "Default shell", help: "The shell new tabs use.", kind: FieldKind::Shell },
    SettingDef { key: "terminal.persistent_shell", label: "Keep one shell per tab", help: "cd, variables and activated environments carry over between commands.", kind: FieldKind::Bool },
    SettingDef { key: "terminal.restore_layout", label: "Restore tabs at start", help: "Reopen the tabs, splits and windows of the last session.", kind: FieldKind::Bool },
    SettingDef { key: "terminal.keep_running_on_close", label: "Keep running when closed", help: "Closing the window with work running keeps MRT in the background.", kind: FieldKind::Bool },
    SettingDef { key: "terminal.session_server", label: "Session host", help: "Run commands in a separate process that survives the window closing or crashing. Takes effect at the next start.", kind: FieldKind::Bool },
    SettingDef { key: "terminal.auto_detect_structured_output", label: "Detect tables and JSON", help: "Show JSON, CSV and Markdown output as trees and tables.", kind: FieldKind::Bool },
    SettingDef { key: "terminal.max_block_output_mb", label: "Output kept per block (MiB)", help: "Older output is dropped from the front; 0 keeps everything.", kind: FieldKind::Int { min: 0, max: 4096 } },
    SettingDef { key: "terminal.fast_native_output", label: "Fast native output (Windows)", help: "Run plain native programs typed in PowerShell through cmd.exe, which passes large output much faster.", kind: FieldKind::Bool },
    SettingDef { key: "safety.enabled", label: "Safety Gate", help: "Ask before risky commands run. Blocked patterns apply either way.", kind: FieldKind::Bool },
    SettingDef { key: "safety.require_approval_for_moderate", label: "Ask for Moderate commands", help: "Also ask before every Moderate command, not only process control.", kind: FieldKind::Bool },
    SettingDef { key: "safety.require_approval_for_critical", label: "Ask for Critical commands", help: "Ask before Critical commands (recommended).", kind: FieldKind::Bool },
    SettingDef { key: "safety.remote_approvals", label: "Approve from paired devices", help: "Let the phone companion (mrt share --web) answer the Safety Gate.", kind: FieldKind::Bool },
    SettingDef { key: "ai.provider", label: "AI provider", help: "Tried first for # suggestions and :assist; auto picks the first one with a key.", kind: FieldKind::Choice(KNOWN_AI_PROVIDERS) },
    SettingDef { key: "ai.model", label: "AI model", help: "Model for the chosen provider. Empty uses the provider's default.", kind: FieldKind::Text { hint: "provider default" } },
    SettingDef { key: "ai.ollama_endpoint", label: "Ollama address", help: "Base URL of a local Ollama (OLLAMA_HOST wins).", kind: FieldKind::Text { hint: "http://localhost:11434" } },
    SettingDef { key: "notifications.enabled", label: "Desktop notifications", help: "Notify when a long command finishes while you are elsewhere.", kind: FieldKind::Bool },
    SettingDef { key: "notifications.threshold_secs", label: "Notify after (seconds)", help: "Only commands that ran at least this long notify.", kind: FieldKind::Int { min: 1, max: 86_400 } },
];

const SECTION_TITLES: &[(&str, &str)] =
    &[("terminal", "Terminal"), ("safety", "Safety Gate"), ("ai", "AI"), ("notifications", "Notifications")];

/// A setting's value in the form.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Float(f64),
    Int(i64),
    Text(String),
}

impl SettingValue {
    /// The TOML value written for it; `None` removes the key (empty text).
    fn to_toml(&self) -> Option<toml_edit::Value> {
        Some(match self {
            SettingValue::Bool(b) => (*b).into(),
            SettingValue::Float(f) => (*f).into(),
            SettingValue::Int(i) => (*i).into(),
            SettingValue::Text(t) if t.trim().is_empty() => return None,
            SettingValue::Text(t) => t.trim().into(),
        })
    }
}

/// The value of `key` in `config`.
pub fn current_value(config: &MrtConfig, key: &str) -> SettingValue {
    use SettingValue::*;
    let t = &config.terminal;
    match key {
        "terminal.font_size" => Float(f64::from(t.effective_font_size())),
        "terminal.font_family" => Text(t.font_family.clone().unwrap_or_default()),
        "terminal.theme" => Text(t.theme.clone().unwrap_or_default()),
        "terminal.default_shell" => Text(t.default_shell.clone()),
        "terminal.persistent_shell" => Bool(t.persistent_shell),
        "terminal.restore_layout" => Bool(t.restore_layout),
        "terminal.keep_running_on_close" => Bool(t.keep_running_on_close),
        "terminal.session_server" => Bool(t.session_server),
        "terminal.auto_detect_structured_output" => Bool(t.auto_detect_structured_output),
        "terminal.max_block_output_mb" => Int(t.max_block_output_mb as i64),
        "terminal.fast_native_output" => Bool(t.fast_native_output),
        "safety.enabled" => Bool(config.safety.enabled),
        "safety.require_approval_for_moderate" => Bool(config.safety.require_approval_for_moderate),
        "safety.require_approval_for_critical" => Bool(config.safety.require_approval_for_critical),
        "safety.remote_approvals" => Bool(config.safety.remote_approvals),
        "ai.provider" => Text(config.ai.provider.clone()),
        "ai.model" => Text(config.ai.model.clone().unwrap_or_default()),
        "ai.ollama_endpoint" => Text(config.ai.ollama_endpoint.clone().unwrap_or_default()),
        "notifications.enabled" => Bool(config.notifications.enabled),
        "notifications.threshold_secs" => Int(config.notifications.threshold_secs as i64),
        _ => Text(String::new()),
    }
}

/// One change to the config file: `[section] name = value`, or the key removed.
#[derive(Debug, Clone)]
pub struct ConfigEdit {
    pub section: String,
    pub name: String,
    pub value: Option<toml_edit::Value>,
}

impl ConfigEdit {
    fn setting(key: &str, value: Option<toml_edit::Value>) -> Self {
        let (section, name) = key.split_once('.').unwrap_or(("", key));
        Self { section: section.into(), name: name.into(), value }
    }
}

/// Applies `edits` to the text of a config file, keeping comments, formatting and
/// every other key. Tables are created as needed; removing the last key of a table
/// leaves the (empty) table.
pub fn apply_edits(text: &str, edits: &[ConfigEdit]) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("the config file is not valid TOML: {e}"))?;
    for edit in edits {
        let table = doc
            .entry(&edit.section)
            .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
            .as_table_like_mut()
            .ok_or_else(|| format!("`{}` in the config file is not a table", edit.section))?;
        match &edit.value {
            Some(value) => match table.get_mut(&edit.name).and_then(|item| item.as_value_mut()) {
                // Replace only the value: the key, its comments and spacing stay.
                Some(existing) => {
                    let decor = existing.decor().clone();
                    *existing = value.clone();
                    *existing.decor_mut() = decor;
                }
                None => {
                    table.insert(&edit.name, toml_edit::Item::Value(value.clone()));
                }
            },
            None => {
                table.remove(&edit.name);
            }
        }
    }
    Ok(doc.to_string())
}

/// Writes `edits` into the file at `path` (created with its folder when missing),
/// through a temporary file and a rename.
pub fn write_edits(path: &Path, edits: &[ConfigEdit]) -> Result<(), String> {
    let before = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let after = apply_edits(&before, edits)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, after).map_err(|e| format!("could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// Dotted keys set in a project `.mrt.toml` (they override the user file).
fn project_keys(path: Option<&PathBuf>) -> BTreeSet<String> {
    let Some(text) = path.and_then(|p| std::fs::read_to_string(p).ok()) else { return BTreeSet::new() };
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else { return BTreeSet::new() };
    let mut keys = BTreeSet::new();
    for (section, item) in doc.iter() {
        if let Some(inner) = item.as_table_like() {
            for (name, _) in inner.iter() {
                keys.insert(format!("{section}.{name}"));
            }
        }
    }
    keys
}

/// Whether `setting` matches the search `query` (label, key, help or section).
pub fn matches_query(setting: &SettingDef, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let section = setting.key.split('.').next().unwrap_or("");
    let title = SECTION_TITLES.iter().find(|(s, _)| *s == section).map(|(_, t)| *t).unwrap_or("");
    [setting.label, setting.key, setting.help, title].iter().any(|text| text.to_lowercase().contains(&query))
}

/// The settings window's state.
#[derive(Default)]
pub struct SettingsState {
    pub open: bool,
    pub query: String,
    /// The values in the form, by key.
    draft: BTreeMap<&'static str, SettingValue>,
    /// The values when the form was filled, to find what changed.
    loaded: BTreeMap<&'static str, SettingValue>,
    /// Keys set back to their default: removed from the file on save.
    reset: BTreeSet<&'static str>,
    /// Keys a project `.mrt.toml` sets (shown as overridden).
    project_keys: BTreeSet<String>,
    /// The result of the last save or keybinding change, and whether it failed.
    status: Option<(String, bool)>,
    new_binding_keys: String,
    new_binding_action: String,
    /// Give the search box the keyboard on the next frame (just opened).
    focus_search: bool,
    /// Where each of `mrt_ai::keys::API_KEYS` comes from, read on a background thread
    /// when the window opens (`None` until then).
    key_sources: std::sync::Arc<std::sync::Mutex<Option<Vec<KeySource>>>>,
    /// Keys typed into the API key fields, by variable name.
    key_inputs: HashMap<&'static str, String>,
}

impl SettingsState {
    /// Fills the form from `config` and opens the window.
    pub fn open_with(&mut self, config: &MrtConfig, project: Option<&PathBuf>) {
        self.loaded = SETTINGS.iter().map(|s| (s.key, current_value(config, s.key))).collect();
        self.draft = self.loaded.clone();
        self.reset.clear();
        self.project_keys = project_keys(project);
        self.status = None;
        self.focus_search = !self.open;
        self.open = true;
        self.refresh_key_sources();
    }

    /// Reads where every API key comes from, on a background thread (the keyring can
    /// be slow to answer).
    fn refresh_key_sources(&mut self) {
        let slot = self.key_sources.clone();
        std::thread::spawn(move || {
            let sources = API_KEYS.iter().map(|(name, _)| mrt_ai::keys::key_source(name)).collect();
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(sources);
            }
        });
    }

    /// The edits that turn the file into the form: changed keys and reset ones.
    pub fn pending_edits(&self) -> Vec<ConfigEdit> {
        SETTINGS
            .iter()
            .filter_map(|s| {
                if self.reset.contains(s.key) {
                    return Some(ConfigEdit::setting(s.key, None));
                }
                let value = self.draft.get(s.key)?;
                (self.loaded.get(s.key) != Some(value)).then(|| ConfigEdit::setting(s.key, value.to_toml()))
            })
            .collect()
    }

    pub fn set(&mut self, key: &'static str, value: SettingValue) {
        self.reset.remove(key);
        self.draft.insert(key, value);
    }

    fn reset_to_default(&mut self, key: &'static str) {
        self.draft.insert(key, current_value(&MrtConfig::default(), key));
        self.reset.insert(key);
    }
}

/// Checks a new binding against the current ones: `None` when it can be added,
/// otherwise why not.
pub fn binding_problem(existing: &HashMap<String, String>, keys: &str, action: &str) -> Option<String> {
    let keys = keys.trim();
    if keys.is_empty() {
        return Some("type the keys, for example ctrl+shift+t".into());
    }
    let mut overrides = existing.clone();
    overrides.insert(keys.to_string(), action.to_string());
    let (_, issues) = Keymap::with_overrides(&overrides, Platform::current());
    issues.iter().map(|issue| issue.to_string()).find(|text| text.contains(&format!("`{keys}`")) || text.contains(keys))
}

impl MrtGuiApp {
    /// Opens the settings window with the current configuration.
    pub(crate) fn open_settings(&mut self) {
        let project = self.config_state.sources.project.clone();
        self.settings.open_with(&self.config_state.config, project.as_ref());
    }

    /// Writes `edits` to the user config file and reloads it.
    fn save_config_edits(&mut self, edits: &[ConfigEdit]) -> Result<PathBuf, String> {
        let path = self.config_state.sources.user.clone().ok_or("no user config file location (set MRT_CONFIG)")?;
        write_edits(&path, edits)?;
        self.config_state.reload();
        Ok(path)
    }

    /// The settings window, drawn in the main window while it is open.
    pub(crate) fn render_settings_window(&mut self, ctx: &egui::Context) {
        if !self.settings.open {
            return;
        }
        let mut open = true;
        let mut save = false;
        let mut revert = false;
        let mut binding_edit: Option<ConfigEdit> = None;
        let shells: Vec<String> = std::iter::once("auto".to_string()).chain(self.shells.iter().map(|s| s.id.clone())).collect();
        let entries = self.key_resolver.keymap().entries();
        let issues = self.keymap_issues.clone();
        let user_path = self.config_state.sources.user.clone();
        let user_bindings = self.config_state.config.keybindings().clone();
        let state = &mut self.settings;
        egui::Window::new("Settings")
            .id(egui::Id::new("mrt_settings_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([640.0, 620.0])
            .show(ctx, |ui| {
                let search = ui.add(
                    egui::TextEdit::singleline(&mut state.query)
                        .hint_text("Search settings and keybindings")
                        .desired_width(f32::INFINITY),
                );
                if std::mem::take(&mut state.focus_search) {
                    search.request_focus();
                }
                ui.add_space(6.0);
                let query = state.query.clone();
                egui::ScrollArea::vertical().max_height(ui.available_height() - 48.0).auto_shrink([false, false]).show(ui, |ui| {
                    for (section, title) in SECTION_TITLES {
                        let rows: Vec<&SettingDef> =
                            SETTINGS.iter().filter(|s| s.key.starts_with(&format!("{section}.")) && matches_query(s, &query)).collect();
                        if rows.is_empty() {
                            continue;
                        }
                        ui.add_space(6.0);
                        ui.label(RichText::new(*title).size(15.0).strong().color(Palette::TEXT_PRIMARY()));
                        ui.separator();
                        for setting in rows {
                            setting_row(ui, state, setting, &shells);
                        }
                    }
                    api_keys_section(ui, state, &query);
                    keybindings_section(ui, state, &query, &entries, &issues, &user_bindings, &mut binding_edit);
                });
                ui.separator();
                ui.horizontal(|ui| {
                    let dirty = !state.pending_edits().is_empty();
                    if ui.add_enabled(dirty, egui::Button::new("Save")).clicked() {
                        save = true;
                    }
                    if ui.add_enabled(dirty, egui::Button::new("Revert")).clicked() {
                        revert = true;
                    }
                    match &state.status {
                        Some((text, failed)) => {
                            ui.label(RichText::new(text).color(if *failed { Palette::ACCENT_RED() } else { Palette::ACCENT_GREEN() }));
                        }
                        None => {
                            let file = user_path.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "no config file".into());
                            ui.label(RichText::new(file).small().color(Palette::TEXT_MUTED()));
                        }
                    }
                });
            });
        if revert {
            self.settings.draft = self.settings.loaded.clone();
            self.settings.reset.clear();
        }
        if save {
            let edits = self.settings.pending_edits();
            match self.save_config_edits(&edits) {
                Ok(path) => {
                    let project = self.config_state.sources.project.clone();
                    self.settings.open_with(&self.config_state.config, project.as_ref());
                    self.settings.status = Some((format!("Saved to {}", path.display()), false));
                }
                Err(error) => self.settings.status = Some((error, true)),
            }
        }
        if let Some(edit) = binding_edit {
            match self.save_config_edits(&[edit]) {
                Ok(_) => {
                    self.apply_config_side_effects();
                    self.settings.new_binding_keys.clear();
                    self.settings.status = Some(("Keybindings saved".into(), false));
                }
                Err(error) => self.settings.status = Some((error, true)),
            }
        }
        self.settings.open &= open;
    }
}

/// One setting: label, key and help on the left, its control on the right.
fn setting_row(ui: &mut egui::Ui, state: &mut SettingsState, setting: &SettingDef, shells: &[String]) {
    let key = setting.key;
    let overridden = state.project_keys.contains(key);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_width((ui.available_width() - 230.0).max(160.0));
            ui.label(RichText::new(setting.label).strong().color(Palette::TEXT_PRIMARY()));
            ui.label(RichText::new(key).monospace().small().color(Palette::TEXT_MUTED()));
            ui.label(RichText::new(setting.help).small().color(Palette::TEXT_SECONDARY()));
            if overridden {
                ui.label(RichText::new("Set in this folder's .mrt.toml, which wins over this value.").small().color(Palette::ACCENT_AMBER()));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("↺").on_hover_text("Back to the default").clicked() {
                state.reset_to_default(key);
            }
            let Some(mut value) = state.draft.get(key).cloned() else { return };
            let changed = match (&mut value, setting.kind) {
                (SettingValue::Bool(b), _) => ui.checkbox(b, "").changed(),
                (SettingValue::Float(f), FieldKind::Float { min, max }) => {
                    ui.add(egui::DragValue::new(f).range(min..=max).speed(0.5).fixed_decimals(1)).changed()
                }
                (SettingValue::Int(i), FieldKind::Int { min, max }) => ui.add(egui::DragValue::new(i).range(min..=max)).changed(),
                (SettingValue::Text(t), FieldKind::Choice(options)) => choice(ui, key, t, options.iter().map(|s| s.to_string())),
                (SettingValue::Text(t), FieldKind::Shell) => choice(ui, key, t, shells.iter().cloned()),
                (SettingValue::Text(t), FieldKind::Text { hint }) => {
                    ui.add(egui::TextEdit::singleline(t).hint_text(hint).desired_width(210.0)).changed()
                }
                _ => false,
            };
            if changed {
                state.set(key, value);
            }
        });
    });
    ui.add_space(4.0);
}

fn choice(ui: &mut egui::Ui, key: &str, value: &mut String, options: impl Iterator<Item = String>) -> bool {
    let mut changed = false;
    egui::ComboBox::from_id_salt(("settings_choice", key)).width(200.0).selected_text(value.clone()).show_ui(ui, |ui| {
        for option in options {
            if ui.selectable_label(*value == option, &option).clicked() && *value != option {
                *value = option;
                changed = true;
            }
        }
    });
    changed
}

/// API keys: where each provider's key comes from, a field to store one in the OS
/// keyring, and Remove for a stored one. Keys are written right away, never to the
/// config file.
fn api_keys_section(ui: &mut egui::Ui, state: &mut SettingsState, query: &str) {
    let query = query.trim().to_lowercase();
    let rows: Vec<usize> = (0..API_KEYS.len())
        .filter(|&i| {
            let (name, provider) = API_KEYS[i];
            query.is_empty() || "api keys keyring secrets".contains(&query) || name.to_lowercase().contains(&query) || provider.to_lowercase().contains(&query)
        })
        .collect();
    if rows.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.label(RichText::new("API keys").size(15.0).strong().color(Palette::TEXT_PRIMARY()));
    ui.separator();
    ui.label(
        RichText::new(format!(
            "Stored {} for MRT started without your shell's environment. An environment variable with the same name wins.",
            mrt_ai::keys::keyring_name()
        ))
        .small()
        .color(Palette::TEXT_SECONDARY()),
    );
    let sources = state.key_sources.lock().ok().and_then(|s| s.clone());
    let mut changed = false;
    for i in rows {
        let (name, provider) = API_KEYS[i];
        let source = sources.as_ref().and_then(|s| s.get(i).copied());
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width((ui.available_width() - 300.0).max(160.0));
                ui.label(RichText::new(provider).strong().color(Palette::TEXT_PRIMARY()));
                ui.label(RichText::new(name).monospace().small().color(Palette::TEXT_MUTED()));
                let (text, color) = match source {
                    None => ("checking…", Palette::TEXT_MUTED()),
                    Some(KeySource::Missing) => ("not set", Palette::TEXT_MUTED()),
                    Some(source) => (source.label(), Palette::ACCENT_GREEN()),
                };
                ui.label(RichText::new(text).small().color(color));
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if source == Some(KeySource::Keyring) && ui.small_button("Remove").clicked() {
                    state.status = Some(match mrt_ai::keys::delete_api_key(name) {
                        Ok(()) => (format!("Removed {name} from the keyring"), false),
                        Err(error) => (error, true),
                    });
                    changed = true;
                }
                let input = state.key_inputs.entry(name).or_default();
                let save = ui.add_enabled(!input.trim().is_empty(), egui::Button::new("Save"));
                ui.add(egui::TextEdit::singleline(input).password(true).hint_text("paste a key").desired_width(170.0));
                if save.clicked() {
                    state.status = Some(match mrt_ai::keys::store_api_key(name, input) {
                        Ok(()) => (format!("Stored {name} {}", mrt_ai::keys::keyring_name()), false),
                        Err(error) => (error, true),
                    });
                    input.clear();
                    changed = true;
                }
            });
        });
        ui.add_space(4.0);
    }
    if changed {
        state.refresh_key_sources();
    }
}

/// Keybindings: problems in the user's `[keybindings]`, every binding (user ones can
/// be removed), and a row to add one. Changes are written right away.
fn keybindings_section(
    ui: &mut egui::Ui,
    state: &mut SettingsState,
    query: &str,
    entries: &[crate::keymap::BindingInfo],
    issues: &[String],
    user_bindings: &HashMap<String, String>,
    edit: &mut Option<ConfigEdit>,
) {
    let query = query.trim().to_lowercase();
    let shown: Vec<_> = entries
        .iter()
        .filter(|e| query.is_empty() || [&e.keys, &e.action, &e.title].iter().any(|t| t.to_lowercase().contains(&query)) || "keybindings shortcuts keys".contains(&query))
        .collect();
    if shown.is_empty() && !query.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.label(RichText::new("Keybindings").size(15.0).strong().color(Palette::TEXT_PRIMARY()));
    ui.separator();
    for issue in issues {
        ui.label(RichText::new(format!("⚠ {issue}")).color(Palette::ACCENT_RED()));
    }
    egui::Grid::new("settings_keybindings").striped(true).num_columns(3).show(ui, |ui| {
        for entry in shown {
            ui.label(RichText::new(&entry.keys).monospace().color(Palette::TEXT_PRIMARY()));
            ui.label(RichText::new(&entry.title).color(Palette::TEXT_BODY()))
                .on_hover_text(&entry.action);
            if entry.user {
                if ui.small_button("Remove").clicked() {
                    *edit = Some(ConfigEdit { section: "keybindings".into(), name: entry.written.clone(), value: None });
                }
            } else {
                ui.label(RichText::new("default").small().color(Palette::TEXT_MUTED()));
            }
            ui.end_row();
        }
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut state.new_binding_keys).hint_text("ctrl+shift+t").desired_width(140.0));
        if state.new_binding_action.is_empty() {
            state.new_binding_action = Action::NewTab.name();
        }
        let selected = Action::parse(&state.new_binding_action).map(|a| a.title()).unwrap_or_default();
        egui::ComboBox::from_id_salt("settings_new_binding_action").width(220.0).selected_text(selected).show_ui(ui, |ui| {
            for action in PALETTE_ACTIONS.iter().chain([Action::ToggleCommandPalette, Action::ExitTui].iter()) {
                ui.selectable_value(&mut state.new_binding_action, action.name(), action.title());
            }
        });
        let problem = binding_problem(user_bindings, &state.new_binding_keys, &state.new_binding_action);
        let add = ui.add_enabled(problem.is_none(), egui::Button::new("Add"));
        if add.clicked() {
            *edit = Some(ConfigEdit {
                section: "keybindings".into(),
                name: state.new_binding_keys.trim().to_string(),
                value: Some(state.new_binding_action.as_str().into()),
            });
        }
        if let Some(problem) = problem.filter(|_| !state.new_binding_keys.trim().is_empty()) {
            ui.label(RichText::new(problem).small().color(Palette::ACCENT_RED()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_keep_comments_and_other_keys() {
        let text = "# my config\n[terminal]\n# big text\nfont_size = 14.0\ntheme = \"high-contrast\"\n\n[ai]\nprovider = \"ollama\"\n";
        let edits = vec![
            ConfigEdit::setting("terminal.font_size", Some(16.5.into())),
            ConfigEdit::setting("terminal.theme", None),
            ConfigEdit::setting("safety.enabled", Some(false.into())),
            ConfigEdit { section: "keybindings".into(), name: "ctrl+shift+t".into(), value: Some("new_tab".into()) },
        ];
        let out = apply_edits(text, &edits).unwrap();
        assert!(out.starts_with("# my config\n[terminal]\n# big text\nfont_size = 16.5\n"), "{out}");
        assert!(!out.contains("theme"), "{out}");
        assert!(out.contains("provider = \"ollama\""));
        let config = mrt_core::config::parse_config_str(&out).unwrap();
        assert_eq!(config.terminal.font_size, 16.5);
        assert!(!config.safety.enabled);
        assert_eq!(config.keybindings().get("ctrl+shift+t").map(String::as_str), Some("new_tab"));
        assert!(apply_edits("not = [valid", &edits).is_err());
    }

    #[test]
    fn the_form_writes_only_what_changed() {
        mrt_ai::keys::set_secret_store(std::sync::Arc::new(mrt_ai::keys::MemoryStore::default()));
        let dir = std::env::temp_dir().join(format!("mrt-settings-{}", uuid::Uuid::new_v4()));
        let path = dir.join("nested").join("config.toml");
        let mut state = SettingsState::default();
        state.open_with(&MrtConfig::default(), None);
        assert!(state.pending_edits().is_empty(), "nothing changed yet");
        state.set("terminal.font_size", SettingValue::Float(18.0));
        state.set("ai.model", SettingValue::Text("  llama3.2 ".into()));
        state.reset_to_default("safety.enabled");
        let edits = state.pending_edits();
        assert_eq!(edits.len(), 3, "{edits:?}");
        write_edits(&path, &edits).unwrap();
        let config = mrt_core::config::parse_config_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.terminal.font_size, 18.0);
        assert_eq!(config.ai.model.as_deref(), Some("llama3.2"));
        assert!(config.safety.enabled);
        // Every listed setting reads a value of the kind its control edits.
        for setting in SETTINGS {
            let value = current_value(&config, setting.key);
            let fits = matches!(
                (&value, setting.kind),
                (SettingValue::Bool(_), FieldKind::Bool)
                    | (SettingValue::Float(_), FieldKind::Float { .. })
                    | (SettingValue::Int(_), FieldKind::Int { .. })
                    | (SettingValue::Text(_), FieldKind::Text { .. } | FieldKind::Choice(_) | FieldKind::Shell)
            );
            assert!(fits, "{} reads {value:?}", setting.key);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn saving_from_the_window_applies_the_config() {
        mrt_ai::keys::set_secret_store(std::sync::Arc::new(mrt_ai::keys::MemoryStore::default()));
        let dir = std::env::temp_dir().join(format!("mrt-settings-app-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "# mine
[terminal]
font_size = 13.0
").unwrap();
        let mut app = MrtGuiApp::new();
        app.config_state.sources = mrt_core::config::ConfigSources { user: Some(path.clone()), project: None };
        app.config_state.reload();
        app.submit_input(Some(":settings".into()), false);
        assert!(app.settings.open);

        app.settings.set("terminal.font_size", SettingValue::Float(20.0));
        let edits = app.settings.pending_edits();
        app.save_config_edits(&edits).unwrap();
        assert_eq!(app.config_state.config.terminal.font_size, 20.0);
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("# mine"));

        let binding = ConfigEdit { section: "keybindings".into(), name: "ctrl+shift+y".into(), value: Some("new_window".into()) };
        app.save_config_edits(&[binding]).unwrap();
        app.apply_config_side_effects();
        assert_eq!(app.key_resolver.keymap().bindings_for(&Action::NewWindow).len(), 1);
        assert!(app.key_resolver.keymap().entries().iter().any(|e| e.user && e.action == "new_window"));

        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| app.render_settings_window(ctx));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn search_and_new_bindings() {
        let font = SETTINGS.iter().find(|s| s.key == "terminal.font_size").unwrap();
        assert!(matches_query(font, "FONT"));
        assert!(matches_query(font, "terminal"));
        assert!(!matches_query(font, "ollama"));
        let none = HashMap::new();
        assert_eq!(binding_problem(&none, "ctrl+shift+t", "new_tab"), None);
        assert_eq!(binding_problem(&none, "ctrl+k", "new_tab"), None, "a user binding replaces a default one");
        assert!(binding_problem(&none, "ctrl+nonsense", "new_tab").is_some());
        assert!(binding_problem(&none, "", "new_tab").is_some());
    }
}
