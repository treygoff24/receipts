use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::error::{Provider, ReceiptsError};
use crate::providers::{
    HttpFailure, SharedSpend, USER_AGENT, http_agent, join_url, new_spend, run_with_retries,
};

/// A chat-completions message.
///
/// Serializes to the three shapes Cerebras (OpenAI-compat) accepts:
/// - user/system/assistant-text: `{"role","content"}`
/// - assistant tool_calls (NO `content` key, which Cerebras rejects alongside tool_calls)
/// - tool results: `{"role":"tool","tool_call_id","content"}`
///
/// Fields that don't apply to a given role are omitted from serialization so the
/// wire payload stays well-formed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<MessageToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: Some(text.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Some(text.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// Assistant message carrying tool calls and no `content` (Cerebras rejects
    /// `content` alongside `tool_calls`).
    pub fn assistant_tool_calls(tool_calls: &[ToolCall]) -> Self {
        Self {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(tool_calls.iter().map(MessageToolCall::from).collect()),
            tool_call_id: None,
        }
    }

    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: ToolKind,
    function: MessageToolCallFunction,
}

impl From<&ToolCall> for MessageToolCall {
    fn from(call: &ToolCall) -> Self {
        Self {
            id: call.id.clone(),
            kind: ToolKind::Function,
            function: MessageToolCallFunction {
                name: call.function_name.clone(),
                arguments: call.arguments.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ToolKind {
    Function,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct MessageToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ChatOpts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseFormat {
    Subquestions,
    Claims,
    Relevance,
    Verdict,
}

impl Serialize for ResponseFormat {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Subquestions => json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "subquestions",
                    "strict": true,
                    "schema": {
                        "type": "object",
                        "properties": {
                            "subquestions": {
                                "type": "array",
                                "items": {"type": "string"}
                            }
                        },
                        "required": ["subquestions"],
                        "additionalProperties": false
                    }
                }
            }),
            Self::Claims => json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "claims",
                    "strict": true,
                    "schema": {
                        "type": "object",
                        "properties": {
                            "claims": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "claim": {
                                            "type": "string",
                                            "description": "one atomic factual claim"
                                        },
                                        "url": {
                                            "type": "string",
                                            "description": "full http(s) source URL exactly as written in the answer; if the claim has no http URL, use empty string"
                                        }
                                    },
                                    "required": ["claim", "url"],
                                    "additionalProperties": false
                                }
                            }
                        },
                        "required": ["claims"],
                        "additionalProperties": false
                    }
                }
            }),
            Self::Relevance => json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "relevance",
                    "strict": true,
                    "schema": {
                        "type": "object",
                        "properties": {
                            "relevance": {
                                "type": "string",
                                "enum": ["direct", "partially", "no"]
                            }
                        },
                        "required": ["relevance"],
                        "additionalProperties": false
                    }
                }
            }),
            Self::Verdict => json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "verdict",
                    "strict": true,
                    "schema": {
                        "type": "object",
                        "properties": {
                            "verdict": {
                                "type": "string",
                                "enum": ["supported", "partial", "unsupported"]
                            },
                            "note": {"type": "string"},
                            "quote": {
                                "type": ["string", "null"],
                                "description": "exact supporting quote copied verbatim from SOURCE TEXT; null unless verdict is supported or partial"
                            }
                        },
                        "required": ["verdict", "note", "quote"],
                        "additionalProperties": false
                    }
                }
            }),
        }
        .serialize(serializer)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolDefinition {
    #[serde(rename = "type")]
    kind: &'static str,
    function: FunctionDefinition,
}

