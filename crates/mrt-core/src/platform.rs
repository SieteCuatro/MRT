/// Ensures that when MRT is launched as a GUI app on macOS (from Dock/Spotlight)
/// or Linux (from a desktop launcher), it inherits the user's full login shell `PATH`
/// (such as `/opt/homebrew/bin`, `/usr/local/bin`, `~/.cargo/bin`, `~/.local/bin`).
pub fn hydrate_login_environment() {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use std::path::PathBuf;
        if let Ok(shell) = std::env::var("SHELL") {
            if let Ok(output) = std::process::Command::new(&shell)
                .args(["-l", "-c", "env"])
                .output()
            {
                if output.status.success() {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for line in stdout.lines() {
                        if let Some((key, val)) = line.split_once('=') {
                            if matches!(key, "PATH" | "LANG" | "LC_ALL" | "GOPATH" | "CARGO_HOME" | "RUSTUP_HOME") {
                                std::env::set_var(key, val);
                            }
                        }
                    }
                }
            }
        }

        // Also ensure standard Homebrew & Cargo paths are present on macOS/Linux
        if let Some(current_path) = std::env::var_os("PATH") {
            let mut paths: Vec<PathBuf> = std::env::split_paths(&current_path).collect();
            let extra_candidates = [
                "/opt/homebrew/bin",
                "/opt/homebrew/sbin",
                "/usr/local/bin",
                "/usr/bin",
                "/bin",
            ];
            for candidate in extra_candidates {
                let p = PathBuf::from(candidate);
                if p.is_dir() && !paths.contains(&p) {
                    paths.push(p);
                }
            }
            if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                for rel in [".cargo/bin", ".local/bin"] {
                    let user_bin = home.join(rel);
                    if user_bin.is_dir() && !paths.contains(&user_bin) {
                        paths.insert(0, user_bin);
                    }
                }
            }
            if let Ok(joined) = std::env::join_paths(paths) {
                std::env::set_var("PATH", joined);
            }
        }
    }
}

/// On Windows, `#![windows_subsystem = "windows"]` prevents an extra console window from
/// opening when launching the GUI. However, if the user runs `mrt --version` or `mrt --help`
/// from an existing terminal, attaching to the parent console ensures CLI output is visible.
pub fn attach_parent_console_if_cli() {
    #[cfg(target_os = "windows")]
    {
        const ATTACH_PARENT_PROCESS: u32 = 0xFFFFFFFF;
        #[link(name = "kernel32")]
        extern "system" {
            fn AttachConsole(dwProcessId: u32) -> i32;
        }
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// Environment variable that moves MRT's data folder (history, layout, bridge and
/// pairing files), for portable installs and isolated tests.
pub const DATA_DIR_ENV_VAR: &str = "MRT_DATA_DIR";

/// MRT's per-user data folder: `$MRT_DATA_DIR`, else the platform's local data folder.
pub fn data_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV_VAR).filter(|d| !d.is_empty()) {
        return Some(std::path::PathBuf::from(dir));
    }
    directories::ProjectDirs::from("", "", "MRT").map(|d| d.data_local_dir().to_path_buf())
}
