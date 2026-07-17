use serde::Deserialize;

use crate::error::ReceiptsError;
use crate::pipeline::shared::{ChatProvider, chat_json};
use crate::providers::cerebras::{ChatOpts, Message, ResponseFormat};

#[derive(Debug, Deserialize)]
struct DecomposeOutput {
    subquestions: Vec<String>,
}

pub fn decompose(
    question: &str,
    count: usize,
    today: &str,
    chat: &dyn ChatProvider,
) -> Result<Vec<String>, ReceiptsError> {
    let prompt = format!(
        "Research question: {question}\n\nDecompose into {count} focused sub-questions that together answer it. Today is {today}."
    );
    let output: DecomposeOutput = chat_json(
        chat,
        &[Message::user(prompt)],
        ChatOpts {
            max_completion_tokens: Some(400),
            response_format: Some(ResponseFormat::Subquestions),
            ..ChatOpts::default()
        },
    )?;
    Ok(output.subquestions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::test_support::{ScriptedChat, text_response};

    #[test]
    fn parses_subquestions() {
        let chat = ScriptedChat::new(vec![text_response(r#"{"subquestions":["a","b"]}"#)]);

        let got = decompose("q", 2, "2026-07-01", &chat).unwrap();

        assert_eq!(got, vec!["a", "b"]);
        let opts = chat.opts.lock().unwrap();
        assert_eq!(opts[0].max_completion_tokens, Some(400));
        assert!(opts[0].response_format.is_some());
    }

    #[test]
    fn rerolls_on_bad_json() {
        let chat = ScriptedChat::new(vec![
            text_response("not json"),
            text_response(r#"{"subquestions":["fixed"]}"#),
        ]);

        assert_eq!(
            decompose("q", 1, "2026-07-01", &chat).unwrap(),
            vec!["fixed"]
        );
        assert_eq!(chat.messages.lock().unwrap().len(), 2);
    }
}
