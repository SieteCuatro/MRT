//! Multi-step assistance: the assistant calls MRT's read-only MCP tools, looks at
//! each result and decides the next call, until it can answer.
//!
//! With an AI provider configured, the model picks the calls: each turn it gets the
//! question, the allowed tools and the results so far, and answers with one JSON
//! object, either `{"tool": .., "arguments": {..}}` or `{"answer": .., "command": ..}`.
//! Without one (or when a reply cannot be used), a built-in plan investigates the
//! latest failed command: find it, look for an earlier successful run of the same
//! command, compare the two, diagnose, and answer.
//!
//! The assistant never runs anything itself: tools outside [`ASSIST_TOOLS`] are
//! refused, and a proposed command is returned to the caller, which shows it to the
//! user and routes it through the Safety Approval Gate.

use crate::{ai_preferences, plan_ai_backends, AiBackend};
use serde_json::{json, Value};
use std::future::Future;

/// Read-only MRT tools the assistant may call.
pub const ASSIST_TOOLS: &[(&str, &str)] = &[
    ("mrt_list_tabs", "List tabs with ids, titles, shells, folders and activity. Arguments: {}"),
    ("mrt_list_blocks", "Recent command blocks of a tab, newest first. Arguments: {\"tab_id\": \"\" (active tab), \"limit\": 10}"),
    ("mrt_get_block", "One block in full (command, output, exit code). Arguments: {\"block_id\": \"<uuid>\"}"),
    ("mrt_search_blocks", "Find blocks whose command or output contains text. Arguments: {\"query\": \"..\", \"failed_only\": false, \"limit\": 20}"),
    ("mrt_compare_blocks", "Structured diff of two blocks' outputs. Arguments: {\"previous_block_id\": \"..\", \"current_block_id\": \"..\", \"show_in_gui\": false}"),
    ("mrt_diagnose_block", "Diagnose a failed block (cause and a suggested fix). Arguments: {\"block_id\": \"..\"}"),
    ("mrt_inspect_ports", "Listening TCP/UDP ports and their processes. Arguments: {}"),
    ("mrt_list_processes", "Running processes (name, pid, CPU, memory). Arguments: {\"query\": \"..\", \"limit\": 20}"),
];

/// Most tool calls in one investigation.
pub const MAX_ASSIST_STEPS: usize = 6;

/// One tool call and what it returned.
#[derive(Debug, Clone, PartialEq)]
pub struct AssistStep {
    pub tool: String,
    pub arguments: Value,
    pub result: String,
}

/// What the assistant wants to do next.
#[derive(Debug, Clone, PartialEq)]
pub enum AssistDecision {
    Call { tool: String, arguments: Value },
    Answer { answer: String, command: Option<String> },
}

/// The outcome of [`run_assist`].
#[derive(Debug, Clone, PartialEq)]
pub struct AssistOutcome {
    pub answer: String,
    /// A command the assistant suggests; the caller must route it through the Safety Gate.
    pub command: Option<String>,
    pub steps: Vec<AssistStep>,
    /// Provider label, or "MRT built-in investigator".
    pub provider: String,
    /// Tokens and estimated cost of every model call in the investigation.
    pub usage: crate::AiUsage,
}

pub const OFFLINE_PROVIDER: &str = "MRT built-in investigator";

pub fn is_allowed_tool(name: &str) -> bool {
    ASSIST_TOOLS.iter().any(|(tool, _)| *tool == name)
}

/// Reads a model reply: one JSON object, possibly wrapped in a ``` fence.
pub fn parse_decision(text: &str) -> Option<AssistDecision> {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .map(|rest| rest.trim_end().trim_end_matches("```"))
        .unwrap_or(trimmed);
    let start = body.find('{')?;
    let end = body.rfind('}')?;
    let value: Value = serde_json::from_str(&body[start..=end]).ok()?;
    if let Some(tool) = value.get("tool").and_then(Value::as_str) {
        let arguments = value.get("arguments").cloned().unwrap_or_else(|| json!({}));
        return Some(AssistDecision::Call { tool: tool.to_string(), arguments });
    }
    let answer = value.get("answer").and_then(Value::as_str)?.trim().to_string();
    let command = value
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string);
    Some(AssistDecision::Answer { answer, command })
}

fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The prompt for the next turn.
pub fn build_assist_prompt(question: &str, shell: &str, cwd: &str, steps: &[AssistStep]) -> String {
    let tools: String = ASSIST_TOOLS.iter().map(|(name, desc)| format!("- {name}: {desc}\n")).collect();
    let mut history = String::new();
    for (i, step) in steps.iter().enumerate() {
        history.push_str(&format!(
            "Step {}: {} {}\nResult:\n{}\n\n",
            i + 1,
            step.tool,
            step.arguments,
            clip(&step.result, 6000)
        ));
    }
    format!(
        "You are the assistant inside Modern Rust Terminal (MRT), investigating for the user.\n\
         OS: {}. Shell of the active tab: {shell}. Folder: {cwd}.\n\
         Question: {question}\n\n\
         You can call these read-only tools, one per turn:\n{tools}\n\
         {}\
         Reply with exactly one JSON object and nothing else: either\n\
         {{\"tool\": \"<name>\", \"arguments\": {{...}}}} to call a tool, or\n\
         {{\"answer\": \"<explanation for the user>\", \"command\": \"<optional shell command to fix it>\"}} when you know enough.\n\
         Use results from earlier steps (block ids, commands) to choose the next call. At most {} calls in total. \
         Never invent block ids. A command you suggest is shown to the user, who approves it.",
        std::env::consts::OS,
        if history.is_empty() { String::new() } else { format!("Results so far:\n{history}") },
        MAX_ASSIST_STEPS
    )
}

fn parse_json(text: &str) -> Option<Value> {
    serde_json::from_str(text.trim()).ok()
}

/// The built-in plan: decides the next call from the results so far.
pub fn offline_next(steps: &[AssistStep]) -> AssistDecision {
    let result = |tool: &str| steps.iter().rev().find(|s| s.tool == tool).map(|s| s.result.as_str());
    let Some(listed) = result("mrt_list_blocks") else {
        return AssistDecision::Call { tool: "mrt_list_blocks".into(), arguments: json!({ "tab_id": "", "limit": 20 }) };
    };
    let blocks = parse_json(listed).and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let failed = blocks.iter().find(|b| b["exit_code"].as_i64().is_some_and(|c| c != 0));
    let Some(failed) = failed else {
        return AssistDecision::Answer {
            answer: "No failed command in this tab's recent blocks, so there is nothing to investigate. Ask a question, or configure an AI provider for open-ended help.".into(),
            command: None,
        };
    };
    let failed_id = failed["id"].as_str().unwrap_or_default().to_string();
    let command = failed["command"].as_str().unwrap_or_default().to_string();

    let Some(searched) = result("mrt_search_blocks") else {
        return AssistDecision::Call {
            tool: "mrt_search_blocks".into(),
            arguments: json!({ "query": command, "limit": 50 }),
        };
    };
    let matches = parse_json(searched).and_then(|v| v.as_array().cloned()).unwrap_or_default();
    let previous_ok = matches.iter().find(|b| {
        b["command"].as_str() == Some(command.as_str())
            && b["exit_code"].as_i64() == Some(0)
            && b["id"].as_str() != Some(failed_id.as_str())
    });
    let previous_id = previous_ok.and_then(|b| b["id"].as_str()).map(str::to_string);

    if let Some(previous_id) = &previous_id {
        if result("mrt_compare_blocks").is_none() {
            return AssistDecision::Call {
                tool: "mrt_compare_blocks".into(),
                arguments: json!({ "previous_block_id": previous_id, "current_block_id": failed_id, "show_in_gui": false }),
            };
        }
    }
    let Some(diagnosis) = result("mrt_diagnose_block") else {
        return AssistDecision::Call { tool: "mrt_diagnose_block".into(), arguments: json!({ "block_id": failed_id }) };
    };

    let diagnosis_json = parse_json(diagnosis);
    let explanation = diagnosis_json
        .as_ref()
        .and_then(|d| d.get("explanation").or_else(|| d.get("diagnosis")).and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| diagnosis.trim().to_string());
    let fix = diagnosis_json
        .as_ref()
        .and_then(|d| d.get("suggested_fix").or_else(|| d.get("fix")).and_then(Value::as_str))
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(str::to_string);
    let mut answer = format!("`{command}` failed (exit code {}).", failed["exit_code"]);
    match (&previous_id, result("mrt_compare_blocks")) {
        (Some(_), Some(diff)) => {
            answer.push_str(" It succeeded on an earlier run; what changed in the output:\n");
            answer.push_str(clip(diff.trim(), 1500));
            answer.push('\n');
        }
        _ => answer.push_str(" No earlier successful run of the same command was found.\n"),
    }
    answer.push_str("\nDiagnosis: ");
    answer.push_str(&explanation);
    AssistDecision::Answer { answer, command: fix }
}

