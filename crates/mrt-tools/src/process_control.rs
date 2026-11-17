//! Task Manager process control beyond stopping: suspend, resume and priority.
//!
//! `:suspend <pid|name>...`, `:resume <pid|name>...` and
//! `:priority <idle|low|normal|high|...> <pid|name>...` resolve their targets like
//! `:kill` (same snapshot and Safety Gate classification). Targets the gate rates
//! Critical (OS processes, MRT itself, other users' processes, ambiguous names) are
//! refused; the others are changed directly, because every change can be undone.
//!
//! Windows uses `NtSuspendProcess` / `NtResumeProcess` (ntdll) and
//! `SetPriorityClass`; Unix uses `SIGSTOP` / `SIGCONT` and `setpriority`.

use crate::process_inspector::{plan_kill, KillRequest, OwnedProcessMap, ProcessSelector, ProcessSnapshot};
use anyhow::{bail, Result};
use mrt_core::RiskLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
}

impl Priority {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().replace('-', "_").as_str() {
            "idle" | "lowest" => Priority::Idle,
            "low" | "below" | "below_normal" => Priority::BelowNormal,
            "normal" => Priority::Normal,
            "above" | "above_normal" => Priority::AboveNormal,
            "high" => Priority::High,
            _ => return None,
        })
    }

    pub fn label(&self) -> &'static str {
        match self {
            Priority::Idle => "idle",
            Priority::BelowNormal => "below normal",
            Priority::Normal => "normal",
            Priority::AboveNormal => "above normal",
            Priority::High => "high",
        }
    }

    /// The Unix nice value used for this level.
    pub fn nice(&self) -> i32 {
        match self {
            Priority::Idle => 19,
            Priority::BelowNormal => 10,
            Priority::Normal => 0,
            Priority::AboveNormal => -5,
            Priority::High => -10,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAction {
    Suspend,
    Resume,
    SetPriority(Priority),
}

impl ControlAction {
    pub fn verb(&self) -> String {
        match self {
            ControlAction::Suspend => "Suspended".into(),
            ControlAction::Resume => "Resumed".into(),
            ControlAction::SetPriority(p) => format!("Priority set to {}", p.label()),
        }
    }
}

/// A parsed `:suspend` / `:resume` / `:priority` command.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlRequest {
    pub action: ControlAction,
    pub selectors: Vec<ProcessSelector>,
}

/// Parses `:suspend <targets>`, `:resume <targets>` or `:priority <level> <targets>`.
pub fn parse_control_command(input: &str) -> Result<ControlRequest> {
    let mut words = input.split_whitespace();
    let head = words.next().unwrap_or_default();
    let action = match head {
        ":suspend" => ControlAction::Suspend,
        ":resume" => ControlAction::Resume,
        ":priority" => {
            let level = words.next().unwrap_or_default();
            match Priority::parse(level) {
                Some(p) => ControlAction::SetPriority(p),
                None => bail!("Usage: :priority <idle|low|normal|above|high> <pid|name>..."),
            }
        }
        other => bail!("Unknown process command {other}"),
    };
    let selectors: Vec<ProcessSelector> = words.map(ProcessSelector::parse).collect();
    if selectors.is_empty() {
        bail!("Usage: {head}{} <pid|name>...", if head == ":priority" { " <level>" } else { "" });
    }
    Ok(ControlRequest { action, selectors })
}

/// The outcome for one process.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlOutcome {
    pub pid: u32,
    pub name: String,
    pub result: Result<(), String>,
}

/// Resolves the targets and refuses the request when the Safety Gate rates it
/// Critical. Returns `(pid, name)` pairs.
pub fn plan_control(snapshot: &ProcessSnapshot, request: &ControlRequest, owned: &OwnedProcessMap) -> Result<Vec<(u32, String)>> {
    let kill = KillRequest { selectors: request.selectors.clone(), force: false, tree: false };
    let plan = plan_kill(snapshot, &kill, owned)?;
    if !plan.unresolved.is_empty() && plan.targets.is_empty() {
        bail!("No process matches {}", plan.unresolved.join(", "));
    }
    if plan.assessment.risk == RiskLevel::Critical {
        bail!("Refused: {}", plan.assessment.reason);
    }
    Ok(plan.targets.iter().map(|t| (t.pid, t.name.clone())).collect())
}

/// Applies the action to every target.
pub fn apply_control(action: ControlAction, targets: &[(u32, String)]) -> Vec<ControlOutcome> {
    targets
        .iter()
        .map(|(pid, name)| ControlOutcome { pid: *pid, name: name.clone(), result: apply_one(action, *pid) })
        .collect()
}

pub fn control_outcomes_markdown(action: ControlAction, outcomes: &[ControlOutcome]) -> String {
    let mut out = String::new();
    for o in outcomes {
        match &o.result {
            Ok(()) => out.push_str(&format!("{} {} (PID {}).\n", action.verb(), o.name, o.pid)),
            Err(e) => out.push_str(&format!("Could not change {} (PID {}): {e}\n", o.name, o.pid)),
        }
    }
    if action == ControlAction::Suspend && outcomes.iter().any(|o| o.result.is_ok()) {
        out.push_str("Resume with :resume <pid>.\n");
    }
    out
}

