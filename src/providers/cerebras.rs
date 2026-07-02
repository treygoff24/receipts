use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Provider, ReconError};
use crate::providers::{
    default_sleep, join_url, new_spend, run_with_retries, HttpFailure, SharedSpend, SleepFn,
    USER_AGENT,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatOpts {
    pub temperature: Option<f64>,
    pub max_completion_tokens: Option<u64>,
    pub tools: Option<Value>,
    pub response_format: Option<Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub function_name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: TokenUsage,
    pub wall_time_ms: u64,
}

pub struct CerebrasClient {
    api_key: String,
    base_url: String,
    model: String,
    agent: ureq::Agent,
    spend: SharedSpend,
    sleep_fn: SleepFn,
}

impl CerebrasClient {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        Self {
            api_key,
            base_url,
            model,
            agent: ureq::Agent::new_with_defaults(),
            spend: new_spend(),
            sleep_fn: default_sleep(),
        }
    }

    pub fn with_spend(mut self, spend: SharedSpend) -> Self {
        self.spend = spend;
        self
    }

    pub fn with_sleep_fn(
        mut self,
        sleep_fn: impl Fn(std::time::Duration) + Send + Sync + 'static,
    ) -> Self {
        self.sleep_fn = Arc::new(sleep_fn);
        self
    }

    pub fn spend(&self) -> SharedSpend {
        Arc::clone(&self.spend)
    }

    pub fn chat(&self, messages: &[Message], opts: ChatOpts) -> Result<ChatResponse, ReconError> {
        let url = join_url(&self.base_url, "/chat/completions");
        let body = self.request_body(messages, opts);
        let start = Instant::now();
        let (raw, _) = run_with_retries(
            Provider::Cerebras,
            || self.post_json(&url, &body),
            self.sleep_fn.as_ref(),
        )?;
        let mut response = parse_chat_response(&raw)?;
        response.wall_time_ms = start.elapsed().as_millis() as u64;
        self.record_spend(&response.usage)?;
        Ok(response)
    }

    fn request_body(&self, messages: &[Message], opts: ChatOpts) -> Value {
        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "stream": false,
        });

        if let Some(temperature) = opts.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(max_completion_tokens) = opts.max_completion_tokens {
            body["max_completion_tokens"] = json!(max_completion_tokens);
        }
        if let Some(tools) = opts.tools {
            body["tools"] = tools;
        }
        if let Some(response_format) = opts.response_format {
            body["response_format"] = response_format;
        }

        body
    }

    fn post_json(&self, url: &str, body: &Value) -> Result<String, HttpFailure> {
        let mut response = self
            .agent
            .post(url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("User-Agent", USER_AGENT)
            .send_json(body)
            .map_err(HttpFailure::from)?;

        response
            .body_mut()
            .read_to_string()
            .map_err(|err| HttpFailure::Transport(err.to_string()))
    }

    fn record_spend(&self, usage: &TokenUsage) -> Result<(), ReconError> {
        let dollars = model_cost_dollars(&self.model, usage);
        let mut spend = self.spend.lock().map_err(|_| {
            ReconError::upstream("spend meter lock poisoned").with_provider(Provider::Cerebras)
        })?;

        spend.prompt_tokens += usage.prompt_tokens;
        spend.completion_tokens += usage.completion_tokens;
        spend.dollars += dollars;
        spend.call_count += 1;
        Ok(())
    }
}

fn model_cost_dollars(_model: &str, usage: &TokenUsage) -> f64 {
    (usage.prompt_tokens as f64 / 1_000_000.0 * 2.15)
        + (usage.completion_tokens as f64 / 1_000_000.0 * 2.70)
}

pub fn json_repair(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(ch) = chars.next() {
        if is_stripped_control(ch) {
            continue;
        }

        if ch == '\\' && chars.peek() == Some(&'u') {
            chars.next();
            let valid_escape = chars
                .clone()
                .take(4)
                .collect::<Vec<_>>()
                .as_slice()
                .try_into()
                .is_ok_and(|next: [char; 4]| next.iter().all(char::is_ascii_hexdigit));

            out.push('\\');
            if !valid_escape {
                out.push('\\');
            }
            out.push('u');
        } else {
            out.push(ch);
        }
    }

    out
}