/// Raw completion from one provider, asking for JSON: the reply text, the
/// provider label and the tokens it used.
async fn complete(backend: &AiBackend, prompt: &str) -> anyhow::Result<(String, String, crate::AiUsage)> {
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build()?;
    let (val, label): (Value, String) = match backend {
        AiBackend::Gemini { api_key, model } => {
            let url = format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={api_key}");
            let val = client
                .post(&url)
                .json(&json!({ "contents": [{ "parts": [{ "text": prompt }] }], "generationConfig": { "responseMimeType": "application/json" } }))
                .send()
                .await?
                .json()
                .await?;
            (val, format!("Google Gemini ({model})"))
        }
        AiBackend::Anthropic { api_key, model } => {
            let val = client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&json!({ "model": model, "max_tokens": 1024, "messages": [{ "role": "user", "content": prompt }] }))
                .send()
                .await?
                .json()
                .await?;
            (val, format!("Anthropic Claude ({model})"))
        }
        AiBackend::OpenAiCompatible { endpoint, api_key, model, label, .. } => {
            let val = client
                .post(endpoint)
                .bearer_auth(api_key)
                .json(&json!({ "model": model, "messages": [{ "role": "user", "content": prompt }], "response_format": { "type": "json_object" } }))
                .send()
                .await?
                .json()
                .await?;
            (val, label.clone())
        }
        AiBackend::Ollama { host, model } => {
            let val = client
                .post(format!("{}/api/generate", host.trim_end_matches('/')))
                .json(&json!({ "model": model, "prompt": prompt, "format": "json", "stream": false }))
                .send()
                .await?
                .json()
                .await?;
            (val, format!("Ollama ({model})"))
        }
    };
    let text = crate::reply_text(backend, &val).unwrap_or_default().to_string();
    Ok((text, label, crate::usage_from_response(backend, &val)))
}

/// Asks the first provider that gives a usable reply; `None` when none does. Every
/// reply's tokens are added to `usage`.
async fn ask_providers(backends: &[AiBackend], prompt: &str, usage: &mut crate::AiUsage) -> Option<(AssistDecision, String)> {
    for backend in backends {
        if let Ok((text, label, used)) = complete(backend, prompt).await {
            usage.add(&used);
            if let Some(decision) = parse_decision(&text) {
                return Some((decision, label));
            }
        }
    }
    None
}

/// Runs an investigation. `call_tool(name, arguments)` executes an allowed MRT tool
/// and returns its text; `on_step` is told about each call as it happens.
pub async fn run_assist<F, Fut>(
    question: &str,
    shell: &str,
    cwd: &str,
    mut call_tool: F,
    mut on_step: impl FnMut(&AssistStep),
) -> AssistOutcome
where
    F: FnMut(String, Value) -> Fut,
    Fut: Future<Output = String>,
{
    let backends = plan_ai_backends(&crate::keys::lookup, &ai_preferences());
    run_assist_with(question, shell, cwd, &backends, &mut call_tool, &mut on_step).await
}

async fn run_assist_with<F, Fut>(
    question: &str,
    shell: &str,
    cwd: &str,
    backends: &[AiBackend],
    call_tool: &mut F,
    on_step: &mut impl FnMut(&AssistStep),
) -> AssistOutcome
where
    F: FnMut(String, Value) -> Fut,
    Fut: Future<Output = String>,
{
    let mut steps: Vec<AssistStep> = Vec::new();
    // Once a provider fails or replies with something unusable, the built-in plan
    // finishes the investigation.
    let mut use_model = !backends.is_empty();
    let mut provider = OFFLINE_PROVIDER.to_string();
    let mut usage = crate::AiUsage::default();
    loop {
        let decision = if use_model {
            match ask_providers(backends, &build_assist_prompt(question, shell, cwd, &steps), &mut usage).await {
                Some((decision, label)) => {
                    provider = label;
                    decision
                }
                None => {
                    use_model = false;
                    provider = OFFLINE_PROVIDER.to_string();
                    offline_next(&steps)
                }
            }
        } else {
            offline_next(&steps)
        };
        match decision {
            AssistDecision::Answer { answer, command } => {
                return AssistOutcome { answer, command, steps, provider, usage };
            }
            AssistDecision::Call { tool, arguments } => {
                if steps.len() >= MAX_ASSIST_STEPS {
                    let answer = if use_model {
                        "The assistant used all its tool calls without reaching an answer; see the steps above.".to_string()
                    } else {
                        "The investigation stopped after the maximum number of steps.".to_string()
                    };
                    return AssistOutcome { answer, command: None, steps, provider, usage };
                }
                let result = if is_allowed_tool(&tool) {
                    call_tool(tool.clone(), arguments.clone()).await
                } else {
                    format!("Refused: `{tool}` is not one of the read-only tools the assistant may call.")
                };
                let step = AssistStep { tool, arguments, result };
                on_step(&step);
                steps.push(step);
            }
        }
    }
}

