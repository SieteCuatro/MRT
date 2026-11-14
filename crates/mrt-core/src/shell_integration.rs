use crate::pty::ShellKind;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Osc133Kind {
    /// OSC 133;A - Prompt start
    PromptStart,
    /// OSC 133;B - Command start (prompt finished, user entered command)
    CommandStart,
    /// OSC 133;C - Command executed / output start
    CommandExecuted,
    /// OSC 133;D [; exit_code] - Command finished
    CommandFinished(Option<i32>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Osc133Event {
    pub kind: Osc133Kind,
    pub byte_offset: usize,
}

/// Parses OSC 133 FinalTerm escape sequences from a string or stream slice.
/// Formats:
///   `\x1b]133;A\x07` or `\x1b]133;A\x1b\`
///   `\x1b]133;B\x07`
///   `\x1b]133;C\x07`
///   `\x1b]133;D;0\x07`
pub fn parse_osc133_markers(input: &str) -> Vec<Osc133Event> {
    let mut events = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b']' {
            // Found OSC sequence
            let start = i;
            let mut end = i + 2;
            let mut st_found = false;

            while end < bytes.len() {
                if bytes[end] == 0x07 {
                    // BEL terminator
                    st_found = true;
                    end += 1;
                    break;
                } else if bytes[end] == 0x1b && end + 1 < bytes.len() && bytes[end + 1] == b'\\' {
                    // String Terminator (\x1b\)
                    st_found = true;
                    end += 2;
                    break;
                }
                end += 1;
            }

            if st_found {
                let payload = &input[start + 2..end.saturating_sub(if bytes[end - 1] == b'\\' { 2 } else { 1 })];
                if let Some(rest) = payload.strip_prefix("133;") {
                    let parts: Vec<&str> = rest.split(';').collect();
                    match parts.first().copied() {
                        Some("A") => events.push(Osc133Event {
                            kind: Osc133Kind::PromptStart,
                            byte_offset: start,
                        }),
                        Some("B") => events.push(Osc133Event {
                            kind: Osc133Kind::CommandStart,
                            byte_offset: start,
                        }),
                        Some("C") => events.push(Osc133Event {
                            kind: Osc133Kind::CommandExecuted,
                            byte_offset: start,
                        }),
                        Some("D") => {
                            let code = parts.get(1).and_then(|c| c.parse::<i32>().ok());
                            events.push(Osc133Event {
                                kind: Osc133Kind::CommandFinished(code),
                                byte_offset: start,
                            });
                        }
                        _ => {}
                    }
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }

    events
}

/// Generates the shell integration script/hook for the specified shell kind.
/// These scripts inject OSC 133 sequences into prompt generation and execution hooks.
pub fn generate_shell_integration_script(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Bash => {
            r#"# MRT Shell Integration for Bash (OSC 133)
__mrt_prompt_start() { printf "\e]133;A\e\\"; }
__mrt_command_start() { printf "\e]133;B\e\\"; }
__mrt_command_executed() { printf "\e]133;C\e\\"; }
__mrt_command_finished() { printf "\e]133;D;%s\e\\" "$?"; }

trap '__mrt_command_executed' DEBUG
PROMPT_COMMAND="__mrt_command_finished; __mrt_prompt_start"
PS1="\[\e]133;B\e\\\]$PS1"
"#
        }
        ShellKind::Zsh => {
            r#"# MRT Shell Integration for Zsh (OSC 133)
precmd() {
    printf "\e]133;D;%s\e\\" "$?"
    printf "\e]133;A\e\\"
}
preexec() {
    printf "\e]133;C\e\\"
}
PROMPT="%{\e]133;B\e\\%}$PROMPT"
"#
        }
        ShellKind::Fish => {
            r#"# MRT Shell Integration for Fish (OSC 133)
function __mrt_postexec --on-event fish_postexec
    printf "\e]133;D;%s\e\\" $status
end
function __mrt_preexec --on-event fish_preexec
    printf "\e]133;C\e\\"
end
functions -c fish_prompt __mrt_original_fish_prompt
function fish_prompt
    printf "\e]133;A\e\\"
    __mrt_original_fish_prompt
    printf "\e]133;B\e\\"
end
"#
        }
        ShellKind::PowerShellCore | ShellKind::WindowsPowerShell => {
            r#"# MRT Shell Integration for PowerShell (OSC 133)
$Global:__mrt_last_exit = 0
$ExecutionContext.SessionState.InvokeCommand.PostCommandLookupAction = {
    param($cmdName, $cmdContext)
}
function Global:prompt {
    $exitCode = $Global:LASTEXITCODE
    Write-Host -NoNewline "`e]133;D;$exitCode`e\"
    Write-Host -NoNewline "`e]133;A`e\"
    $p = "PS " + (Get-Location) + "> "
    Write-Host -NoNewline $p
    Write-Host -NoNewline "`e]133;B`e\"
    return " "
}
"#
        }
        ShellKind::CmdBatch => {
            r#"REM MRT Shell Integration for Windows CMD (OSC 133)
PROMPT $E]133;D$E\$E]133;A$E\$P$G$E]133;B$E\
"#
        }
        _ => "# OSC 133 not supported for this shell kind\n",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_osc133_markers() {
        let stream = "Hello\x1b]133;A\x07Prompt> \x1b]133;B\x1b\\cargo build\x1b]133;C\x07Output line 1\nOutput line 2\x1b]133;D;0\x1b\\Done";
        let markers = parse_osc133_markers(stream);

        assert_eq!(markers.len(), 4);
        assert_eq!(markers[0].kind, Osc133Kind::PromptStart);
        assert_eq!(markers[1].kind, Osc133Kind::CommandStart);
        assert_eq!(markers[2].kind, Osc133Kind::CommandExecuted);
        assert_eq!(markers[3].kind, Osc133Kind::CommandFinished(Some(0)));
    }

    #[test]
    fn test_generate_shell_integration_scripts() {
        for kind in [
            ShellKind::Bash,
            ShellKind::Zsh,
            ShellKind::Fish,
            ShellKind::PowerShellCore,
            ShellKind::CmdBatch,
        ] {
            let script = generate_shell_integration_script(kind);
            assert!(script.contains("133;A"));
            assert!(script.contains("133;B"));
            assert!(script.contains("133;D"));
        }
    }
}
