use crate::error::ReconError;
use crate::pipeline::{ChatProvider, ResearchData, Verdict};
use crate::providers::cerebras::{ChatOpts, Message};

pub fn synthesize_brief(
    data: &ResearchData,
    chat: &dyn ChatProvider,
) -> Result<String, ReconError> {
    let claims = data
        .claims
        .iter()
        .filter(|claim| matches!(claim.verdict, Verdict::Supported | Verdict::Partial))
        .map(|claim| {
            format!(
                "- [{}] {} ({})",
                verdict_label(claim.verdict),
                claim.claim,
                claim.source_url
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    if claims.is_empty() {
        return Ok(String::new());
    }

    let response = chat.chat(
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
    Ok(response.content)
}

fn verdict_label(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Supported => "supported",
        Verdict::Partial => "partial",
        Verdict::Unsupported => "unsupported",
        Verdict::NoSource => "no_source",
    }
}
