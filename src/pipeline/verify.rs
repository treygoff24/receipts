use std::collections::HashMap;

use serde::Deserialize;
use serde_json::json;

use crate::pipeline::{
    ClaimCandidate, ResearchClaim, StageContext, Verdict, VerifiedClaim, chat_json, run_chunked,
};
use crate::providers::cerebras::{ChatOpts, Message};
use crate::tiers::{CONTENTS_WORST_CASE_COST, VERIFICATION_WORST_CASE_COST};

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
}

#[derive(Debug, Clone)]
struct SourceHit {
    text: String,
    published: Option<String>,
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
    let Some(source) = source_for_claim(&candidate, ctx) else {
        return disabled_claim(candidate, "no source text available");
    };

    match judge(&candidate, &source.text, policy, ctx) {
        Ok((verdict, note)) => VerifiedClaim {
            subquestion,
            claim: ResearchClaim {
                claim: candidate.claim,
                source_url: candidate.url,
                quote: None,
                verdict,
                note,
                published: source.published,
            },
        },
        Err(err) => VerifiedClaim {
            subquestion,
            claim: ResearchClaim {
                claim: candidate.claim,
                source_url: candidate.url,
                quote: None,
                verdict: Verdict::NoSource,
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
) -> Result<(Verdict, String), crate::error::ReceiptsError> {
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

    Ok(majority(votes))
}

fn judge_once(
    candidate: &ClaimCandidate,
    source_text: &str,
    ctx: &StageContext<'_>,
) -> Result<VerdictOutput, crate::error::ReceiptsError> {
    let prompt = format!(
        "CLAIM: {}\n\nSOURCE TEXT (from {}):\n{}\n\nDoes the source text support the claim? Be strict: SUPPORTED only if the text states it.",
        candidate.claim,
        candidate.url,
        truncate_chars(source_text, 6000)
    );
    chat_json(
        ctx.chat,
        &[Message::user(prompt)],
        ChatOpts {
            temperature: Some(0.1),
            max_completion_tokens: Some(200),
            response_format: Some(response_format()),
            ..ChatOpts::default()
        },
    )
}

fn majority(votes: Vec<VerdictOutput>) -> (Verdict, String) {
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
    let note = votes
        .into_iter()
        .map(|vote| vote.note)
        .collect::<Vec<_>>()
        .join(" | ");
    (verdict, note)
}

fn disabled_claim(candidate: ClaimCandidate, note: &str) -> VerifiedClaim {
    VerifiedClaim {
        subquestion: candidate.subquestion,
        claim: ResearchClaim {
            claim: candidate.claim,
            source_url: candidate.url,
            quote: None,
            verdict: Verdict::NoSource,
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
                    "note": {"type": "string"}
                },
                "required": ["verdict", "note"],
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
            },
            ClaimCandidate {
                subquestion: "sub-b".to_string(),
                claim: "B happened".to_string(),
                url: "https://b.com".to_string(),
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
}
