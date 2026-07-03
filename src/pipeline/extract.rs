use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::json;

use crate::pipeline::worker::WorkerAnswer;
use crate::pipeline::{ChatProvider, ClaimCandidate, Relevance, ResearchClaim, Verdict, chat_json};
use crate::providers::cerebras::{ChatOpts, Message};
use crate::tiers::MAX_CLAIMS_PER_WORKER;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ExtractedClaim {
    pub claim: String,
    pub url: String,
}

#[derive(Debug, Deserialize)]
struct ClaimsOutput {
    claims: Vec<ExtractedClaim>,
}

pub fn extract_claims(
    subquestion: &str,
    answer: &str,
    chat: &dyn ChatProvider,
) -> Vec<ExtractedClaim> {
    let prompt = format!(
        "Question: {subquestion}\n\nResearch answer:\n{answer}\n\nExtract the atomic factual claims with their source URLs (only claims that cite a URL). At most {MAX_CLAIMS_PER_WORKER} claims."
    );
    let parsed: Result<ClaimsOutput, _> = chat_json(
        chat,
        &[Message::user(prompt)],
        ChatOpts {
            max_completion_tokens: Some(2500),
            response_format: Some(response_format()),
            ..ChatOpts::default()
        },
    );
    parsed.map(|output| output.claims).unwrap_or_default()
}

pub(crate) fn extract_candidates(
    answer: WorkerAnswer,
    chat: &dyn ChatProvider,
) -> Vec<ClaimCandidate> {
    extract_claims(&answer.subquestion, &answer.answer, chat)
        .into_iter()
        .map(|claim| ClaimCandidate {
            subquestion: answer.subquestion.clone(),
            claim: claim.claim,
            url: claim.url,
            relevance: Relevance::Direct,
        })
        .collect()
}

pub(crate) fn dedup_candidates(candidates: Vec<ClaimCandidate>) -> Vec<ClaimCandidate> {
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|candidate| !candidate.claim.trim().is_empty())
        .filter(|candidate| {
            seen.insert((
                normalize_claim(&candidate.claim),
                candidate.url.trim().to_string(),
            ))
        })
        .collect()
}

/// Deduplicates research claims by (normalized claim, url). When keys collide,
/// keeps the claim with the better verdict (rank: supported > partial >
/// unsupported > no_source); on equal rank keeps the first. This matters for
/// deep-tier refinement, whose claims are appended AFTER the initial claims —
/// a refined `supported` duplicate must win over an initial `unsupported`.
pub(crate) fn dedup_research_claims(claims: Vec<ResearchClaim>) -> Vec<ResearchClaim> {
    let mut best: HashMap<(String, Option<String>), ResearchClaim> = HashMap::new();
    let mut order: Vec<(String, Option<String>)> = Vec::new();
    for claim in claims {
        if claim.claim.trim().is_empty() {
            continue;
        }
        let key = (normalize_claim(&claim.claim), claim.source_url.clone());
        match best.get(&key) {
            None => {
                order.push(key.clone());
                best.insert(key, claim);
            }
            Some(existing) => {
                if verdict_rank(&claim.verdict) > verdict_rank(&existing.verdict) {
                    best.insert(key, claim);
                }
            }
        }
    }
    order
        .into_iter()
        .map(|key| best.remove(&key).expect("key present"))
        .collect()
}

fn verdict_rank(verdict: &Verdict) -> u8 {
    match verdict {
        Verdict::Supported => 4,
        Verdict::Partial => 3,
        Verdict::Unsupported => 2,
        Verdict::NoSource => 1,
        Verdict::OffTopic => 0,
    }
}

fn normalize_claim(claim: &str) -> String {
    claim.trim().to_lowercase()
}