#[cfg(windows)]
mod sys {
    type Handle = isize;
    const PROCESS_SET_INFORMATION: u32 = 0x0200;
    const PROCESS_SUSPEND_RESUME: u32 = 0x0800;

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn CloseHandle(handle: Handle) -> i32;
        fn SetPriorityClass(handle: Handle, class: u32) -> i32;
        fn GetLastError() -> u32;
    }

    #[link(name = "ntdll")]
    extern "system" {
        fn NtSuspendProcess(handle: Handle) -> i32;
        fn NtResumeProcess(handle: Handle) -> i32;
    }

    fn with_process(pid: u32, access: u32, f: impl FnOnce(Handle) -> Result<(), String>) -> Result<(), String> {
        // SAFETY: plain Win32 calls; the handle is closed before returning.
        unsafe {
            let handle = OpenProcess(access, 0, pid);
            if handle == 0 {
                return Err(format!("cannot open the process (Windows error {})", GetLastError()));
            }
            let result = f(handle);
            CloseHandle(handle);
            result
        }
    }

    pub fn suspend(pid: u32, resume: bool) -> Result<(), String> {
        with_process(pid, PROCESS_SUSPEND_RESUME, |h| {
            // SAFETY: `h` is a valid process handle with PROCESS_SUSPEND_RESUME.
            let status = unsafe { if resume { NtResumeProcess(h) } else { NtSuspendProcess(h) } };
            if status < 0 {
                Err(format!("NTSTATUS 0x{:08X}", status as u32))
            } else {
                Ok(())
            }
        })
    }

    pub fn set_priority(pid: u32, priority: super::Priority) -> Result<(), String> {
        let class = match priority {
            super::Priority::Idle => 0x40,
            super::Priority::BelowNormal => 0x4000,
            super::Priority::Normal => 0x20,
            super::Priority::AboveNormal => 0x8000,
            super::Priority::High => 0x80,
        };
        with_process(pid, PROCESS_SET_INFORMATION, |h| {
            // SAFETY: `h` is a valid process handle with PROCESS_SET_INFORMATION.
            if unsafe { SetPriorityClass(h, class) } == 0 {
                Err(format!("Windows error {}", unsafe { GetLastError() }))
            } else {
                Ok(())
            }
        })
    }
}

#[cfg(unix)]
mod sys {
    fn errno() -> String {
        std::io::Error::last_os_error().to_string()
    }

    pub fn suspend(pid: u32, resume: bool) -> Result<(), String> {
        let signal = if resume { libc::SIGCONT } else { libc::SIGSTOP };
        // SAFETY: sending a signal to a PID has no memory-safety requirements.
        if unsafe { libc::kill(pid as libc::pid_t, signal) } == 0 {
            Ok(())
        } else {
            Err(errno())
        }
    }

    pub fn set_priority(pid: u32, priority: super::Priority) -> Result<(), String> {
        // SAFETY: setpriority only reads its integer arguments.
        let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, pid as libc::id_t, priority.nice()) };
        if rc == 0 {
            Ok(())
        } else {
            Err(format!("{} (raising priority usually needs administrator rights)", errno()))
        }
    }
}

fn apply_one(action: ControlAction, pid: u32) -> Result<(), String> {
    if pid == std::process::id() {
        return Err("MRT does not change its own process".into());
    }
    match action {
        ControlAction::Suspend => sys::suspend(pid, false),
        ControlAction::Resume => sys::suspend(pid, true),
        ControlAction::SetPriority(p) => sys::set_priority(pid, p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        let r = parse_control_command(":suspend 42 node").unwrap();
        assert_eq!(r.action, ControlAction::Suspend);
        assert_eq!(r.selectors.len(), 2);
        assert_eq!(
            parse_control_command(":priority below-normal 7").unwrap().action,
            ControlAction::SetPriority(Priority::BelowNormal)
        );
        assert!(parse_control_command(":priority turbo 7").is_err());
        assert!(parse_control_command(":resume").is_err());
        assert!(parse_control_command(":kill 1").is_err());
    }

    fn spawn_sleeper() -> std::process::Child {
        let mut cmd = if cfg!(windows) {
            let mut c = std::process::Command::new("ping");
            c.args(["-n", "30", "127.0.0.1"]);
            c
        } else {
            let mut c = std::process::Command::new("sleep");
            c.arg("30");
            c
        };
        cmd.stdout(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000);
        }
        cmd.spawn().expect("spawn sleeper")
    }

    #[test]
    fn suspends_resumes_and_lowers_priority_of_a_child() {
        let mut child = spawn_sleeper();
        let pid = child.id();
        let targets = vec![(pid, "sleeper".to_string())];
        for action in [ControlAction::Suspend, ControlAction::Resume, ControlAction::SetPriority(Priority::Idle)] {
            let outcomes = apply_control(action, &targets);
            assert_eq!(outcomes[0].result, Ok(()), "{action:?}");
        }
        #[cfg(unix)]
        {
            // SAFETY: getpriority reads integers only.
            let nice = unsafe { libc::getpriority(libc::PRIO_PROCESS, pid as libc::id_t) };
            assert_eq!(nice, 19);
        }
        let text = control_outcomes_markdown(ControlAction::Suspend, &apply_control(ControlAction::Suspend, &targets));
        assert!(text.contains("Suspended sleeper") && text.contains(":resume"), "{text}");
        let _ = apply_control(ControlAction::Resume, &targets);
        let _ = child.kill();
        let _ = child.wait();
        assert!(apply_control(ControlAction::Suspend, &[(std::process::id(), "mrt".into())])[0].result.is_err());
    }
}
