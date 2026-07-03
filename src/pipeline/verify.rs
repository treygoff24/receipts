use std::collections::HashMap;

use serde::Deserialize;
use serde_json::json;

use crate::pipeline::{
    ClaimCandidate, Relevance, ResearchClaim, StageContext, Verdict, VerifiedClaim, chat_json,
    run_chunked,
};
use crate::providers::cerebras::{ChatOpts, Message};
use crate::tiers::{
    CONTENTS_WORST_CASE_COST, RELEVANCE_WORST_CASE_COST, VERIFICATION_WORST_CASE_COST,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyPolicy {
    Adaptive,
    Paranoid,
    Off,
}

#[derive(Debug, Clone, Deserialize)]
struct VerdictOutput {
    verdict: Verdict,
    note: String,
    quote: Option<String>,
}

#[derive(Debug, Clone)]
struct SourceHit {
    text: String,
    published: Option<String>,
}

/// Raw vote from the relevance-gate model call. Distinct from
/// `pipeline::Relevance` (the envelope-facing `direct`/`related`/`off_topic`
/// field) — this is the gate's direct/partially/no question, mapped onto that
/// output type in `classify_relevance`. Vote names must match the prompt's
/// answer vocabulary AND the strict output schema in
/// `relevance_response_format` — a mismatch forces the model to emit a word
/// the prompt never taught it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RelevanceVote {
    Direct,
    Partially,
    No,
}

#[derive(Debug, Clone, Deserialize)]
struct RelevanceOutput {
    relevance: RelevanceVote,
}

/// Relevance gate + verification, in that order: cheap claim-vs-question
/// check first, so claims that don't bear on the original question never pay
/// for the expensive claim-vs-source verifier. Skipped entirely when
/// verification itself is off, so `--verify off` stays a single, predictable
/// no-chat-calls path.
pub(crate) fn gate_and_verify(
    candidates: Vec<ClaimCandidate>,
    question: &str,
    policy: VerifyPolicy,
    ctx: &StageContext<'_>,
) -> Vec<VerifiedClaim> {
    if policy == VerifyPolicy::Off {
        return verify_candidates(candidates, policy, ctx);
    }
    let (on_topic, mut off_topic) = relevance_gate(candidates, question, ctx);
    off_topic.extend(verify_candidates(on_topic, policy, ctx));
    off_topic
}

/// `classify_relevance`'s verdict: relevant claims flow on to verification,
/// off-topic claims are already a finished `VerifiedClaim`. A plain enum
/// (rather than `Result`) sidesteps clippy's large-error lint, since a
/// `VerifiedClaim` embeds a full `ResearchClaim`.
enum RelevanceResult {
    OnTopic(ClaimCandidate),
    OffTopic(VerifiedClaim),
}

fn relevance_gate(
    candidates: Vec<ClaimCandidate>,
    question: &str,
    ctx: &StageContext<'_>,
) -> (Vec<ClaimCandidate>, Vec<VerifiedClaim>) {
    let mut on_topic = Vec::new();
    let mut off_topic = Vec::new();
    for result in run_chunked(candidates, ctx.max_concurrency, |candidate| {
        classify_relevance(candidate, question, ctx)
    }) {
        match result {
            RelevanceResult::OnTopic(candidate) => on_topic.push(candidate),
            RelevanceResult::OffTopic(verified) => off_topic.push(verified),
        }
    }
    (on_topic, off_topic)
}

/// Returns `OnTopic` for anything relevant (or unclassifiable — the gate
/// fails open so a model hiccup doesn't silently drop a real claim) and
/// `OffTopic` for claims the model judged off-topic.
fn classify_relevance(
    candidate: ClaimCandidate,
    question: &str,
    ctx: &StageContext<'_>,
) -> RelevanceResult {
    if !matches!(ctx.may_launch(RELEVANCE_WORST_CASE_COST), Ok(true)) {
        return RelevanceResult::OnTopic(candidate);
    }

    let parsed: Result<RelevanceOutput, _> = chat_json(
        ctx.chat,
        &[Message::user(relevance_prompt(question, &candidate.claim))],
        ChatOpts {
            temperature: Some(0.1),
            max_completion_tokens: Some(120),
            response_format: Some(relevance_response_format()),
            ..ChatOpts::default()
        },
    );
    match parsed {
        Ok(output) if output.relevance == RelevanceVote::No => {
            RelevanceResult::OffTopic(VerifiedClaim {
                subquestion: candidate.subquestion,
                claim: ResearchClaim {
                    claim: candidate.claim,
                    source_url: Some(candidate.url),
                    quote: None,
                    verdict: Verdict::OffTopic,
                    relevance: Relevance::OffTopic,
                    note: "claim does not answer or bear on the original question".to_string(),
                    published: None,
                },
            })
        }
        Ok(output) => RelevanceResult::OnTopic(ClaimCandidate {
            relevance: match output.relevance {
                RelevanceVote::Direct => Relevance::Direct,
                RelevanceVote::Partially => Relevance::Related,
                RelevanceVote::No => unreachable!("handled above"),
            },
            ..candidate
        }),
        Err(_) => RelevanceResult::OnTopic(candidate),
    }
}

