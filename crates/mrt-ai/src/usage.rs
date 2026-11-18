//! Token use and estimated cost of AI requests.
//!
//! Every provider reports the tokens a request used; MRT reads them from the response
//! and prices them with the `[ai.prices]` table from the config (US dollars per
//! million tokens, by model). Local Ollama models cost nothing. A model without a
//! price still counts its tokens; its cost is reported as unknown.

use crate::AiBackend;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Price of a model in US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct AiPrice {
    pub input: f64,
    pub output: f64,
}

static AI_PRICES: std::sync::RwLock<BTreeMap<String, AiPrice>> = std::sync::RwLock::new(BTreeMap::new());

/// Replaces the process-wide price table (`[ai.prices]`, keyed by model name or
/// `provider/model`), called when the config loads.
pub fn set_ai_prices(prices: BTreeMap<String, AiPrice>) {
    match AI_PRICES.write() {
        Ok(mut guard) => *guard = prices,
        Err(poisoned) => *poisoned.into_inner() = prices,
    }
}

/// The price for `model` of `provider`: `provider/model` first, then `model`.
pub fn price_for(provider: &str, model: &str) -> Option<AiPrice> {
    let prices = match AI_PRICES.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    prices.get(&format!("{provider}/{model}")).or_else(|| prices.get(model)).copied()
}

/// Tokens and estimated cost of one or more AI requests.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AiUsage {
    /// Requests counted (each provider call that got a reply).
    pub requests: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Estimated cost of the requests whose model has a price (or is local).
    pub cost_usd: f64,
    /// Requests whose model has no price in `[ai.prices]`.
    pub unpriced_requests: u32,
    /// The last model used, for messages such as "no price for gpt-4o-mini".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_model: Option<String>,
}

impl AiUsage {
    /// One request to `provider`/`model` with these token counts, priced now.
    pub fn request(provider: &str, model: &str, input_tokens: u64, output_tokens: u64) -> Self {
        let price = if provider == "ollama" { Some(AiPrice::default()) } else { price_for(provider, model) };
        let (cost_usd, unpriced_requests) = match price {
            Some(p) => ((input_tokens as f64 * p.input + output_tokens as f64 * p.output) / 1_000_000.0, 0),
            None => (0.0, 1),
        };
        Self {
            requests: 1,
            input_tokens,
            output_tokens,
            cost_usd,
            unpriced_requests,
            last_model: Some(model.to_string()),
        }
    }

