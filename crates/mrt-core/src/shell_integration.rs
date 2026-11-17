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

/// Extracts the working directory from an OSC 7 payload (`file://<host>/<path>`, the part
/// after `7;`). Percent escapes are decoded and Windows drive paths (`/C:/x`) lose the
/// leading slash. Returns `None` for anything that is not a `file://` URL.
pub fn parse_osc7_path(payload: &str) -> Option<std::path::PathBuf> {
    let rest = payload.strip_prefix("file://")?;
    let path = &rest[rest.find('/')?..];
    let decoded = percent_decode(path);
    let bytes = decoded.as_bytes();
    let is_drive_path =
        bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':';
    let path = if is_drive_path {
        decoded[1..].replace('/', "\\")
    } else {
        decoded
    };
    Some(std::path::PathBuf::from(path))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(value) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Generates the shell integration script for the specified shell kind.
///
/// Each script reports `OSC 133;A` (prompt start), `B` (prompt end), `D;<exit code>`
/// (command finished) and `OSC 7` (working directory). `OSC 133;C` (output start) is printed
/// by the `__mrt_c` helper (`__mrt_sb` in PowerShell), which the persistent
/// [`ShellSession`](crate::shell_session::ShellSession) puts in front of every command it
/// types, because most shells have no reliable pre-execution hook. CMD has no hooks at all, so
/// its script only marks the prompt; the session prints `C` and `D` from helper batch files.
/// The scripts are idempotent: running one twice installs the hooks once.
pub fn generate_shell_integration_script(kind: ShellKind) -> &'static str {
    match kind {
        ShellKind::Bash | ShellKind::GitBash => BASH_SCRIPT,
        ShellKind::Sh => SH_SCRIPT,
        ShellKind::Zsh => ZSH_SCRIPT,
        ShellKind::Fish => FISH_SCRIPT,
        ShellKind::PowerShellCore | ShellKind::WindowsPowerShell => POWERSHELL_SCRIPT,
        ShellKind::CmdBatch => CMD_SCRIPT,
        ShellKind::Wsl | ShellKind::Nushell => "# OSC 133 not supported for this shell kind\n",
    }
}

const BASH_SCRIPT: &str = r#"# MRT Shell Integration for Bash (OSC 133 + OSC 7)
if [ -z "${__mrt_installed-}" ]; then
__mrt_installed=1
__mrt_running=
__mrt_user_prompt_command=${PROMPT_COMMAND-}
__mrt_osc() { printf '\033]%s\007' "$1"; }
__mrt_c() { __mrt_running=1; __mrt_osc '133;C'; }
__mrt_status() { return "$1"; }
__mrt_precmd() {
    local __mrt_s=$?
    __mrt_osc "7;file://${HOSTNAME:-localhost}${PWD}"
    if [ -n "$__mrt_running" ]; then
        __mrt_running=
        __mrt_osc "133;D;$__mrt_s"
    fi
    if [ -n "$__mrt_user_prompt_command" ]; then
        __mrt_status "$__mrt_s"
        eval "$__mrt_user_prompt_command"
    fi
    case "$PS1" in
        *'133;A'*) ;;
        *) PS1='\[\033]133;A\007\]'"$PS1"'\[\033]133;B\007\]' ;;
    esac
    return "$__mrt_s"
}
PROMPT_COMMAND=__mrt_precmd
fi
"#;

const SH_SCRIPT: &str = r#"# MRT Shell Integration for POSIX sh (OSC 133 + OSC 7)
# sh has no prompt hook, so commands are wrapped as `__mrt_c; <command>; __mrt_d $?`.
if [ -z "${__mrt_installed-}" ]; then
__mrt_installed=1
__mrt_osc() { printf '\033]%s\007' "$1"; }
__mrt_c() { __mrt_osc '133;C'; }
__mrt_d() { __mrt_osc "7;file://${HOSTNAME:-localhost}${PWD}"; __mrt_osc "133;D;$1"; return "$1"; }
__mrt_e=$(printf '\033')
__mrt_b=$(printf '\007')
PS1="${__mrt_e}]133;A${__mrt_b}${PS1-\$ }${__mrt_e}]133;B${__mrt_b}"
unset __mrt_e __mrt_b
fi
"#;

const ZSH_SCRIPT: &str = r#"# MRT Shell Integration for Zsh (OSC 133 + OSC 7)
if [[ -z ${__mrt_installed-} ]]; then
typeset -g __mrt_installed=1 __mrt_running=
__mrt_osc() { printf '\033]%s\007' "$1"; }
__mrt_c() { __mrt_running=1; __mrt_osc '133;C'; }
__mrt_precmd() {
    local __mrt_s=$?
    __mrt_osc "7;file://${HOST:-localhost}${PWD}"
    if [[ -n $__mrt_running ]]; then
        __mrt_running=
        __mrt_osc "133;D;$__mrt_s"
    fi
    return $__mrt_s
}
__mrt_prompt_markers() {
    if [[ $PS1 != *'133;A'* ]]; then
        PS1=$'%{\e]133;A\a%}'"$PS1"$'%{\e]133;B\a%}'
    fi
}
typeset -ga precmd_functions
precmd_functions=(__mrt_precmd $precmd_functions __mrt_prompt_markers)
# PROMPT_SP prints a "%" and a line of spaces before the prompt to flag output that
# did not end in a newline. zsh writes it before the precmd hooks, so it would land
# inside the command's output; MRT already shows each command as its own block.
unsetopt PROMPT_SP
fi
"#;