/// The relevance gate's core failure mode (observed live) is matching
/// question SHAPE instead of question ENTITY: a claim about a different
/// case/person/bill that merely looks like an answer (same kind of filing,
/// same date pattern) scores as relevant, while a genuinely on-topic claim
/// that only answers part of a multi-part question gets downgraded. This
/// prompt makes entity identification the first step and states the
/// entity-mismatch rule explicitly, with few-shot examples modeled on both
/// failure directions.
fn relevance_prompt(question: &str, claim: &str) -> String {
    format!(
        "You are checking whether a CLAIM is relevant to a QUESTION.\n\n\
        Step 1: identify the specific named entities the QUESTION anchors to — a case name/number, a person, an organization, a bill, a place — the thing the question is actually about.\n\
        Step 2: check whether the CLAIM concerns those SAME named entities, or a DIFFERENT one that merely has a similar shape (same kind of filing, same kind of event, same date pattern).\n\n\
        Answer one of:\n\
        - no: the QUESTION names a specific entity and the CLAIM is about a DIFFERENT one — even if the claim's shape matches what the question asks for (a deadline, a filing, a status update). Entity mismatch always overrides shape match; a well-formed answer about the wrong subject is still \"no\".\n\
        - direct: the CLAIM concerns the SAME entity/entities the QUESTION names AND provides information the QUESTION asks for. This includes a claim that answers only PART of a multi-part question — do not downgrade a correct, on-entity partial answer.\n\
        - partially: the CLAIM concerns the SAME entity/subject but does NOT provide the information asked for, or the claim is closely adjacent subject matter (same general topic, not a clearly separate named entity).\n\n\
        Examples:\n\
        Q: \"What is the status of the consent decree in United States v. Acme Corp (Case No. 1:20-cv-001)?\"\n\
        Claim: \"In Case No. 3:22-cv-999, a motion for summary judgment was scheduled for May 2026.\"\n\
        -> no (different case number; the claim's shape — a scheduled motion — matches what was asked, but the entity doesn't)\n\n\
        Q: \"When was the XYZ Reauthorization Act most recently reauthorized, and through what year does its charter run?\"\n\
        Claim: \"The XYZ Reauthorization Act was reauthorized in 2019 via P.L. 116-94.\"\n\
        -> direct (same entity, correctly answers half of a two-part question)\n\n\
        Q: \"What deadlines are pending on the Acme Corp consent decree docket?\"\n\
        Claim: \"Acme Corp is required to implement a $6 million remediation plan under the decree.\"\n\
        -> partially (same entity, but doesn't state a deadline — not what was asked)\n\n\
        QUESTION: {question}\n\nCLAIM: {claim}"
    )
}

fn relevance_response_format() -> serde_json::Value {
    json!({
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
    })
}

