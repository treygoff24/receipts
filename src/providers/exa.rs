use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{Provider, ReceiptsError};
use crate::providers::{
    HttpFailure, SharedSpend, USER_AGENT, http_agent, join_url, new_spend, run_with_retries,
};

pub const DEFAULT_BASE_URL: &str = "https://api.exa.ai";

pub trait SearchProvider: Send + Sync {
    fn search(&self, query: &str) -> Result<Vec<SourceDoc>, ReceiptsError>;
    fn contents(&self, url: &str) -> Result<Option<String>, ReceiptsError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDoc {
    pub url: String,
    pub title: Option<String>,
    pub published: Option<String>,
    pub text: String,
}

pub struct ExaClient {
    api_key: String,
    base_url: String,
    search_type: String,
    agent: ureq::Agent,
    spend: SharedSpend,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchRequest<'a> {
    query: &'a str,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    search_type: Option<&'a str>,
    num_results: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    contents: Option<SearchContents>,
}

#[derive(Serialize)]
struct SearchContents {
    text: bool,
}

#[derive(Serialize)]
struct ContentsRequest<'a> {
    urls: [&'a str; 1],
    text: bool,
}

impl ExaClient {
    pub fn new(api_key: String, base_url: String) -> Self {
        Self {
            api_key,
            base_url: if base_url.is_empty() {
                DEFAULT_BASE_URL.to_string()
            } else {
                base_url
            },
            search_type: crate::config::DEFAULT_EXA_SEARCH_TYPE.to_string(),
            agent: http_agent(),
            spend: new_spend(),
        }
    }

    pub fn with_search_type(mut self, search_type: String) -> Self {
        self.search_type = search_type;
        self
    }

    pub fn with_spend(mut self, spend: SharedSpend) -> Self {
        self.spend = spend;
        self
    }

    pub fn spend(&self) -> SharedSpend {
        Arc::clone(&self.spend)
    }

    /// Minimal probe for `doctor --online`: POST /search with numResults 1 and
    /// no `contents.text`, so the call is unbilled/cheapest. Keeps spend
    /// metering on whatever Exa reports.
    pub fn probe(&self) -> Result<(), ReceiptsError> {
        let body = SearchRequest {
            query: "receipts doctor probe",
            search_type: None,
            num_results: 1,
            contents: None,
        };
        self.request("/search", &body, "probe")?;
        Ok(())
    }

    fn request(
        &self,
        path: &str,
        body: &impl Serialize,
        operation: &str,
    ) -> Result<RawExaResponse, ReceiptsError> {
        let (raw, retries) = run_with_retries(
            Provider::Exa,
            || self.post_json(path, body),
            &std::thread::sleep,
        )?;
        let response = parse_response(&raw, operation)?;
        self.record_search_spend(response.cost_dollars.total(), retries)?;
        Ok(response)
    }

    fn post_json(&self, path: &str, body: &impl Serialize) -> Result<String, HttpFailure> {
        let url = join_url(&self.base_url, path);
        let mut response = self
            .agent
            .post(&url)
            .header("x-api-key", self.api_key.clone())
            .header("User-Agent", USER_AGENT)
            .send_json(body)
            .map_err(HttpFailure::from)?;

        response
            .body_mut()
            .read_to_string()
            .map_err(|err| HttpFailure::Transport(err.to_string()))
    }

    fn record_search_spend(&self, dollars: f64, retries: u32) -> Result<(), ReceiptsError> {
        let mut spend = self.spend.lock().map_err(|_| {
            ReceiptsError::upstream("spend meter lock poisoned").with_provider(Provider::Exa)
        })?;

        spend.search_dollars += dollars;
        spend.call_count += 1;
        spend.retries += retries as u64;
        Ok(())
    }
}

impl SearchProvider for ExaClient {
    fn search(&self, query: &str) -> Result<Vec<SourceDoc>, ReceiptsError> {
        let body = SearchRequest {
            query,
            search_type: Some(&self.search_type),
            num_results: 4,
            contents: Some(SearchContents { text: true }),
        };
        let response = self.request("/search", &body, "search")?;
        Ok(response.results.into_iter().map(SourceDoc::from).collect())
    }

