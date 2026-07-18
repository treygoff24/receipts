use serde::Deserialize;

use crate::error::ReceiptsError;
use crate::pipeline::shared::{SearchTrailEntry, SourceMeta, StageContext, parse_model_json};
use crate::providers::cerebras::{ChatOpts, Message, ToolCall, ToolDefinition};
use crate::providers::exa::SourceDoc;
use crate::tiers::{WORKER_ROUND_WORST_CASE_COST, WorkerTask};

pub const MAX_ROUNDS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerAnswer {
    pub subquestion: String,
    pub answer: String,
    pub budget_stopped: bool,
}

pub(crate) fn run_worker(
    task: WorkerTask,
    ctx: &StageContext<'_>,
    provider_errors: &mut Vec<ReceiptsError>,
) -> Result<WorkerAnswer, ReceiptsError> {
    let mut messages = vec![
        Message::system(format!(
            "You are a research agent. Use the search tool (multiple queries if needed) to answer the question with specific, dated, sourced facts. When done, answer in plain text citing URLs inline. Today is {}.",
            ctx.today
        )),
        Message::user(task.prompt.clone()),
    ];
    let mut last_text = String::new();

    for _ in 0..MAX_ROUNDS {
        if !ctx.may_launch(WORKER_ROUND_WORST_CASE_COST) {
            return Ok(WorkerAnswer {
                subquestion: task.subquestion,
                answer: answer_with_marker(&last_text, "(budget hit before worker round)"),
                budget_stopped: true,
            });
        }

        let response = ctx.chat.chat(
            &messages,
            ChatOpts {
                temperature: Some(0.3),
                max_completion_tokens: Some(1500),
                tools: Some(vec![ToolDefinition::search()]),
                ..ChatOpts::default()
            },
        )?;
        if !response.content.trim().is_empty() {
            last_text = response.content.clone();
        }
        if response.tool_calls.is_empty() {
            return Ok(WorkerAnswer {
                subquestion: task.subquestion,
                answer: if response.content.trim().is_empty() {
                    "(no answer)".to_string()
                } else {
                    response.content
                },
                budget_stopped: false,
            });
        }

        messages.push(Message::assistant_tool_calls(&response.tool_calls));
        for call in response.tool_calls {
            let content = run_tool_call(&call, ctx, provider_errors);
            messages.push(Message::tool(call.id, content));
        }
    }

    Ok(WorkerAnswer {
        subquestion: task.subquestion,
        answer: answer_with_marker(&last_text, "(round limit hit)"),
        budget_stopped: false,
    })
}

fn run_tool_call(
    call: &ToolCall,
    ctx: &StageContext<'_>,
    provider_errors: &mut Vec<ReceiptsError>,
) -> String {
    if call.function_name != "search" {
        return format!("unsupported tool: {}", call.function_name);
    }

    let query = parse_query(&call.arguments).unwrap_or_default();
    if query.trim().is_empty() {
        return "missing search query".to_string();
    }

    match ctx.search.search(&query) {
        Ok(results) => {
            record_results(&query, &results, ctx);
            format_results(&results)
        }
        Err(err) => {
            record_trail(&query, 0, ctx);
            let message = format!("search failed: {err}");
            provider_errors.push(err);
            message
        }
    }
}

fn parse_query(args: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct SearchArguments {
        query: String,
    }

    parse_model_json::<SearchArguments>(args)
        .ok()
        .map(|arguments| arguments.query)
}

fn record_results(query: &str, results: &[SourceDoc], ctx: &StageContext<'_>) {
    record_trail(query, results.len(), ctx);
    let mut cache = ctx
        .state
        .source_cache
        .lock()
        .expect("source cache lock poisoned");
    let mut meta = ctx
        .state
        .source_meta
        .lock()
        .expect("source metadata lock poisoned");
    for doc in results {
        cache.insert(doc.url.clone(), doc.text.clone());
        meta.insert(
            doc.url.clone(),
            SourceMeta {
                published: doc.published.clone(),
            },
        );
    }
}