pub(crate) fn verify_candidates(
    candidates: Vec<ClaimCandidate>,
    policy: VerifyPolicy,
    ctx: &StageContext<'_>,
) -> Vec<VerifiedClaim> {
    if policy == VerifyPolicy::Off {
        return candidates
            .into_iter()
            .map(|candidate| disabled_claim(candidate, "verification disabled"))
            .collect();
    }

    let per_claim_cost = projected_verify_cost(policy);
    let mut out = Vec::new();
    let mut iter = candidates.into_iter().peekable();
    while iter.peek().is_some() {
        let mut batch = Vec::new();
        while batch.len() < ctx.max_concurrency {
            let Some(candidate) = iter.next() else { break };
            match ctx.may_launch(per_claim_cost) {
                Ok(true) => batch.push(candidate),
                Ok(false) => {
                    out.push(disabled_claim(
                        candidate,
                        "verification not launched: budget hit",
                    ));
                    out.extend(
                        iter.map(|rest| {
                            disabled_claim(rest, "verification not launched: budget hit")
                        }),
                    );
                    return out;
                }
                Err(err) => {
                    out.push(disabled_claim(
                        candidate,
                        &format!("verification not launched: {err}"),
                    ));
                    out.extend(iter.map(|rest| {
                        disabled_claim(rest, "verification not launched after gate failure")
                    }));
                    return out;
                }
            }
        }

        out.extend(run_chunked(batch, ctx.max_concurrency, |candidate| {
            verify_claim(candidate, policy, ctx)
        }));
    }
    out
}

/// Per-claim worst-case verification cost projected before launch, accounting
/// for multi-judge policies. Paranoid always runs 3 judges; Adaptive runs 1
/// (escalation to +2 is gated separately inside `judge`). Off is never gated
/// here (handled by the early return above).
fn projected_verify_cost(policy: VerifyPolicy) -> f64 {
    match policy {
        VerifyPolicy::Paranoid => VERIFICATION_WORST_CASE_COST * 3.0,
        VerifyPolicy::Adaptive => VERIFICATION_WORST_CASE_COST * 1.0,
        VerifyPolicy::Off => VERIFICATION_WORST_CASE_COST,
    }
}

pub(crate) fn verify_claim(
    candidate: ClaimCandidate,
    policy: VerifyPolicy,
    ctx: &StageContext<'_>,
) -> VerifiedClaim {
    if policy == VerifyPolicy::Off {
        return disabled_claim(candidate, "verification disabled");
    }

    let subquestion = candidate.subquestion.clone();
    let relevance = candidate.relevance;
    let Some(source) = source_for_claim(&candidate, ctx) else {
        return disabled_claim(candidate, "no source text available");
    };

    match judge(&candidate, &source.text, policy, ctx) {
        Ok((verdict, note, quote)) => VerifiedClaim {
            subquestion,
            claim: ResearchClaim {
                claim: candidate.claim,
                source_url: Some(candidate.url),
                quote,
                verdict,
                relevance,
                note,
                published: source.published,
            },
        },
        Err(err) => VerifiedClaim {
            subquestion,
            claim: ResearchClaim {
                claim: candidate.claim,
                source_url: Some(candidate.url),
                quote: None,
                verdict: Verdict::NoSource,
                relevance,
                note: format!("verification failed: {err}"),
                published: source.published,
            },
        },
    }
}

fn source_for_claim(candidate: &ClaimCandidate, ctx: &StageContext<'_>) -> Option<SourceHit> {
    let url = candidate.url.trim();
    if url.is_empty() {
        return None;
    }

    if let Some(hit) = cached_source(url, ctx) {
        return Some(hit);
    }

    // Gate the paid Exa contents call before launching it.
    match ctx.may_launch(CONTENTS_WORST_CASE_COST) {
        Ok(true) => {}
        _ => return None,
    }

    let fetched = ctx.search.contents(url).ok().flatten()?;
    if let Ok(mut cache) = ctx.state.source_cache.lock() {
        cache.insert(url.to_string(), fetched.clone());
    }
    Some(SourceHit {
        text: fetched,
        published: None,
    })
}

fn cached_source(url: &str, ctx: &StageContext<'_>) -> Option<SourceHit> {
    let cache = ctx.state.source_cache.lock().ok()?;
    let meta = ctx.state.source_meta.lock().ok()?;

    if let Some(text) = cache.get(url) {
        return Some(SourceHit {
            text: text.clone(),
            published: meta.get(url).and_then(|m| m.published.clone()),
        });
    }

    cache
        .iter()
        .find(|(key, _)| key.contains(url) || url.contains(key.as_str()))
        .map(|(key, text)| SourceHit {
            text: text.clone(),
            published: meta.get(key).and_then(|m| m.published.clone()),
        })
}

