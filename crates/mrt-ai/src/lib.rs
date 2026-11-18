pub mod assist;
pub mod keys;
pub mod usage;
pub use assist::{run_assist, AssistOutcome, AssistStep, ASSIST_TOOLS};
pub use usage::{format_usd, group_digits, set_ai_prices, usage_from_response, AiPrice, AiUsage};
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
    /// Tokens and estimated cost of the provider calls behind this proposal (also
    /// calls that failed to give a usable reply). `None` for the offline engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<AiUsage>,
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

    let backends = plan_ai_backends(&crate::keys::lookup, &ai_preferences());

    // Every reply costs tokens, also one that was not usable.
    let mut usage = AiUsage::default();
    for backend in backends {
        let result = match &backend {
            AiBackend::Gemini { api_key, model } => call_gemini_translator(api_key, model, &system_prompt).await,
            AiBackend::Anthropic { api_key, model } => call_anthropic_translator(api_key, model, &system_prompt).await,
            AiBackend::OpenAiCompatible { endpoint, api_key, model, label, .. } => {
                call_openai_compatible_translator(endpoint, api_key, model, label, &system_prompt).await
            }
            AiBackend::Ollama { host, model } => call_ollama_translator(host, model, &system_prompt).await,
        };
        let Ok(reply) = result else { continue };
        usage.add(&usage_from_response(&backend, &reply));
        let label = match &backend {
            AiBackend::Gemini { model, .. } => format!("Google Gemini ({model})"),
            AiBackend::Anthropic { model, .. } => format!("Anthropic Claude ({model})"),
            AiBackend::OpenAiCompatible { label, .. } => label.clone(),
            AiBackend::Ollama { model, .. } => format!("Ollama ({model})"),
        };
        let text = reply_text(&backend, &reply);
        if let Some(Ok(mut proposal)) = text.map(|t| parse_ai_json_response(t, cleaned_query, shell, &label)) {
            proposal.usage = Some(usage);
            return Ok(proposal);
        }
    }

    // Built-in Offline Polyglot Heuristic Engine
    let mut proposal = heuristic_shell_translator(cleaned_query, shell);
    proposal.usage = (!usage.is_empty()).then_some(usage);
    Ok(proposal)
}

/// The text of a provider's reply.
pub(crate) fn reply_text<'a>(backend: &AiBackend, reply: &'a serde_json::Value) -> Option<&'a str> {
    let pointer = match backend {
        AiBackend::Gemini { .. } => "/candidates/0/content/parts/0/text",
        AiBackend::Anthropic { .. } => "/content/0/text",
        AiBackend::OpenAiCompatible { .. } => "/choices/0/message/content",
        AiBackend::Ollama { .. } => "/response",
    };
    reply.pointer(pointer).and_then(|v| v.as_str())
}

/// Provider preferences from MRT's configuration file (`[ai]`). API keys never come
/// from here: they are read from environment variables only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AiPreferences {
    /// `None`/`"auto"`: try providers in the default order. A provider id tries that
    /// provider first; `"offline"` always uses the heuristic engine.
    pub provider: Option<String>,
    /// Model for the preferred provider (`OLLAMA_MODEL` wins for Ollama).
    pub model: Option<String>,
    /// Ollama base URL (`OLLAMA_HOST` wins).
    pub ollama_endpoint: Option<String>,
}

static AI_PREFERENCES: std::sync::RwLock<AiPreferences> = std::sync::RwLock::new(AiPreferences {
    provider: None,
    model: None,
    ollama_endpoint: None,
});

/// Replaces the process-wide AI provider preferences (called when the config loads).
pub fn set_ai_preferences(preferences: AiPreferences) {
    match AI_PREFERENCES.write() {
        Ok(mut guard) => *guard = preferences,
        Err(poisoned) => *poisoned.into_inner() = preferences,
    }
}

/// The current process-wide AI provider preferences.
pub fn ai_preferences() -> AiPreferences {
    match AI_PREFERENCES.read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// One AI provider MRT can call, with its resolved key, endpoint and model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiBackend {
    Gemini {
        api_key: String,
        model: String,
    },
    Anthropic {
        api_key: String,
        model: String,
    },
    OpenAiCompatible {
        id: &'static str,
        endpoint: String,
        api_key: String,
        model: String,
        label: String,
    },
    Ollama {
        host: String,
        model: String,
    },
}

