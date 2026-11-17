//! MRT configuration file: schema, discovery, layered merge, and change watching.
//!
//! MRT reads two optional TOML files and merges them in this order (later wins):
//!
//! 1. The user file: `$MRT_CONFIG` if set, otherwise `~/.config/mrt/config.toml`
//!    (`$XDG_CONFIG_HOME/mrt/config.toml` when that is set) on Linux/macOS and
//!    `%APPDATA%\mrt\config.toml` on Windows.
//! 2. The project file: `.mrt.toml` in the directory MRT was started from.
//!
//! Every field is optional; anything left out keeps its built-in default, so a
//! missing, empty or partial file is always valid. A file that fails to parse is
//! reported as a [`ConfigError`] and the caller keeps its last good configuration.

use crate::safety::{RiskLevel, SafetyAssessment};
use serde::Deserialize;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// File name of the per-project configuration, looked up in the working directory.
pub const PROJECT_CONFIG_FILE: &str = ".mrt.toml";
/// Environment variable that points MRT at a different user configuration file.
pub const CONFIG_PATH_ENV_VAR: &str = "MRT_CONFIG";
/// Smallest and largest accepted `terminal.font_size`; values outside are clamped.
pub const FONT_SIZE_RANGE: (f32, f32) = (8.0, 40.0);
/// Terminal font size used when the config does not set one.
pub const DEFAULT_FONT_SIZE: f32 = 13.0;
/// Theme names MRT ships. `dark` and `default` are aliases of `graphite-calm`.
pub const KNOWN_THEMES: &[&str] = &["graphite-calm", "dark", "default"];
/// AI provider names accepted by `ai.provider`.
pub const KNOWN_AI_PROVIDERS: &[&str] = &[
    "auto",
    "gemini",
    "anthropic",
    "openai",
    "xai",
    "grok",
    "deepseek",
    "openrouter",
    "ollama",
    "offline",
];

/// The fully resolved MRT configuration (defaults filled in).
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct MrtConfig {
    pub terminal: TerminalConfig,
    pub safety: SafetyConfig,
    pub ai: AiConfig,
    pub mcp: McpConfig,
    pub notifications: NotificationsConfig,
    /// `[keybindings]`: binding (e.g. `"ctrl+t"`) -> action name. Parsed only; the
    /// keymap module interprets it. Read it with [`MrtConfig::keybindings`].
    pub keybindings: HashMap<String, String>,
}

/// `[notifications]`: notify when a long-running command finishes.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct NotificationsConfig {
    pub enabled: bool,
    /// Commands running at least this many seconds trigger a notification.
    pub threshold_secs: u64,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_secs: 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct TerminalConfig {
    /// `"auto"` (first discovered shell) or a shell label such as `pwsh`, `bash`, `cmd`.
    pub default_shell: String,
    pub font_size: f32,
    /// Optional theme name; see [`KNOWN_THEMES`].
    pub theme: Option<String>,
    pub scrollback_blocks: usize,
    pub auto_detect_structured_output: bool,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            default_shell: "auto".to_string(),
            font_size: DEFAULT_FONT_SIZE,
            theme: None,
            scrollback_blocks: 500,
            auto_detect_structured_output: true,
        }
    }
}

impl TerminalConfig {
    /// The font size clamped to [`FONT_SIZE_RANGE`] (NaN falls back to the default).
    pub fn effective_font_size(&self) -> f32 {
        if self.font_size.is_nan() {
            return DEFAULT_FONT_SIZE;
        }
        self.font_size.clamp(FONT_SIZE_RANGE.0, FONT_SIZE_RANGE.1)
    }

