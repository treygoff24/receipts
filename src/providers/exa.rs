use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{Provider, ReceiptsError};
use crate::providers::{
    HttpFailure, SharedSpend, SleepFn, USER_AGENT, default_sleep, http_agent, join_url, new_spend,
    run_with_retries,
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
    sleep_fn: SleepFn,
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
            sleep_fn: default_sleep(),
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

    /// Minimal probe for `doctor --online`: POST /search with numResults 1 and
    /// no `contents.text`, so the call is unbilled/cheapest. Keeps spend
    /// metering on whatever Exa reports.
    pub fn probe(&self) -> Result<(), ReceiptsError> {
        let body = json!({
            "query": "receipts doctor probe",
            "numResults": 1,
        });
        let (raw, retries) = run_with_retries(
            Provider::Exa,
            || self.post_json("/search", &body),
            self.sleep_fn.as_ref(),
        )?;
        let response: RawExaResponse = serde_json::from_str(&raw).map_err(|err| {
            ReceiptsError::upstream(format!("failed to parse Exa probe JSON: {err}"))
                .with_provider(Provider::Exa)
                .with_retryable(false)
        })?;
        self.record_search_spend(response.cost_dollars.total(), retries)?;
        Ok(())
    }

    fn post_json(&self, path: &str, body: &Value) -> Result<String, HttpFailure> {
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
        let body = json!({
            "query": query,
            "type": self.search_type,
            "numResults": 4,
            "contents": {"text": true},
        });
        let (raw, retries) = run_with_retries(
            Provider::Exa,
            || self.post_json("/search", &body),
            self.sleep_fn.as_ref(),
        )?;
        let response: RawExaResponse = serde_json::from_str(&raw).map_err(|err| {
            ReceiptsError::upstream(format!("failed to parse Exa search JSON: {err}"))
                .with_provider(Provider::Exa)
                .with_retryable(false)
        })?;

        self.record_search_spend(response.cost_dollars.total(), retries)?;
        Ok(response.results.into_iter().map(SourceDoc::from).collect())
    }

    fn contents(&self, url: &str) -> Result<Option<String>, ReceiptsError> {
        let body = json!({
            "urls": [url],
            "text": true,
        });
        let (raw, retries) = run_with_retries(
            Provider::Exa,
            || self.post_json("/contents", &body),
            self.sleep_fn.as_ref(),
        )?;
        let response: RawExaResponse = serde_json::from_str(&raw).map_err(|err| {
            ReceiptsError::upstream(format!("failed to parse Exa contents JSON: {err}"))
                .with_provider(Provider::Exa)
                .with_retryable(false)
        })?;

        self.record_search_spend(response.cost_dollars.total(), retries)?;
        Ok(response.results.into_iter().find_map(|result| result.text))
    }
}

#[derive(Debug, Default, Deserialize)]
struct RawExaResponse {
    #[serde(default)]
    results: Vec<RawExaResult>,
    #[serde(default, rename = "costDollars")]
    cost_dollars: RawCostDollars,
}

#[derive(Debug, Default, Deserialize)]
struct RawCostDollars {
    total: Option<f64>,
}

impl RawCostDollars {
    fn total(&self) -> f64 {
        self.total.unwrap_or_default()
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
