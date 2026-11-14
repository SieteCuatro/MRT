use mrt_core::CommandBlock;

/// Redacts known secret tokens, credentials, and private keys from command strings
/// and output logs before generating a bug report or sharing.
pub fn redact_sensitive_text(input: &str) -> String {
    let mut text = input.to_string();

    // 1. Redact PEM private key blocks
    while let Some(start) = text.find("-----BEGIN ") {
        if let Some(end_marker) = text[start..].find("-----END ") {
            let actual_end_marker = start + end_marker;
            if let Some(final_dashes) = text[actual_end_marker..].find("-----") {
                let block_end = actual_end_marker + final_dashes + 5;
                text.replace_range(start..block_end, "[REDACTED_PRIVATE_KEY]");
                continue;
            }
        }
        break;
    }

    // 2. Token prefixes and known patterns:
    // (prefix, min_len, name)
    let secret_prefixes: &[(&str, usize, &str)] = &[
        ("ghp_", 30, "[REDACTED_GITHUB_TOKEN]"),
        ("gho_", 30, "[REDACTED_GITHUB_TOKEN]"),
        ("ghu_", 30, "[REDACTED_GITHUB_TOKEN]"),
        ("ghs_", 30, "[REDACTED_GITHUB_TOKEN]"),
        ("ghr_", 30, "[REDACTED_GITHUB_TOKEN]"),
        ("sk-proj-", 20, "[REDACTED_OPENAI_KEY]"),
        ("sk-ant-", 20, "[REDACTED_ANTHROPIC_KEY]"),
        ("sk-", 20, "[REDACTED_API_KEY]"),
        ("AIzaSy", 30, "[REDACTED_GOOGLE_API_KEY]"),
        ("AKIA", 16, "[REDACTED_AWS_KEY]"),
    ];

    for (prefix, min_len, replacement) in secret_prefixes {
        let mut search_idx = 0;
        while let Some(pos) = text[search_idx..].find(prefix) {
            let actual_pos = search_idx + pos;
            let token_start = actual_pos;
            let mut token_end = token_start + prefix.len();

            let bytes = text.as_bytes();
            while token_end < bytes.len() {
                let b = bytes[token_end];
                if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
                    token_end += 1;
                } else {
                    break;
                }
            }

            let token_len = token_end - token_start;
            if token_len >= *min_len {
                text.replace_range(token_start..token_end, replacement);
                search_idx = token_start + replacement.len();
            } else {
                search_idx = token_end;
            }
        }
    }

    // 3. Redact Authorization Bearer headers
    let mut search_idx = 0;
    while let Some(pos) = text[search_idx..].to_lowercase().find("bearer ") {
        let actual_pos = search_idx + pos;
        let token_start = actual_pos + "bearer ".len();
        let mut token_end = token_start;
        let bytes = text.as_bytes();
        while token_end < bytes.len() {
            let b = bytes[token_end];
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_' || b == b'~' || b == b'+' || b == b'/' || b == b'=' {
                token_end += 1;
            } else {
                break;
            }
        }
        if token_end > token_start {
            text.replace_range(token_start..token_end, "[REDACTED_BEARER_TOKEN]");
            search_idx = token_start + "[REDACTED_BEARER_TOKEN]".len();
        } else {
            search_idx = actual_pos + "bearer ".len();
        }
    }

    // 4. Redact assignment patterns: password=..., secret=..., token=..., api_key=...
    let sensitive_keys: &[&str] = &[
        "password=",
        "password:",
        "passwd=",
        "secret=",
        "secret:",
        "api_key=",
        "apikey=",
        "access_token=",
        "auth_token=",
    ];

    for key in sensitive_keys {
        let mut search_idx = 0;
        while let Some(pos) = text[search_idx..].to_lowercase().find(key) {
            let actual_pos = search_idx + pos;
            let val_start = actual_pos + key.len();

            // Skip quotes if present
            let bytes = text.as_bytes();
            let mut val_end = val_start;
            let is_quoted = val_start < bytes.len() && (bytes[val_start] == b'"' || bytes[val_start] == b'\'');
            let quote_char = if is_quoted { bytes[val_start] } else { 0 };

            if is_quoted {
                val_end = val_start + 1;
                while val_end < bytes.len() && bytes[val_end] != quote_char {
                    val_end += 1;
                }
                if val_end < bytes.len() && bytes[val_end] == quote_char {
                    val_end += 1;
                }
            } else {
                while val_end < bytes.len() && !bytes[val_end].is_ascii_whitespace() && bytes[val_end] != b'&' && bytes[val_end] != b';' && bytes[val_end] != b',' {
                    val_end += 1;
                }
            }

            if val_end > val_start {
                text.replace_range(val_start..val_end, "[REDACTED_SECRET]");
                search_idx = val_start + "[REDACTED_SECRET]".len();
            } else {
                search_idx = actual_pos + key.len();
            }
        }
    }

    text
}

