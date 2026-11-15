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
    ];

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
