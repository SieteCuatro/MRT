use anyhow::{bail, Context, Result};
use mrt_core::StructuredOutput;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DnsRecordEntry {
    pub record_type: String,
    pub ip: String,
    pub latency_ms: Option<u64>,
    pub status: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnsTraceReport {
    pub host: String,
    pub port: u16,
    pub lookup_duration_ms: u64,
    pub records: Vec<DnsRecordEntry>,
    pub fastest_ip: Option<String>,
    pub min_latency_ms: Option<u64>,
}

/// Parses a user input string into a clean host and port.
/// Handles `:dns domain.com`, `https://host.com/path`, `host:8080`, etc.
pub fn parse_dns_query(input: &str) -> Result<(String, u16)> {
    let mut raw = input.trim();
    if let Some(stripped) = raw.strip_prefix(":dns") {
        raw = stripped.trim();
    }

    if raw.is_empty() {
        bail!("Missing hostname. Usage: :dns <hostname or domain>");
    }

    // Strip URL schemes if user pasted full URL
    let default_port = if raw.starts_with("http://") {
        raw = &raw["http://".len()..];
        80
    } else if raw.starts_with("https://") {
        raw = &raw["https://".len()..];
        443
    } else {
        443
    };

    // Strip trailing path/query/fragment
    if let Some(idx) = raw.find(['/', '?', '#']) {
        raw = &raw[..idx];
    }

    raw = raw.trim();

    // Check for IPv6 bracketed address with port, e.g. [::1]:8080
    if raw.starts_with('[') {
        if let Some(end_bracket) = raw.find(']') {
            let ip_part = &raw[1..end_bracket];
            let rest = &raw[end_bracket + 1..];
            let port = if let Some(colon) = rest.strip_prefix(':') {
                colon.parse::<u16>().context("Invalid port number")?
            } else {
                default_port
            };
            return Ok((ip_part.to_string(), port));
        }
    }

    // Check for standard host:port
    if let Some((h, p)) = raw.split_once(':') {
        if !h.contains(':') {
            // Single colon, host:port
            let port = p.parse::<u16>().context("Invalid port number")?;
            return Ok((h.trim().to_string(), port));
        }
    }

    Ok((raw.to_string(), default_port))
}

/// Resolves a hostname, tests TCP handshake latency to all resolved IPv4 & IPv6
/// addresses, and returns a structured table and plain text summary.
pub async fn inspect_dns(query: &str) -> Result<(StructuredOutput, String, DnsTraceReport)> {
    let (host, port) = parse_dns_query(query)?;

    let lookup_start = std::time::Instant::now();
    let addr_str = format!("{host}:{port}");

    let mut addrs: Vec<SocketAddr> = tokio::net::lookup_host(&addr_str)
        .await
        .with_context(|| format!("DNS lookup failed for '{host}' on port {port}"))?
        .collect();

    let lookup_duration_ms = lookup_start.elapsed().as_millis() as u64;

    if addrs.is_empty() {
        bail!("No IP addresses resolved for '{host}'");
    }

    // Deduplicate while preserving order
    addrs.dedup();

    // Spawn concurrent latency probes for each resolved address
    let mut tasks = Vec::new();
    for addr in addrs {
        let target_str = format!("{addr}");
        tasks.push(tokio::spawn(async move {
            let probe_start = std::time::Instant::now();
            let connect_fut = tokio::net::TcpStream::connect(addr);
            let (latency, status) = match tokio::time::timeout(Duration::from_millis(1500), connect_fut).await {
                Ok(Ok(stream)) => {
                    let ms = probe_start.elapsed().as_millis() as u64;
                    drop(stream);
                    (Some(ms), "Connected".to_string())
                }
                Ok(Err(err)) => {
                    let ms = probe_start.elapsed().as_millis() as u64;
                    let kind = format!("{:?}", err.kind());
                    (Some(ms), format!("Refused ({kind})"))
                }
                Err(_) => (None, "Timeout (1500ms)".to_string()),
            };

            let record_type = if addr.is_ipv4() {
                "A (IPv4)".to_string()
            } else {
                "AAAA (IPv6)".to_string()
            };

            DnsRecordEntry {
                record_type,
                ip: addr.ip().to_string(),
                latency_ms: latency,
                status,
                target: target_str,
            }
        }));
    }

    let mut records = Vec::new();
    for task in tasks {
        if let Ok(record) = task.await {
            records.push(record);
        }
    }

    // Sort records: connected with lowest latency first, then by IP
    records.sort_by(|a, b| {
        match (a.latency_ms, b.latency_ms) {
            (Some(la), Some(lb)) => la.cmp(&lb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.ip.cmp(&b.ip),
        }
    });

    let (fastest_ip, min_latency_ms) = records
        .iter()
        .filter(|r| r.latency_ms.is_some() && r.status == "Connected")
        .min_by_key(|r| r.latency_ms.unwrap_or(u64::MAX))
        .map(|r| (Some(r.ip.clone()), r.latency_ms))
        .unwrap_or((None, None));

    let report = DnsTraceReport {
        host: host.clone(),
        port,
        lookup_duration_ms,
        records: records.clone(),
        fastest_ip: fastest_ip.clone(),
        min_latency_ms,
    };

    // Build structured table
    let headers = vec![
        "Record".to_string(),
        "IP Address".to_string(),
        "Latency".to_string(),
        "Status".to_string(),
        "Target Socket".to_string(),
    ];

    let rows: Vec<Vec<String>> = records
        .iter()
        .map(|r| {
            vec![
                r.record_type.clone(),
                r.ip.clone(),
                r.latency_ms
                    .map(|ms| format!("{ms}ms"))
                    .unwrap_or_else(|| "-".to_string()),
                r.status.clone(),
                r.target.clone(),
            ]
        })
        .collect();

    let structured = StructuredOutput::Table { headers, rows };

    // Build plain text report
    let mut plain = String::new();
    plain.push_str(&format!(
        "DNS & Latency Trace for: {host}\nLookup time: {lookup_duration_ms}ms · Target Port: {port} · Resolved: {} address(es)\n\n",
        records.len()
    ));

    for r in &records {
        let lat_str = r
            .latency_ms
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "-".to_string());
        plain.push_str(&format!(
            "  • {:12} {:39}  Latency: {:6}  [{}]\n",
            r.record_type, r.ip, lat_str, r.status
        ));
    }

    if let (Some(fastest), Some(min_lat)) = (&fastest_ip, min_latency_ms) {
        plain.push_str(&format!("\nLowest latency: {fastest} ({min_lat}ms)\n"));
    }

    Ok((structured, plain, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_dns_query_basic() {
        let (host, port) = parse_dns_query("google.com").unwrap();
        assert_eq!(host, "google.com");
        assert_eq!(port, 443);
    }

    #[test]
    fn test_parse_dns_query_with_colon_prefix() {
        let (host, port) = parse_dns_query(":dns github.com").unwrap();
        assert_eq!(host, "github.com");
        assert_eq!(port, 443);
    }

    #[test]
    fn test_parse_dns_query_with_custom_port() {
        let (host, port) = parse_dns_query(":dns localhost:8080").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 8080);
    }

    #[test]
    fn test_parse_dns_query_from_full_url() {
        let (host, port) = parse_dns_query("http://example.com/api/v1?query=1").unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 80);

        let (host, port) = parse_dns_query("https://crates.io/crates/tokio").unwrap();
        assert_eq!(host, "crates.io");
        assert_eq!(port, 443);
    }

    #[test]
    fn test_parse_dns_query_bracketed_ipv6() {
        let (host, port) = parse_dns_query("[::1]:9090").unwrap();
        assert_eq!(host, "::1");
        assert_eq!(port, 9090);
    }

    #[test]
    fn test_parse_dns_query_empty_fails() {
        assert!(parse_dns_query(":dns").is_err());
        assert!(parse_dns_query("   ").is_err());
    }

    #[tokio::test]
    async fn test_inspect_dns_local_loopback() {
        // Test inspecting localhost (always resolvable offline)
        let res = inspect_dns("127.0.0.1:9").await;
        assert!(res.is_ok());
        let (structured, plain, report) = res.unwrap();
        assert_eq!(report.host, "127.0.0.1");
        assert_eq!(report.port, 9);
        assert!(!report.records.is_empty());
        assert!(plain.contains("127.0.0.1"));
        assert!(structured.is_structured());
    }
}