fn record_trail(query: &str, results: usize, ctx: &StageContext<'_>) {
    ctx.state
        .search_trail
        .lock()
        .expect("search trail lock poisoned")
        .push(SearchTrailEntry {
            query: query.to_string(),
            results,
        });
}

pub fn format_results(results: &[SourceDoc]) -> String {
    if results.is_empty() {
        return "(no search results)".to_string();
    }

    results
        .iter()
        .map(|doc| {
            format!(
                "URL: {}\nTITLE: {}\nDATE: {}\nTEXT:\n{}",
                doc.url,
                doc.title.as_deref().unwrap_or(""),
                doc.published.as_deref().unwrap_or(""),
                truncate_chars(&doc.text, 2000)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn truncate_chars(text: &str, limit: usize) -> String {
    let mut out: String = text.chars().take(limit).collect();
    if text.chars().count() > limit {
        out.push('…');
    }
    out
}

fn answer_with_marker(last_text: &str, marker: &str) -> String {
    if last_text.trim().is_empty() {
        marker.to_string()
    } else {
        format!("{last_text}\n{marker}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::Budget;
    use crate::pipeline::RunParams;
    use crate::pipeline::test_support::{
        FakeSearch, ScriptedChat, test_ctx, text_response, tool_response,
    };
    use crate::providers::exa::SourceDoc;
    use crate::providers::new_spend;

    #[test]
    fn tool_round_preserves_message_shapes_cache_and_trail() {
        let chat = ScriptedChat::new(vec![
            tool_response("call_1", "maritime law", ""),
            text_response("answer https://example.com"),
        ]);
        let search = FakeSearch::default();
        search.search_results.lock().unwrap().insert(
            "maritime law".to_string(),
            vec![SourceDoc {
                url: "https://example.com".to_string(),
                title: Some("Example".to_string()),
                published: Some("2026-07-01".to_string()),
                text: "full source text".to_string(),
            }],
        );
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 2, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);

        let answer = run_worker(
            WorkerTask {
                subquestion: "subq".to_string(),
                prompt: "subq".to_string(),
                refinement: false,
            },
            &ctx,
            &mut Vec::new(),
        )
        .unwrap();

        assert_eq!(answer.answer, "answer https://example.com");
        let histories = chat.messages.lock().unwrap();
        let second = &histories[1];
        assert!(matches!(&second[2], Message::Assistant { tool_calls } if tool_calls.len() == 1));
        assert!(matches!(
            &second[3],
            Message::Tool {
                tool_call_id,
                content,
            } if tool_call_id == "call_1" && content.contains("URL: https://example.com")
        ));
        assert_eq!(
            ctx.state
                .source_cache
                .lock()
                .unwrap()
                .get("https://example.com")
                .unwrap(),
            "full source text"
        );
        assert_eq!(
            ctx.state.search_trail.lock().unwrap()[0],
            SearchTrailEntry {
                query: "maritime law".to_string(),
                results: 1,
            }
        );
    }

    #[test]
    fn round_limit_returns_last_text_with_marker() {
        let chat = ScriptedChat::new(
            (0..MAX_ROUNDS)
                .map(|idx| tool_response(&format!("call_{idx}"), "q", "draft"))
                .collect(),
        );
        let search = FakeSearch::default();
        let budget = Budget::new(None, None);
        let params = RunParams::new("2026-07-01", 1, new_spend());
        let ctx = test_ctx(&chat, &search, &budget, &params);

        let answer = run_worker(
            WorkerTask {
                subquestion: "subq".to_string(),
                prompt: "subq".to_string(),
                refinement: false,
            },
            &ctx,
            &mut Vec::new(),
        )
        .unwrap();

        assert_eq!(answer.answer, "draft\n(round limit hit)");
        assert_eq!(chat.messages.lock().unwrap().len(), MAX_ROUNDS);
    }
}
