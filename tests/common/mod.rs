use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub struct MockServer {
    base_url: String,
    requests: Arc<AtomicUsize>,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MockServer {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let running = Arc::new(AtomicBool::new(true));
        let thread_requests = Arc::clone(&requests);
        let thread_running = Arc::clone(&running);
        let handle = thread::spawn(move || {
            while thread_running.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        thread_requests.fetch_add(1, Ordering::SeqCst);
                        handle_client(stream);
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(err) => panic!("mock server accept failed: {err}"),
                }
            }
        });

        Self {
            base_url: format!("http://{addr}"),
            requests,
            running,
            handle: Some(handle),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn request_count(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            handle.join().expect("mock server thread");
        }
    }
}

fn handle_client(mut stream: TcpStream) {
    let (path, body, headers) = read_request(&mut stream).expect("mock request");
    let response = match path.as_str() {
        "/chat/completions" => chat_response(&body),
        "/search" => search_response(&headers),
        "/contents" => contents_response(),
        _ => (
            404,
            MockResponse::Error(ErrorResponse { error: "not found" }),
        ),
    };
    write_json(&mut stream, response.0, &response.1);
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<(String, String, String)> {
    let mut buf = Vec::new();
    let mut tmp = [0; 1024];
    loop {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }

    let header_end = buf
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|idx| idx + 4)
        .expect("mock request header terminator");
    let headers =
        String::from_utf8(buf[..header_end].to_vec()).expect("mock request headers UTF-8");
    let path = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("mock request path")
        .to_string();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then_some(value.trim())
        })
        .expect("mock request content-length")
        .parse::<usize>()
        .expect("mock request content-length integer");

    while buf.len() < header_end + content_length {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = String::from_utf8(buf[header_end..header_end + content_length].to_vec())
        .expect("mock request body UTF-8");
    Ok((path, body, headers))
}

#[derive(Deserialize)]
struct ChatRequest {
    response_format: Option<ResponseFormat>,
    messages: Vec<RequestMessage>,
    tools: Option<Vec<RequestTool>>,
}

#[derive(Deserialize)]
struct ResponseFormat {
    json_schema: JsonSchema,
}

#[derive(Deserialize)]
struct JsonSchema {
    name: String,
}

#[derive(Deserialize)]
struct RequestMessage {
    role: MessageRole,
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Deserialize)]
struct RequestTool {
    #[serde(rename = "type")]
    _kind: ToolKind,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum ToolKind {
    Function,
}

#[derive(Serialize)]
#[serde(untagged)]
enum MockResponse {
    Chat(ChatResponse),
    Search(SearchResponse),
    Error(ErrorResponse),
}

#[derive(Serialize)]
struct ChatResponse {
    choices: [Choice; 1],
    usage: Usage,
}

#[derive(Serialize)]
struct Choice {
    message: AssistantMessage,
}

#[derive(Serialize)]
#[serde(untagged)]
enum AssistantMessage {
    Content {
        content: &'static str,
    },
    ToolCalls {
        content: &'static str,
        tool_calls: [ResponseToolCall; 1],
    },
}

#[derive(Serialize)]
struct ResponseToolCall {
    id: &'static str,
    function: ResponseFunction,
}

#[derive(Serialize)]
struct ResponseFunction {
    name: &'static str,
    arguments: &'static str,
}

#[derive(Serialize)]
struct Usage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchResponse {
    results: Vec<SearchResult>,
    cost_dollars: CostDollars,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchResult {
    url: &'static str,
    title: &'static str,
    published_date: &'static str,
    text: &'static str,
}

#[derive(Serialize)]
struct CostDollars {
    total: f64,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

fn chat_response(body: &str) -> (u16, MockResponse) {
    let body: ChatRequest = serde_json::from_str(body).expect("mock chat request JSON");
    let schema_name = body
        .response_format
        .as_ref()
        .map(|format| format.json_schema.name.as_str());
    let has_tool_result = body
        .messages
        .iter()
        .any(|message| message.role == MessageRole::Tool);
    let has_tools = body.tools.is_some();

    let message = match (schema_name, has_tool_result, has_tools) {
        (Some("claims"), _, _) => AssistantMessage::Content {
            content: "{\"claims\":[{\"claim\":\"Mock fact is supported\",\"url\":\"https://example.com/source\"}]}",
        },
        (Some("verdict"), _, _) => AssistantMessage::Content {
            content: "{\"verdict\":\"supported\",\"note\":\"mock source text supports the claim\",\"quote\":\"Mock fact is supported in this source text.\"}",
        },
        (Some("relevance"), _, _) => AssistantMessage::Content {
            content: "{\"relevance\":\"direct\"}",
        },
        (_, true, _) => AssistantMessage::Content {
            content: "Mock fact is supported by https://example.com/source.",
        },
        (_, false, true) => AssistantMessage::ToolCalls {
            content: "",
            tool_calls: [ResponseToolCall {
                id: "call_search",
                function: ResponseFunction {
                    name: "search",
                    arguments: "{\"query\":\"mock receipts source\"}",
                },
            }],
        },
        _ => AssistantMessage::Content { content: "ok" },
    };

    (
        200,
        MockResponse::Chat(ChatResponse {
            choices: [Choice { message }],
            usage: Usage {
                prompt_tokens: 1000,
                completion_tokens: 1000,
            },
        }),
    )
}

fn search_response(headers: &str) -> (u16, MockResponse) {
    let bad_key = headers
        .lines()
        .any(|line| line.eq_ignore_ascii_case("x-api-key: bad-exa"));
    if bad_key {
        return (
            401,
            MockResponse::Error(ErrorResponse {
                error: "invalid api key",
            }),
        );
    }
    (
        200,
        MockResponse::Search(SearchResponse {
            results: vec![search_result()],
            cost_dollars: CostDollars { total: 0.01 },
        }),
    )
}

fn contents_response() -> (u16, MockResponse) {
    (
        200,
        MockResponse::Search(SearchResponse {
            results: vec![search_result()],
            cost_dollars: CostDollars { total: 0.005 },
        }),
    )
}

fn search_result() -> SearchResult {
    SearchResult {
        url: "https://example.com/source",
        title: "Mock source",
        published_date: "2026-07-01",
        text: "Mock fact is supported in this source text.",
    }
}

fn write_json(stream: &mut TcpStream, status: u16, value: &impl Serialize) {
    let reason = if status == 200 { "OK" } else { "Not Found" };
    let body = serde_json::to_string(value).unwrap();
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("mock response write");
}
