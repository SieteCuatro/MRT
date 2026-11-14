use anyhow::Result;
use mrt_core::{ShellKind, ShellProfile};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiCommandProposal {
    pub query: String,
    pub target_shell: String,
    pub proposed_command: String,
    pub explanation: String,
    pub provider_used: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiErrorTriage {
    pub explanation: String,
    pub suggested_fix: Option<String>,
}

/// Translates a natural language prompt (`# ...` or `? ...`) into an idiomatic command
/// for the user's active shell (`pwsh`, `powershell`, `cmd`, `bash`, `wsl`, `zsh`, `nu`).
///
/// Supports ALL major AI providers out of the box (checked in order of configured keys):
/// 1. Google Gemini (`GEMINI_API_KEY`)
/// 2. Anthropic Claude (`ANTHROPIC_API_KEY`)
/// 3. OpenAI (`OPENAI_API_KEY`)
/// 4. xAI Grok (`XAI_API_KEY`)
/// 5. DeepSeek (`DEEPSEEK_API_KEY`)
/// 6. OpenRouter (`OPENROUTER_API_KEY`)
/// 7. Local Ollama (`OLLAMA_MODEL` / `OLLAMA_HOST`)
/// 8. Deterministic Offline Polyglot Engine (100% free, zero-key fallback)
pub async fn translate_nl_to_command(
    query: &str,
    shell: &ShellProfile,
    cwd: &str,
    mcp_tool_names: &[String],
) -> Result<AiCommandProposal> {
    let cleaned_query = query
        .trim()
        .trim_start_matches('#')
        .trim_start_matches('?')
        .trim();

    let system_prompt = build_system_prompt(cleaned_query, shell, cwd, mcp_tool_names);

    // 1. Google Gemini
    if let Ok(api_key) = std::env::var("GEMINI_API_KEY") {
        if !api_key.trim().is_empty() {
            if let Ok(p) = call_gemini_translator(&api_key, cleaned_query, shell, &system_prompt).await {
                return Ok(p);
            }
        }
    }

    // 2. Anthropic Claude
    if let Ok(api_key) = std::env::var("ANTHROPIC_API_KEY") {
        if !api_key.trim().is_empty() {
            if let Ok(p) = call_anthropic_translator(&api_key, cleaned_query, shell, &system_prompt).await {
                return Ok(p);
            }
        }
    }

    // 3. OpenAI, xAI Grok, DeepSeek, or OpenRouter (OpenAI-compatible chat completions)
    let openai_compatible_providers = [
        (
            "OPENAI_API_KEY",
            "https://api.openai.com/v1/chat/completions",
            "gpt-4o-mini",
            "OpenAI (gpt-4o-mini)",
        ),
        (
            "XAI_API_KEY",
            "https://api.x.ai/v1/chat/completions",
            "grok-2-latest",
            "xAI Grok",
        ),
        (
            "DEEPSEEK_API_KEY",
            "https://api.deepseek.com/chat/completions",
            "deepseek-chat",
            "DeepSeek",
        ),
        (
            "OPENROUTER_API_KEY",
            "https://openrouter.ai/api/v1/chat/completions",
            "openrouter/auto",
            "OpenRouter",
        ),
    ];

    for (env_var, endpoint, model, label) in openai_compatible_providers {
        if let Ok(api_key) = std::env::var(env_var) {
            if !api_key.trim().is_empty() {
                if let Ok(p) = call_openai_compatible_translator(
                    endpoint,
                    &api_key,
                    model,
                    label,
                    cleaned_query,
                    shell,
                    &system_prompt,
                )
                .await
                {
                    return Ok(p);
                }
            }
        }
    }

    // 4. Local Ollama (if OLLAMA_MODEL is set)
    if let Ok(ollama_model) = std::env::var("OLLAMA_MODEL") {
        if !ollama_model.trim().is_empty() {
            let host = std::env::var("OLLAMA_HOST")
                .unwrap_or_else(|_| "http://localhost:11434".to_string());
            if let Ok(p) = call_ollama_translator(
                &host,
                &ollama_model,
                cleaned_query,
                shell,
                &system_prompt,
            )
            .await
            {
                return Ok(p);
            }
        }
    }

    // 5. Built-in Offline Polyglot Heuristic Engine
    Ok(heuristic_shell_translator(cleaned_query, shell))
}

fn build_system_prompt(
    query: &str,
    shell: &ShellProfile,
    cwd: &str,
    mcp_tool_names: &[String],
) -> String {
    format!(
        "You are the built-in AI assistant inside Modern Rust Terminal (MRT).\n\
         Target OS: {}\n\
         Target Shell: {} ({})\n\
         Current Directory: {}\n\
         Connected MCP Tools: {:?}\n\
         User Request: {}\n\n\
         Respond strictly in JSON with two fields: \"command\" (the exact shell command or MRT built-in tool like :http, :web, :ports) and \"explanation\" (one concise sentence).",
        std::env::consts::OS,
        shell.name,
        shell.program,
        cwd,
        mcp_tool_names,
        query
    )
}

async fn call_gemini_translator(
    api_key: &str,
    query: &str,
    shell: &ShellProfile,
    prompt: &str,
) -> Result<AiCommandProposal> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key={api_key}"
    );
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()?;
    let resp = client
        .post(&url)
        .json(&json!({
            "contents": [{ "parts": [{ "text": prompt }] }],
            "generationConfig": { "responseMimeType": "application/json" }
        }))
        .send()
        .await?;

    let val: serde_json::Value = resp.json().await?;
    let text = val
        .pointer("/candidates/0/content/parts/0/text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid Gemini response"))?;
    parse_ai_json_response(text, query, shell, "Gemini 2.5 Flash")
}

