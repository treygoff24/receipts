use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Value, json};

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
                    Err(_) => break,
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
        let _ = TcpStream::connect(self.base_url.trim_start_matches("http://"));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn handle_client(mut stream: TcpStream) {
    let Ok((path, body)) = read_request(&mut stream) else {
        return;
    };
    let response = match path.as_str() {
        "/chat/completions" => chat_response(&body),
        "/search" => search_response(),
        "/contents" => contents_response(),
        _ => (404, json!({"error":"not found"})),
    };
    write_json(&mut stream, response.0, &response.1);
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<(String, String)> {
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
        .unwrap_or(buf.len());
    let headers = String::from_utf8_lossy(&buf[..header_end]);
    let path = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);

    while buf.len() < header_end + content_length {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = String::from_utf8_lossy(&buf[header_end..header_end + content_length]).to_string();
    Ok((path, body))
}

fn chat_response(body: &str) -> (u16, Value) {
    let body: Value = serde_json::from_str(body).unwrap_or_else(|_| json!({}));
    let schema_name = body
        .pointer("/response_format/json_schema/name")
        .and_then(Value::as_str);
    let has_tool_result = body
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| {
            messages
                .iter()
                .any(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
        });
    let has_tools = body.get("tools").is_some();

    let message = match (schema_name, has_tool_result, has_tools) {
        (Some("claims"), _, _) => {
            json!({"content":"{\"claims\":[{\"claim\":\"Mock fact is supported\",\"url\":\"https://example.com/source\"}]}"})
        }
        (Some("verdict"), _, _) => {
            json!({"content":"{\"verdict\":\"supported\",\"note\":\"mock source text supports the claim\"}"})
        }
        (_, true, _) => json!({"content":"Mock fact is supported by https://example.com/source."}),
        (_, false, true) => json!({
            "content":"",
            "tool_calls":[{
                "id":"call_search",
                "function":{"name":"search","arguments":"{\"query\":\"mock recon source\"}"}
            }]
        }),
        _ => json!({"content":"ok"}),
    };

    (
        200,
        json!({
            "choices": [{"message": message}],
            "usage": {"prompt_tokens": 1000, "completion_tokens": 1000}
        }),
    )
}

fn search_response() -> (u16, Value) {
    (
        200,
        json!({
            "results": [{
                "url": "https://example.com/source",
                "title": "Mock source",
                "publishedDate": "2026-07-01",
                "text": "Mock fact is supported in this source text."
            }],
            "costDollars": {"total": 0.01}
        }),
    )
}

fn contents_response() -> (u16, Value) {
    (
        200,
        json!({
            "results": [{
                "url": "https://example.com/source",
                "title": "Mock source",
                "publishedDate": "2026-07-01",
                "text": "Mock fact is supported in this source text."
            }],
            "costDollars": {"total": 0.005}
        }),
    )
}

fn write_json(stream: &mut TcpStream, status: u16, value: &Value) {
    let reason = if status == 200 { "OK" } else { "Not Found" };
    let body = serde_json::to_string(value).unwrap();
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}