/// An investigation with these providers and a tool that returns nothing, for tests
/// in other modules.
#[cfg(test)]
pub(crate) async fn run_assist_with_backends_for_test(backends: &[AiBackend]) -> AssistOutcome {
    let mut call_tool = |_tool: String, _arguments: Value| async { String::new() };
    run_assist_with("why?", "bash", ".", backends, &mut call_tool, &mut |_| {}).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions_parse_from_plain_or_fenced_json() {
        assert_eq!(
            parse_decision("```json\n{\"tool\":\"mrt_get_block\",\"arguments\":{\"block_id\":\"x\"}}\n```"),
            Some(AssistDecision::Call { tool: "mrt_get_block".into(), arguments: json!({ "block_id": "x" }) })
        );
        assert_eq!(
            parse_decision("Sure! {\"answer\": \"It is the port.\", \"command\": \"\"}"),
            Some(AssistDecision::Answer { answer: "It is the port.".into(), command: None })
        );
        assert_eq!(parse_decision("no json here"), None);
        assert!(!is_allowed_tool("mrt_run_command"));
        assert!(is_allowed_tool("mrt_compare_blocks"));
    }

    /// A fake MRT with an earlier passing and a later failing `cargo test`.
    fn fake_tool(tool: &str, arguments: &Value) -> String {
        match tool {
            "mrt_list_blocks" => json!([
                { "id": "b3", "command": "ls", "exit_code": 0 },
                { "id": "b2", "command": "cargo test", "exit_code": 101, "output": "test x ... FAILED" },
                { "id": "b1", "command": "cargo test", "exit_code": 0, "output": "test x ... ok" }
            ])
            .to_string(),
            "mrt_search_blocks" => {
                assert_eq!(arguments["query"], "cargo test");
                json!([
                    { "id": "b2", "command": "cargo test", "exit_code": 101 },
                    { "id": "b1", "command": "cargo test", "exit_code": 0 }
                ])
                .to_string()
            }
            "mrt_compare_blocks" => {
                assert_eq!(arguments["previous_block_id"], "b1");
                assert_eq!(arguments["current_block_id"], "b2");
                "- test x ... ok\n+ test x ... FAILED".to_string()
            }
            "mrt_diagnose_block" => json!({ "explanation": "A test assertion failed.", "suggested_fix": "cargo test -- --nocapture" }).to_string(),
            other => panic!("unexpected tool {other}"),
        }
    }

    #[tokio::test]
    async fn the_built_in_plan_finds_the_previous_run_and_compares() {
        let mut seen = Vec::new();
        let outcome = run_assist_with(
            "why did it fail?",
            "bash",
            "/src",
            &[],
            &mut |tool: String, arguments: Value| {
                let result = fake_tool(&tool, &arguments);
                async move { result }
            },
            &mut |step: &AssistStep| seen.push(step.tool.clone()),
        )
        .await;
        assert_eq!(seen, vec!["mrt_list_blocks", "mrt_search_blocks", "mrt_compare_blocks", "mrt_diagnose_block"]);
        assert_eq!(outcome.provider, OFFLINE_PROVIDER);
        assert!(outcome.answer.contains("`cargo test` failed (exit code 101)"), "{}", outcome.answer);
        assert!(outcome.answer.contains("+ test x ... FAILED"));
        assert!(outcome.answer.contains("A test assertion failed."));
        assert_eq!(outcome.command.as_deref(), Some("cargo test -- --nocapture"));
    }

    #[test]
    fn without_a_previous_success_the_plan_skips_the_comparison() {
        let steps = vec![
            AssistStep { tool: "mrt_list_blocks".into(), arguments: json!({}), result: json!([{ "id": "b9", "command": "make", "exit_code": 2 }]).to_string() },
            AssistStep { tool: "mrt_search_blocks".into(), arguments: json!({}), result: json!([{ "id": "b9", "command": "make", "exit_code": 2 }]).to_string() },
        ];
        assert_eq!(
            offline_next(&steps),
            AssistDecision::Call { tool: "mrt_diagnose_block".into(), arguments: json!({ "block_id": "b9" }) }
        );
        let none_failed = vec![AssistStep { tool: "mrt_list_blocks".into(), arguments: json!({}), result: "[]".into() }];
        assert!(matches!(offline_next(&none_failed), AssistDecision::Answer { command: None, .. }));
    }

    #[test]
    fn prompts_carry_the_question_tools_and_results() {
        let steps = vec![AssistStep { tool: "mrt_list_tabs".into(), arguments: json!({}), result: "[{\"title\":\"main\"}]".into() }];
        let prompt = build_assist_prompt("what is on port 3000?", "pwsh", "C:\\src", &steps);
        assert!(prompt.contains("what is on port 3000?"));
        assert!(prompt.contains("mrt_inspect_ports"));
        assert!(prompt.contains("Step 1: mrt_list_tabs"));
        assert!(!prompt.contains("mrt_run_command"));
    }
}