    /// `None` for `"auto"` or an empty value, otherwise the requested shell label.
    pub fn requested_shell(&self) -> Option<&str> {
        let shell = self.default_shell.trim();
        if shell.is_empty() || shell.eq_ignore_ascii_case("auto") {
            None
        } else {
            Some(shell)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct SafetyConfig {
    /// When `false` the Safety Gate never asks (blocked patterns still apply).
    pub enabled: bool,
    /// Ask before every Moderate command, not only process-control ones.
    pub require_approval_for_moderate: bool,
    /// Ask before Critical commands (default `true`).
    pub require_approval_for_critical: bool,
    /// Case-insensitive substrings; a command containing one is refused outright.
    pub blocked_patterns: Vec<String>,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            require_approval_for_moderate: false,
            require_approval_for_critical: true,
            blocked_patterns: Vec::new(),
        }
    }
}

impl SafetyConfig {
    /// The first blocked pattern contained in `command`, ignoring case.
    pub fn blocked_pattern(&self, command: &str) -> Option<&str> {
        let lower = command.to_lowercase();
        self.blocked_patterns
            .iter()
            .map(|p| p.trim())
            .find(|p| !p.is_empty() && lower.contains(&p.to_lowercase()))
    }

    /// Whether the Safety Gate should stop `assessment` for approval under this config.
    pub fn requires_approval(&self, assessment: &SafetyAssessment) -> bool {
        if !self.enabled {
            return false;
        }
        match assessment.risk {
            RiskLevel::Critical => self.require_approval_for_critical,
            RiskLevel::Moderate => {
                assessment.requires_approval || self.require_approval_for_moderate
            }
            RiskLevel::Safe => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    /// `"auto"` or one of [`KNOWN_AI_PROVIDERS`]; the named provider is tried first.
    pub provider: String,
    /// Model for the chosen provider (for Ollama, `OLLAMA_MODEL` wins).
    pub model: Option<String>,
    /// Ollama base URL (`OLLAMA_HOST` wins).
    pub ollama_endpoint: Option<String>,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: "auto".to_string(),
            model: None,
            ollama_endpoint: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(default)]
pub struct McpConfig {
    pub servers: Vec<McpServerEntry>,
}

/// One `[[mcp.servers]]` entry. `name` and `command` are required.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct McpServerEntry {
    pub name: String,
    #[serde(default = "default_transport")]
    pub transport: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_transport() -> String {
    "stdio".to_string()
}

fn default_true() -> bool {
    true
}

impl MrtConfig {
    /// The raw `[keybindings]` map (binding -> action name); empty when unset.
    pub fn keybindings(&self) -> &HashMap<String, String> {
        &self.keybindings
    }

    /// The `[notifications]` settings.
    pub fn notifications(&self) -> &NotificationsConfig {
        &self.notifications
    }

    /// MCP servers that should be connected at startup (enabled, stdio transport).
    pub fn startup_mcp_servers(&self) -> impl Iterator<Item = &McpServerEntry> {
        self.mcp
            .servers
            .iter()
            .filter(|s| s.enabled && s.transport.eq_ignore_ascii_case("stdio"))
    }

    /// Non-fatal problems worth showing to the user (unknown names, clamped values).
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let size = self.terminal.font_size;
        if size.is_nan() || size < FONT_SIZE_RANGE.0 || size > FONT_SIZE_RANGE.1 {
            warnings.push(format!(
                "terminal.font_size {size} is outside {}–{}; using {}.",
                FONT_SIZE_RANGE.0,
                FONT_SIZE_RANGE.1,
                self.terminal.effective_font_size()
            ));
        }
        if let Some(theme) = &self.terminal.theme {
            if !KNOWN_THEMES.iter().any(|t| t.eq_ignore_ascii_case(theme.trim())) {
                warnings.push(format!(
                    "terminal.theme \"{theme}\" is unknown; using graphite-calm."
                ));
            }
        }
        let provider = self.ai.provider.trim();
        if !KNOWN_AI_PROVIDERS.iter().any(|p| p.eq_ignore_ascii_case(provider)) {
            warnings.push(format!(
                "ai.provider \"{provider}\" is unknown; using auto."
            ));
        }
        for server in &self.mcp.servers {
            if server.enabled && !server.transport.eq_ignore_ascii_case("stdio") {
                warnings.push(format!(
                    "MCP server \"{}\" uses transport \"{}\"; only stdio is supported, so it is skipped.",
                    server.name, server.transport
                ));
            }
        }
        warnings
    }
}

/// A configuration file that could not be read or parsed.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigError {
    pub path: PathBuf,
    pub message: String,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}

impl std::error::Error for ConfigError {}

/// Parses one TOML document into a resolved config (defaults for missing fields).
pub fn parse_config_str(text: &str) -> Result<MrtConfig, String> {
    let table = parse_layer(text)?;
    config_from_table(table)
}

/// Parses and validates one file's TOML, returning the raw table for merging.
pub fn parse_layer(text: &str) -> Result<toml::Table, String> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
    // Validate the layer on its own so type errors point at the right file.
    config_from_table(table.clone())?;
    Ok(table)
}