fn response_format() -> serde_json::Value {
    json!({
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::test_support::{ScriptedChat, text_response};

    #[test]
    fn extracts_happy_path() {
        let chat = ScriptedChat::new(vec![text_response(
            r#"{"claims":[{"claim":"A happened","url":"https://example.com"}]}"#,
        )]);

        let claims = extract_claims("q", "a", &chat);

        assert_eq!(
            claims,
            vec![ExtractedClaim {
                claim: "A happened".to_string(),
                url: "https://example.com".to_string(),
            }]
        );
    }

    #[test]
    fn bad_json_rerolls_to_good_json() {
        let chat = ScriptedChat::new(vec![
            text_response("not json"),
            text_response(r#"{"claims":[{"claim":"A","url":""}]}"#),
        ]);

        assert_eq!(extract_claims("q", "a", &chat).len(), 1);
        assert_eq!(chat.messages.lock().unwrap().len(), 2);
    }

    #[test]
    fn double_failure_returns_empty_claims() {
        let chat = ScriptedChat::new(vec![text_response("bad"), text_response("still bad")]);

        assert!(extract_claims("q", "a", &chat).is_empty());
    }

    #[test]
    fn dedups_by_normalized_claim_and_url() {
        let claims = dedup_candidates(vec![
            ClaimCandidate {
                subquestion: "q1".to_string(),
                claim: " A ".to_string(),
                url: "https://example.com".to_string(),
                relevance: Relevance::Direct,
            },
            ClaimCandidate {
                subquestion: "q2".to_string(),
                claim: "a".to_string(),
                url: "https://example.com".to_string(),
                relevance: Relevance::Direct,
            },
            ClaimCandidate {
                subquestion: "q3".to_string(),
                claim: "a".to_string(),
                url: "https://other.com".to_string(),
                relevance: Relevance::Direct,
            },
        ]);

        assert_eq!(claims.len(), 2);
    }

    #[test]
    fn dedup_research_claims_keeps_better_verdict_on_collision() {
        // Finding 3: unsupported-then-supported duplicate dedups to supported.
        let claims = vec![
            research_claim("A happened", "https://example.com", Verdict::Unsupported),
            research_claim("A happened", "https://example.com", Verdict::Supported),
        ];

        let deduped = dedup_research_claims(claims);

        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].verdict, Verdict::Supported);
    }

    #[test]
    fn dedup_research_claims_keeps_first_on_equal_rank() {
        let claims = vec![
            research_claim("A happened", "https://example.com", Verdict::Supported),
            research_claim("A happened", "https://example.com", Verdict::Supported),
        ];

        let deduped = dedup_research_claims(claims);

        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].note, "first");
    }

    #[test]
    fn dedup_research_claims_preserves_insertion_order() {
        let claims = vec![
            research_claim("B happened", "https://b.com", Verdict::Supported),
            research_claim("A happened", "https://a.com", Verdict::Supported),
            research_claim("C happened", "https://c.com", Verdict::Supported),
        ];

        let deduped = dedup_research_claims(claims);

        assert_eq!(deduped.len(), 3);
        assert_eq!(deduped[0].claim, "B happened");
        assert_eq!(deduped[1].claim, "A happened");
        assert_eq!(deduped[2].claim, "C happened");
    }

    #[test]
    fn dedup_research_claims_prefers_verified_over_off_topic_duplicate() {
        let claims = vec![
            research_claim("A happened", "https://example.com", Verdict::OffTopic),
            research_claim("A happened", "https://example.com", Verdict::Unsupported),
        ];

        let deduped = dedup_research_claims(claims);

        assert_eq!(deduped.len(), 1);
        assert_eq!(deduped[0].verdict, Verdict::Unsupported);
    }

    fn research_claim(claim: &str, url: &str, verdict: Verdict) -> ResearchClaim {
        ResearchClaim {
            claim: claim.to_string(),
            source_url: Some(url.to_string()),
            quote: None,
            verdict,
            relevance: Relevance::Direct,
            note: if verdict == Verdict::Supported && claim.starts_with('A') {
                "first".to_string()
            } else {
                "note".to_string()
            },
            published: None,
        }
    }
}
