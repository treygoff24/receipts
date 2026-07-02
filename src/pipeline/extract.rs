use std::collections::HashSet;

use serde::Deserialize;
use serde_json::json;

use crate::pipeline::worker::WorkerAnswer;
use crate::pipeline::{ChatProvider, ClaimCandidate, ResearchClaim, chat_json};
use crate::providers::cerebras::{ChatOpts, Message};

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
        "Question: {subquestion}\n\nResearch answer:\n{answer}\n\nExtract the atomic factual claims with their source URLs (only claims that cite a URL). At most 15 claims."
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

pub(crate) fn dedup_research_claims(claims: Vec<ResearchClaim>) -> Vec<ResearchClaim> {
    let mut seen = HashSet::new();
    claims
        .into_iter()
        .filter(|claim| seen.insert((normalize_claim(&claim.claim), claim.source_url.clone())))
        .collect()
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
            },
            ClaimCandidate {
                subquestion: "q2".to_string(),
                claim: "a".to_string(),
                url: "https://example.com".to_string(),
            },
            ClaimCandidate {
                subquestion: "q3".to_string(),
                claim: "a".to_string(),
                url: "https://other.com".to_string(),
            },
        ]);

        assert_eq!(claims.len(), 2);
    }
}
