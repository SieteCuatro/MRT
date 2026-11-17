//! Layout restore: the window's tabs, splits, shells, folders and remote (SSH) tabs
//! are saved to `layout.json` in MRT's data folder while MRT runs and when it
//! closes, and recreated at the next start with fresh shells
//! (`terminal.restore_layout`). Command blocks are not part of the layout; they
//! are in the command history.

use crate::app::{MrtGuiApp, SplitDirection, TerminalTab};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Format version written to the file; newer files are ignored by older MRT.
const LAYOUT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedLayout {
    pub version: u32,
    #[serde(default)]
    pub active_tab: usize,
    #[serde(default)]
    pub mosaic: bool,
    pub tabs: Vec<SavedTab>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedTab {
    pub title: String,
    /// Shell id (`pwsh`, `bash`, `wsl`...).
    pub shell: String,
    pub cwd: PathBuf,
    /// `ssh` arguments of a remote tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<SavedSplit>,
    /// Root of an agent sandbox tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedSplit {
    /// `"horizontal"` (side by side) or `"vertical"` (stacked), as `:split-h` / `:split-v`.
    pub direction: String,
    pub shell: String,
    pub cwd: PathBuf,
}

/// `layout.json` in MRT's local data folder (next to the history database).
pub fn layout_path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "MRT").map(|dirs| dirs.data_local_dir().join("layout.json"))
}

pub fn load(path: &Path) -> Option<SavedLayout> {
    let text = std::fs::read_to_string(path).ok()?;
    let layout: SavedLayout = serde_json::from_str(&text).ok()?;
    (layout.version <= LAYOUT_VERSION && !layout.tabs.is_empty()).then_some(layout)
}

/// Writes the layout through a temporary file and a rename, so a crash mid-write
/// never leaves a half-written file.
pub fn save(path: &Path, layout: &SavedLayout) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(layout).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, path)
}

impl MrtGuiApp {
    /// The current layout, as saved.
    pub(crate) fn capture_layout(&self) -> SavedLayout {
        let shell_id = |idx: usize| self.shells.get(idx).map(|s| s.id.clone()).unwrap_or_default();
        SavedLayout {
            version: LAYOUT_VERSION,
            active_tab: self.active_tab,
            mosaic: self.mosaic,
            tabs: self
                .tabs
                .iter()
                .map(|tab| SavedTab {
                    title: tab.title.clone(),
                    shell: shell_id(tab.active_shell_idx),
                    cwd: tab.cwd.clone(),
                    remote: tab.remote.clone(),
                    sandbox: tab.sandbox_root.clone(),
                    split: tab.secondary_pane.as_ref().and_then(|pane| {
                        let direction = match tab.split_direction {
                            SplitDirection::Horizontal => "horizontal",
                            SplitDirection::Vertical => "vertical",
                            SplitDirection::None => return None,
                        };
                        Some(SavedSplit {
                            direction: direction.into(),
                            shell: shell_id(pane.active_shell_idx),
                            cwd: pane.cwd.clone(),
                        })
                    }),
                })
                .collect(),
        }
    }

    /// Replaces the tabs with the saved ones. Shells that are no longer installed
    /// become the default shell; folders that no longer exist become `fallback_cwd`.
    pub(crate) fn restore_layout(&mut self, layout: &SavedLayout, fallback_cwd: &Path) {
        if layout.tabs.is_empty() || self.shells.is_empty() {
            return;
        }
        let default_shell = self.tabs.first().map(|t| t.active_shell_idx).unwrap_or(0);
        let shell_idx = |id: &str| self.shells.iter().position(|s| s.id == id).unwrap_or(default_shell);
        let folder = |cwd: &Path| if cwd.is_dir() { cwd.to_path_buf() } else { fallback_cwd.to_path_buf() };
        let shells_count = self.shells.len();
        let tabs: Vec<TerminalTab> = layout
            .tabs
            .iter()
            .map(|saved| {
                let cwd = if saved.remote.is_some() { std::env::temp_dir() } else { folder(&saved.cwd) };
                let mut tab = TerminalTab::new(saved.title.clone(), shell_idx(&saved.shell), cwd);
                tab.remote = saved.remote.clone().filter(|args| !args.is_empty());
                // A sandbox whose folder is gone becomes a normal tab, never an unbounded one.
                if let Some(root) = saved.sandbox.as_ref().filter(|r| r.is_dir()) {
                    tab.sandbox_root = Some(root.clone());
                    tab.cwd = root.clone();
                }
                if let Some(split) = &saved.split {
                    let direction = match split.direction.as_str() {
                        "vertical" => SplitDirection::Vertical,
                        _ => SplitDirection::Horizontal,
                    };
                    tab.split(direction, shells_count);
                    if let Some(pane) = tab.secondary_pane.as_mut() {
                        pane.active_shell_idx = shell_idx(&split.shell);
                        pane.cwd = folder(&split.cwd);
                    }
                    tab.active_pane_idx = 0;
                }
                tab
            })
            .collect();
        self.tabs = tabs;
        self.active_tab = layout.active_tab.min(self.tabs.len() - 1);
        self.mosaic = layout.mosaic;
    }