impl ToolDefinition {
    pub fn search() -> Self {
        Self {
            kind: "function",
            function: FunctionDefinition {
                name: "search",
                description: "Web search. Returns top results with text excerpts.",
                parameters: FunctionParameters {
                    kind: "object",
                    properties: SearchProperties {
                        query: StringParameter { kind: "string" },
                    },
                    required: ["query"],
                },
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct FunctionDefinition {
    name: &'static str,
    description: &'static str,
    parameters: FunctionParameters,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct FunctionParameters {
    #[serde(rename = "type")]
    kind: &'static str,
    properties: SearchProperties,
    required: [&'static str; 1],
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct SearchProperties {
    query: StringParameter,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct StringParameter {
    #[serde(rename = "type")]
    kind: &'static str,
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
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    stream: bool,
    #[serde(flatten)]
    opts: ChatOpts,
}

impl CerebrasClient {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        Self {
            api_key,
            base_url,
            model,
            agent: http_agent(),
            spend: new_spend(),
        }
    }

    pub fn with_spend(mut self, spend: SharedSpend) -> Self {
        self.spend = spend;
        self
    }

    pub fn spend(&self) -> SharedSpend {
        Arc::clone(&self.spend)
    }

    pub fn chat(
        &self,
        messages: &[Message],
        opts: ChatOpts,
    ) -> Result<ChatResponse, ReceiptsError> {
        let url = join_url(&self.base_url, "/chat/completions");
        let body = self.request_body(messages, opts);
        let start = Instant::now();
        let (raw, retries) = run_with_retries(
            Provider::Cerebras,
            || self.post_json(&url, &body),
            &std::thread::sleep,
        )?;
        let mut response = parse_chat_response(&raw)?;
        response.wall_time_ms = start.elapsed().as_millis() as u64;
        self.record_spend(&response.usage, retries)?;
        Ok(response)
    }

    fn request_body<'a>(&'a self, messages: &'a [Message], opts: ChatOpts) -> ChatRequest<'a> {
        ChatRequest {
            model: &self.model,
            messages,
            stream: false,
            opts,
        }
    }

    fn post_json(&self, url: &str, body: &impl Serialize) -> Result<String, HttpFailure> {
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

    fn record_spend(&self, usage: &TokenUsage, retries: u32) -> Result<(), ReceiptsError> {
        let dollars = model_cost_dollars(&self.model, usage);
        let mut spend = self.spend.lock().map_err(|_| {
            ReceiptsError::upstream("spend meter lock poisoned").with_provider(Provider::Cerebras)
        })?;

        spend.prompt_tokens += usage.prompt_tokens;
        spend.completion_tokens += usage.completion_tokens;
        spend.dollars += dollars;
        spend.call_count += 1;
        spend.retries += retries as u64;
        Ok(())
    }
}

/// Per-model price table: `(input $/1M tokens, output $/1M tokens)`.
///
/// Unknown models fall back to the gemma pricing so the meter errs on the high
/// side rather than silently zeroing cost (which would corrupt `--max-dollars`).
fn model_price_dollars_per_mtok(model: &str) -> (f64, f64) {
    match model {
        "gpt-oss-120b" => (0.35, 0.75),
        "gemma-4-31b" | "zai-glm-4.7" => (2.15, 2.70),
        _ => (2.15, 2.70),
    }
}

fn model_cost_dollars(model: &str, usage: &TokenUsage) -> f64 {
    let (in_per_mtok, out_per_mtok) = model_price_dollars_per_mtok(model);
    (usage.prompt_tokens as f64 / 1_000_000.0 * in_per_mtok)
        + (usage.completion_tokens as f64 / 1_000_000.0 * out_per_mtok)
}

/// Apply to model-emitted JSON text (structured output content), never to the
/// HTTP envelope. Strips bad control chars and escapes bare `\u` sequences so
/// model-mangled JSON becomes parseable. The envelope is valid JSON from the
/// server; running this on it would corrupt legitimate escaped content
/// (e.g. `"C:\\users\\x"` arriving server-escaped).
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

fn parse_chat_response(raw: &str) -> Result<ChatResponse, ReceiptsError> {
    let response: RawChatResponse = serde_json::from_str(raw).map_err(|err| {
        ReceiptsError::upstream(format!("failed to parse Cerebras response JSON: {err}"))
            .with_provider(Provider::Cerebras)
            .with_retryable(false)
    })?;

    let message = &response
        .choices
        .first()
        .ok_or_else(|| {
            ReceiptsError::upstream("Cerebras response contained no choices")
                .with_provider(Provider::Cerebras)
                .with_retryable(false)
        })?
        .message;

    Ok(ChatResponse {
        content: message.content.clone().unwrap_or_default(),
        tool_calls: tool_calls(message),
        usage: TokenUsage {
            prompt_tokens: response.usage.prompt_tokens,
            completion_tokens: response.usage.completion_tokens,
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
            id: call.id.clone(),
            function_name: call.function.name.clone(),
            arguments: call.function.arguments.clone(),
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct RawChatResponse {
    choices: Vec<RawChoice>,
    usage: RawUsage,
}

#[derive(Debug, Deserialize)]
struct RawChoice {
    message: RawMessage,
}

#[derive(Debug, Deserialize)]
struct RawMessage {
    content: Option<String>,
    tool_calls: Option<Vec<RawToolCall>>,
}

#[derive(Debug, Deserialize)]
struct RawToolCall {
    id: String,
    function: RawFunction,
}

#[derive(Debug, Deserialize)]
struct RawFunction {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct RawUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Spend;
    use serde_json::Value;
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
    fn parses_content_tool_calls_and_usage_verbatim() {
        let raw = r#"{
            "choices": [{
                "message": {
                    "content": "Próspera nope",
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {"name": "search", "arguments": "{\"q\":\"x\"}"}
                    }]
                }
            }],
            "usage": {"prompt_tokens": 1000, "completion_tokens": 2000}
        }"#;

        let parsed = parse_chat_response(raw).unwrap();

        assert_eq!(parsed.content, "Próspera nope");
        assert_eq!(parsed.tool_calls[0].id, "call_1");
        assert_eq!(parsed.tool_calls[0].function_name, "search");
        assert_eq!(parsed.usage.prompt_tokens, 1000);
        assert_eq!(parsed.usage.completion_tokens, 2000);
    }

    #[test]
    fn rejects_incomplete_chat_responses() {
        assert!(
            parse_chat_response(
                r#"{"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#
            )
            .is_err()
        );
        assert!(
            parse_chat_response(r#"{"choices":[{"message":{"content":"ok"}}],"usage":{}}"#)
                .is_err()
        );
    }

    #[test]
    fn json_repair_on_model_content_yields_parseable_json() {
        // Escape a literal "\\u" without changing the valid \u00E9 sequence.
        let repaired = json_repair("{\"claim\":\"Café\u{0001} \\u broken \\u00E9\"}");
        let parsed: Value = serde_json::from_str(&repaired).unwrap();

        assert_eq!(parsed["claim"], "Café \\u broken é");
    }

    #[test]
    fn message_serializes_each_role_shape_correctly() {
        let user = serde_json::to_value(Message::user("hi")).unwrap();
        let user_obj = user.as_object().unwrap();
        assert_eq!(user_obj.len(), 2);
        assert_eq!(user["role"], "user");
        assert_eq!(user["content"], "hi");
        assert!(user.get("tool_calls").is_none());
        assert!(user.get("tool_call_id").is_none());

        let assistant = serde_json::to_value(Message::assistant_tool_calls(&[ToolCall {
            id: "call_1".into(),
            function_name: "search".into(),
            arguments: "{}".into(),
        }]))
        .unwrap();
        let assistant_obj = assistant.as_object().unwrap();
        assert_eq!(assistant_obj.len(), 2);
        assert_eq!(assistant["role"], "assistant");
        assert!(assistant.get("content").is_none(), "content must be absent");
        assert!(assistant["tool_calls"].is_array());

        let tool = serde_json::to_value(Message::tool("call_1", "result body")).unwrap();
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["tool_call_id"], "call_1");
        assert_eq!(tool["content"], "result body");
        assert!(tool.get("tool_calls").is_none());
    }

    #[test]
    fn search_tool_serializes_exact_wire_schema() {
        assert_eq!(
            serde_json::to_value(ToolDefinition::search()).unwrap(),
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": "search",
                    "description": "Web search. Returns top results with text excerpts.",
                    "parameters": {
                        "type": "object",
                        "properties": {"query": {"type": "string"}},
                        "required": ["query"]
                    }
                }
            })
        );
    }

    #[test]
    fn chat_request_flattens_options_into_the_wire_payload() {
        let client = CerebrasClient::new("key".into(), "http://localhost".into(), "model".into());
        let messages = [Message::user("hi")];
        let body = client.request_body(
            &messages,
            ChatOpts {
                temperature: Some(0.2),
                max_completion_tokens: Some(100),
                ..ChatOpts::default()
            },
        );

        assert_eq!(
            serde_json::to_value(body).unwrap(),
            serde_json::json!({
                "model": "model",
                "messages": [{"role": "user", "content": "hi"}],
                "stream": false,
                "temperature": 0.2,
                "max_completion_tokens": 100
            })
        );
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
            .record_spend(
                &TokenUsage {
                    prompt_tokens: 1_000_000,
                    completion_tokens: 1_000_000,
                },
                2,
            )
            .unwrap();

        let spend = spend.lock().unwrap();
        assert_eq!(spend.prompt_tokens, 1_000_000);
        assert_eq!(spend.completion_tokens, 1_000_000);
        assert!((spend.dollars - 4.85).abs() < f64::EPSILON);
        assert_eq!(spend.call_count, 1);
        assert_eq!(spend.retries, 2);
    }

    #[test]
    fn non_gemma_model_uses_correct_pricing() {
        let spend = Arc::new(Mutex::new(Spend::default()));
        let client = CerebrasClient::new(
            "key".into(),
            "http://localhost".into(),
            "gpt-oss-120b".into(),
        )
        .with_spend(Arc::clone(&spend));

        client
            .record_spend(
                &TokenUsage {
                    prompt_tokens: 1_000_000,
                    completion_tokens: 1_000_000,
                },
                0,
            )
            .unwrap();

        let spend = spend.lock().unwrap();
        assert!((spend.dollars - 1.10).abs() < f64::EPSILON);
    }
}
