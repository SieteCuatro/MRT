//! Service management for the Task Manager: `:services [filter]` lists system
//! services, `:service start|stop|restart <name>` changes one.
//!
//! Listing uses the platform's own tool (`sc.exe queryex` on Windows,
//! `systemctl list-units` on Linux, `launchctl list` on macOS). Actions become the
//! matching shell command (`sc start`, `systemctl restart`, `launchctl kickstart`…),
//! which the GUI submits like typed input, so the Safety Gate always asks first
//! ([`mrt_core::assess_command`] rates service control Moderate with approval, and
//! services the OS needs Critical).

use anyhow::{bail, Result};
use mrt_core::StructuredOutput;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceEntry {
    pub name: String,
    pub state: String,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

/// Parses `:service <start|stop|restart> <name>`.
pub fn parse_service_command(input: &str) -> Result<(ServiceAction, String)> {
    let mut words = input.split_whitespace().skip(1);
    let action = match words.next() {
        Some("start") => ServiceAction::Start,
        Some("stop") => ServiceAction::Stop,
        Some("restart") => ServiceAction::Restart,
        _ => bail!("Usage: :service <start|stop|restart> <name>"),
    };
    let name: Vec<&str> = words.collect();
    if name.len() != 1 || name[0].chars().any(|c| !(c.is_alphanumeric() || "._-@:".contains(c))) {
        bail!("Usage: :service <start|stop|restart> <name> (one service name: letters, digits, . _ - @ :)");
    }
    Ok((action, name[0].to_string()))
}

/// The shell command that performs `action` on this OS (`os` is `std::env::consts::OS`).
pub fn service_shell_command(os: &str, action: ServiceAction, name: &str) -> String {
    match (os, action) {
        ("windows", ServiceAction::Start) => format!("sc.exe start {name}"),
        ("windows", ServiceAction::Stop) => format!("sc.exe stop {name}"),
        ("windows", ServiceAction::Restart) => format!("sc.exe stop {name} & sc.exe start {name}"),
        ("macos", ServiceAction::Start) => format!("launchctl kickstart gui/$(id -u)/{name}"),
        ("macos", ServiceAction::Stop) => format!("launchctl kill TERM gui/$(id -u)/{name}"),
        ("macos", ServiceAction::Restart) => format!("launchctl kickstart -k gui/$(id -u)/{name}"),
        (_, ServiceAction::Start) => format!("sudo systemctl start {name}"),
        (_, ServiceAction::Stop) => format!("sudo systemctl stop {name}"),
        (_, ServiceAction::Restart) => format!("sudo systemctl restart {name}"),
    }
}

/// Parses `sc.exe queryex type= service state= all` output.
pub fn parse_sc_query(text: &str) -> Vec<ServiceEntry> {
    let mut out = Vec::new();
    let mut current: Option<ServiceEntry> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("SERVICE_NAME:") {
            if let Some(done) = current.take() {
                out.push(done);
            }
            current = Some(ServiceEntry { name: name.trim().to_string(), state: String::new(), description: String::new() });
        } else if let Some(entry) = current.as_mut() {
            if let Some(display) = line.strip_prefix("DISPLAY_NAME:") {
                entry.description = display.trim().to_string();
            } else if let Some(state) = line.strip_prefix("STATE") {
                // "STATE              : 4  RUNNING"
                entry.state = state.split_whitespace().last().unwrap_or_default().to_ascii_lowercase();
            }
        }
    }
    out.extend(current);
    out
}

/// Parses `systemctl list-units --type=service --all --no-pager --plain --no-legend`.
pub fn parse_systemctl(text: &str) -> Vec<ServiceEntry> {
    text.lines()
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let unit = cols.next()?;
            let _load = cols.next()?;
            let active = cols.next()?;
            let sub = cols.next()?;
            let description = cols.collect::<Vec<_>>().join(" ");
            Some(ServiceEntry {
                name: unit.trim_end_matches(".service").to_string(),
                state: format!("{active} ({sub})"),
                description,
            })
        })
        .collect()
}