    /// Adds `other` to this total.
    pub fn add(&mut self, other: &AiUsage) {
        self.requests += other.requests;
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cost_usd += other.cost_usd;
        self.unpriced_requests += other.unpriced_requests;
        if other.last_model.is_some() {
            self.last_model.clone_from(&other.last_model);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.requests == 0
    }

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    /// `"1,234 in · 56 out tokens · $0.0012"`, with the requests when more than one,
    /// and a note when some have no price.
    pub fn summary(&self) -> String {
        let mut text = String::new();
        if self.requests > 1 {
            text.push_str(&format!("{} requests · ", self.requests));
        }
        text.push_str(&format!(
            "{} in · {} out tokens",
            group_digits(self.input_tokens),
            group_digits(self.output_tokens)
        ));
        let priced = self.requests - self.unpriced_requests;
        if priced > 0 {
            text.push_str(&format!(" · {}", format_usd(self.cost_usd)));
        }
        if self.unpriced_requests > 0 {
            match (&self.last_model, priced) {
                (Some(model), 0) => text.push_str(&format!(" · no price for {model} in [ai.prices]")),
                _ => text.push_str(&format!(" · {} without a price", self.unpriced_requests)),
            }
        }
        text
    }
}

/// `1234567` → `"1,234,567"`.
pub fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Dollars with enough decimals for small amounts: `$0.0012`, `$1.23`.
pub fn format_usd(amount: f64) -> String {
    if amount == 0.0 {
        "$0".to_string()
    } else if amount < 0.01 {
        format!("${amount:.4}")
    } else {
        format!("${amount:.2}")
    }
}

/// The tokens a provider's reply says the request used.
pub fn usage_from_response(backend: &AiBackend, reply: &Value) -> AiUsage {
    let n = |pointer: &str| reply.pointer(pointer).and_then(Value::as_u64).unwrap_or(0);
    let (provider, model, input, output) = match backend {
        AiBackend::Gemini { model, .. } => (
            "gemini",
            model,
            n("/usageMetadata/promptTokenCount"),
            // Thinking models bill their thoughts as output.
            n("/usageMetadata/candidatesTokenCount") + n("/usageMetadata/thoughtsTokenCount"),
        ),
        AiBackend::Anthropic { model, .. } => (
            "anthropic",
            model,
            n("/usage/input_tokens") + n("/usage/cache_creation_input_tokens") + n("/usage/cache_read_input_tokens"),
            n("/usage/output_tokens"),
        ),
        AiBackend::OpenAiCompatible { id, model, .. } => (*id, model, n("/usage/prompt_tokens"), n("/usage/completion_tokens")),
        AiBackend::Ollama { model, .. } => ("ollama", model, n("/prompt_eval_count"), n("/eval_count")),
    };
    AiUsage::request(provider, model, input, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_provider_reports_its_tokens() {
        let gemini = AiBackend::Gemini { api_key: "k".into(), model: "g-test".into() };
        let u = usage_from_response(&gemini, &json!({ "usageMetadata": { "promptTokenCount": 100, "candidatesTokenCount": 20, "thoughtsTokenCount": 5 } }));
        assert_eq!((u.input_tokens, u.output_tokens), (100, 25));
        let anthropic = AiBackend::Anthropic { api_key: "k".into(), model: "a-test".into() };
        let u = usage_from_response(&anthropic, &json!({ "usage": { "input_tokens": 7, "cache_read_input_tokens": 3, "output_tokens": 9 } }));
        assert_eq!((u.input_tokens, u.output_tokens), (10, 9));
        let openai = AiBackend::OpenAiCompatible { id: "openai", endpoint: String::new(), api_key: "k".into(), model: "o-test".into(), label: String::new() };
        let u = usage_from_response(&openai, &json!({ "usage": { "prompt_tokens": 50, "completion_tokens": 4 } }));
        assert_eq!((u.input_tokens, u.output_tokens), (50, 4));
        let ollama = AiBackend::Ollama { host: String::new(), model: "llama-test".into() };
        let u = usage_from_response(&ollama, &json!({ "prompt_eval_count": 30, "eval_count": 6 }));
        assert_eq!((u.input_tokens, u.output_tokens, u.unpriced_requests, u.cost_usd), (30, 6, 0, 0.0), "local models are free");
        // A reply without usage still counts as a request.
        let u = usage_from_response(&openai, &json!({}));
        assert_eq!((u.requests, u.total_tokens()), (1, 0));
    }

    #[test]
    fn costs_come_from_the_price_table() {
        let mut prices = BTreeMap::new();
        prices.insert("price-test-model".to_string(), AiPrice { input: 3.0, output: 15.0 });
        prices.insert("openrouter/price-test-model".to_string(), AiPrice { input: 1.0, output: 1.0 });
        set_ai_prices(prices);
        let u = AiUsage::request("anthropic", "price-test-model", 1_000, 200);
        assert!((u.cost_usd - 0.006).abs() < 1e-12, "{}", u.cost_usd);
        assert_eq!(u.summary(), "1,000 in · 200 out tokens · $0.0060");
        // provider/model wins over the bare model name.
        let routed = AiUsage::request("openrouter", "price-test-model", 1_000_000, 0);
        assert!((routed.cost_usd - 1.0).abs() < 1e-9);

        let unknown = AiUsage::request("openai", "no-price-model", 10, 2);
        assert_eq!(unknown.summary(), "10 in · 2 out tokens · no price for no-price-model in [ai.prices]");
        let mut total = u.clone();
        total.add(&unknown);
        total.add(&routed);
        assert_eq!(total.requests, 3);
        assert_eq!(total.summary(), "3 requests · 1,001,010 in · 202 out tokens · $1.01 · 1 without a price");
        set_ai_prices(BTreeMap::new());
    }

    /// A fake Ollama on localhost that answers every request with `body`.
    async fn fake_ollama(body: Value) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let body = body.to_string();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    let mut read = 0;
                    // Read the headers and the body (Content-Length) before answering.
                    loop {
                        let Ok(n) = socket.read(&mut buf[read..]).await else { return };
                        if n == 0 {
                            return;
                        }
                        read += n;
                        let text = String::from_utf8_lossy(&buf[..read]);
                        if let Some(end) = text.find("\r\n\r\n") {
                            let length = text[..end]
                                .lines()
                                .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0)))
                                .unwrap_or(0);
                            if read >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    let reply = format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
                    let _ = socket.write_all(reply.as_bytes()).await;
                });
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn translations_and_investigations_carry_their_usage() {
        let host = fake_ollama(json!({
            "response": "{\"command\": \"ls -la\", \"explanation\": \"Lists files.\", \"answer\": \"Nothing failed.\"}",
            "prompt_eval_count": 120,
            "eval_count": 15
        }))
        .await;
        let backend = AiBackend::Ollama { host: host.clone(), model: "fake-model".into() };
        let reply = call_fake(&backend).await;
        let u = usage_from_response(&backend, &reply);
        assert_eq!((u.input_tokens, u.output_tokens, u.cost_usd), (120, 15, 0.0));
        assert_eq!(crate::reply_text(&backend, &reply), Some(reply["response"].as_str().unwrap()));

        // `#` translation with Ollama as the configured provider.
        crate::set_ai_preferences(crate::AiPreferences {
            provider: Some("ollama".into()),
            model: Some("fake-model".into()),
            ollama_endpoint: Some(host),
        });
        let shell = mrt_core::discover_shells().into_iter().next().unwrap();
        if std::env::var("OLLAMA_HOST").is_err() && std::env::var("OLLAMA_MODEL").is_err() {
            let proposal = crate::translate_nl_to_command("# list files", &shell, ".", &[]).await.unwrap();
            assert_eq!(proposal.proposed_command, "ls -la");
            let usage = proposal.usage.clone().expect("usage");
            assert_eq!((usage.requests, usage.input_tokens, usage.output_tokens), (1, 120, 15));
            assert_eq!(serde_json::to_value(&proposal).unwrap()["usage"]["input_tokens"], 120, "MCP sees it too");
        }
        crate::set_ai_preferences(crate::AiPreferences::default());

        // `:assist` adds up every model call.
        let outcome = crate::assist::run_assist_with_backends_for_test(&[backend]).await;
        assert_eq!(outcome.answer, "Nothing failed.");
        assert_eq!((outcome.usage.requests, outcome.usage.input_tokens), (1, 120));
    }

    async fn call_fake(backend: &AiBackend) -> Value {
        let AiBackend::Ollama { host, .. } = backend else { unreachable!() };
        reqwest::Client::new()
            .post(format!("{host}/api/generate"))
            .json(&serde_json::json!({ "model": "fake-model", "prompt": "x", "stream": false }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    #[test]
    fn numbers_are_readable() {
        assert_eq!(group_digits(0), "0");
        assert_eq!(group_digits(999), "999");
        assert_eq!(group_digits(1_234_567), "1,234,567");
        assert_eq!(format_usd(0.0), "$0");
        assert_eq!(format_usd(0.00042), "$0.0004");
        assert_eq!(format_usd(12.345), "$12.35");
    }
}