const FISH_SCRIPT: &str = r#"# MRT Shell Integration for Fish (OSC 133 + OSC 7)
if not set -q __mrt_installed
    set -g __mrt_installed 1
    set -g __mrt_running 0
    function __mrt_osc
        printf '\e]%s\a' $argv[1]
    end
    function __mrt_c
        set -g __mrt_running 1
        __mrt_osc '133;C'
    end
    function __mrt_postexec --on-event fish_postexec
        set -l s $status
        if test "$__mrt_running" = 1
            set -g __mrt_running 0
            __mrt_osc "7;file://$hostname$PWD"
            __mrt_osc "133;D;$s"
        end
    end
    functions -q fish_prompt
    functions -c fish_prompt __mrt_original_fish_prompt
    function fish_prompt
        __mrt_osc '133;A'
        __mrt_osc "7;file://$hostname$PWD"
        __mrt_original_fish_prompt
        __mrt_osc '133;B'
    end
end
"#;

const POWERSHELL_SCRIPT: &str = r#"# MRT Shell Integration for PowerShell (OSC 133 + OSC 7), Windows PowerShell 5.1 and 7+
if (-not $global:__mrt_installed) {
    $global:__mrt_installed = $true
    $global:__mrt_running = $false
    $global:__mrt_ok = $null
    $global:__mrt_saved_lec = $null
    $global:__mrt_original_prompt = $function:prompt
    function global:__mrt_osc([string]$Payload) {
        [Console]::Write([string][char]27 + ']' + $Payload + [string][char]27 + '\')
    }
    # Returns the command stored in $Path as a script block. The caller dot-sources it
    # (`. (__mrt_sb <path>)`) so variables, functions and the location persist globally.
    # `$?` does not survive dot-sourcing, so the block records it itself.
    function global:__mrt_sb([string]$Path) {
        $global:__mrt_saved_lec = $global:LASTEXITCODE
        $global:LASTEXITCODE = $null
        $global:__mrt_ok = $null
        $global:__mrt_running = $true
        __mrt_osc '133;C'
        [scriptblock]::Create([IO.File]::ReadAllText($Path) + "`n`$global:__mrt_ok = `$?")
    }
    function global:prompt {
        $lastOk = $?
        $loc = $ExecutionContext.SessionState.Path.CurrentLocation
        if ($loc.Provider.Name -eq 'FileSystem') {
            $p = $loc.ProviderPath -replace '\\', '/'
            if (-not $p.StartsWith('/')) { $p = '/' + $p }
            __mrt_osc ('7;file://localhost' + $p)
        }
        if ($global:__mrt_running) {
            $global:__mrt_running = $false
            $ok = if ($null -ne $global:__mrt_ok) { [bool]$global:__mrt_ok } else { $lastOk }
            $lec = $global:LASTEXITCODE
            if ($null -eq $lec) { $global:LASTEXITCODE = $global:__mrt_saved_lec }
            $code = if ($ok) { 0 } elseif ($lec -is [int] -and $lec -ne 0) { $lec } else { 1 }
            __mrt_osc ('133;D;' + $code)
        }
        __mrt_osc '133;A'
        $text = if ($global:__mrt_original_prompt) { & $global:__mrt_original_prompt } else { 'PS ' + $loc + '> ' }
        [string]$text + [string][char]27 + ']133;B' + [string][char]27 + '\'
    }
}
"#;

const CMD_SCRIPT: &str = r#"REM MRT Shell Integration for Windows CMD (OSC 133 + OSC 7)
REM CMD has no command hooks: the prompt marks A and B, and the MRT session prints
REM 133;C and 133;D;%ERRORLEVEL% from helper batch files around each command.
PROMPT $E]133;A$E\$E]7;file://localhost/$P$E\$P$G$E]133;B$E\
"#;

// ---------------------------------------------------------------------------
// Remote sessions (SSH)
// ---------------------------------------------------------------------------

/// Longest line MRT types into a remote shell. Before `readline` starts, a terminal in
/// canonical mode keeps at most ~4 KB per line, so the bootstrap is sent in chunks.
const REMOTE_LINE_LIMIT: usize = 900;

/// Longest base64-encoded command typed into a remote session in one line.
pub const REMOTE_COMMAND_LIMIT: usize = 3000;