    /// Saves the layout when it changed since the last save (checked every few
    /// seconds while MRT runs, and when it closes).
    pub(crate) fn persist_layout(&mut self, force: bool) {
        let Some(path) = self.layout_file.clone() else { return };
        let now = std::time::Instant::now();
        if !force && now < self.layout_next_check {
            return;
        }
        self.layout_next_check = now + std::time::Duration::from_secs(3);
        let layout = self.capture_layout();
        if self.layout_saved.as_ref() == Some(&layout) {
            return;
        }
        match save(&path, &layout) {
            Ok(()) => self.layout_saved = Some(layout),
            Err(error) => tracing::warn!("could not save the layout to {}: {error}", path.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_round_trips_through_the_app() {
        let dir = std::env::temp_dir().join(format!("mrt-layout-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = MrtGuiApp::new();
        let shell = app.shells[0].id.clone();
        let layout = SavedLayout {
            version: LAYOUT_VERSION,
            active_tab: 1,
            mosaic: true,
            tabs: vec![
                SavedTab {
                    title: "build".into(),
                    shell: shell.clone(),
                    cwd: dir.clone(),
                    remote: None,
                    sandbox: None,
                    split: Some(SavedSplit { direction: "vertical".into(), shell: "no-such-shell".into(), cwd: dir.join("gone") }),
                },
                SavedTab {
                    title: "ssh box".into(),
                    shell: shell.clone(),
                    cwd: "/home/me".into(),
                    remote: Some(vec!["me@box".into()]),
                    split: None,
                    sandbox: None,
                },
            ],
        };
        let path = dir.join("layout.json");
        save(&path, &layout).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded, layout);

        app.restore_layout(&loaded, &dir);
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(app.active_tab, 1);
        assert!(app.mosaic);
        assert_eq!(app.tabs[0].title, "build");
        assert_eq!(app.tabs[0].cwd, dir);
        assert_eq!(app.tabs[0].split_direction, SplitDirection::Vertical);
        let pane = app.tabs[0].secondary_pane.as_ref().unwrap();
        assert_eq!(pane.cwd, dir, "a folder that no longer exists falls back");
        assert_eq!(app.tabs[1].remote.as_deref(), Some(&["me@box".to_string()][..]));
        assert!(app.tabs[1].blocks.is_empty(), "restoring runs nothing");

        // Capturing the restored app gives the same layout, except for the fallbacks.
        let again = app.capture_layout();
        assert_eq!(again.tabs[0].title, "build");
        assert_eq!(again.tabs[0].split.as_ref().unwrap().direction, "vertical");
        assert_eq!(again.tabs[1].remote, layout.tabs[1].remote);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unreadable_or_future_files_are_ignored() {
        let dir = std::env::temp_dir().join(format!("mrt-layout-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("layout.json");
        assert!(load(&path).is_none());
        std::fs::write(&path, "{not json").unwrap();
        assert!(load(&path).is_none());
        std::fs::write(&path, r#"{"version": 99, "tabs": [{"title":"x","shell":"sh","cwd":"/"}]}"#).unwrap();
        assert!(load(&path).is_none());
        std::fs::write(&path, r#"{"version": 1, "tabs": []}"#).unwrap();
        assert!(load(&path).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