fn judge(
    candidate: &ClaimCandidate,
    source_text: &str,
    policy: VerifyPolicy,
    ctx: &StageContext<'_>,
) -> Result<(Verdict, String, Option<String>), crate::error::ReceiptsError> {
    let first = judge_once(candidate, source_text, ctx)?;
    let mut votes = vec![first.clone()];

    let needs_more = match policy {
        VerifyPolicy::Adaptive => votes[0].verdict == Verdict::Partial,
        VerifyPolicy::Paranoid => true,
        VerifyPolicy::Off => false,
    };
    if needs_more && policy == VerifyPolicy::Adaptive {
        // Gate the +2 escalation judges. If the budget refuses, keep the
        // single judge's verdict as-is (partial stands, no escalation).
        if ctx
            .may_launch(2.0 * VERIFICATION_WORST_CASE_COST)
            .unwrap_or(false)
        {
            votes.push(judge_once(candidate, source_text, ctx)?);
            votes.push(judge_once(candidate, source_text, ctx)?);
        }
    } else if needs_more {
        // Paranoid: cost already projected at 3x in verify_candidates.
        votes.push(judge_once(candidate, source_text, ctx)?);
        votes.push(judge_once(candidate, source_text, ctx)?);
    }

    Ok(majority(votes, source_text))
}

fn judge_once(
    candidate: &ClaimCandidate,
    source_text: &str,
    ctx: &StageContext<'_>,
) -> Result<VerdictOutput, crate::error::ReceiptsError> {
    let prompt = format!(
        "CLAIM: {}\n\nSOURCE TEXT (from {}):\n{}\n\nDoes the source text support the claim? Be strict: SUPPORTED only if the text states it. If SUPPORTED or PARTIAL, also return `quote`: the exact supporting sentence(s) copied verbatim from SOURCE TEXT (do not paraphrase, do not translate). If UNSUPPORTED, return `quote` as null.",
        candidate.claim,
        candidate.url,
        truncate_chars(source_text, 6000)
    );
    chat_json(
        ctx.chat,
        &[Message::user(prompt)],
        ChatOpts {
            temperature: Some(0.1),
            max_completion_tokens: Some(600),
            response_format: Some(response_format()),
            ..ChatOpts::default()
        },
    )
}

fn majority(votes: Vec<VerdictOutput>, source_text: &str) -> (Verdict, String, Option<String>) {
    let mut counts: HashMap<Verdict, usize> = HashMap::new();
    for vote in &votes {
        *counts.entry(vote.verdict).or_default() += 1;
    }

    let verdicts = [Verdict::Supported, Verdict::Partial, Verdict::Unsupported];
    let best = verdicts
        .into_iter()
        .max_by_key(|verdict| counts.get(verdict).copied().unwrap_or_default())
        .unwrap_or(Verdict::Partial);
    let max_count = counts.get(&best).copied().unwrap_or_default();
    let tied = counts.values().filter(|count| **count == max_count).count() > 1;
    let verdict = if tied { Verdict::Partial } else { best };

    let raw_quote = votes
        .iter()
        .find(|vote| vote.verdict == verdict)
        .and_then(|vote| vote.quote.clone());
    let mut note = votes
        .into_iter()
        .map(|vote| vote.note)
        .collect::<Vec<_>>()
        .join(" | ");

    let quote = resolve_quote(verdict, raw_quote, source_text, &mut note);
    (verdict, note, quote)
}

/// Only `supported`/`partial` verdicts may carry a quote. The model's raw
/// quote must be an exact (whitespace/case-normalized) substring of the
/// fetched source text; a quote that fails that check is dropped rather than
/// trusted, with a short note appended so the failure is visible.
fn resolve_quote(
    verdict: Verdict,
    raw_quote: Option<String>,
    source_text: &str,
    note: &mut String,
) -> Option<String> {
    if !matches!(verdict, Verdict::Supported | Verdict::Partial) {
        return None;
    }
    let quote = match raw_quote {
        Some(quote) if !quote.trim().is_empty() => quote,
        _ => {
            append_note(note, "no quote returned by verifier");
            return None;
        }
    };
    if quote_matches_source(&quote, source_text) {
        Some(quote)
    } else {
        append_note(note, "quote failed source-match validation");
        None
    }
}

fn append_note(note: &mut String, addition: &str) {
    if note.trim().is_empty() {
        *note = addition.to_string();
    } else {
        note.push_str(" | ");
        note.push_str(addition);
    }
}

