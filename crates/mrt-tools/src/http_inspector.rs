use anyhow::{Context, Result};
use mrt_core::HttpExchangeSummary;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::str::FromStr;
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HttpRequestSpec {
    pub method: String,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
}

impl Default for HttpRequestSpec {
    fn default() -> Self {
        Self {
            method: "GET".to_string(),
            url: "https://httpbin.org/json".to_string(),
            headers: BTreeMap::new(),
            body: None,
        }
    }
}

/// Parses an inline command like `:http GET https://api.github.com/zen` or `:http POST https://httpbin.org/post {"a":1}`
pub fn parse_http_command(input: &str) -> Result<HttpRequestSpec> {
    let stripped = input
        .trim()
        .strip_prefix(":http")
        .or_else(|| input.trim().strip_prefix(":curl"))
        .unwrap_or(input.trim())
        .trim();

    if stripped.is_empty() {
        anyhow::bail!("Usage: :http [GET|POST|PUT|DELETE] <url> [optional_json_body]");
    }

    let mut parts = stripped.splitn(3, char::is_whitespace);
    let first = parts.next().unwrap_or("");
    let upper = first.to_uppercase();

    let (method, url, body) = if matches!(
        upper.as_str(),
        "GET" | "POST" | "PUT" | "DELETE" | "PATCH" | "HEAD"
    ) {
        let url_part = parts
            .next()
            .context("Missing URL after HTTP method. Example: :http GET https://httpbin.org/json")?;
        let body_part = parts.next().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        (upper, url_part.trim().to_string(), body_part)
    } else {
        let rest = parts.next().map(|s| s.trim().to_string());
        ("GET".to_string(), first.trim().to_string(), rest)
    };

    let normalized_url = if !url.starts_with("http://") && !url.starts_with("https://") {
        format!("https://{url}")
    } else {
        url
    };

    let mut headers = BTreeMap::new();
    headers.insert("User-Agent".to_string(), concat!("ModernRustTerminal/", env!("CARGO_PKG_VERSION")).to_string());
    if body.is_some() {
        headers.insert("Content-Type".to_string(), "application/json".to_string());
    }

    Ok(HttpRequestSpec {
        method,
        url: normalized_url,
        headers,
        body,
    })
}

pub async fn execute_http_request(spec: &HttpRequestSpec) -> Result<HttpExchangeSummary> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;

    let method = Method::from_str(&spec.method.to_uppercase()).unwrap_or(Method::GET);
    let mut req = client.request(method.clone(), &spec.url);

    for (k, v) in &spec.headers {
        req = req.header(k, v);
    }

    if let Some(body) = &spec.body {
        if !body.trim().is_empty() {
            req = req.body(body.clone());
        }
    }

    let start = Instant::now();
    let resp = req
        .send()
        .await
        .with_context(|| format!("HTTP request failed for {}", spec.url))?;
    let status = resp.status();
    let status_code = status.as_u16();
    let status_text = status.canonical_reason().unwrap_or("OK").to_string();

    let mut response_headers = BTreeMap::new();
    for (name, val) in resp.headers() {
        response_headers.insert(
            name.to_string(),
            val.to_str().unwrap_or("<binary>").to_string(),
        );
    }

    let body_raw = resp.text().await.unwrap_or_default();
    let duration_ms = start.elapsed().as_millis() as u64;

    let body_json = serde_json::from_str::<serde_json::Value>(&body_raw).ok();
    let formatted_raw = if let Some(ref j) = body_json {
        serde_json::to_string_pretty(j).unwrap_or_else(|_| body_raw.clone())
    } else {
        body_raw
    };

    Ok(HttpExchangeSummary {
        method: method.to_string(),
        url: spec.url.clone(),
        status_code,
        status_text,
        duration_ms,
        response_headers,
        body_raw: formatted_raw,
        body_json,
    })
}