/// The integration installed in a remote shell over SSH: the Bash or Zsh script,
/// picked by the shell itself, plus `__mrt_decode` for the base64 commands MRT types.
/// Other shells (fish, plain sh) install nothing, so no prompt marker appears and the
/// session reports that integration did not start.
pub fn remote_integration_script() -> String {
    format!(
        "if [ -n \"${{BASH_VERSION-}}\" ]; then\n{BASH_SCRIPT}\nelif [ -n \"${{ZSH_VERSION-}}\" ]; then\n{ZSH_SCRIPT}\nfi\n\
if printf 'YQ==' | base64 -d >/dev/null 2>&1; then __mrt_decode() {{ base64 -d; }}; else __mrt_decode() {{ base64 -D; }}; fi\n"
    )
}

/// Lines MRT types into a freshly started remote shell to install the integration
/// without writing any file on the remote machine: the base64 script is assembled in
/// a variable over several short lines, then decoded and `eval`ed. Every line starts
/// with a space so shells with `HISTCONTROL=ignorespace` keep them out of history.
pub fn remote_bootstrap_lines() -> Vec<String> {
    let encoded = base64_encode(remote_integration_script().as_bytes());
    let chunk = REMOTE_LINE_LIMIT - 32;
    let mut lines = vec![" __mrt_boot=''".to_string()];
    for piece in encoded.as_bytes().chunks(chunk) {
        // Base64 only contains [A-Za-z0-9+/=], safe inside double quotes.
        lines.push(format!(" __mrt_boot=\"${{__mrt_boot}}{}\"", String::from_utf8_lossy(piece)));
    }
    lines.push(
        " export MRT_SHELL_INTEGRATION=1; eval \"$(printf %s \"$__mrt_boot\" | base64 -d 2>/dev/null || printf %s \"$__mrt_boot\" | base64 -D)\"; unset __mrt_boot"
            .to_string(),
    );
    lines
}

/// The line typed into a remote session to run `command`: marks the start of the
/// command (`133;C`) and `eval`s it from base64, so quotes, backslashes and multiple
/// lines reach the shell unchanged. `None` when the command is too long for one line.
pub fn remote_command_line(command: &str) -> Option<String> {
    let encoded = base64_encode(command.replace("\r\n", "\n").as_bytes());
    (encoded.len() <= REMOTE_COMMAND_LIMIT)
        .then(|| format!(" __mrt_c; eval \"$(printf %s '{encoded}' | __mrt_decode)\""))
}

/// Standard base64 with padding (RFC 4648).
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(input.as_bytes()), expected);
        }
    }

    #[test]
    fn remote_bootstrap_lines_are_short_and_rebuild_the_script() {
        let lines = remote_bootstrap_lines();
        assert!(lines.iter().all(|l| l.len() <= REMOTE_LINE_LIMIT + 200), "a line is too long");
        assert!(lines.iter().all(|l| l.starts_with(' ')), "lines must stay out of history");
        let encoded: String = lines[1..lines.len() - 1]
            .iter()
            .map(|l| l.trim_start().trim_start_matches("__mrt_boot=\"${__mrt_boot}").trim_end_matches('"'))
            .collect();
        assert_eq!(encoded, base64_encode(remote_integration_script().as_bytes()));
    }

    #[test]
    fn remote_command_lines_encode_the_command() {
        let line = remote_command_line("echo 'a b' \"c\"\nprintf x").unwrap();
        assert!(line.starts_with(" __mrt_c; eval"));
        assert!(line.contains(&base64_encode(b"echo 'a b' \"c\"\nprintf x")));
        assert!(remote_command_line(&"x".repeat(REMOTE_COMMAND_LIMIT)).is_none());
    }

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
            ShellKind::GitBash,
            ShellKind::Sh,
            ShellKind::Zsh,
            ShellKind::Fish,
            ShellKind::PowerShellCore,
            ShellKind::WindowsPowerShell,
            ShellKind::CmdBatch,
        ] {
            let script = generate_shell_integration_script(kind.clone());
            assert!(script.contains("133;A"), "{kind:?}");
            assert!(script.contains("133;B"), "{kind:?}");
            assert!(script.contains("133;D"), "{kind:?}");
            assert!(script.contains("7;file://"), "{kind:?}");
        }
        assert!(generate_shell_integration_script(ShellKind::Nushell).contains("not supported"));
    }

    #[test]
    fn osc7_paths_decode_for_unix_and_windows() {
        use std::path::PathBuf;
        assert_eq!(
            parse_osc7_path("file://host/home/me/my%20dir"),
            Some(PathBuf::from("/home/me/my dir"))
        );
        assert_eq!(
            parse_osc7_path("file://localhost/C:/Users/me"),
            Some(PathBuf::from(r"C:\Users\me"))
        );
        assert_eq!(
            parse_osc7_path(r"file://localhost/C:\Users\me"),
            Some(PathBuf::from(r"C:\Users\me"))
        );
        assert_eq!(parse_osc7_path("file://localhost/100%"), Some(PathBuf::from("/100%")));
        assert_eq!(parse_osc7_path("https://example.com/"), None);
    }
}