/// Turns a (merged) table into a resolved config.
pub fn config_from_table(table: toml::Table) -> Result<MrtConfig, String> {
    MrtConfig::deserialize(toml::Value::Table(table)).map_err(|e| e.to_string())
}

/// Merges `overlay` on top of `base`.
///
/// Tables merge key by key, `[[mcp.servers]]` entries merge by `name` (the overlay
/// replaces an entry with the same name, others are appended), `safety.blocked_patterns`
/// is the union of both lists, and every other value is replaced by the overlay.
pub fn merge_layers(base: &mut toml::Table, overlay: toml::Table) {
    merge_table(base, overlay, "");
}

fn merge_table(base: &mut toml::Table, overlay: toml::Table, path: &str) {
    for (key, value) in overlay {
        let child_path = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(base_child)), toml::Value::Table(overlay_child)) => {
                merge_table(base_child, overlay_child, &child_path);
            }
            (Some(toml::Value::Array(base_items)), toml::Value::Array(overlay_items))
                if child_path == "mcp.servers" =>
            {
                for item in overlay_items {
                    let name = item.get("name").and_then(|n| n.as_str()).map(str::to_string);
                    let existing = name.as_deref().and_then(|name| {
                        base_items
                            .iter()
                            .position(|b| b.get("name").and_then(|n| n.as_str()) == Some(name))
                    });
                    match existing {
                        Some(idx) => base_items[idx] = item,
                        None => base_items.push(item),
                    }
                }
            }
            (Some(toml::Value::Array(base_items)), toml::Value::Array(overlay_items))
                if child_path == "safety.blocked_patterns" =>
            {
                for item in overlay_items {
                    if !base_items.contains(&item) {
                        base_items.push(item);
                    }
                }
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// The user configuration path: `$MRT_CONFIG`, else the platform default.
pub fn user_config_path() -> Option<PathBuf> {
    if let Some(custom) = std::env::var_os(CONFIG_PATH_ENV_VAR) {
        if !custom.is_empty() {
            return Some(PathBuf::from(custom));
        }
    }
    default_user_config_path()
}

/// `~/.config/mrt/config.toml` on Linux/macOS, `%APPDATA%\mrt\config.toml` on Windows.
pub fn default_user_config_path() -> Option<PathBuf> {
    if cfg!(target_os = "windows") {
        directories::BaseDirs::new().map(|d| d.config_dir().join("mrt").join("config.toml"))
    } else {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| directories::BaseDirs::new().map(|d| d.home_dir().join(".config")))?;
        Some(base.join("mrt").join("config.toml"))
    }
}

/// The files that make up MRT's configuration, lowest precedence first.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConfigSources {
    pub user: Option<PathBuf>,
    pub project: Option<PathBuf>,
}

impl ConfigSources {
    /// The user file plus `.mrt.toml` in `cwd`.
    pub fn discover(cwd: &Path) -> Self {
        Self {
            user: user_config_path(),
            project: Some(cwd.join(PROJECT_CONFIG_FILE)),
        }
    }

    /// All candidate paths in merge order (they may not exist).
    pub fn paths(&self) -> Vec<PathBuf> {
        self.user.iter().chain(self.project.iter()).cloned().collect()
    }
}

/// A successfully loaded configuration and the files it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    pub config: MrtConfig,
    /// Files that existed and were merged, in merge order.
    pub loaded_from: Vec<PathBuf>,
}

