//! The opt-in update check (`[updates] check = true`): once per start, MRT asks
//! GitHub Releases for the latest release and, when it is newer than this build,
//! shows a notice with a link to its page. It never downloads or installs anything.

use crate::app::MrtGuiApp;
use crate::theme::Palette;
use egui::RichText;
use std::time::Duration;

/// The latest-release endpoint of MRT's repository.
pub const RELEASES_API: &str = "https://api.github.com/repos/SieteCuatro/MRT/releases/latest";

/// A release newer than this build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    /// The release tag, e.g. `v0.5.0`.
    pub version: String,
    /// Its page on GitHub.
    pub url: String,
}

/// `major.minor.patch` of a version or tag (`v0.4.0`, `0.4.0-rc1`); a pre-release
/// suffix is ignored.
pub fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let core = text.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next()?;
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    let version = (parts.next()??, parts.next().flatten().unwrap_or(0), parts.next().flatten().unwrap_or(0));
    parts.next().is_none().then_some(version)
}

/// The release in a GitHub "latest release" reply when it is newer than `current`
/// (drafts and pre-releases are not offered).
pub fn newer_release(reply: &serde_json::Value, current: &str) -> Option<UpdateInfo> {
    if reply["draft"].as_bool() == Some(true) || reply["prerelease"].as_bool() == Some(true) {
        return None;
    }
    let tag = reply["tag_name"].as_str()?;
    (parse_version(tag)? > parse_version(current)?).then(|| UpdateInfo {
        version: tag.to_string(),
        url: reply["html_url"].as_str().unwrap_or("https://github.com/SieteCuatro/MRT/releases/latest").to_string(),
    })
}

/// Asks `url` (a GitHub latest-release endpoint) whether a release newer than
/// `current` exists. The request carries only a generic user agent with MRT's version.
pub async fn check(url: &str, current: &str) -> Result<Option<UpdateInfo>, String> {
    let client = reqwest::Client::builder()
        .user_agent(format!("mrt/{current}"))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("GitHub answered {}", response.status()));
    }
    let reply: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    Ok(newer_release(&reply, current))
}

impl MrtGuiApp {
    /// Starts the check once, when `[updates] check` is on (also when it is turned on
    /// while MRT runs).
    pub(crate) fn maybe_check_for_update(&mut self, ctx: &egui::Context) {
        if self.update_check_started || !self.config_state.config.updates.check {
            return;
        }
        self.update_check_started = true;
        let slot = self.update_available.clone();
        let ctx = ctx.clone();
        self.runtime.spawn(async move {
            match check(RELEASES_API, env!("CARGO_PKG_VERSION")).await {
                Ok(found) => {
                    if let Ok(mut slot) = slot.lock() {
                        *slot = found;
                    }
                    ctx.request_repaint();
                }
                Err(error) => tracing::info!("update check failed: {error}"),
            }
        });
    }

    /// The notice under the header when a newer release exists.
    pub(crate) fn render_update_notice(&mut self, ctx: &egui::Context) {
        let Some(update) = self.update_available.lock().ok().and_then(|u| u.clone()) else { return };
        let mut dismiss = false;
        egui::TopBottomPanel::top("mrt_update_notice")
            .frame(egui::Frame::none().fill(Palette::BG_SURFACE()).inner_margin(egui::Margin::symmetric(20.0, 6.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("⬆").color(Palette::ACCENT()));
                    ui.label(
                        RichText::new(format!("MRT {} is available (this is {}).", update.version, env!("CARGO_PKG_VERSION")))
                            .color(Palette::TEXT_PRIMARY()),
                    );
                    if ui.small_button("Release notes").on_hover_text(&update.url).clicked() {
                        ctx.open_url(egui::OpenUrl::new_tab(&update.url));
                    }
                    if ui.small_button("Dismiss").clicked() {
                        dismiss = true;
                    }
                });
            });
        if dismiss {
            if let Ok(mut slot) = self.update_available.lock() {
                *slot = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn versions_compare_by_number() {
        assert_eq!(parse_version("v0.4.0"), Some((0, 4, 0)));
        assert_eq!(parse_version("0.10.2-rc1"), Some((0, 10, 2)));
        assert_eq!(parse_version("1"), Some((1, 0, 0)));
        assert_eq!(parse_version("nightly"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        let reply = |tag: &str| serde_json::json!({ "tag_name": tag, "html_url": "https://example.invalid/r", "draft": false, "prerelease": false });
        assert_eq!(newer_release(&reply("v0.10.0"), "0.9.3").map(|u| u.version), Some("v0.10.0".into()), "10 > 9, not text order");
        assert_eq!(newer_release(&reply("v0.4.0"), "0.4.0"), None);
        assert_eq!(newer_release(&reply("v0.3.1"), "0.4.0"), None);
        let mut pre = reply("v9.0.0");
        pre["prerelease"] = true.into();
        assert_eq!(newer_release(&pre, "0.4.0"), None);
    }

    /// Asks the real GitHub API: `cargo test -p mrt-app -- --ignored live_github`.
    #[test]
    #[ignore = "needs the network"]
    fn live_github_release() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let found = runtime.block_on(check(RELEASES_API, "0.0.1")).unwrap().expect("a release newer than 0.0.1");
        assert!(found.version.starts_with('v'), "{found:?}");
        assert!(found.url.starts_with("https://github.com/SieteCuatro/MRT/releases/"), "{found:?}");
        assert_eq!(runtime.block_on(check(RELEASES_API, "999.0.0")).unwrap(), None);
    }

    #[test]
    fn check_reads_a_github_reply() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/repos/x/y/releases/latest", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 2048];
            let n = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..n]).to_string();
            let body = r#"{"tag_name":"v99.0.0","html_url":"https://github.com/SieteCuatro/MRT/releases/tag/v99.0.0","draft":false,"prerelease":false}"#;
            let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            stream.write_all(reply.as_bytes()).unwrap();
            request
        });
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let found = runtime.block_on(check(&url, "0.4.0")).unwrap();
        assert_eq!(found, Some(UpdateInfo { version: "v99.0.0".into(), url: "https://github.com/SieteCuatro/MRT/releases/tag/v99.0.0".into() }));
        let request = server.join().unwrap().to_lowercase();
        assert!(request.contains("user-agent: mrt/0.4.0"), "{request}");
    }
}
