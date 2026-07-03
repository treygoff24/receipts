pub mod brief;
pub mod decompose;
pub mod extract;
pub mod verify;
pub mod worker;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::budget::Budget;
use crate::error::{Provider, ReceiptsError};
use crate::providers::cerebras::{CerebrasClient, ChatOpts, ChatResponse, Message, json_repair};
use crate::providers::exa::SearchProvider;
use crate::providers::{SharedSpend, new_spend};
use crate::tiers::{
    DECOMPOSE_WORST_CASE_COST, Depth, EXTRACT_WORST_CASE_COST, WORKER_ROUND_WORST_CASE_COST,
    dead_subquestions, initial_worker_tasks, refinement_tasks,
};

pub use verify::VerifyPolicy;

pub trait ChatProvider: Send + Sync {
    fn chat(&self, messages: &[Message], opts: ChatOpts) -> Result<ChatResponse, ReceiptsError>;
}

impl ChatProvider for CerebrasClient {
    fn chat(&self, messages: &[Message], opts: ChatOpts) -> Result<ChatResponse, ReceiptsError> {
        CerebrasClient::chat(self, messages, opts)
    }
}

pub type SourceCache = Arc<Mutex<HashMap<String, String>>>;
pub(crate) type SourceMetaCache = Arc<Mutex<HashMap<String, SourceMeta>>>;

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

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchData {
    pub question: String,
    pub outcome: Outcome,
    pub claims: Vec<ResearchClaim>,
    pub search_trail: Vec<SearchTrailEntry>,
    pub uncertainties: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Answered,
    Partial,
    Unanswered,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResearchClaim {
    pub claim: String,
    pub source_url: String,
    pub quote: Option<String>,
    pub verdict: Verdict,
    pub note: String,
    pub published: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Supported,
    Partial,
    Unsupported,
    NoSource,
}

impl Verdict {
    pub fn is_supported_or_partial(self) -> bool {
        matches!(self, Verdict::Supported | Verdict::Partial)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
}

/// Internal verification result that retains the subquestion attribution from
/// the originating `ClaimCandidate`. This is what `verify_candidates` returns
/// so the deep-tier refinement logic can derive `dead_subquestions` from the
/// carried attribution instead of a positional zip (which breaks when
/// `run_chunked` drops a panicked thread).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VerifiedClaim {
    pub subquestion: String,
    pub claim: ResearchClaim,
}

#[derive(Clone)]
pub(crate) struct SharedState {
    pub spend: SharedSpend,
    pub budget_gate: Arc<Mutex<()>>,
    // lock order: source_cache before source_meta — keep it consistent to avoid deadlock.
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
    fn new(
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

    pub fn may_launch(&self, projected_unit_cost: f64) -> Result<bool, ReceiptsError> {
        let _gate = self
            .state
            .budget_gate
            .lock()
            .map_err(|_| ReceiptsError::upstream("budget gate lock poisoned"))?;
        let spend = self
            .state
            .spend
            .lock()
            .map_err(|_| ReceiptsError::upstream("spend meter lock poisoned"))?;
        Ok(self.budget.may_launch(&spend, projected_unit_cost))
    }
}

pub fn run(
    question: &str,
    depth: Depth,
    verify_policy: VerifyPolicy,
    budget: &Budget,
    chat: &dyn ChatProvider,
    search: &dyn SearchProvider,
    params: RunParams,
) -> Result<ResearchData, ReceiptsError> {
    let ctx = StageContext::new(chat, search, budget, &params);
    let mut uncertainties = Vec::new();

    let subquestions = prepare_subquestions(question, depth, &ctx, &mut uncertainties)?;
    let mut answers = launch_workers(
        initial_worker_tasks(depth, question, subquestions.clone()),
        &ctx,
        &mut uncertainties,
    )?;
    let mut candidates = extract_all(&answers, &ctx, &mut uncertainties);
    let mut verified = verify::verify_candidates(candidates.clone(), verify_policy, &ctx);

    if depth == Depth::Deep {
        // Derive dead subquestions from the subquestion attribution carried by
        // each VerifiedClaim — NOT a positional zip against `candidates`, which
        // breaks when `run_chunked` drops a panicked thread and misaligns every
        // subsequent pair.
        let verdicts: Vec<_> = verified
            .iter()
            .map(|vc| {
                (
                    vc.subquestion.clone(),
                    vc.claim.verdict.is_supported_or_partial(),
                )
            })
            .collect();
        let dead = dead_subquestions(&subquestions, &verdicts);
        if !dead.is_empty() {
            let refinement_start = answers.len();
            let refinement_answers =
                launch_workers(refinement_tasks(dead), &ctx, &mut uncertainties)?;
            answers.extend(refinement_answers);
            let refined = extract_all(&answers[refinement_start..], &ctx, &mut uncertainties);
            let start = candidates.len();
            candidates.extend(refined);
            let refined_verified =
                verify::verify_candidates(candidates[start..].to_vec(), verify_policy, &ctx);
            verified.extend(refined_verified);
        }
    }

    let claims = extract::dedup_research_claims(verified.into_iter().map(|vc| vc.claim).collect());
    let search_trail = ctx
        .state
        .search_trail
        .lock()
        .map_err(|_| ReceiptsError::upstream("search trail lock poisoned"))?
        .clone();
    if let Some(hit) = budget.hit() {
        uncertainties.push(format!("budget hit: {hit}"));
    }
    if !claims
        .iter()
        .any(|claim| claim.verdict.is_supported_or_partial())
        && budget.hit().is_none()
    {
        uncertainties.push("no supported or partial claims found".to_string());
    }

    let outcome = if budget.hit().is_some() {
        Outcome::Partial
    } else if claims
        .iter()
        .any(|claim| claim.verdict.is_supported_or_partial())
    {
        Outcome::Answered
    } else {
        Outcome::Unanswered
    };

    Ok(ResearchData {
        question: question.to_string(),
        outcome,
        claims,
        search_trail,
        uncertainties,
    })
}

/// Thin pub wrapper that runs `brief::synthesize_brief` with the same
/// chat/budget/spend the run used. Returns:
/// - `Ok(Some(text))` on success,
/// - `Ok(None)` when the budget gate refused the synthesis chat call,
/// - `Err(_)` on an upstream chat failure.
pub fn synthesize_brief(
    data: &ResearchData,
    chat: &dyn ChatProvider,
    search: &dyn SearchProvider,
    budget: &Budget,
    params: &RunParams,
) -> Result<Option<String>, ReceiptsError> {
    let ctx = StageContext::new(chat, search, budget, params);
    brief::synthesize_brief(data, &ctx)
}

fn prepare_subquestions(
    question: &str,
    depth: Depth,
    ctx: &StageContext<'_>,
    uncertainties: &mut Vec<String>,
) -> Result<Vec<String>, ReceiptsError> {
    if !depth.needs_decompose() {
        return Ok(vec![question.to_string()]);
    }
    if !ctx.may_launch(DECOMPOSE_WORST_CASE_COST)? {
        uncertainties.push("decomposition not launched: budget gate refused".to_string());
        return Ok(vec![question.to_string()]);
    }

    match decompose::decompose(question, depth.decompose_count(), ctx.today, ctx.chat) {
        Ok(subquestions) => Ok(limit_or_fallback(
            subquestions,
            depth.decompose_count(),
            question,
        )),
        Err(err) => {
            uncertainties.push(format!(
                "decomposition failed, using original question: {err}"
            ));
            Ok(vec![question.to_string()])
        }
    }
}

fn limit_or_fallback(mut subquestions: Vec<String>, count: usize, question: &str) -> Vec<String> {
    subquestions.retain(|s| !s.trim().is_empty());
    subquestions.truncate(count);
    if subquestions.is_empty() {
        vec![question.to_string()]
    } else {
        subquestions
    }
}

fn launch_workers(
    tasks: Vec<crate::tiers::WorkerTask>,
    ctx: &StageContext<'_>,
    uncertainties: &mut Vec<String>,
) -> Result<Vec<worker::WorkerAnswer>, ReceiptsError> {
    let mut out = Vec::new();
    let mut iter = tasks.into_iter().peekable();

    while iter.peek().is_some() {
        let mut batch = Vec::new();
        while batch.len() < ctx.max_concurrency {
            let Some(task) = iter.next() else { break };
            if ctx.may_launch(WORKER_ROUND_WORST_CASE_COST)? {
                batch.push(task);
            } else {
                uncertainties.push(format!(
                    "worker not launched for subquestion {:?}: budget gate refused",
                    task.subquestion
                ));
                for rest in iter.by_ref() {
                    uncertainties.push(format!(
                        "worker not launched for subquestion {:?}: budget gate refused",
                        rest.subquestion
                    ));
                }
                break;
            }
        }

        if batch.is_empty() {
            break;
        }

        let results = run_chunked(batch, ctx.max_concurrency, |task| {
            worker::run_worker(task, ctx)
        });
        for result in results {
            match result {
                Ok(answer) => {
                    if answer.budget_stopped {
                        uncertainties.push(format!(
                            "worker stopped by budget for subquestion {:?}",
                            answer.subquestion
                        ));
                    }
                    out.push(answer);
                }
                Err(err) => uncertainties.push(format!("worker failed: {err}")),
            }
        }
    }

    Ok(out)
}

fn extract_all(
    answers: &[worker::WorkerAnswer],
    ctx: &StageContext<'_>,
    uncertainties: &mut Vec<String>,
) -> Vec<ClaimCandidate> {
    let mut nested: Vec<Vec<ClaimCandidate>> = Vec::new();
    let mut iter = answers.iter().peekable();
    while iter.peek().is_some() {
        let mut batch: Vec<&worker::WorkerAnswer> = Vec::new();
        while batch.len() < ctx.max_concurrency {
            let Some(answer) = iter.next() else { break };
            match ctx.may_launch(EXTRACT_WORST_CASE_COST) {
                Ok(true) => batch.push(answer),
                Ok(false) => {
                    uncertainties.push(format!(
                        "extraction not launched for subquestion {:?}: budget gate refused",
                        answer.subquestion
                    ));
                    for rest in iter.by_ref() {
                        uncertainties.push(format!(
                            "extraction not launched for subquestion {:?}: budget gate refused",
                            rest.subquestion
                        ));
                    }
                    break;
                }
                Err(err) => {
                    uncertainties.push(format!("extraction gate failed: {err}"));
                    break;
                }
            }
        }

        if batch.is_empty() {
            break;
        }

        let results = run_chunked(batch, ctx.max_concurrency, |answer| {
            extract::extract_candidates(answer.clone(), ctx.chat)
        });
        nested.extend(results);
    }
    extract::dedup_candidates(nested.into_iter().flatten().collect())
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
        let batch: Vec<_> = iter.by_ref().take(max_concurrency.max(1)).collect();
        if batch.is_empty() {
            break;
        }
        std::thread::scope(|scope| {
            let handles: Vec<_> = batch
                .into_iter()
                .map(|item| scope.spawn(|| f(item)))
                .collect();
            out.extend(handles.into_iter().filter_map(|handle| handle.join().ok()));
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

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::{HashMap, VecDeque};
    use std::sync::Mutex;

    use crate::error::ReceiptsError;
    use crate::pipeline::{ChatProvider, StageContext};
    use crate::providers::cerebras::{ChatOpts, ChatResponse, Message, TokenUsage, ToolCall};
    use crate::providers::exa::{SearchProvider, SourceDoc};

    pub(crate) fn text_response(text: &str) -> ChatResponse {
        ChatResponse {
            content: text.to_string(),
            tool_calls: Vec::new(),
            usage: TokenUsage::default(),
            wall_time_ms: 0,
        }
    }

    pub(crate) fn tool_response(id: &str, query: &str, content: &str) -> ChatResponse {
        ChatResponse {
            content: content.to_string(),
            tool_calls: vec![ToolCall {
                id: id.to_string(),
                function_name: "search".to_string(),
                arguments: format!(r#"{{"query":"{query}"}}"#),
            }],
            usage: TokenUsage::default(),
            wall_time_ms: 0,
        }
    }

    pub(crate) struct ScriptedChat {
        responses: Mutex<VecDeque<Result<ChatResponse, ReceiptsError>>>,
        pub messages: Mutex<Vec<Vec<Message>>>,
        pub opts: Mutex<Vec<ChatOpts>>,
    }

    impl ScriptedChat {
        pub(crate) fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().map(Ok).collect()),
                messages: Mutex::new(Vec::new()),
                opts: Mutex::new(Vec::new()),
            }
        }
    }

    impl ChatProvider for ScriptedChat {
        fn chat(
            &self,
            messages: &[Message],
            opts: ChatOpts,
        ) -> Result<ChatResponse, ReceiptsError> {
            self.messages.lock().unwrap().push(messages.to_vec());
            self.opts.lock().unwrap().push(opts);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Ok(text_response("")))
        }
    }

    #[derive(Default)]
    pub(crate) struct FakeSearch {
        pub search_results: Mutex<HashMap<String, Vec<SourceDoc>>>,
        pub contents_results: Mutex<HashMap<String, Option<String>>>,
        pub searches: Mutex<Vec<String>>,
        pub contents_calls: Mutex<Vec<String>>,
    }

    impl SearchProvider for FakeSearch {
        fn search(&self, query: &str) -> Result<Vec<SourceDoc>, ReceiptsError> {
            self.searches.lock().unwrap().push(query.to_string());
            Ok(self
                .search_results
                .lock()
                .unwrap()
                .get(query)
                .cloned()
                .unwrap_or_default())
        }

        fn contents(&self, url: &str) -> Result<Option<String>, ReceiptsError> {
            self.contents_calls.lock().unwrap().push(url.to_string());
            Ok(self
                .contents_results
                .lock()
                .unwrap()
                .get(url)
                .cloned()
                .unwrap_or(None))
        }
    }

    pub(crate) fn test_ctx<'a>(
        chat: &'a dyn ChatProvider,
        search: &'a dyn SearchProvider,
        budget: &'a crate::budget::Budget,
        params: &'a crate::pipeline::RunParams,
    ) -> StageContext<'a> {
        StageContext::new(chat, search, budget, params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::Budget;
    use crate::pipeline::test_support::{FakeSearch, ScriptedChat};
    use crate::providers::new_spend;

    #[test]
    fn budget_refusal_mid_run_yields_partial_output() {
        let chat = ScriptedChat::new(Vec::new());
        let search = FakeSearch::default();
        let budget = Budget::new(Some(0.0), None);
        let params = RunParams::new("2026-07-01", 2, new_spend());

        let data = run(
            "question",
            Depth::Quick,
            VerifyPolicy::Adaptive,
            &budget,
            &chat,
            &search,
            params,
        )
        .unwrap();

        assert_eq!(data.outcome, Outcome::Partial);
        assert_eq!(budget.hit(), Some("dollars"));
        assert!(data.claims.is_empty());
    }

    #[test]
    fn decompose_budget_refusal_falls_back_to_original_question() {
        // Finding 2: a budget that refuses decompose must still produce worker
        // tasks on the original question, not zero workers.
        let chat = ScriptedChat::new(Vec::new());
        let search = FakeSearch::default();
        let budget = Budget::new(Some(0.0), None);
        let params = RunParams::new("2026-07-01", 2, new_spend());
        let ctx = StageContext::new(&chat, &search, &budget, &params);
        let mut uncertainties = Vec::new();

        let subquestions =
            prepare_subquestions("question", Depth::Standard, &ctx, &mut uncertainties).unwrap();

        // Falls back to the original question, not an empty vec.
        assert_eq!(subquestions, vec!["question".to_string()]);
        assert!(
            uncertainties
                .iter()
                .any(|u| u.contains("decomposition not launched"))
        );

        // Worker tasks are created from the fallback question.
        let tasks = initial_worker_tasks(Depth::Standard, "question", subquestions);
        assert!(!tasks.is_empty());
        assert_eq!(tasks[0].subquestion, "question");
    }

    #[test]
    fn extract_budget_refusal_yields_zero_claims_and_uncertainty() {
        // Finding 4: budget refused at extract → zero claims, uncertainty
        // recorded. We set up a worker answer but refuse the extraction gate.
        let chat = ScriptedChat::new(Vec::new());
        let search = FakeSearch::default();
        let budget = Budget::new(Some(0.0), None);
        let params = RunParams::new("2026-07-01", 2, new_spend());
        let ctx = StageContext::new(&chat, &search, &budget, &params);
        let mut uncertainties = Vec::new();

        let answers = vec![worker::WorkerAnswer {
            subquestion: "subq".to_string(),
            answer: "some answer".to_string(),
            budget_stopped: false,
        }];

        let candidates = extract_all(&answers, &ctx, &mut uncertainties);

        assert!(candidates.is_empty());
        assert!(
            uncertainties
                .iter()
                .any(|u| u.contains("extraction not launched"))
        );
    }

    #[test]
    fn dead_subquestion_selection_uses_verified_claim_attribution() {
        // Finding 1: dead-subquestion selection uses the subquestion attribution
        // carried by VerifiedClaim, not a positional zip. Simulate a scenario
        // where the first claim is unsupported (dead) and the second is
        // supported — the dead set should contain only the first subquestion.
        use crate::pipeline::verify::VerifyPolicy;

        let chat = ScriptedChat::new(vec![
            test_support::text_response(r#"{"verdict":"unsupported","note":"no"}"#),
            test_support::text_response(r#"{"verdict":"supported","note":"yes"}"#),
        ]);
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        // concurrency=1 so candidates are verified sequentially in order.
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = StageContext::new(&chat, &search, &budget, &params);
        ctx.state
            .source_cache
            .lock()
            .unwrap()
            .insert("https://a.com".to_string(), "text".to_string());
        ctx.state
            .source_cache
            .lock()
            .unwrap()
            .insert("https://b.com".to_string(), "text".to_string());

        let candidates = vec![
            ClaimCandidate {
                subquestion: "sub-a".to_string(),
                claim: "A happened".to_string(),
                url: "https://a.com".to_string(),
            },
            ClaimCandidate {
                subquestion: "sub-b".to_string(),
                claim: "B happened".to_string(),
                url: "https://b.com".to_string(),
            },
        ];

        let verified = verify::verify_candidates(candidates, VerifyPolicy::Adaptive, &ctx);

        // Derive dead subquestions from the carried attribution (no zip).
        let verdicts: Vec<_> = verified
            .iter()
            .map(|vc| {
                (
                    vc.subquestion.clone(),
                    vc.claim.verdict.is_supported_or_partial(),
                )
            })
            .collect();
        let subquestions = vec!["sub-a".to_string(), "sub-b".to_string()];
        let dead = dead_subquestions(&subquestions, &verdicts);

        // sub-a is unsupported → dead; sub-b is supported → alive.
        assert_eq!(dead, vec!["sub-a"]);
    }
}