/// Parses `launchctl list` (`PID Status Label`).
pub fn parse_launchctl(text: &str) -> Vec<ServiceEntry> {
    text.lines()
        .skip_while(|l| l.starts_with("PID"))
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let pid = cols.next()?;
            let status = cols.next()?;
            let label = cols.next()?;
            let state = if pid == "-" { format!("stopped (last exit {status})") } else { format!("running (pid {pid})") };
            Some(ServiceEntry { name: label.to_string(), state, description: String::new() })
        })
        .collect()
}

/// Lists services, keeping those whose name or description contains `filter`.
pub async fn list_services(filter: &str) -> Result<Vec<ServiceEntry>> {
    let (program, args, parse): (&str, &[&str], fn(&str) -> Vec<ServiceEntry>) = match std::env::consts::OS {
        "windows" => ("sc.exe", &["queryex", "type=", "service", "state=", "all"], parse_sc_query),
        "macos" => ("launchctl", &["list"], parse_launchctl),
        _ => ("systemctl", &["list-units", "--type=service", "--all", "--no-pager", "--plain", "--no-legend"], parse_systemctl),
    };
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args).stdin(std::process::Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let output = cmd.output().await.map_err(|e| anyhow::anyhow!("could not run {program}: {e}"))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let filter = filter.trim().to_lowercase();
    Ok(parse(&text)
        .into_iter()
        .filter(|s| filter.is_empty() || s.name.to_lowercase().contains(&filter) || s.description.to_lowercase().contains(&filter))
        .collect())
}

/// A table block for `:services`.
pub fn services_to_table(services: &[ServiceEntry]) -> (StructuredOutput, String) {
    let rows: Vec<Vec<String>> = services.iter().map(|s| vec![s.name.clone(), s.state.clone(), s.description.clone()]).collect();
    let mut text = format!("{} services\n", services.len());
    for s in services {
        text.push_str(&format!("{}\t{}\t{}\n", s.name, s.state, s.description));
    }
    (StructuredOutput::Table { headers: vec!["Name".into(), "State".into(), "Description".into()], rows }, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_service_commands() {
        assert_eq!(parse_service_command(":service restart nginx").unwrap(), (ServiceAction::Restart, "nginx".into()));
        assert!(parse_service_command(":service stop").is_err());
        assert!(parse_service_command(":service stop a;rm").is_err(), "names cannot carry shell syntax");
        assert!(parse_service_command(":service pause x").is_err());
        assert_eq!(service_shell_command("linux", ServiceAction::Stop, "nginx"), "sudo systemctl stop nginx");
        assert_eq!(service_shell_command("windows", ServiceAction::Start, "Spooler"), "sc.exe start Spooler");
    }

    #[test]
    fn parses_platform_listings() {
        let sc = "SERVICE_NAME: Spooler\n        DISPLAY_NAME: Print Spooler\n        TYPE               : 110  WIN32_OWN_PROCESS\n        STATE              : 4  RUNNING\n\nSERVICE_NAME: wuauserv\nDISPLAY_NAME: Windows Update\n        STATE              : 1  STOPPED\n";
        let parsed = parse_sc_query(sc);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0], ServiceEntry { name: "Spooler".into(), state: "running".into(), description: "Print Spooler".into() });
        assert_eq!(parsed[1].state, "stopped");

        let systemd = "nginx.service loaded active running A high performance web server\ncron.service loaded inactive dead Regular background program\n";
        let parsed = parse_systemctl(systemd);
        assert_eq!(parsed[0].name, "nginx");
        assert_eq!(parsed[0].state, "active (running)");
        assert_eq!(parsed[1].description, "Regular background program");

        let launchd = "PID\tStatus\tLabel\n123\t0\tcom.example.agent\n-\t78\tcom.example.stopped\n";
        let parsed = parse_launchctl(launchd);
        assert_eq!(parsed.len(), 2);
        assert!(parsed[0].state.starts_with("running"));
        assert!(parsed[1].state.starts_with("stopped"));

        let (table, text) = services_to_table(&parsed);
        assert!(matches!(table, StructuredOutput::Table { ref rows, .. } if rows.len() == 2));
        assert!(text.starts_with("2 services"));
    }

    #[tokio::test]
    async fn lists_services_on_this_machine() {
        // WSL and containers may have no service manager; only check it does not fail badly.
        match list_services("").await {
            Ok(list) => assert!(list.iter().all(|s| !s.name.is_empty())),
            Err(e) => eprintln!("no service manager here: {e}"),
        }
    }
}