/// Generates a GitHub-flavored Markdown bug report from a failed command block
/// and its surrounding context, with automatic sensitive data redaction.
pub fn generate_markdown_bug_report(
    target: &CommandBlock,
    history: &[CommandBlock],
    os_info: &str,
    app_version: &str,
) -> String {
    let exit_code_str = target
        .exit_code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "none (interrupted)".into());

    let redacted_cmd = redact_sensitive_text(&target.command);
    let redacted_output = redact_sensitive_text(&target.plain_output);
    let redacted_cwd = redact_sensitive_text(&target.cwd);

    let mut md = String::new();
    md.push_str(&format!(
        "## Bug Report: `{}` failed (exit {})\n\n",
        redacted_cmd, exit_code_str
    ));

    md.push_str("### Environment\n");
    md.push_str(&format!("- **OS:** {}\n", os_info));
    md.push_str(&format!("- **Shell:** {}\n", target.shell_name));
    md.push_str(&format!("- **MRT Version:** {}\n", app_version));
    md.push_str(&format!("- **Working Directory:** `{}`\n", redacted_cwd));
    md.push_str(&format!(
        "- **Timestamp:** {}\n",
        target.timestamp.format("%Y-%m-%d %H:%M:%S")
    ));
    md.push_str(&format!("- **Execution Time:** {}ms\n\n", target.duration_ms));

    md.push_str("### Failed Command\n");
    md.push_str("```shell\n");
    md.push_str(&redacted_cmd);
    md.push_str("\n```\n\n");

    md.push_str("### Command Output\n");
    md.push_str("```text\n");
    if redacted_output.trim().is_empty() {
        md.push_str("(No output captured)\n");
    } else {
        md.push_str(redacted_output.trim());
        md.push_str("\n");
    }
    md.push_str("```\n\n");

    if !history.is_empty() {
        md.push_str("### Preceding Command History\n");
        for (i, blk) in history.iter().rev().take(5).collect::<Vec<_>>().into_iter().rev().enumerate() {
            let code = blk
                .exit_code
                .map(|c| format!("exit {c}"))
                .unwrap_or_else(|| "running/killed".into());
            let safe_cmd = redact_sensitive_text(&blk.command);
            md.push_str(&format!("{}. `{}` ({})\n", i + 1, safe_cmd, code));
        }
        md.push_str("\n");
    }

    if let Some(ai) = &target.ai_explanation {
        let safe_ai = redact_sensitive_text(ai);
        md.push_str("### Automated Diagnosis\n");
        md.push_str(&safe_ai);
        md.push_str("\n\n");
    }

    md.push_str("---\n");
    md.push_str("*Generated by [Modern Rust Terminal (MRT)](https://github.com/SieteCuatro/MRT)*\n");

    md
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_sensitive_tokens() {
        let raw = "Exporting GITHUB_TOKEN=ghp_1234567890abcdef1234567890abcdef12345 and OpenAI key=sk-12345678901234567890123456";
        let redacted = redact_sensitive_text(raw);
        assert!(!redacted.contains("ghp_1234567890abcdef"));
        assert!(redacted.contains("[REDACTED_GITHUB_TOKEN]"));
        assert!(!redacted.contains("sk-12345678901234567890123456"));
        assert!(redacted.contains("[REDACTED_API_KEY]"));
    }

    #[test]
    fn test_redact_bearer_and_passwords() {
        let raw = "curl -H 'Authorization: Bearer mySecretTokenValue123==' https://api.com?password=SuperSecretPassword123";
        let redacted = redact_sensitive_text(raw);
        assert!(!redacted.contains("mySecretTokenValue123=="));
        assert!(redacted.contains("[REDACTED_BEARER_TOKEN]"));
        assert!(!redacted.contains("SuperSecretPassword123"));
        assert!(redacted.contains("[REDACTED_SECRET]"));
    }

    #[test]
    fn test_redact_private_key() {
        let raw = "Here is my key:\n-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA0...\n-----END RSA PRIVATE KEY-----\nDone.";
        let redacted = redact_sensitive_text(raw);
        assert!(!redacted.contains("MIIEowIBAAKCAQEA0"));
        assert!(redacted.contains("[REDACTED_PRIVATE_KEY]"));
    }

    #[test]
    fn test_generate_markdown_bug_report() {
        let blk = CommandBlock::new_completed(
            "cargo test --failed-pkg",
            "pwsh",
            "C:\\project",
            "error[E0432]: unresolved import `foo::bar`\nAuthorization: Bearer secret_tok",
            101,
            340,
        );

        let report = generate_markdown_bug_report(
            &blk,
            &[],
            "Windows 11 (x86_64)",
            "0.1.0",
        );

        assert!(report.contains("## Bug Report: `cargo test --failed-pkg` failed (exit 101)"));
        assert!(report.contains("- **OS:** Windows 11 (x86_64)"));
        assert!(report.contains("- **Shell:** pwsh"));
        assert!(report.contains("unresolved import `foo::bar`"));
        assert!(!report.contains("secret_tok"));
        assert!(report.contains("[REDACTED_BEARER_TOKEN]"));
    }
}