async fn call_anthropic_translator(
    api_key: &str,
    query: &str,
    shell: &ShellProfile,
    prompt: &str,
) -> Result<AiCommandProposal> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()?;
    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": "claude-3-7-sonnet-latest",
            "max_tokens": 300,
            "messages": [{ "role": "user", "content": prompt }]
        }))
        .send()
        .await?;

    let val: serde_json::Value = resp.json().await?;
    let text = val
        .pointer("/content/0/text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid Anthropic response"))?;
    parse_ai_json_response(text, query, shell, "Anthropic Claude")
}

async fn call_openai_compatible_translator(
    endpoint: &str,
    api_key: &str,
    model: &str,
    provider_label: &str,
    query: &str,
    shell: &ShellProfile,
    prompt: &str,
) -> Result<AiCommandProposal> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()?;
    let resp = client
        .post(endpoint)
        .bearer_auth(api_key)
        .json(&json!({
            "model": model,
            "messages": [{ "role": "user", "content": prompt }],
            "response_format": { "type": "json_object" }
        }))
        .send()
        .await?;

    let val: serde_json::Value = resp.json().await?;
    let text = val
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid response from {provider_label}"))?;
    parse_ai_json_response(text, query, shell, provider_label)
}

async fn call_ollama_translator(
    host: &str,
    model: &str,
    query: &str,
    shell: &ShellProfile,
    prompt: &str,
) -> Result<AiCommandProposal> {
    let url = format!("{}/api/generate", host.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let resp = client
        .post(&url)
        .json(&json!({
            "model": model,
            "prompt": prompt,
            "format": "json",
            "stream": false
        }))
        .send()
        .await?;

    let val: serde_json::Value = resp.json().await?;
    let text = val
        .get("response")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid Ollama response"))?;
    parse_ai_json_response(text, query, shell, &format!("Ollama ({model})"))
}

fn parse_ai_json_response(
    raw_text: &str,
    query: &str,
    shell: &ShellProfile,
    provider_label: &str,
) -> Result<AiCommandProposal> {
    let parsed: serde_json::Value = serde_json::from_str(raw_text.trim())?;
    let cmd = parsed
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let exp = parsed
        .get("explanation")
        .and_then(|v| v.as_str())
        .unwrap_or("Generated via AI provider")
        .to_string();

    if cmd.is_empty() {
        anyhow::bail!("Empty command from {provider_label}");
    }

    Ok(AiCommandProposal {
        query: query.to_string(),
        target_shell: shell.name.clone(),
        proposed_command: cmd,
        explanation: exp,
        provider_used: provider_label.to_string(),
    })
}

/// Deterministic, shell-aware polyglot command synthesizer supporting PowerShell, CMD/Batch, Bash/Zsh/WSL, and MRT tools.
pub fn heuristic_shell_translator(query: &str, shell: &ShellProfile) -> AiCommandProposal {
    let lower = query.to_lowercase();
    let is_ps = matches!(
        shell.kind,
        ShellKind::PowerShellCore | ShellKind::WindowsPowerShell
    );
    let is_cmd = matches!(shell.kind, ShellKind::CmdBatch);

    let (cmd, explanation) = if lower.contains("port") || lower.contains("listening") {
        (
            ":ports".to_string(),
            "Uses MRT's built-in interactive local port & PID inspector.".to_string(),
        )
    } else if lower.contains("list") && (lower.contains("file") || lower.contains("dir")) {
        if is_ps {
            (
                "Get-ChildItem -Force | Select-Object Mode, LastWriteTime, Length, Name | ConvertTo-Json"
                    .to_string(),
                "Lists directory entries as structured JSON for MRT's JSON Tree viewer in PowerShell."
                    .to_string(),
            )
        } else if is_cmd {
            (
                "dir /a".to_string(),
                "Lists all files including hidden items using Windows Command Prompt (Batch)."
                    .to_string(),
            )
        } else {
            (
                "ls -la".to_string(),
                "Lists all files with permissions and sizes in POSIX/Bash shell.".to_string(),
            )
        }
    } else if lower.contains("git") && (lower.contains("log") || lower.contains("history") || lower.contains("graph")) {
        (
            "git log --oneline --graph --decorate -n 15".to_string(),
            "Displays a compact 15-commit branch topology graph.".to_string(),
        )
    } else if lower.contains("system") || lower.contains("os") || lower.contains("info") {
        if is_ps {
            (
                "[PSCustomObject]@{ OS = $PSVersionTable.OS; PSVersion = $PSVersionTable.PSVersion.ToString(); User = $env:USERNAME; Path = (Get-Location).Path } | ConvertTo-Json".to_string(),
                "Outputs current OS and PowerShell runtime info as structured JSON.".to_string(),
            )
        } else if is_cmd {
            (
                "ver && echo User: %USERNAME% && cd".to_string(),
                "Displays Windows version, user, and working directory in CMD/Batch.".to_string(),
            )
        } else {
            (
                "uname -a && id && pwd".to_string(),
                "Prints kernel, user identity, and working directory in Bash/POSIX.".to_string(),
            )
        }
    } else if lower.contains("process") || lower.contains("running") || lower.contains("cpu") {
        if is_ps {
            (
                "Get-Process | Sort-Object WorkingSet64 -Descending | Select-Object -First 10 Id, ProcessName, CPU, WorkingSet64 | ConvertTo-Csv -NoTypeInformation".to_string(),
                "Lists top 10 memory-consuming processes as a sortable CSV table in MRT.".to_string(),
            )
        } else if is_cmd {
            (
                "tasklist /fo csv /nh".to_string(),
                "Outputs running Windows processes in CSV format for MRT's table view.".to_string(),
            )
        } else {
            (
                "ps aux --sort=-%mem | head -n 15".to_string(),
                "Lists top memory-consuming processes in Bash.".to_string(),
            )
        }
    } else if lower.contains("ip") || lower.contains("network") {
        if is_ps {
            (
                "Get-NetIPAddress | Select-Object InterfaceAlias, IPAddress, AddressFamily | ConvertTo-Json".to_string(),
                "Retrieves network interface IP addresses as structured JSON.".to_string(),
            )
        } else if is_cmd {
            (
                "ipconfig /all".to_string(),
                "Shows full network adapter configuration in Windows CMD.".to_string(),
            )
        } else {
            (
                "ip addr || ifconfig".to_string(),
                "Shows network interface addresses in Unix/Bash.".to_string(),
            )
        }
    } else if lower.contains("http") || lower.contains("api") || lower.contains("test") {
        (
            ":http GET https://httpbin.org/json".to_string(),
            "Runs MRT's built-in HTTP API Inspector against a sample JSON endpoint.".to_string(),
        )
    } else {
        if is_ps {
            (
                format!("Get-ChildItem -Recurse -Filter \"*{}*\" -ErrorAction SilentlyContinue", query.split_whitespace().last().unwrap_or("*")),
                format!("Searches recursively in PowerShell for items matching '{query}'."),
            )
        } else if is_cmd {
            (
                format!("dir /s /b *{}*", query.split_whitespace().last().unwrap_or("*")),
                format!("Searches recursively in CMD/Batch for files matching '{query}'."),
            )
        } else {
            (
                format!("find . -iname \"*{}*\"", query.split_whitespace().last().unwrap_or("*")),
                format!("Searches recursively in Bash/POSIX for files matching '{query}'."),
            )
        }
    };

    AiCommandProposal {
        query: query.to_string(),
        target_shell: shell.name.clone(),
        proposed_command: cmd,
        explanation,
        provider_used: format!("MRT Polyglot Engine ({})", shell.kind.short_label()),
    }
}

/// Diagnoses a failed command block (non-zero exit code) and suggests a shell-appropriate fix.
pub fn diagnose_command_error(
    command: &str,
    output: &str,
    exit_code: i32,
    shell: &ShellProfile,
) -> AiErrorTriage {
    let lower_cmd = command.trim().to_lowercase();
    let lower_out = output.to_lowercase();
    let is_cmd = matches!(shell.kind, ShellKind::CmdBatch);
    let is_ps = matches!(
        shell.kind,
        ShellKind::PowerShellCore | ShellKind::WindowsPowerShell
    );

    if lower_cmd.starts_with("ls ") && is_cmd {
        return AiErrorTriage {
            explanation: "`ls` is a POSIX/PowerShell command and is not built into Windows `cmd.exe` (Batch).".into(),
            suggested_fix: Some("dir".into()),
        };
    }

    if lower_cmd.starts_with("export ") && (is_cmd || is_ps) {
        let var_part = command.trim().strip_prefix("export ").unwrap_or("VAR=value");
        let fix = if is_cmd {
            format!("set {var_part}")
        } else if let Some((k, v)) = var_part.split_once('=') {
            format!("$env:{} = \"{}\"", k.trim(), v.trim())
        } else {
            format!("$env:{var_part}")
        };
        return AiErrorTriage {
            explanation: format!("`export` is Bash syntax; `{}` uses a different syntax for environment variables.", shell.name),
            suggested_fix: Some(fix),
        };
    }

    if lower_out.contains("is not recognized as an internal or external command")
        || lower_out.contains(" is not recognized as the name of a cmdlet")
        || lower_out.contains("command not found")
    {
        let bin = command.split_whitespace().next().unwrap_or(command);
        return AiErrorTriage {
            explanation: format!(
                "Command `{bin}` (exit code {exit_code}) was not found in `{}`'s PATH or is specific to a different shell.",
                shell.name
            ),
            suggested_fix: if is_ps {
                Some(format!("Get-Command *{bin}*"))
            } else if is_cmd {
                Some(format!("where {bin}"))
            } else {
                Some(format!("which {bin} || type {bin}"))
            },
        };
    }

    if lower_out.contains("directory not found") || lower_out.contains("cannot find path") || lower_out.contains("no such file or directory") {
        return AiErrorTriage {
            explanation: "The target file or directory path does not exist relative to the current working directory.".into(),
            suggested_fix: Some(if is_cmd { "dir".into() } else if is_ps { "Get-ChildItem".into() } else { "ls -la".into() }),
        };
    }

    AiErrorTriage {
        explanation: format!(
            "Command `{command}` exited with code {exit_code} in {}. Inspect stderr above or switch shell profiles if the syntax targets another shell.",
            shell.name
        ),
        suggested_fix: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mrt_core::discover_shells;

    #[test]
    fn test_polyglot_translator_adapts_to_shell() {
        let shells = discover_shells();
        let first = &shells[0];
        let prop = heuristic_shell_translator("list all files", first);
        assert!(!prop.proposed_command.is_empty());
    }
}
