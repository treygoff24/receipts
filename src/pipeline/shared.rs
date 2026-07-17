use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::budget::Budget;
pub use crate::claim::{Relevance, ResearchClaim, Verdict};
use crate::error::{Provider, ReceiptsError};
use crate::providers::cerebras::{CerebrasClient, ChatOpts, ChatResponse, Message, json_repair};
use crate::providers::exa::SearchProvider;
use crate::providers::{SharedSpend, new_spend};

pub trait ChatProvider: Send + Sync {
    fn chat(&self, messages: &[Message], opts: ChatOpts) -> Result<ChatResponse, ReceiptsError>;
}

impl ChatProvider for CerebrasClient {
    fn chat(&self, messages: &[Message], opts: ChatOpts) -> Result<ChatResponse, ReceiptsError> {
        CerebrasClient::chat(self, messages, opts)
    }
}

pub(crate) type SourceCache = Arc<Mutex<HashMap<String, String>>>;
type SourceMetaCache = Arc<Mutex<HashMap<String, SourceMeta>>>;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SourceMeta {
    pub published: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RunParams {
    pub today: String,
    pub max_concurrency: usize,
    pub spend: SharedSpend,
}

impl RunParams {
    pub fn new(today: impl Into<String>, max_concurrency: usize, spend: SharedSpend) -> Self {
        Self {
            today: today.into(),
            max_concurrency: max_concurrency.max(1),
            spend,
        }
    }
}

impl Default for RunParams {
    fn default() -> Self {
        Self::new("1970-01-01", 1, new_spend())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchData {
    pub question: String,
    pub outcome: Outcome,
    pub claims: Vec<ResearchClaim>,
    pub search_trail: Vec<SearchTrailEntry>,
    pub uncertainties: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Answered,
    Partial,
    Unanswered,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchTrailEntry {
    pub query: String,
    pub results: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaimCandidate {
    pub subquestion: String,
    pub claim: String,
    pub url: String,
    /// Set by the relevance gate (`Direct` for "yes", `Related` for
    /// "partially"). Defaults to `Direct` at extraction time and whenever the
    /// gate itself doesn't run (budget refusal, `--verify off`, unparseable
    /// model response) — the gate fails open, so an unclassified claim is
    /// treated as fully on-topic rather than silently downgraded.
    pub relevance: Relevance,
}

/// Internal verification result that retains the subquestion attribution from
/// the originating `ClaimCandidate`. This is what `verify_candidates` returns
/// so the deep-tier refinement logic can derive `dead_subquestions` from the
/// carried attribution instead of a positional zip.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VerifiedClaim {
    pub subquestion: String,
    pub claim: ResearchClaim,
}

#[derive(Clone)]
pub(crate) struct SharedState {
    pub spend: SharedSpend,
    pub budget_gate: Arc<Mutex<()>>,
    // Lock source_cache before source_meta to avoid deadlock.
    pub source_cache: SourceCache,
    pub source_meta: SourceMetaCache,
    pub search_trail: Arc<Mutex<Vec<SearchTrailEntry>>>,
}

#[derive(Clone)]
pub(crate) struct StageContext<'a> {
    pub chat: &'a dyn ChatProvider,
    pub search: &'a dyn SearchProvider,
    pub budget: &'a Budget,
    pub today: &'a str,
    pub max_concurrency: usize,
    pub state: SharedState,
}

impl<'a> StageContext<'a> {
    pub(super) fn new(
        chat: &'a dyn ChatProvider,
        search: &'a dyn SearchProvider,
        budget: &'a Budget,
        params: &'a RunParams,
    ) -> Self {
        Self {
            chat,
            search,
            budget,
            today: &params.today,
            max_concurrency: params.max_concurrency.max(1),
            state: SharedState {
                spend: Arc::clone(&params.spend),
                budget_gate: Arc::new(Mutex::new(())),
                source_cache: Arc::new(Mutex::new(HashMap::new())),
                source_meta: Arc::new(Mutex::new(HashMap::new())),
                search_trail: Arc::new(Mutex::new(Vec::new())),
            },
        }
    }

    pub(crate) fn may_launch(&self, projected_unit_cost: f64) -> bool {
        let _gate = self
            .state
            .budget_gate
            .lock()
            .expect("budget gate lock poisoned");
        let spend = self.state.spend.lock().expect("spend meter lock poisoned");
        self.budget.may_launch(&spend, projected_unit_cost)
    }
}

pub(crate) fn run_chunked<T, R, F>(items: Vec<T>, max_concurrency: usize, f: F) -> Vec<R>
where
    T: Send,
    R: Send,
    F: Fn(T) -> R + Sync,
{
    let mut out = Vec::new();
    let mut iter = items.into_iter();
    loop {
        let batch: Vec<_> = iter.by_ref().take(max_concurrency).collect();
        if batch.is_empty() {
            break;
        }
        std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .into_iter()
                .map(|item| scope.spawn(|| f(item)))
                .collect();
            out.extend(
                handles
                    .into_iter()
                    .map(|handle| handle.join().expect("worker thread panicked")),
            );
        });
    }
    out
}

pub(crate) fn parse_model_json<T>(text: &str) -> Result<T, ReceiptsError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(&json_repair(text)).map_err(|err| {
        ReceiptsError::upstream(format!("failed to parse model JSON: {err}"))
            .with_provider(Provider::Cerebras)
            .with_retryable(false)
    })
}

pub(crate) fn chat_json<T>(
    chat: &dyn ChatProvider,
    messages: &[Message],
    opts: ChatOpts,
) -> Result<T, ReceiptsError>
where
    T: for<'de> Deserialize<'de>,
{
    let first = chat.chat(messages, opts.clone())?;
    match parse_model_json(&first.content) {
        Ok(value) => Ok(value),
        Err(_) => {
            let second = chat.chat(messages, opts)?;
            parse_model_json(&second.content)
        }
    }
}

pub(crate) fn append_note(note: &mut String, addition: &str) {
    if note.trim().is_empty() {
        *note = addition.to_string();
    } else {
        note.push_str(" | ");
        note.push_str(addition);
    }
}

#[cfg(test)]
mod tests {
    use super::run_chunked;

    #[test]
    #[should_panic(expected = "worker thread panicked")]
    fn run_chunked_propagates_worker_panics() {
        let _: Vec<()> = run_chunked(vec![()], 1, |_| panic!("boom"));
    }
}
