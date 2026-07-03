use crate::error::ReceiptsError;
use crate::pipeline::{ResearchData, StageContext, Verdict};
use crate::providers::cerebras::{ChatOpts, Message};
use crate::tiers::WORKER_ROUND_WORST_CASE_COST;

/// Synthesizes a prose brief from supported+partial claims. Returns:
/// - `Ok(Some(text))` on success,
/// - `Ok(None)` when the budget gate refused the synthesis chat call (brief
///   skipped: budget),
/// - `Err(_)` on an upstream chat failure.
pub(crate) fn synthesize_brief(
    data: &ResearchData,
    ctx: &StageContext<'_>,
) -> Result<Option<String>, ReceiptsError> {
    let claims = data
        .claims
        .iter()
        .filter(|claim| matches!(claim.verdict, Verdict::Supported | Verdict::Partial))
        .map(|claim| {
            format!(
                "- [{}] {} ({})",
                verdict_label(claim.verdict),
                claim.claim,
                claim.source_url.as_deref().unwrap_or("no source")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if claims.is_empty() {
        return Ok(Some(String::new()));
    }

    // Gate the paid synthesis chat call.
    if !ctx.may_launch(WORKER_ROUND_WORST_CASE_COST)? {
        return Ok(None);
    }

    let response = ctx.chat.chat(
        &[Message::user(format!(
            "Question: {}\n\nSupported and partial claims:\n{}\n\nWrite a concise brief with inline [url] citations and note uncertainties.",
            data.question, claims
        ))],
        ChatOpts {
            temperature: Some(0.3),
            max_completion_tokens: Some(1200),
            ..ChatOpts::default()
        },
    )?;
    Ok(Some(response.content))
}

fn verdict_label(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Supported => "supported",
        Verdict::Partial => "partial",
        Verdict::Unsupported => "unsupported",
        Verdict::NoSource => "no_source",
        Verdict::OffTopic => "off_topic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::Budget;
    use crate::pipeline::Outcome;
    use crate::pipeline::test_support::{FakeSearch, ScriptedChat, test_ctx, text_response};
    use crate::providers::new_spend;

    fn data_with_supported_claim() -> ResearchData {
        ResearchData {
            question: "what is x?".to_string(),
            outcome: Outcome::Answered,
            claims: vec![crate::pipeline::ResearchClaim {
                claim: "X is a thing".to_string(),
                source_url: Some("https://example.com".to_string()),
                quote: None,
                verdict: Verdict::Supported,
                relevance: crate::pipeline::Relevance::Direct,
                note: "ok".to_string(),
                published: None,
            }],
            search_trail: Vec::new(),
            uncertainties: Vec::new(),
        }
    }

    #[test]
    fn budget_refusal_skips_brief_synthesis() {
        // Finding 7: budget refused → Ok(None), no chat call.
        let chat = ScriptedChat::new(vec![text_response("should not be called")]);
        let search = FakeSearch::default();
        let budget = Budget::new(Some(0.0), None);
        let params = crate::pipeline::RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);

        let result = synthesize_brief(&data_with_supported_claim(), &ctx).unwrap();

        assert!(result.is_none());
        assert!(chat.messages.lock().unwrap().is_empty());
    }

    #[test]
    fn brief_synthesizes_when_budget_allows() {
        let chat = ScriptedChat::new(vec![text_response("a brief summary")]);
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = crate::pipeline::RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);

        let result = synthesize_brief(&data_with_supported_claim(), &ctx).unwrap();

        assert_eq!(result.as_deref(), Some("a brief summary"));
        assert_eq!(chat.messages.lock().unwrap().len(), 1);
    }

    #[test]
    fn empty_claims_yields_empty_brief_without_chat() {
        let chat = ScriptedChat::new(Vec::new());
        let search = FakeSearch::default();
        let budget = Budget::new(Some(0.0), None);
        let params = crate::pipeline::RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);
        let data = ResearchData {
            question: "what is x?".to_string(),
            outcome: Outcome::Unanswered,
            claims: Vec::new(),
            search_trail: Vec::new(),
            uncertainties: Vec::new(),
        };

        let result = synthesize_brief(&data, &ctx).unwrap();

        assert_eq!(result.as_deref(), Some(""));
        assert!(chat.messages.lock().unwrap().is_empty());
    }
}