/// Whitespace-collapsed, case-insensitive substring check. Deliberately
/// forgiving of formatting differences (line wraps, extra spaces) between
/// what the model copies and how the source text was extracted, while still
/// rejecting paraphrase or fabrication.
fn quote_matches_source(quote: &str, source_text: &str) -> bool {
    let needle = normalize_for_match(quote);
    !needle.is_empty() && normalize_for_match(source_text).contains(&needle)
}

fn normalize_for_match(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn disabled_claim(candidate: ClaimCandidate, note: &str) -> VerifiedClaim {
    VerifiedClaim {
        subquestion: candidate.subquestion,
        claim: ResearchClaim {
            claim: candidate.claim,
            source_url: Some(candidate.url),
            quote: None,
            verdict: Verdict::NoSource,
            relevance: candidate.relevance,
            note: note.to_string(),
            published: None,
        },
    }
}

fn response_format() -> serde_json::Value {
    json!({
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
    })
}

fn truncate_chars(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::Budget;
    use crate::pipeline::test_support::{FakeSearch, ScriptedChat, test_ctx, text_response};
    use crate::pipeline::{RunParams, SourceMeta};
    use crate::providers::new_spend;

    fn candidate(url: &str) -> ClaimCandidate {
        ClaimCandidate {
            subquestion: "q".to_string(),
            claim: "A happened".to_string(),
            url: url.to_string(),
            relevance: crate::pipeline::Relevance::Direct,
        }
    }

    fn verdict(verdict: &str, note: &str) -> crate::providers::cerebras::ChatResponse {
        text_response(&format!(r#"{{"verdict":"{verdict}","note":"{note}"}}"#))
    }

    #[test]
    fn exact_cache_hit_uses_source_and_published_date() {
        let chat = ScriptedChat::new(vec![verdict("supported", "ok")]);
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);
        ctx.state
            .source_cache
            .lock()
            .unwrap()
            .insert("https://example.com".to_string(), "A happened".to_string());
        ctx.state.source_meta.lock().unwrap().insert(
            "https://example.com".to_string(),
            SourceMeta {
                published: Some("2026-07-01".to_string()),
            },
        );

        let claim = verify_claim(
            candidate("https://example.com"),
            VerifyPolicy::Adaptive,
            &ctx,
        );

        assert_eq!(claim.claim.verdict, Verdict::Supported);
        assert_eq!(claim.subquestion, "q");
        assert_eq!(claim.claim.published.as_deref(), Some("2026-07-01"));
    }

    #[test]
    fn fuzzy_cache_hit_accepts_containing_urls() {
        let chat = ScriptedChat::new(vec![verdict("supported", "ok")]);
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);
        ctx.state.source_cache.lock().unwrap().insert(
            "https://example.com/page?utm=1".to_string(),
            "A happened".to_string(),
        );

        let claim = verify_claim(
            candidate("https://example.com/page"),
            VerifyPolicy::Adaptive,
            &ctx,
        );

        assert_eq!(claim.claim.verdict, Verdict::Supported);
        assert!(search.contents_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn contents_fallback_fetches_source_text() {
        let chat = ScriptedChat::new(vec![verdict("supported", "ok")]);
        let search = FakeSearch::default();
        search.contents_results.lock().unwrap().insert(
            "https://example.com".to_string(),
            Some("A happened".to_string()),
        );
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);

        let claim = verify_claim(
            candidate("https://example.com"),
            VerifyPolicy::Adaptive,
            &ctx,
        );

        assert_eq!(claim.claim.verdict, Verdict::Supported);
        assert_eq!(
            ctx.state
                .source_cache
                .lock()
                .unwrap()
                .get("https://example.com")
                .unwrap(),
            "A happened"
        );
    }

    #[test]
    fn no_source_when_cache_and_contents_miss() {
        let chat = ScriptedChat::new(Vec::new());
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);

        let claim = verify_claim(
            candidate("https://missing.com"),
            VerifyPolicy::Adaptive,
            &ctx,
        );

        assert_eq!(claim.claim.verdict, Verdict::NoSource);
        assert!(chat.messages.lock().unwrap().is_empty());
    }

    #[test]
    fn adaptive_escalates_partial_and_majority_wins() {
        let chat = ScriptedChat::new(vec![
            verdict("partial", "weak"),
            verdict("supported", "yes1"),
            verdict("supported", "yes2"),
        ]);
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);
        ctx.state
            .source_cache
            .lock()
            .unwrap()
            .insert("https://example.com".to_string(), "A happened".to_string());

        let claim = verify_claim(
            candidate("https://example.com"),
            VerifyPolicy::Adaptive,
            &ctx,
        );

        assert_eq!(claim.claim.verdict, Verdict::Supported);
        assert_eq!(chat.messages.lock().unwrap().len(), 3);
    }

    #[test]
    fn paranoid_runs_three_judges_for_every_claim() {
        let chat = ScriptedChat::new(vec![
            verdict("supported", "yes"),
            verdict("unsupported", "no1"),
            verdict("unsupported", "no2"),
        ]);
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);
        ctx.state
            .source_cache
            .lock()
            .unwrap()
            .insert("https://example.com".to_string(), "A happened".to_string());

        let claim = verify_claim(
            candidate("https://example.com"),
            VerifyPolicy::Paranoid,
            &ctx,
        );

        assert_eq!(claim.claim.verdict, Verdict::Unsupported);
        assert_eq!(chat.messages.lock().unwrap().len(), 3);
    }

    #[test]
    fn off_policy_marks_every_claim_no_source_without_chat() {
        let chat = ScriptedChat::new(Vec::new());
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);

        let claim = verify_claim(candidate("https://example.com"), VerifyPolicy::Off, &ctx);

        assert_eq!(claim.claim.verdict, Verdict::NoSource);
        assert_eq!(claim.claim.note, "verification disabled");
        assert!(chat.messages.lock().unwrap().is_empty());
    }

    #[test]
    fn adaptive_escalation_skipped_under_exhausted_budget() {
        // Budget is enough for the single judge but NOT for the +2 escalation
        // judges. The verdict stays partial (no escalation). Source text is
        // pre-cached so the contents gate is not involved.
        let chat = ScriptedChat::new(vec![verdict("partial", "weak")]);
        let search = FakeSearch::default();
        let budget = Budget::new(Some(0.01), None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        // Pre-load spend so: verify gate (0.002) passes (0.007+0.002=0.009<=0.01)
        // but escalation gate (2*0.002=0.004) fails (0.007+0.004=0.011>0.01).
        {
            let mut spend = params.spend.lock().unwrap();
            spend.dollars = 0.007;
        }
        let ctx = test_ctx(&chat, &search, &budget, &params);
        ctx.state
            .source_cache
            .lock()
            .unwrap()
            .insert("https://example.com".to_string(), "A happened".to_string());

        let claim = verify_claim(
            candidate("https://example.com"),
            VerifyPolicy::Adaptive,
            &ctx,
        );

        assert_eq!(claim.claim.verdict, Verdict::Partial);
        // Only the single judge chat call happened — no escalation.
        assert_eq!(chat.messages.lock().unwrap().len(), 1);
    }

    #[test]
    fn budget_refused_contents_fetch_yields_no_source() {
        let chat = ScriptedChat::new(Vec::new());
        let search = FakeSearch::default();
        search.contents_results.lock().unwrap().insert(
            "https://example.com".to_string(),
            Some("A happened".to_string()),
        );
        // Budget exhausted: even the contents gate (0.005) is refused.
        let budget = Budget::new(Some(0.0), None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);

        let claim = verify_claim(
            candidate("https://example.com"),
            VerifyPolicy::Adaptive,
            &ctx,
        );

        assert_eq!(claim.claim.verdict, Verdict::NoSource);
        // No contents call was made — the gate refused before fetching.
        assert!(search.contents_calls.lock().unwrap().is_empty());
        // No chat call either.
        assert!(chat.messages.lock().unwrap().is_empty());
    }

    #[test]
    fn verified_claim_carries_subquestion_attribution() {
        // Finding 1: verification preserves the subquestion so deep-tier
        // refinement does not need a positional zip. Test that the attribution
        // is carried by the verified claims, not position.
        let chat = ScriptedChat::new(vec![
            verdict("supported", "ok"),
            verdict("unsupported", "no"),
        ]);
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        // Concurrency 1: ScriptedChat pops responses off a shared queue, so
        // parallel judges would race for who gets "supported".
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);
        ctx.state
            .source_cache
            .lock()
            .unwrap()
            .insert("https://a.com".to_string(), "A happened".to_string());
        ctx.state
            .source_cache
            .lock()
            .unwrap()
            .insert("https://b.com".to_string(), "A happened".to_string());

        let candidates = vec![
            ClaimCandidate {
                subquestion: "sub-a".to_string(),
                claim: "A happened".to_string(),
                url: "https://a.com".to_string(),
                relevance: crate::pipeline::Relevance::Direct,
            },
            ClaimCandidate {
                subquestion: "sub-b".to_string(),
                claim: "B happened".to_string(),
                url: "https://b.com".to_string(),
                relevance: crate::pipeline::Relevance::Direct,
            },
        ];

        let verified = verify_candidates(candidates, VerifyPolicy::Adaptive, &ctx);

        // Each verified claim carries its own subquestion regardless of order.
        let sub_a = verified
            .iter()
            .find(|vc| vc.subquestion == "sub-a")
            .expect("sub-a present");
        assert_eq!(sub_a.claim.verdict, Verdict::Supported);
        let sub_b = verified
            .iter()
            .find(|vc| vc.subquestion == "sub-b")
            .expect("sub-b present");
        assert_eq!(sub_b.claim.verdict, Verdict::Unsupported);
    }

    fn verdict_with_quote(
        verdict: &str,
        note: &str,
        quote: &str,
    ) -> crate::providers::cerebras::ChatResponse {
        text_response(&format!(
            r#"{{"verdict":"{verdict}","note":"{note}","quote":"{quote}"}}"#
        ))
    }

    #[test]
    fn supported_claim_carries_validated_quote() {
        let chat = ScriptedChat::new(vec![verdict_with_quote(
            "supported",
            "ok",
            "A happened yesterday",
        )]);
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);
        ctx.state.source_cache.lock().unwrap().insert(
            "https://example.com".to_string(),
            "Some preamble.   A   happened yesterday. Some epilogue.".to_string(),
        );

        let claim = verify_claim(
            candidate("https://example.com"),
            VerifyPolicy::Adaptive,
            &ctx,
        );

        assert_eq!(claim.claim.verdict, Verdict::Supported);
        assert_eq!(claim.claim.quote.as_deref(), Some("A happened yesterday"));
        assert!(!claim.claim.note.contains("quote failed"));
    }

    #[test]
    fn quote_that_does_not_match_source_is_dropped_with_note() {
        let chat = ScriptedChat::new(vec![verdict_with_quote(
            "supported",
            "ok",
            "this text is nowhere in the source",
        )]);
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);
        ctx.state
            .source_cache
            .lock()
            .unwrap()
            .insert("https://example.com".to_string(), "A happened".to_string());

        let claim = verify_claim(
            candidate("https://example.com"),
            VerifyPolicy::Adaptive,
            &ctx,
        );

        assert_eq!(claim.claim.verdict, Verdict::Supported);
        assert_eq!(claim.claim.quote, None);
        assert!(
            claim
                .claim
                .note
                .contains("quote failed source-match validation")
        );
    }

    #[test]
    fn supported_verdict_with_missing_quote_notes_it() {
        let chat = ScriptedChat::new(vec![verdict("supported", "ok")]);
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);
        ctx.state
            .source_cache
            .lock()
            .unwrap()
            .insert("https://example.com".to_string(), "A happened".to_string());

        let claim = verify_claim(
            candidate("https://example.com"),
            VerifyPolicy::Adaptive,
            &ctx,
        );

        assert_eq!(claim.claim.verdict, Verdict::Supported);
        assert_eq!(claim.claim.quote, None);
        assert!(claim.claim.note.contains("no quote returned by verifier"));
    }

    #[test]
    fn unsupported_verdict_never_carries_a_quote() {
        let chat = ScriptedChat::new(vec![verdict_with_quote("unsupported", "no", "A happened")]);
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);
        ctx.state
            .source_cache
            .lock()
            .unwrap()
            .insert("https://example.com".to_string(), "A happened".to_string());

        let claim = verify_claim(
            candidate("https://example.com"),
            VerifyPolicy::Adaptive,
            &ctx,
        );

        assert_eq!(claim.claim.verdict, Verdict::Unsupported);
        assert_eq!(claim.claim.quote, None);
    }

    #[test]
    fn quote_match_normalizes_whitespace_and_case() {
        assert!(quote_matches_source(
            "A   Happened\nYesterday",
            "prefix a happened yesterday suffix"
        ));
        assert!(!quote_matches_source(
            "never in source",
            "prefix a happened yesterday suffix"
        ));
        assert!(!quote_matches_source("", "any source text"));
    }

    #[test]
    fn relevance_gate_routes_no_to_off_topic_without_running_verify() {
        let chat = ScriptedChat::new(vec![text_response(r#"{"relevance":"no"}"#)]);
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);

        let verified = gate_and_verify(
            vec![candidate("https://example.com")],
            "the original question",
            VerifyPolicy::Adaptive,
            &ctx,
        );

        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0].claim.verdict, Verdict::OffTopic);
        assert_eq!(verified[0].claim.quote, None);
        // Only the relevance call happened — no claim-vs-source verify call.
        assert_eq!(chat.messages.lock().unwrap().len(), 1);
    }

    #[test]
    fn relevance_gate_passes_yes_through_to_verify() {
        let chat = ScriptedChat::new(vec![
            text_response(r#"{"relevance":"direct"}"#),
            verdict("supported", "ok"),
        ]);
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);
        ctx.state
            .source_cache
            .lock()
            .unwrap()
            .insert("https://example.com".to_string(), "A happened".to_string());

        let verified = gate_and_verify(
            vec![candidate("https://example.com")],
            "the original question",
            VerifyPolicy::Adaptive,
            &ctx,
        );

        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0].claim.verdict, Verdict::Supported);
        assert_eq!(chat.messages.lock().unwrap().len(), 2);
    }

    #[test]
    fn relevance_gate_skipped_entirely_when_verify_is_off() {
        let chat = ScriptedChat::new(Vec::new());
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);

        let verified = gate_and_verify(
            vec![candidate("https://example.com")],
            "the original question",
            VerifyPolicy::Off,
            &ctx,
        );

        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0].claim.verdict, Verdict::NoSource);
        assert_eq!(verified[0].claim.note, "verification disabled");
        assert!(chat.messages.lock().unwrap().is_empty());
    }

    #[test]
    fn relevance_gate_fails_open_on_unparseable_response() {
        // A model hiccup on the relevance call must not silently drop a real
        // claim — it should fall through to normal verification.
        let chat = ScriptedChat::new(vec![
            text_response("not json"),
            text_response("still not json"),
            verdict("supported", "ok"),
        ]);
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);
        ctx.state
            .source_cache
            .lock()
            .unwrap()
            .insert("https://example.com".to_string(), "A happened".to_string());

        let verified = gate_and_verify(
            vec![candidate("https://example.com")],
            "the original question",
            VerifyPolicy::Adaptive,
            &ctx,
        );

        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0].claim.verdict, Verdict::Supported);
    }

    #[test]
    fn relevance_gate_partially_maps_to_related_and_still_verifies() {
        // Codex finding 1: "partially" must not be treated as fully on-topic.
        // It proceeds to verification (unlike "no"), but is tagged `related`
        // so a supported-but-only-related claim can't flip outcome to
        // `answered` on its own (see pipeline::tests::
        // derive_outcome_partial_when_supported_but_only_related).
        let chat = ScriptedChat::new(vec![
            text_response(r#"{"relevance":"partially"}"#),
            verdict("supported", "ok"),
        ]);
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);
        ctx.state
            .source_cache
            .lock()
            .unwrap()
            .insert("https://example.com".to_string(), "A happened".to_string());

        let verified = gate_and_verify(
            vec![candidate("https://example.com")],
            "the original question",
            VerifyPolicy::Adaptive,
            &ctx,
        );

        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0].claim.verdict, Verdict::Supported);
        assert_eq!(
            verified[0].claim.relevance,
            crate::pipeline::Relevance::Related
        );
    }

    #[test]
    fn relevance_prompt_states_entity_mismatch_rule_and_embeds_inputs() {
        // Live validation found the gate matching question SHAPE instead of
        // question ENTITY (a claim about a different case that merely looked
        // like an answer scored as relevant). This locks in that the prompt
        // carries the entity-first rubric, not just the question/claim text.
        let prompt = relevance_prompt(
            "What deadlines are pending on the Foo Corp docket?",
            "Bar Corp was ordered to pay damages.",
        );

        assert!(prompt.contains("What deadlines are pending on the Foo Corp docket?"));
        assert!(prompt.contains("Bar Corp was ordered to pay damages."));
        assert!(prompt.contains("Entity mismatch always overrides shape match"));
        assert!(prompt.to_lowercase().contains("named entities"));
    }
}