/// Reads and merges every existing source. Missing files are skipped; the first
/// unreadable or invalid file aborts with an error so the caller can keep its
/// last good configuration.
pub fn load_config(sources: &ConfigSources) -> Result<LoadedConfig, ConfigError> {
    let mut merged = toml::Table::new();
    let mut loaded_from = Vec::new();
    for path in sources.paths() {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(ConfigError {
                    path,
                    message: e.to_string(),
                })
            }
        };
        let layer = parse_layer(&text).map_err(|message| ConfigError {
            path: path.clone(),
            message,
        })?;
        merge_layers(&mut merged, layer);
        loaded_from.push(path);
    }
    let config = config_from_table(merged).map_err(|message| ConfigError {
        path: loaded_from.last().cloned().unwrap_or_default(),
        message,
    })?;
    Ok(LoadedConfig {
        config,
        loaded_from,
    })
}

type FileStamp = Option<(Option<SystemTime>, u64)>;

fn stamp(path: &Path) -> FileStamp {
    std::fs::metadata(path)
        .ok()
        .map(|m| (m.modified().ok(), m.len()))
}

/// Polls configuration files for changes and reports a reload once they settle.
///
/// Uses file metadata (modification time and size) rather than OS notifications,
/// so it behaves the same on every platform and costs one `stat` per file per poll.
#[derive(Debug, Clone)]
pub struct ConfigWatcher {
    paths: Vec<PathBuf>,
    stamps: Vec<FileStamp>,
    debounce: Duration,
    poll_interval: Duration,
    last_poll: Option<Instant>,
    changed_at: Option<Instant>,
}

