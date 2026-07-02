use std::collections::HashMap;

use serde::Deserialize;
use serde_json::json;

use crate::pipeline::{
    ClaimCandidate, ResearchClaim, StageContext, Verdict, chat_json, run_chunked,
};
use crate::providers::cerebras::{ChatOpts, Message};
use crate::tiers::VERIFICATION_WORST_CASE_COST;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyPolicy {
    Adaptive,
    Paranoid,
    Off,
}

#[derive(Debug, Deserialize)]
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
) -> Vec<ResearchClaim> {
    if policy == VerifyPolicy::Off {
        return candidates
            .into_iter()
            .map(|candidate| disabled_claim(candidate, "verification disabled"))
            .collect();
    }

    let mut out = Vec::new();
    let mut iter = candidates.into_iter().peekable();
    while iter.peek().is_some() {
        let mut batch = Vec::new();
        while batch.len() < ctx.max_concurrency {
            let Some(candidate) = iter.next() else { break };
            match ctx.may_launch(VERIFICATION_WORST_CASE_COST) {
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

pub(crate) fn verify_claim(
    candidate: ClaimCandidate,
    policy: VerifyPolicy,
    ctx: &StageContext<'_>,
) -> ResearchClaim {
    if policy == VerifyPolicy::Off {
        return disabled_claim(candidate, "verification disabled");
    }

    let Some(source) = source_for_claim(&candidate, ctx) else {
        return disabled_claim(candidate, "no source text available");
    };

    match judge(&candidate, &source.text, policy, ctx) {
        Ok((verdict, note)) => ResearchClaim {
            claim: candidate.claim,
            source_url: candidate.url,
            quote: None,
            verdict,
            note,
            published: source.published,
        },
        Err(err) => ResearchClaim {
            claim: candidate.claim,
            source_url: candidate.url,
            quote: None,
            verdict: Verdict::NoSource,
            note: format!("verification failed: {err}"),
            published: source.published,
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
) -> Result<(Verdict, String), crate::error::ReconError> {
    let first = judge_once(candidate, source_text, ctx)?;
    let mut votes = vec![first];

    let needs_more = match policy {
        VerifyPolicy::Adaptive => votes[0].verdict == Verdict::Partial,
        VerifyPolicy::Paranoid => true,
        VerifyPolicy::Off => false,
    };
    if needs_more {
        votes.push(judge_once(candidate, source_text, ctx)?);
        votes.push(judge_once(candidate, source_text, ctx)?);
    }

    Ok(majority(votes))
}

fn judge_once(
    candidate: &ClaimCandidate,
    source_text: &str,
    ctx: &StageContext<'_>,
) -> Result<VerdictOutput, crate::error::ReconError> {
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

fn disabled_claim(candidate: ClaimCandidate, note: &str) -> ResearchClaim {
    ResearchClaim {
        claim: candidate.claim,
        source_url: candidate.url,
        quote: None,
        verdict: Verdict::NoSource,
        note: note.to_string(),
        published: None,
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

        assert_eq!(claim.verdict, Verdict::Supported);
        assert_eq!(claim.published.as_deref(), Some("2026-07-01"));
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

        assert_eq!(claim.verdict, Verdict::Supported);
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

        assert_eq!(claim.verdict, Verdict::Supported);
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

        assert_eq!(claim.verdict, Verdict::NoSource);
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

        assert_eq!(claim.verdict, Verdict::Supported);
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

        assert_eq!(claim.verdict, Verdict::Unsupported);
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

        assert_eq!(claim.verdict, Verdict::NoSource);
        assert_eq!(claim.note, "verification disabled");
        assert!(chat.messages.lock().unwrap().is_empty());
    }
}
