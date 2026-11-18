use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RiskLevel {
    Safe,
    Moderate,
    Critical,
}

impl RiskLevel {
    pub fn label(&self) -> &'static str {
        match self {
            RiskLevel::Safe => "Safe",
            RiskLevel::Moderate => "Moderate Risk",
            RiskLevel::Critical => "Critical / Destructive",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SafetyAssessment {
    pub command: String,
    pub risk: RiskLevel,
    pub reason: String,
    pub requires_approval: bool,
}

/// Evaluates a shell command across Bash, PowerShell, CMD/Batch, Git, SQL, and K8s
/// to determine whether the Safety Approval Gate should intercept it before running.
/// Services the OS needs to keep running; stopping or disabling them is Critical.
pub const PROTECTED_SERVICES: &[&str] = &[
    "windefend", "eventlog", "rpcss", "rpceptmapper", "lsm", "samss", "dcomlaunch", "plugplay", "power",
    "winmgmt", "cryptsvc", "mpssvc", "bfe", "schedule", "profsvc", "systemd-", "dbus", "polkit", "networkmanager",
    "systemd-journald", "systemd-logind", "com.apple.",
];

/// Starting, stopping, restarting, disabling or deleting a system service: always
/// asks first, and is Critical for services the OS needs.
fn assess_service_control(lower: &str) -> Option<(RiskLevel, String)> {
    const VERBS: &[(&str, &[&str])] = &[
        ("sc.exe ", &["start", "stop", "delete", "config", "pause"]),
        ("sc ", &["start", "stop", "delete", "config", "pause"]),
        ("net ", &["start", "stop"]),
        ("systemctl ", &["start", "stop", "restart", "reload", "disable", "mask", "kill"]),
        ("service ", &["start", "stop", "restart"]),
        ("launchctl ", &["kill", "kickstart", "bootout", "unload", "disable", "stop"]),
    ];
    let cmdlets = ["stop-service", "start-service", "restart-service", "set-service", "remove-service", "suspend-service"];
    let mut hit = cmdlets.iter().any(|c| lower.contains(c));
    for segment in lower.split(['&', ';', '|']) {
        let words: Vec<&str> = segment.split_whitespace().filter(|w| *w != "sudo").collect();
        for (tool, verbs) in VERBS {
            let tool = tool.trim();
            // The tool must be the command of its segment (`echo service start` is not).
            if words.first() == Some(&tool) && words.iter().skip(1).take(2).any(|w| verbs.contains(w)) {
                hit = true;
            }
        }
    }
    if !hit {
        return None;
    }
    let deletes = lower.contains(" delete") || lower.contains("remove-service") || lower.contains(" mask");
    let protected = PROTECTED_SERVICES.iter().find(|name| lower.contains(*name));
    Some(match (protected, deletes) {
        (Some(name), _) => (RiskLevel::Critical, format!("Changes a service the operating system needs ({name})")),
        (None, true) => (RiskLevel::Critical, "Deletes or masks a system service".to_string()),
        (None, false) => (RiskLevel::Moderate, "Starts, stops or reconfigures a system service".to_string()),
    })
}

pub fn assess_command(command: &str) -> SafetyAssessment {
    let lower = command.trim().to_lowercase();

    let critical_rules: &[(&str, &str)] = &[
        ("rm -rf", "Recursive forced deletion of files/directories (Unix/Bash)"),
        ("rm -fr", "Recursive forced deletion of files/directories (Unix/Bash)"),
        ("remove-item -recurse -force", "Recursive forced deletion in PowerShell"),
        ("ri -r -fo", "Recursive forced deletion alias in PowerShell"),
        ("del /s /q", "Silent recursive file deletion in Windows CMD/Batch"),
        ("rd /s /q", "Silent directory tree deletion in Windows CMD/Batch"),
        ("rmdir /s /q", "Silent directory tree deletion in Windows CMD/Batch"),
        ("format ", "Disk volume formatting command"),
        ("mkfs", "Filesystem formatting command"),
        ("dd if=", "Raw block device write via dd"),
        ("drop table", "Destructive SQL DROP TABLE operation"),
        ("drop database", "Destructive SQL DROP DATABASE operation"),
        ("truncate table", "Destructive SQL TRUNCATE TABLE operation"),
        ("git reset --hard", "Discards all uncommitted working tree and index changes"),
        ("git push --force", "Overwrites remote Git commit history"),
        ("git push -f", "Overwrites remote Git commit history"),
        ("git clean -fd", "Permanently deletes untracked files and directories"),
        ("worktree remove --force", "Deletes a git worktree folder together with its uncommitted changes"),
        ("worktree remove -f", "Deletes a git worktree folder together with its uncommitted changes"),
        ("kubectl delete", "Deletes Kubernetes cluster resources"),
        ("terraform destroy", "Destroys managed cloud infrastructure"),
        ("shutdown", "Initiates system shutdown or reboot"),
        ("reboot", "Initiates system reboot"),
    ];

    for (pattern, explanation) in critical_rules {
        if lower.contains(pattern) {
            return SafetyAssessment {
                command: command.to_string(),
                risk: RiskLevel::Critical,
                reason: (*explanation).to_string(),
                requires_approval: true,
            };
        }
    }

    // `-D` and `-d` differ only in case, so this one looks at the command as typed.
    if lower.contains("git ") && lower.contains("branch") && (command.contains(" -D") || lower.contains("--delete --force")) {
        return SafetyAssessment {
            command: command.to_string(),
            risk: RiskLevel::Critical,
            reason: "Deletes a git branch even when its commits are not merged anywhere".to_string(),
            requires_approval: true,
        };
    }

    if let Some((risk, reason)) = assess_service_control(&lower) {
        return SafetyAssessment {
            command: command.to_string(),
            risk,
            reason,
            requires_approval: true,
        };
    }

    if let Some((risk, reason)) = assess_process_control(&lower) {
        return SafetyAssessment {
            command: command.to_string(),
            risk,
            reason,
            requires_approval: true,
        };
    }

    let moderate_rules: &[(&str, &str)] = &[
        ("sudo ", "Executes command with elevated superuser privileges"),
        ("rm ", "Deletes one or more files"),
        ("del ", "Deletes one or more files in CMD/PowerShell"),
        ("remove-item ", "Deletes items in PowerShell"),
        ("chmod 777", "Grants world-writable permissions"),
        ("curl ", "Performs outbound network transfer"),
        ("invoke-webrequest", "Performs outbound network request"),
        ("gh pr create", "Opens a pull request on GitHub"),
    ];

    let git_moderate = match git_subcommand(&lower) {
        Some("push") => Some("Publishes commits to a remote repository"),
        Some("merge") => Some("Merges another branch into the checked-out branch"),
        _ => None,
    };
    if let Some(explanation) = git_moderate {
        return SafetyAssessment {
            command: command.to_string(),
            risk: RiskLevel::Moderate,
            reason: explanation.to_string(),
            requires_approval: false,
        };
    }

    for (pattern, explanation) in moderate_rules {
        if lower.contains(pattern) {
            return SafetyAssessment {
                command: command.to_string(),
                risk: RiskLevel::Moderate,
                reason: (*explanation).to_string(),
                requires_approval: false,
            };
        }
    }

    SafetyAssessment {
        command: command.to_string(),
        risk: RiskLevel::Safe,
        reason: "Standard command execution".to_string(),
        requires_approval: false,
    }
}

/// The git subcommand of a (lower-cased) command line, skipping git's own options
/// such as `-C <folder>` and `-c key=value`: `git -C repo merge x` gives `merge`.
fn git_subcommand(lower: &str) -> Option<&str> {
    let mut words = lower.split_whitespace().skip_while(|w| {
        let w = w.trim_matches('"');
        !(w == "git" || w == "git.exe" || w.ends_with("/git") || w.ends_with("\\git.exe"))
    });
    words.next()?;
    while let Some(word) = words.next() {
        match word {
            "-c" | "-C" | "--git-dir" | "--work-tree" | "--namespace" => {
                words.next();
            }
            w if w.starts_with('-') => {}
            w => return Some(w),
        }
    }
    None
}

/// Process names whose termination can crash the session, log the user out, or destabilise
/// the OS (Windows, macOS and Linux). Compared after [`normalize_process_name`].
pub const PROTECTED_PROCESS_NAMES: &[&str] = &[
    // Windows
    "system",
    "system idle process",
    "idle",
    "registry",
    "secure system",
    "memory compression",
    "smss",
    "csrss",
    "wininit",
    "winlogon",
    "services",
    "lsass",
    "lsaiso",
    "svchost",
    "fontdrvhost",
    "dwm",
    "ntoskrnl",
    // macOS
    "kernel_task",
    "launchd",
    "windowserver",
    "loginwindow",
    "opendirectoryd",
    // Linux / Unix
    "init",
    "systemd",
    "systemd-journald",
    "systemd-logind",
    "kthreadd",
    "dbus-daemon",
    "xorg",
    "gnome-shell",
    "kwin_x11",
    "kwin_wayland",
];

/// Lower-cases a process name and strips a trailing `.exe` and surrounding quotes, so
/// `"LSASS.EXE"`, `lsass.exe` and `lsass` compare equal.
pub fn normalize_process_name(name: &str) -> String {
    let lower = name.trim().trim_matches(|c| c == '"' || c == '\'').to_lowercase();
    lower.strip_suffix(".exe").map(str::to_string).unwrap_or(lower)
}

/// True when the process is critical to the OS session (see [`PROTECTED_PROCESS_NAMES`]).
pub fn is_protected_process_name(name: &str) -> bool {
    let normalized = normalize_process_name(name);
    PROTECTED_PROCESS_NAMES.contains(&normalized.as_str())
}

const KILL_VERBS: &[&str] = &[
    "kill", "pkill", "killall", "taskkill", "tskill", "stop-process", "spps", "xkill",
];
const KILL_BY_NAME_VERBS: &[&str] = &["pkill", "killall"];

/// Detects process-termination commands (`kill`, `pkill`, `killall`, `taskkill`,
/// `Stop-Process`, ...) in command position of any pipeline segment, so every shell
/// command that stops processes passes through the Safety Approval Gate. Targets that
/// are OS-critical processes, PID 1, or "every process" (`-1`) are Critical.
fn assess_process_control(lower: &str) -> Option<(RiskLevel, String)> {
    let mut found: Option<(RiskLevel, String)> = None;
    for segment in lower.split(['\n', ';', '|', '&']) {
        let mut tokens = segment.split_whitespace().peekable();
        while let Some(tok) = tokens.peek() {
            if matches!(*tok, "sudo" | "doas" | "exec" | "command" | "builtin") {
                tokens.next();
            } else {
                break;
            }
        }
        let Some(verb_token) = tokens.next() else {
            continue;
        };
        let verb = std::path::Path::new(verb_token)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(verb_token)
            .to_string();
        if !KILL_VERBS.contains(&verb.as_str()) {
            continue;
        }
        let args: Vec<&str> = tokens.collect();
        let protected = args.iter().find(|arg| {
            let value = arg.rsplit(['=', ':']).next().unwrap_or(arg);
            value.split(',').any(is_protected_process_name)
        });
        let assessment = if let Some(target) = protected {
            (
                RiskLevel::Critical,
                format!("Terminates an OS-critical process ({target}); this can crash or log out the session"),
            )
        } else if verb == "kill" && args.iter().any(|a| *a == "1" || *a == "-1") {
            (
                RiskLevel::Critical,
                "Signals PID 1 (init) or every process the user owns (-1)".to_string(),
            )
        } else if KILL_BY_NAME_VERBS.contains(&verb.as_str())
            || (verb == "taskkill" && args.iter().any(|a| *a == "/im"))
            || ((verb == "stop-process" || verb == "spps") && args.iter().any(|a| a.starts_with("-n")))
        {
            (
                RiskLevel::Moderate,
                "Terminates every process matching a name; several processes may be stopped".to_string(),
            )
        } else {
            (RiskLevel::Moderate, "Terminates a running process".to_string())
        };
        if found.as_ref().map(|(risk, _)| assessment.0 > *risk).unwrap_or(true) {
            found = Some(assessment);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{classify_output, StructuredOutput};

    #[test]
    fn service_control_always_asks_and_protects_os_services() {
        for cmd in ["sc.exe stop Spooler", "sudo systemctl restart nginx", "Restart-Service nginx", "net stop spooler", "launchctl kill TERM gui/501/com.x"] {
            let a = assess_command(cmd);
            assert!(a.requires_approval, "{cmd}");
            assert!(a.risk >= RiskLevel::Moderate, "{cmd}");
        }
        assert_eq!(assess_command("sudo systemctl stop systemd-journald").risk, RiskLevel::Critical);
        assert_eq!(assess_command("sc.exe stop WinDefend").risk, RiskLevel::Critical);
        assert_eq!(assess_command("sc delete myservice").risk, RiskLevel::Critical);
        assert_eq!(assess_command("systemctl status nginx").risk, RiskLevel::Safe, "reading is safe");
        assert_eq!(assess_command("echo service start").risk, RiskLevel::Safe);
    }

    #[test]
    fn test_process_control_commands_require_approval() {
        let a = assess_command("kill 4242");
        assert_eq!(a.risk, RiskLevel::Moderate);
        assert!(a.requires_approval);

        let b = assess_command("Get-Process node | Stop-Process -Force");
        assert_eq!(b.risk, RiskLevel::Moderate);
        assert!(b.requires_approval);

        let c = assess_command("taskkill /F /IM lsass.exe");
        assert_eq!(c.risk, RiskLevel::Critical);

        let d = assess_command("sudo kill -9 1");
        assert_eq!(d.risk, RiskLevel::Critical);

        let e = assess_command("pkill -f launchd");
        assert_eq!(e.risk, RiskLevel::Critical);

        let f = assess_command("killall node");
        assert_eq!(f.risk, RiskLevel::Moderate);
        assert!(f.reason.contains("matching a name"));

        // Mentions of "kill" outside command position are not process control.
        let g = assess_command("git commit -m \"kill the flaky test\"");
        assert_eq!(g.risk, RiskLevel::Safe);
    }

    #[test]
    fn git_publishing_merging_and_discarding_are_rated() {
        assert_eq!(assess_command("git push -u origin feat").risk, RiskLevel::Moderate);
        assert_eq!(assess_command("gh pr create --fill").risk, RiskLevel::Moderate);
        assert_eq!(assess_command("git -C repo merge --no-ff feat").risk, RiskLevel::Moderate);
        assert_eq!(assess_command("git branch -D feat").risk, RiskLevel::Critical);
        assert_eq!(assess_command("git branch --delete --force feat").risk, RiskLevel::Critical);
        assert_eq!(assess_command("git branch -d feat").risk, RiskLevel::Safe, "-d refuses unmerged branches");
        assert_eq!(assess_command("git worktree remove --force ../x").risk, RiskLevel::Critical);
        assert_eq!(assess_command("git worktree remove ../x").risk, RiskLevel::Safe, "refuses with changes");
        assert_eq!(assess_command("git status").risk, RiskLevel::Safe);
        assert_eq!(assess_command(r#"git -C "C:\src\proj" push -u origin feat"#).risk, RiskLevel::Moderate);
        assert_eq!(assess_command("git -c core.editor=true merge feat").risk, RiskLevel::Moderate);
        assert_eq!(assess_command("git log --grep merge").risk, RiskLevel::Safe, "only the subcommand counts");
        assert_eq!(assess_command("echo git push").risk, RiskLevel::Moderate, "conservative: git anywhere in the line");
    }

    #[test]
    fn test_protected_process_names_are_normalized() {
        assert!(is_protected_process_name("LSASS.EXE"));
        assert!(is_protected_process_name("launchd"));
        assert!(!is_protected_process_name("node.exe"));
        assert_eq!(normalize_process_name("\"Code.exe\""), "code");
    }
    use crate::vt::parse_ansi_text;

    #[test]
    fn test_safety_classifier_detects_critical_commands() {
        let a1 = assess_command("rm -rf ./build");
        assert_eq!(a1.risk, RiskLevel::Critical);
        assert!(a1.requires_approval);

        let a2 = assess_command("Remove-Item -Recurse -Force C:\\temp\\foo");
        assert_eq!(a2.risk, RiskLevel::Critical);

        let a3 = assess_command("rd /s /q build_output");
        assert_eq!(a3.risk, RiskLevel::Critical);

        let a4 = assess_command("git status");
        assert_eq!(a4.risk, RiskLevel::Safe);
        assert!(!a4.requires_approval);
    }

    #[tokio::test]
    async fn test_multi_shell_discovery_and_execution() {
        let shells = crate::pty::discover_shells();
        assert!(!shells.is_empty(), "Should discover at least one host shell");
        let cwd = std::env::current_dir().unwrap();
        for shell in shells.iter().take(2) {
            let res = crate::pty::execute_command_in_shell(shell, "echo MRT_MULTI_SHELL_OK", &cwd)
                .await
                .unwrap();
            assert_eq!(res.exit_code, 0);
            assert!(res.combined_output.contains("MRT_MULTI_SHELL_OK"));
        }
    }

    #[test]
    fn test_structured_output_detection() {
        let json_out = classify_output(r#"{"status": "ok", "items": [1, 2, 3]}"#);
        assert!(matches!(json_out, StructuredOutput::Json(_)));

        let csv_out = classify_output("name,role,port\napi,backend,8080\nweb,frontend,3000");
        match csv_out {
            StructuredOutput::Table { headers, rows } => {
                assert_eq!(headers, vec!["name", "role", "port"]);
                assert_eq!(rows.len(), 2);
            }
            _ => panic!("Expected Table output"),
        }

        let md_out = classify_output("# Heading\n- item 1\n- item 2\n```rust\nfn main() {}\n```");
        assert!(matches!(md_out, StructuredOutput::Markdown(_)));
    }

    #[test]
    fn test_ansi_parser() {
        let raw = "\x1b[1;32mSUCCESS:\x1b[0m Build finished";
        let (spans, plain) = parse_ansi_text(raw);
        assert_eq!(plain, "SUCCESS: Build finished");
        assert_eq!(spans.len(), 2);
        assert!(spans[0].bold);
        assert!(spans[0].fg.is_some());
        assert!(!spans[1].bold);
    }
}