impl AiBackend {
    /// Provider id as written in `ai.provider`.
    pub fn id(&self) -> &'static str {
        match self {
            AiBackend::Gemini { .. } => "gemini",
            AiBackend::Anthropic { .. } => "anthropic",
            AiBackend::OpenAiCompatible { id, .. } => id,
            AiBackend::Ollama { .. } => "ollama",
        }
    }
}

const DEFAULT_OLLAMA_HOST: &str = "http://localhost:11434";

/// Resolves which providers to try, in order, from variables (`env`: the
/// environment, then the OS keyring for API keys, see [`keys::lookup`]) and the
/// configuration preferences. Variables win over the config: `OLLAMA_MODEL` /
/// `OLLAMA_HOST` override `ai.model` / `ai.ollama_endpoint` for Ollama.
pub fn plan_ai_backends(
    env: &dyn Fn(&str) -> Option<String>,
    preferences: &AiPreferences,
) -> Vec<AiBackend> {
    let var = |name: &str| env(name).map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
    let preferred = preferences
        .provider
        .as_deref()
        .map(|p| p.trim().to_ascii_lowercase())
        .map(|p| if p == "grok" { "xai".to_string() } else { p })
        .filter(|p| !p.is_empty() && p != "auto");
    if preferred.as_deref() == Some("offline") {
        return Vec::new();
    }
    let config_model = preferences
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty());
    let model_for = |id: &str, default: &str| -> String {
        match (preferred.as_deref(), config_model) {
            (Some(p), Some(model)) if p == id => model.to_string(),
            _ => default.to_string(),
        }
    };

    let mut backends = Vec::new();
    if let Some(api_key) = var("GEMINI_API_KEY") {
        backends.push(AiBackend::Gemini {
            api_key,
            model: model_for("gemini", "gemini-2.5-flash"),
        });
    }
    if let Some(api_key) = var("ANTHROPIC_API_KEY") {
        backends.push(AiBackend::Anthropic {
            api_key,
            model: model_for("anthropic", "claude-3-7-sonnet-latest"),
        });
    }
    // OpenAI, xAI Grok, DeepSeek, or OpenRouter (OpenAI-compatible chat completions)
    let openai_compatible_providers = [
        (
            "openai",
            "OPENAI_API_KEY",
            "https://api.openai.com/v1/chat/completions",
            "gpt-4o-mini",
            "OpenAI",
        ),
        (
            "xai",
            "XAI_API_KEY",
            "https://api.x.ai/v1/chat/completions",
            "grok-2-latest",
            "xAI Grok",
        ),
        (
            "deepseek",
            "DEEPSEEK_API_KEY",
            "https://api.deepseek.com/chat/completions",
            "deepseek-chat",
            "DeepSeek",
        ),
        (
            "openrouter",
            "OPENROUTER_API_KEY",
            "https://openrouter.ai/api/v1/chat/completions",
            "openrouter/auto",
            "OpenRouter",
        ),
    ];
    for (id, env_var, endpoint, default_model, name) in openai_compatible_providers {
        if let Some(api_key) = var(env_var) {
            let model = model_for(id, default_model);
            backends.push(AiBackend::OpenAiCompatible {
                id,
                endpoint: endpoint.to_string(),
                api_key,
                label: format!("{name} ({model})"),
                model,
            });
        }
    }
    // Local Ollama: OLLAMA_MODEL, or ai.model when ai.provider = "ollama".
    let ollama_model = var("OLLAMA_MODEL").or_else(|| {
        if preferred.as_deref() == Some("ollama") {
            config_model.map(str::to_string)
        } else {
            None
        }
    });
    if let Some(model) = ollama_model {
        let host = var("OLLAMA_HOST")
            .or_else(|| {
                preferences
                    .ollama_endpoint
                    .as_deref()
                    .map(str::trim)
                    .filter(|h| !h.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| DEFAULT_OLLAMA_HOST.to_string());
        backends.push(AiBackend::Ollama { host, model });
    }

    if let Some(preferred) = preferred {
        // Stable sort: the preferred provider moves to the front, the rest keep their order.
        backends.sort_by_key(|b| b.id() != preferred);
    }
    backends
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

async fn call_gemini_translator(api_key: &str, model: &str, prompt: &str) -> Result<serde_json::Value> {
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}"
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
    Ok(resp.json().await?)
}

async fn call_anthropic_translator(api_key: &str, model: &str, prompt: &str) -> Result<serde_json::Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()?;
    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": model,
            "max_tokens": 300,
            "messages": [{ "role": "user", "content": prompt }]
        }))
        .send()
        .await?;
    Ok(resp.json().await?)
}

