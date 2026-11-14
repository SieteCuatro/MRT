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

    let moderate_rules: &[(&str, &str)] = &[
        ("sudo ", "Executes command with elevated superuser privileges"),
        ("rm ", "Deletes one or more files"),
        ("del ", "Deletes one or more files in CMD/PowerShell"),
        ("remove-item ", "Deletes items in PowerShell"),
        ("kill -9", "Forcefully terminates a running process"),
        ("stop-process", "Terminates a running process in PowerShell"),
        ("taskkill /f", "Forcefully terminates a process in Windows"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{classify_output, StructuredOutput};
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