    fn contents(&self, url: &str) -> Result<Option<String>, ReceiptsError> {
        let body = ContentsRequest {
            urls: [url],
            text: true,
        };
        let response = self.request("/contents", &body, "contents")?;
        Ok(response.results.into_iter().find_map(|result| result.text))
    }
}

#[derive(Debug, Deserialize)]
struct RawExaResponse {
    results: Vec<RawExaResult>,
    #[serde(rename = "costDollars")]
    cost_dollars: RawCostDollars,
}

fn parse_response(raw: &str, operation: &str) -> Result<RawExaResponse, ReceiptsError> {
    serde_json::from_str(raw).map_err(|err| {
        ReceiptsError::upstream(format!("failed to parse Exa {operation} JSON: {err}"))
            .with_provider(Provider::Exa)
            .with_retryable(false)
    })
}

#[derive(Debug, Deserialize)]
struct RawCostDollars {
    total: f64,
}

impl RawCostDollars {
    fn total(&self) -> f64 {
        self.total
    }
}

#[derive(Debug, Deserialize)]
struct RawExaResult {
    url: String,
    title: Option<String>,
    #[serde(alias = "publishedDate")]
    published: Option<String>,
    text: Option<String>,
}

impl From<RawExaResult> for SourceDoc {
    fn from(result: RawExaResult) -> Self {
        Self {
            url: result.url,
            title: result.title,
            published: result.published,
            text: result.text.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Spend;
    use std::sync::Mutex;

    #[test]
    fn requests_serialize_exact_wire_shapes() {
        let search = SearchRequest {
            query: "q",
            search_type: Some("fast"),
            num_results: 4,
            contents: Some(SearchContents { text: true }),
        };
        assert_eq!(
            serde_json::to_value(search).unwrap(),
            serde_json::json!({
                "query": "q",
                "type": "fast",
                "numResults": 4,
                "contents": {"text": true}
            })
        );

        let contents = ContentsRequest {
            urls: ["https://example.com"],
            text: true,
        };
        assert_eq!(
            serde_json::to_value(contents).unwrap(),
            serde_json::json!({"urls": ["https://example.com"], "text": true})
        );
    }

    #[test]
    fn parses_search_results_and_cost() {
        let raw = r#"{
            "results": [{
                "url": "https://example.com",
                "title": "Example",
                "publishedDate": "2026-07-01",
                "text": "Body"
            }],
            "costDollars": {"total": 0.02}
        }"#;

        let parsed: RawExaResponse = serde_json::from_str(raw).unwrap();
        let docs: Vec<SourceDoc> = parsed.results.into_iter().map(SourceDoc::from).collect();

        assert_eq!(
            docs,
            vec![SourceDoc {
                url: "https://example.com".into(),
                title: Some("Example".into()),
                published: Some("2026-07-01".into()),
                text: "Body".into(),
            }]
        );
    }

    #[test]
    fn rejects_responses_without_metered_cost() {
        assert!(parse_response(r#"{"results":[]}"#, "search").is_err());
    }

    #[test]
    fn records_search_spend_separately() {
        let spend = Arc::new(Mutex::new(Spend::default()));
        let client =
            ExaClient::new("key".into(), "http://localhost".into()).with_spend(Arc::clone(&spend));

        client.record_search_spend(0.03, 0).unwrap();

        let spend = spend.lock().unwrap();
        assert_eq!(spend.dollars, 0.0);
        assert_eq!(spend.search_dollars, 0.03);
        assert_eq!(spend.call_count, 1);
    }

    #[test]
    fn empty_exa_base_url_uses_default() {
        let client = ExaClient::new("key".into(), String::new());

        assert_eq!(client.base_url, DEFAULT_BASE_URL);
    }
}