impl ConfigWatcher {
    pub fn new(paths: Vec<PathBuf>, poll_interval: Duration, debounce: Duration) -> Self {
        let stamps = paths.iter().map(|p| stamp(p)).collect();
        Self {
            paths,
            stamps,
            debounce,
            poll_interval,
            last_poll: None,
            changed_at: None,
        }
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Returns `true` once, when a change has been quiet for the debounce period.
    pub fn poll(&mut self, now: Instant) -> bool {
        let due = self
            .last_poll
            .is_none_or(|last| now.duration_since(last) >= self.poll_interval);
        if due {
            self.last_poll = Some(now);
            for (path, old) in self.paths.iter().zip(self.stamps.iter_mut()) {
                let current = stamp(path);
                if current != *old {
                    *old = current;
                    self.changed_at = Some(now);
                }
            }
        }
        match self.changed_at {
            Some(at) if now.duration_since(at) >= self.debounce => {
                self.changed_at = None;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mrt-config-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn assessment(risk: RiskLevel, requires_approval: bool) -> SafetyAssessment {
        SafetyAssessment {
            command: "x".into(),
            risk,
            reason: String::new(),
            requires_approval,
        }
    }

    #[test]
    fn empty_file_yields_defaults() {
        let cfg = parse_config_str("").unwrap();
        assert_eq!(cfg, MrtConfig::default());
        assert_eq!(cfg.terminal.default_shell, "auto");
        assert_eq!(cfg.terminal.font_size, DEFAULT_FONT_SIZE);
        assert!(cfg.safety.enabled);
        assert!(cfg.safety.require_approval_for_critical);
        assert!(!cfg.safety.require_approval_for_moderate);
        assert_eq!(cfg.ai.provider, "auto");
        assert!(cfg.mcp.servers.is_empty());
        assert!(cfg.keybindings().is_empty());
        assert_eq!(cfg.notifications(), &NotificationsConfig { enabled: true, threshold_secs: 10 });
        assert!(cfg.warnings().is_empty());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let cfg = parse_config_str("[terminal]\nfont_size = 16.0\n").unwrap();
        assert_eq!(cfg.terminal.font_size, 16.0);
        assert_eq!(cfg.terminal.default_shell, "auto");
        assert_eq!(cfg.terminal.scrollback_blocks, 500);
        assert_eq!(cfg.safety, SafetyConfig::default());
    }

    #[test]
    fn example_file_parses() {
        let text = include_str!("../../../mrt.example.toml");
        let cfg = parse_config_str(text).expect("mrt.example.toml must stay valid");
        assert_eq!(cfg.mcp.servers.len(), 2);
        assert_eq!(cfg.notifications().threshold_secs, 10);
        assert!(cfg.warnings().is_empty(), "{:?}", cfg.warnings());
    }

    #[test]
    fn full_schema_parses() {
        let cfg = parse_config_str(
            r#"
            [terminal]
            default_shell = "bash"
            font_size = 15
            theme = "graphite-calm"

            [safety]
            enabled = false
            blocked_patterns = ["drop database"]

            [ai]
            provider = "ollama"
            model = "qwen2.5-coder"
            ollama_endpoint = "http://box:11434"

            [[mcp.servers]]
            name = "git"
            command = "uvx"
            args = ["mcp-server-git"]

            [notifications]
            enabled = false
            threshold_secs = 30

            [keybindings]
            "ctrl+t" = "new_tab"
            "ctrl+shift+p" = "command_palette"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.terminal.requested_shell(), Some("bash"));
        assert_eq!(cfg.terminal.font_size, 15.0, "integers are accepted for floats");
        assert!(!cfg.safety.enabled);
        assert_eq!(cfg.ai.model.as_deref(), Some("qwen2.5-coder"));
        let server = &cfg.mcp.servers[0];
        assert_eq!(server.transport, "stdio");
        assert!(server.enabled);
        let keys = cfg.keybindings();
        assert_eq!(keys["ctrl+t"], "new_tab");
        assert_eq!(keys["ctrl+shift+p"], "command_palette");
        assert!(!cfg.notifications().enabled);
        assert_eq!(cfg.notifications().threshold_secs, 30);
    }

    #[test]
    fn invalid_files_are_errors() {
        assert!(parse_config_str("[terminal\nfont_size = 1").is_err());
        let type_error = parse_config_str("[terminal]\nfont_size = \"big\"").unwrap_err();
        assert!(type_error.contains("font_size"), "{type_error}");
        assert!(parse_config_str("[[mcp.servers]]\nname = \"x\"").is_err(), "command is required");
        assert!(parse_config_str("[keybindings]\n\"ctrl+t\" = 3").is_err(), "actions are strings");
        assert!(parse_config_str("[notifications]\nthreshold_secs = -1").is_err());
    }

    #[test]
    fn warnings_cover_unknown_values() {
        let cfg = parse_config_str(
            "[terminal]\nfont_size = 200\ntheme = \"neon\"\n[ai]\nprovider = \"skynet\"\n\
             [[mcp.servers]]\nname = \"web\"\ntransport = \"http\"\ncommand = \"x\"\n",
        )
        .unwrap();
        assert_eq!(cfg.terminal.effective_font_size(), FONT_SIZE_RANGE.1);
        assert_eq!(cfg.warnings().len(), 4);
        assert_eq!(cfg.startup_mcp_servers().count(), 0);
    }

    #[test]
    fn merge_order_project_wins() {
        let mut base = parse_layer(
            r#"
            [terminal]
            default_shell = "pwsh"
            font_size = 14.0
            [safety]
            blocked_patterns = ["rm -rf /"]
            [[mcp.servers]]
            name = "fs"
            command = "npx"
            enabled = true
            [[mcp.servers]]
            name = "git"
            command = "uvx"
            [keybindings]
            a = "one"
            b = "two"
            "#,
        )
        .unwrap();
        let overlay = parse_layer(
            r#"
            [terminal]
            font_size = 18.0
            [safety]
            blocked_patterns = ["rm -rf /", "terraform destroy"]
            [[mcp.servers]]
            name = "fs"
            command = "npx"
            enabled = false
            [[mcp.servers]]
            name = "sqlite"
            command = "uvx"
            [keybindings]
            b = "three"
            "#,
        )
        .unwrap();
        merge_layers(&mut base, overlay);
        let cfg = config_from_table(base).unwrap();
        assert_eq!(cfg.terminal.default_shell, "pwsh", "user value kept");
        assert_eq!(cfg.terminal.font_size, 18.0, "project value wins");
        assert_eq!(cfg.safety.blocked_patterns, vec!["rm -rf /", "terraform destroy"]);
        let names: Vec<_> = cfg.mcp.servers.iter().map(|s| (s.name.as_str(), s.enabled)).collect();
        assert_eq!(names, vec![("fs", false), ("git", true), ("sqlite", true)]);
        let keys = cfg.keybindings();
        assert_eq!(keys["a"], "one");
        assert_eq!(keys["b"], "three");
    }

    #[test]
    fn load_config_merges_files_and_skips_missing() {
        let dir = temp_dir();
        let user = dir.join("config.toml");
        let project = dir.join(PROJECT_CONFIG_FILE);
        std::fs::write(&user, "[terminal]\nfont_size = 15.0\ndefault_shell = \"zsh\"\n").unwrap();

        let sources = ConfigSources {
            user: Some(user.clone()),
            project: Some(project.clone()),
        };
        let loaded = load_config(&sources).unwrap();
        assert_eq!(loaded.loaded_from, vec![user.clone()]);
        assert_eq!(loaded.config.terminal.font_size, 15.0);

        std::fs::write(&project, "[terminal]\nfont_size = 20.0\n").unwrap();
        let loaded = load_config(&sources).unwrap();
        assert_eq!(loaded.loaded_from, vec![user.clone(), project.clone()]);
        assert_eq!(loaded.config.terminal.font_size, 20.0);
        assert_eq!(loaded.config.terminal.default_shell, "zsh");

        let none = load_config(&ConfigSources::default()).unwrap();
        assert_eq!(none.config, MrtConfig::default());
        assert!(none.loaded_from.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn load_config_reports_the_invalid_file() {
        let dir = temp_dir();
        let user = dir.join("config.toml");
        let project = dir.join(PROJECT_CONFIG_FILE);
        std::fs::write(&user, "[terminal]\nfont_size = 15.0\n").unwrap();
        std::fs::write(&project, "[terminal\n").unwrap();
        let err = load_config(&ConfigSources {
            user: Some(user),
            project: Some(project.clone()),
        })
        .unwrap_err();
        assert_eq!(err.path, project);
        assert!(err.to_string().contains(PROJECT_CONFIG_FILE));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn safety_rules_follow_config() {
        let default = SafetyConfig::default();
        assert!(default.requires_approval(&assessment(RiskLevel::Critical, true)));
        assert!(default.requires_approval(&assessment(RiskLevel::Moderate, true)));
        assert!(!default.requires_approval(&assessment(RiskLevel::Moderate, false)));
        assert!(!default.requires_approval(&assessment(RiskLevel::Safe, false)));

        let strict = SafetyConfig {
            require_approval_for_moderate: true,
            ..SafetyConfig::default()
        };
        assert!(strict.requires_approval(&assessment(RiskLevel::Moderate, false)));

        let off = SafetyConfig {
            enabled: false,
            ..SafetyConfig::default()
        };
        assert!(!off.requires_approval(&assessment(RiskLevel::Critical, true)));

        let blocked = SafetyConfig {
            blocked_patterns: vec!["Format C:".into(), "  ".into()],
            ..off
        };
        assert_eq!(blocked.blocked_pattern("format c: /q"), Some("Format C:"));
        assert_eq!(blocked.blocked_pattern("dir"), None);
    }

    #[test]
    fn watcher_debounces_changes() {
        let dir = temp_dir();
        let file = dir.join("config.toml");
        let mut watcher = ConfigWatcher::new(
            vec![file.clone()],
            Duration::ZERO,
            Duration::from_millis(300),
        );
        let t0 = Instant::now();
        assert!(!watcher.poll(t0), "no change yet");

        std::fs::write(&file, "[terminal]\n").unwrap();
        assert!(!watcher.poll(t0 + Duration::from_millis(10)), "change seen, debouncing");
        std::fs::write(&file, "[terminal]\nfont_size = 15.0\n").unwrap();
        assert!(!watcher.poll(t0 + Duration::from_millis(200)), "second write restarts debounce");
        assert!(!watcher.poll(t0 + Duration::from_millis(400)));
        assert!(watcher.poll(t0 + Duration::from_millis(600)), "quiet for 300 ms");
        assert!(!watcher.poll(t0 + Duration::from_millis(900)), "reported once");

        std::fs::remove_file(&file).unwrap();
        assert!(!watcher.poll(t0 + Duration::from_millis(1000)));
        assert!(watcher.poll(t0 + Duration::from_millis(1400)), "deletion is a change too");
        std::fs::remove_dir_all(dir).ok();
    }
}
