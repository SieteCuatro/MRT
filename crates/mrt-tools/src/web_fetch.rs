use anyhow::{Context, Result};

/// Fetches a web page URL and extracts clean readable Markdown suitable for terminal blocks.
pub async fn fetch_url_as_markdown(url_input: &str) -> Result<String> {
    let trimmed = url_input
        .trim()
        .strip_prefix(":web")
        .unwrap_or(url_input.trim())
        .trim();

    if trimmed.is_empty() {
        anyhow::bail!("Usage: :web <url>");
    }

    let url = if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        format!("https://{trimmed}")
    } else {
        trimmed.to_string()
    };

    let client = reqwest::Client::builder()
        .user_agent("ModernRustTerminal/0.1 (+https://mrt.dev)")
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("Failed to fetch web page: {url}"))?;

    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let body = resp.text().await?;

    if content_type.contains("application/json") {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&body) {
            return Ok(format!(
                "# Web JSON Resource: {url}\n- **Status**: {}\n\n```json\n{}\n```",
                status,
                serde_json::to_string_pretty(&val).unwrap_or(body)
            ));
        }
    }

    let md = html_to_readable_markdown(&url, &body);
    Ok(md)
}

/// Converts raw HTML into clean, readable Markdown without external C dependencies.
pub fn html_to_readable_markdown(url: &str, html: &str) -> String {
    let title = extract_tag_text(html, "title").unwrap_or_else(|| url.to_string());
    let cleaned = strip_blocks(html, &["script", "style", "svg", "noscript", "head"]);

    let mut out = String::new();
    out.push_str(&format!("# {}\n", decode_html_entities(title.trim())));
    out.push_str(&format!("- **Source**: {}\n\n", url));

    // Convert structural HTML tags into Markdown equivalents before stripping remaining tags
    let mut working = cleaned;
    for (open, repl) in [
        ("<h1", "\n## "),
        ("<h2", "\n## "),
        ("<h3", "\n### "),
        ("<li", "\n- "),
        ("<p", "\n\n"),
        ("<br", "\n"),
        ("<tr", "\n"),
        ("<pre", "\n```\n"),
        ("</pre>", "\n```\n"),
    ] {
        working = replace_case_insensitive_tag_start(&working, open, repl);
    }

    let text_only = strip_all_tags(&working);
    let decoded = decode_html_entities(&text_only);

    let mut consecutive_blank = 0;
    for line in decoded.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            consecutive_blank += 1;
            if consecutive_blank <= 1 {
                out.push('\n');
            }
        } else {
            consecutive_blank = 0;
            out.push_str(trimmed);
            out.push('\n');
        }
    }

    out.trim().to_string()
}

fn extract_tag_text(html: &str, tag: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let open_pat = format!("<{tag}");
    let close_pat = format!("</{tag}>");
    let start_idx = lower.find(&open_pat)?;
    let gt_idx = lower[start_idx..].find('>')? + start_idx + 1;
    let end_idx = lower[gt_idx..].find(&close_pat)? + gt_idx;
    Some(strip_all_tags(&html[gt_idx..end_idx]))
}

fn strip_blocks(html: &str, tags: &[&str]) -> String {
    let mut current = html.to_string();
    for tag in tags {
        let open_pat = format!("<{tag}");
        let close_pat = format!("</{tag}>");
        loop {
            let lower = current.to_lowercase();
            if let Some(start) = lower.find(&open_pat) {
                if let Some(end_rel) = lower[start..].find(&close_pat) {
                    let end = start + end_rel + close_pat.len();
                    current.replace_range(start..end, " ");
                    continue;
                }
            }
            break;
        }
    }
    current
}

fn replace_case_insensitive_tag_start(input: &str, tag_prefix: &str, replacement: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let lower = input.to_lowercase();
    let pat = tag_prefix.to_lowercase();
    let mut cursor = 0;

    while let Some(pos) = lower[cursor..].find(&pat) {
        let abs_pos = cursor + pos;
        result.push_str(&input[cursor..abs_pos]);
        if pat.ends_with('>') {
            result.push_str(replacement);
            cursor = abs_pos + pat.len();
        } else if let Some(gt_rel) = lower[abs_pos..].find('>') {
            result.push_str(replacement);
            cursor = abs_pos + gt_rel + 1;
        } else {
            result.push_str(&input[abs_pos..abs_pos + pat.len()]);
            cursor = abs_pos + pat.len();
        }
    }
    result.push_str(&input[cursor..]);
    result
}

fn strip_all_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn decode_html_entities(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
}
