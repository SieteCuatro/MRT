use anyhow::Result;
use mrt_core::StructuredOutput;
use serde::{Deserialize, Serialize};
use std::process::Stdio;
use tokio::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortEntry {
    pub protocol: String,
    pub local_address: String,
    pub port: String,
    pub state: String,
    pub pid: String,
}

/// Inspects active listening TCP/UDP ports on the host OS and returns both a structured
/// table (`StructuredOutput::Table`) and a plain text summary.
pub async fn inspect_listening_ports() -> Result<(StructuredOutput, String)> {
    let entries = if cfg!(target_os = "windows") {
        let mut cmd = Command::new("netstat");
        cmd.args(["-ano"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(target_os = "windows")]
        {
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let output = cmd.output().await?;
        let text = String::from_utf8_lossy(&output.stdout);
        parse_windows_netstat(&text)
    } else {
        let output = Command::new("ss")
            .args(["-tulnp"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;
        match output {
            Ok(out) if out.status.success() => {
                parse_unix_ss(&String::from_utf8_lossy(&out.stdout))
            }
            _ => Vec::new(),
        }
    };

    let headers = vec![
        "Protocol".to_string(),
        "Local Address".to_string(),
        "Port".to_string(),
        "State".to_string(),
        "PID".to_string(),
    ];

    let mut rows = Vec::new();
    let mut csv_text = String::from("Protocol,Local Address,Port,State,PID\n");

    for entry in entries.into_iter().take(60) {
        csv_text.push_str(&format!(
            "{},{},{},{},{}\n",
            entry.protocol, entry.local_address, entry.port, entry.state, entry.pid
        ));
        rows.push(vec![
            entry.protocol,
            entry.local_address,
            entry.port,
            entry.state,
            entry.pid,
        ]);
    }

    Ok((StructuredOutput::Table { headers, rows }, csv_text))
}

pub fn parse_windows_netstat(text: &str) -> Vec<PortEntry> {
    let mut entries = Vec::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 4 {
            let proto = parts[0].to_uppercase();
            if proto == "TCP" && parts.len() >= 5 {
                let state = parts[3];
                if state.eq_ignore_ascii_case("LISTENING") {
                    let (addr, port) = split_addr_port(parts[1]);
                    entries.push(PortEntry {
                        protocol: proto,
                        local_address: addr,
                        port,
                        state: state.to_string(),
                        pid: parts[4].to_string(),
                    });
                }
            }
        }
    }
    entries
}

pub fn parse_unix_ss(text: &str) -> Vec<PortEntry> {
    let mut entries = Vec::new();
    for line in text.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 5 {
            let proto = parts[0].to_uppercase();
            let state = parts[1].to_string();
            let (addr, port) = split_addr_port(parts[4]);
            let pid = parts.get(6).copied().unwrap_or("-").to_string();
            entries.push(PortEntry {
                protocol: proto,
                local_address: addr,
                port,
                state,
                pid,
            });
        }
    }
    entries
}

fn split_addr_port(endpoint: &str) -> (String, String) {
    if let Some(idx) = endpoint.rfind(':') {
        (
            endpoint[..idx].to_string(),
            endpoint[idx + 1..].to_string(),
        )
    } else {
        (endpoint.to_string(), "-".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_inspector::parse_http_command;
    use crate::web_fetch::html_to_readable_markdown;

    #[test]
    fn test_parse_http_command() {
        let spec = parse_http_command(":http POST https://httpbin.org/post {\"x\":42}").unwrap();
        assert_eq!(spec.method, "POST");
        assert_eq!(spec.url, "https://httpbin.org/post");
        assert_eq!(spec.body.as_deref(), Some("{\"x\":42}"));
    }

    #[test]
    fn test_html_to_markdown() {
        let sample = "<html><head><title>Rust Docs</title><script>alert(1)</script></head><body><h1>Welcome</h1><p>Fast and safe.</p><ul><li>Zero cost</li><li>Memory safe</li></ul></body></html>";
        let md = html_to_readable_markdown("https://rust-lang.org", sample);
        assert!(md.contains("# Rust Docs"));
        assert!(md.contains("## Welcome"));
        assert!(md.contains("- Zero cost"));
        assert!(!md.contains("alert(1)"));
    }

    #[test]
    fn test_windows_netstat_parser() {
        let sample = "  TCP    0.0.0.0:8080           0.0.0.0:0              LISTENING       1234\n  TCP    127.0.0.1:5432         0.0.0.0:0              LISTENING       5678\n";
        let entries = parse_windows_netstat(sample);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].port, "8080");
        assert_eq!(entries[0].pid, "1234");
        assert_eq!(entries[1].port, "5432");
    }
}
