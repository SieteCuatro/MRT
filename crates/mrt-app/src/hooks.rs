//! Event hooks (`[[hooks]]` in the config): commands MRT runs in the background,
//! through the default shell and without a window, when a command finishes or
//! fails, a tab's folder changes, or a tab opens. Details are passed as `MRT_*`
//! environment variables; hooks never create blocks and never trigger hooks.

use mrt_core::{HookEntry, ShellProfile};
use std::time::Duration;

/// A hook that runs longer than this is stopped.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(60);

/// What happened, for the hooks' environment.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HookEvent {
    /// One of [`mrt_core::HOOK_EVENTS`].
    pub event: &'static str,
    pub command: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub cwd: String,
    pub tab_title: String,
}

impl HookEvent {
    /// `MRT_EVENT`, `MRT_COMMAND`, `MRT_EXIT_CODE`, `MRT_DURATION_MS`, `MRT_CWD`, `MRT_TAB`.
    pub fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("MRT_EVENT", self.event.to_string()),
            ("MRT_COMMAND", self.command.clone()),
            ("MRT_EXIT_CODE", self.exit_code.map(|c| c.to_string()).unwrap_or_default()),
            ("MRT_DURATION_MS", self.duration_ms.to_string()),
            ("MRT_CWD", self.cwd.clone()),
            ("MRT_TAB", self.tab_title.clone()),
        ]
    }
}

/// The hooks that apply to `event`.
pub fn matching<'a>(hooks: &'a [HookEntry], event: &HookEvent) -> Vec<&'a HookEntry> {
    hooks.iter().filter(|h| h.applies(event.event, &event.command, event.duration_ms)).collect()
}

/// Starts every matching hook on `runtime` and returns how many were started.
pub fn fire(runtime: &tokio::runtime::Runtime, hooks: &[HookEntry], shell: &ShellProfile, event: &HookEvent) -> usize {
    let hooks = matching(hooks, event);
    for hook in &hooks {
        let mut cmd = tokio::process::Command::new(&shell.program);
        // cmd.exe does not understand the `\"` quoting Rust uses for arguments, so
        // the hook's command line is passed to it as written.
        #[cfg(windows)]
        if matches!(shell.kind, mrt_core::ShellKind::CmdBatch) {
            cmd.args(&shell.exec_flag).raw_arg(&hook.run);
        } else {
            cmd.args(shell.build_exec_args(&hook.run));
        }
        #[cfg(not(windows))]
        cmd.args(shell.build_exec_args(&hook.run));
        cmd.envs(event.env())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        if std::path::Path::new(&event.cwd).is_dir() {
            cmd.current_dir(&event.cwd);
        }
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        let run = hook.run.clone();
        runtime.spawn(async move {
            match cmd.spawn() {
                Ok(mut child) => {
                    if tokio::time::timeout(HOOK_TIMEOUT, child.wait()).await.is_err() {
                        let _ = child.kill().await;
                        tracing::warn!("hook `{run}` ran longer than {HOOK_TIMEOUT:?} and was stopped");
                    }
                }
                Err(error) => tracing::warn!("hook `{run}` could not start: {error}"),
            }
        });
    }
    hooks.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(event: &str, run: &str) -> HookEntry {
        HookEntry { event: event.into(), run: run.into(), match_command: None, min_duration_secs: None }
    }

    #[test]
    fn env_and_matching() {
        let event = HookEvent {
            event: "command_failed",
            command: "cargo test".into(),
            exit_code: Some(101),
            duration_ms: 1500,
            cwd: "/src".into(),
            tab_title: "build".into(),
        };
        let env = event.env();
        assert!(env.contains(&("MRT_EXIT_CODE", "101".to_string())));
        assert!(env.contains(&("MRT_TAB", "build".to_string())));
        let hooks = vec![hook("command_failed", "a"), hook("command_finished", "b"), hook("COMMAND_FAILED", "c")];
        let runs: Vec<&str> = matching(&hooks, &event).iter().map(|h| h.run.as_str()).collect();
        assert_eq!(runs, vec!["a", "c"]);
    }

    #[test]
    fn hooks_run_in_the_background_with_the_event_in_their_environment() {
        let shell = mrt_core::discover_shells()
            .into_iter()
            .find(|s| {
                if cfg!(windows) {
                    // `bash` on Windows may be WSL's, which cannot write to a Windows path.
                    matches!(s.kind, mrt_core::ShellKind::CmdBatch)
                } else {
                    matches!(s.kind, mrt_core::ShellKind::Sh | mrt_core::ShellKind::Bash)
                }
            });
        let Some(shell) = shell else { return };
        let dir = std::env::temp_dir().join(format!("mrt-hook-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out.txt");
        let run = if matches!(shell.kind, mrt_core::ShellKind::CmdBatch) {
            format!("echo %MRT_EVENT% %MRT_EXIT_CODE% > \"{}\"", out.display())
        } else {
            format!("echo \"$MRT_EVENT $MRT_EXIT_CODE\" > '{}'", out.display())
        };
        let event = HookEvent { event: "command_failed", exit_code: Some(3), cwd: dir.display().to_string(), ..Default::default() };
        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert_eq!(fire(&runtime, &[hook("command_failed", &run)], &shell, &event), 1);
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let text = loop {
            if let Ok(text) = std::fs::read_to_string(&out) {
                if text.contains('3') {
                    break text;
                }
            }
            assert!(std::time::Instant::now() < deadline, "hook did not run");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(text.trim(), "command_failed 3");
        drop(runtime);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