fn is_stripped_control(ch: char) -> bool {
    matches!(ch as u32, 0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F)
}

fn parse_chat_response(raw: &str) -> Result<ChatResponse, ReconError> {
    let repaired = json_repair(raw);
    let response: RawChatResponse = serde_json::from_str(&repaired).map_err(|err| {
        ReconError::upstream(format!("failed to parse Cerebras response JSON: {err}"))
            .with_provider(Provider::Cerebras)
            .with_retryable(false)
    })?;

    let message = response
        .choices
        .first()
        .and_then(|choice| choice.message.as_ref());

    Ok(ChatResponse {
        content: message.and_then(|m| m.content.clone()).unwrap_or_default(),
        tool_calls: message.map(tool_calls).unwrap_or_default(),
        usage: TokenUsage {
            prompt_tokens: response.usage.prompt_tokens.unwrap_or_default(),
            completion_tokens: response.usage.completion_tokens.unwrap_or_default(),
        },
        wall_time_ms: 0,
    })
}

fn tool_calls(message: &RawMessage) -> Vec<ToolCall> {
    message
        .tool_calls
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|call| ToolCall {
            id: call.id.clone().unwrap_or_default(),
            function_name: call.function.name.clone().unwrap_or_default(),
            arguments: call.function.arguments.clone().unwrap_or_default(),
        })
        .collect()
}

#[derive(Debug, Default, Deserialize)]
struct RawChatResponse {
    #[serde(default)]
    choices: Vec<RawChoice>,
    #[serde(default)]
    usage: RawUsage,
}

#[derive(Debug, Deserialize)]
struct RawChoice {
    message: Option<RawMessage>,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    content: Option<String>,
    tool_calls: Option<Vec<RawToolCall>>,
}

#[derive(Debug, Deserialize)]
struct RawToolCall {
    id: Option<String>,
    function: RawFunction,
}

#[derive(Debug, Deserialize)]
struct RawFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Spend;
    use std::sync::Mutex;

    #[test]
    fn json_repair_strips_bad_control_chars_but_keeps_accented_text() {
        let repaired = json_repair("{\"name\":\"Prós\u{0001}pera\"}");
        let parsed: Value = serde_json::from_str(&repaired).unwrap();

        assert_eq!(parsed["name"], "Próspera");
    }

    #[test]
    fn json_repair_escapes_bad_unicode_escape_but_keeps_valid_accent_escape() {
        let repaired = json_repair(r#"{"name":"Próspera \u broken \u00F3"}"#);
        let parsed: Value = serde_json::from_str(&repaired).unwrap();

        assert_eq!(parsed["name"], "Próspera \\u broken ó");
    }

    #[test]
    fn parses_content_tool_calls_usage_and_repairs_body() {
        let raw = r#"{
            "choices": [{
                "message": {
                    "content": "Próspera\u nope",
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {"name": "search", "arguments": "{\"q\":\"x\"}"}
                    }]
                }
            }],
            "usage": {"prompt_tokens": 1000, "completion_tokens": 2000}
        }"#;

        let parsed = parse_chat_response(raw).unwrap();

        assert_eq!(parsed.content, "Próspera\\u nope");
        assert_eq!(parsed.tool_calls[0].id, "call_1");
        assert_eq!(parsed.tool_calls[0].function_name, "search");
        assert_eq!(parsed.usage.prompt_tokens, 1000);
        assert_eq!(parsed.usage.completion_tokens, 2000);
    }

    #[test]
    fn records_model_spend_into_shared_meter() {
        let spend = Arc::new(Mutex::new(Spend::default()));
        let client = CerebrasClient::new(
            "key".into(),
            "http://localhost".into(),
            "gemma-4-31b".into(),
        )
        .with_spend(Arc::clone(&spend));

        client
            .record_spend(&TokenUsage {
                prompt_tokens: 1_000_000,
                completion_tokens: 1_000_000,
            })
            .unwrap();

        let spend = spend.lock().unwrap();
        assert_eq!(spend.prompt_tokens, 1_000_000);
        assert_eq!(spend.completion_tokens, 1_000_000);
        assert!((spend.dollars - 4.85).abs() < f64::EPSILON);
        assert_eq!(spend.call_count, 1);
    }
}
