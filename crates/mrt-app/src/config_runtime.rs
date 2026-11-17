//! Applies MRT's configuration file (`mrt_core::config`) to the running GUI:
//! initial load, debounced reload on change, and the in-window notice for errors.

use crate::app::{AsyncWorkerEvent, MrtGuiApp};
use crate::theme::Palette;
use egui::RichText;
use mrt_core::config::{ConfigSources, ConfigWatcher, MrtConfig};
use mrt_core::{load_config, ShellProfile};
use std::time::{Duration, Instant};

/// How often the config files are checked for changes.
const CONFIG_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// How long a changed file must stay unchanged before MRT reloads it.
const CONFIG_DEBOUNCE: Duration = Duration::from_millis(300);

/// A message about the configuration shown at the top of the window.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigNotice {
    pub text: String,
    pub is_error: bool,
}

/// Runtime state for the configuration file.
pub struct ConfigState {
    pub config: MrtConfig,
    pub sources: ConfigSources,
    watcher: Option<ConfigWatcher>,
    pub notice: Option<ConfigNotice>,
    /// Set when the font size must be re-applied to the egui context.
    pub style_dirty: bool,
    /// Fonts last installed in the egui context; reinstalled when the config changes them.
    applied_fonts: Option<crate::fonts::FontPrefs>,
}

impl ConfigState {
    /// Loads the config from `sources`. On error MRT starts with the defaults and
    /// shows the error. `watch` is off in unit tests.
    pub fn load(sources: ConfigSources, watch: bool) -> Self {
        let watcher = watch
            .then(|| ConfigWatcher::new(sources.paths(), CONFIG_POLL_INTERVAL, CONFIG_DEBOUNCE));
        let mut state = Self {
            config: MrtConfig::default(),
            sources,
            watcher,
            notice: None,
            style_dirty: true,
            applied_fonts: None,
        };
        state.reload();
        state
    }

    /// Re-reads the files. Returns `true` when the active config changed. An invalid
    /// file keeps the last good config and sets an error notice.
    pub fn reload(&mut self) -> bool {
        match load_config(&self.sources) {
            Ok(loaded) => {
                let warnings = loaded.config.warnings();
                self.notice = (!warnings.is_empty()).then(|| ConfigNotice {
                    text: format!("Config: {}", warnings.join(" ")),
                    is_error: false,
                });
                if loaded.config == self.config {
                    return false;
                }
                self.config = loaded.config;
                self.style_dirty = true;
                true
            }
            Err(error) => {
                self.notice = Some(ConfigNotice {
                    text: format!("Config not applied — keeping the last good settings. {error}"),
                    is_error: true,
                });
                false
            }
        }
    }

    /// Checks the watched files; reloads once a change has settled.
    pub fn poll(&mut self, now: Instant) -> bool {
        let changed = self.watcher.as_mut().is_some_and(|w| w.poll(now));
        changed && self.reload()
    }
}

/// Index into `shells` of `terminal.default_shell`, falling back to the first shell.
pub fn default_shell_index(shells: &[ShellProfile], config: &MrtConfig) -> usize {
    let Some(wanted) = config.terminal.requested_shell() else {
        return 0;
    };
    let wanted = wanted.to_ascii_lowercase();
    shells
        .iter()
        .position(|s| {
            let label = s.kind.short_label();
            label == wanted
                || label.split_whitespace().next() == Some(wanted.as_str())
                || s.id.eq_ignore_ascii_case(&wanted)
        })
        .unwrap_or(0)
}

impl MrtGuiApp {
    /// Pushes the parts of the config that live outside the GUI state: AI provider
    /// preferences and the MCP servers to connect (already-known names are skipped).
    pub(crate) fn apply_config_side_effects(&mut self) {
        let config = &self.config_state.config;
        // `key = ...` of [[actions]] entries are bindings to `run:<name>`; explicit
        // [keybindings] entries win.
        let mut bindings = config.keybindings().clone();
        for action in &config.actions {
            if let Some(key) = action.key.as_deref().filter(|k| !k.trim().is_empty()) {
                bindings.entry(key.trim().to_string()).or_insert_with(|| format!("run:{}", action.id()));
            }
        }
        let (key_resolver, keymap_issues) = crate::app::load_key_resolver(&bindings);
        self.key_resolver = key_resolver;
        self.keymap_issues = keymap_issues;
        // Unit tests opt in to persistent shells themselves.
        if !cfg!(test) {
            self.shell_pool.set_enabled(config.terminal.persistent_shell);
        }
        mrt_core::set_max_block_output_bytes(config.terminal.max_block_output_bytes());
        let notifications = config.notifications();
        self.notifier.settings = crate::notifications::NotificationSettings {
            enabled: notifications.enabled,
            threshold_secs: notifications.threshold_secs,
        };

        let ai = &self.config_state.config.ai;
        mrt_ai::set_ai_preferences(mrt_ai::AiPreferences {
            provider: Some(ai.provider.clone()),
            model: ai.model.clone(),
            ollama_endpoint: ai.ollama_endpoint.clone(),
        });

        let to_connect: Vec<_> = self
            .config_state
            .config
            .startup_mcp_servers()
            .filter(|entry| !self.mcp_host.servers.iter().any(|s| s.config.name == entry.name))
            .cloned()
            .collect();
        for entry in to_connect {
            let tx = self.tx.clone();
            let mut host = self.mcp_host.clone();
            self.runtime.spawn(async move {
                if let Ok(status) = host
                    .connect_external_stdio_server(entry.name, entry.command, entry.args)
                    .await
                {
                    let _ = tx.send(AsyncWorkerEvent::ExternalMcpConnected(status));
                }
            });
        }
    }