async fn call_openai_compatible_translator(
    endpoint: &str,
    api_key: &str,
    model: &str,
    provider_label: &str,
    prompt: &str,
) -> Result<serde_json::Value> {
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
        .await
        .map_err(|e| anyhow::anyhow!("{provider_label}: {e}"))?;
    Ok(resp.json().await?)
}

async fn call_ollama_translator(host: &str, model: &str, prompt: &str) -> Result<serde_json::Value> {
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
    Ok(resp.json().await?)
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
        usage: None,
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
        usage: None,
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

/// Streams tokens of an AI error triage progressively via an async callback.
pub async fn diagnose_command_error_streaming<F>(
    command: &str,
    output: &str,
    exit_code: i32,
    shell: &ShellProfile,
    mut on_chunk: F,
) -> AiErrorTriage
where
    F: FnMut(&str) + Send,
{
    let triage = diagnose_command_error(command, output, exit_code, shell);
    let words: Vec<&str> = triage.explanation.split_inclusive(' ').collect();
    for word in words {
        on_chunk(word);
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    }
    triage
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

    fn env_of(vars: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    fn prefs(provider: &str, model: Option<&str>, endpoint: Option<&str>) -> AiPreferences {
        AiPreferences {
            provider: Some(provider.to_string()),
            model: model.map(str::to_string),
            ollama_endpoint: endpoint.map(str::to_string),
        }
    }

    #[test]
    fn auto_keeps_default_provider_order() {
        let env = env_of(&[
            ("OPENAI_API_KEY", "sk-1"),
            ("GEMINI_API_KEY", "g-1"),
            ("ANTHROPIC_API_KEY", " "),
            ("OLLAMA_MODEL", "llama3.2"),
        ]);
        let ids: Vec<_> = plan_ai_backends(&env, &AiPreferences::default())
            .iter()
            .map(|b| b.id())
            .collect();
        assert_eq!(ids, vec!["gemini", "openai", "ollama"], "blank keys are ignored");
    }

    #[test]
    fn preferred_provider_goes_first_with_config_model() {
        let env = env_of(&[("GEMINI_API_KEY", "g"), ("XAI_API_KEY", "x")]);
        let plan = plan_ai_backends(&env, &prefs("grok", Some("grok-4"), None));
        assert_eq!(plan[0].id(), "xai");
        match &plan[0] {
            AiBackend::OpenAiCompatible { model, label, .. } => {
                assert_eq!(model, "grok-4");
                assert!(label.contains("grok-4"));
            }
            other => panic!("unexpected backend {other:?}"),
        }
        match &plan[1] {
            AiBackend::Gemini { model, .. } => assert_eq!(model, "gemini-2.5-flash"),
            other => panic!("unexpected backend {other:?}"),
        }
    }

    #[test]
    fn ollama_env_wins_over_config() {
        let config = prefs("ollama", Some("qwen2.5-coder"), Some("http://box:11434"));
        let from_config = plan_ai_backends(&env_of(&[]), &config);
        assert_eq!(
            from_config,
            vec![AiBackend::Ollama {
                host: "http://box:11434".into(),
                model: "qwen2.5-coder".into()
            }]
        );
        let env = env_of(&[("OLLAMA_MODEL", "llama3.2"), ("OLLAMA_HOST", "http://env:1")]);
        assert_eq!(
            plan_ai_backends(&env, &config),
            vec![AiBackend::Ollama {
                host: "http://env:1".into(),
                model: "llama3.2".into()
            }]
        );
    }

    #[test]
    fn offline_skips_every_provider() {
        let env = env_of(&[("OPENAI_API_KEY", "sk-1")]);
        assert!(plan_ai_backends(&env, &prefs("offline", None, None)).is_empty());
    }
}