    /// Called every frame: reloads the config when its files change and re-applies
    /// the font size when needed.
    pub(crate) fn update_config(&mut self, ctx: &egui::Context) {
        if self.config_state.poll(Instant::now()) {
            self.apply_config_side_effects();
        }
        let prefs = crate::fonts::FontPrefs {
            family: self.config_state.config.terminal.font_family.clone(),
            fallback: self.config_state.config.terminal.font_fallback.clone(),
        };
        if self.config_state.applied_fonts.as_ref() != Some(&prefs) {
            let (fonts, report) = crate::fonts::build_font_definitions(&prefs);
            ctx.set_fonts(fonts);
            if !report.missing.is_empty() {
                self.config_state.notice = Some(ConfigNotice {
                    text: format!(
                        "Config: font not found: {}. Give a family name installed on this computer or a .ttf/.otf path.",
                        report.missing.join(", ")
                    ),
                    is_error: false,
                });
            }
            self.config_state.applied_fonts = Some(prefs);
        }
        if self.config_state.style_dirty {
            self.config_state.style_dirty = false;
            let size = self.config_state.config.terminal.effective_font_size();
            ctx.style_mut(|style| {
                style
                    .text_styles
                    .insert(egui::TextStyle::Monospace, egui::FontId::monospace(size));
            });
        }
    }

    /// The dismissible config notice under the header, when there is one.
    pub(crate) fn render_config_notice(&mut self, ctx: &egui::Context) {
        let Some(notice) = self.config_state.notice.clone() else {
            return;
        };
        let color = if notice.is_error {
            Palette::ACCENT_RED
        } else {
            Palette::ACCENT_AMBER
        };
        let mut dismiss = false;
        egui::TopBottomPanel::top("mrt_config_notice")
            .frame(
                egui::Frame::none()
                    .fill(Palette::BG_SURFACE)
                    .inner_margin(egui::Margin::symmetric(20.0, 6.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("⚠").color(color));
                    ui.add(egui::Label::new(RichText::new(&notice.text).color(color)).wrap());
                    if ui.small_button("Dismiss").clicked() {
                        dismiss = true;
                    }
                });
            });
        if dismiss {
            self.config_state.notice = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mrt_core::config::{parse_config_str, PROJECT_CONFIG_FILE};
    use mrt_core::discover_shells;

    #[test]
    fn invalid_reload_keeps_last_good_config() {
        let dir = std::env::temp_dir().join(format!("mrt-app-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let project = dir.join(PROJECT_CONFIG_FILE);
        std::fs::write(&project, "[terminal]\nfont_size = 17.0\n").unwrap();
        let sources = ConfigSources {
            user: None,
            project: Some(project.clone()),
        };
        let mut state = ConfigState::load(sources, false);
        assert_eq!(state.config.terminal.font_size, 17.0);
        assert!(state.notice.is_none());

        std::fs::write(&project, "[terminal\nfont_size = 9.0\n").unwrap();
        assert!(!state.reload());
        assert_eq!(state.config.terminal.font_size, 17.0, "last good config kept");
        let notice = state.notice.clone().expect("error notice");
        assert!(notice.is_error);
        assert!(notice.text.contains(PROJECT_CONFIG_FILE));

        std::fs::write(&project, "[terminal]\nfont_size = 19.0\n").unwrap();
        assert!(state.reload());
        assert_eq!(state.config.terminal.font_size, 19.0);
        assert!(state.notice.is_none(), "fixing the file clears the error");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn invalid_file_at_startup_uses_defaults() {
        let dir = std::env::temp_dir().join(format!("mrt-app-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("config.toml");
        std::fs::write(&user, "font_size = = 3").unwrap();
        let state = ConfigState::load(
            ConfigSources {
                user: Some(user),
                project: None,
            },
            false,
        );
        assert_eq!(state.config, MrtConfig::default());
        assert!(state.notice.as_ref().is_some_and(|n| n.is_error));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn default_shell_matches_labels() {
        let shells = discover_shells();
        let auto = parse_config_str("").unwrap();
        assert_eq!(default_shell_index(&shells, &auto), 0);
        let missing = parse_config_str("[terminal]\ndefault_shell = \"no-such-shell\"").unwrap();
        assert_eq!(default_shell_index(&shells, &missing), 0);
        let last = shells.last().unwrap();
        let first_word = last.kind.short_label().split_whitespace().next().unwrap();
        let cfg = parse_config_str(&format!("[terminal]\ndefault_shell = \"{first_word}\"")).unwrap();
        assert_eq!(
            shells[default_shell_index(&shells, &cfg)].kind,
            last.kind,
            "\"cmd\" also matches \"cmd (batch)\""
        );
    }
}
