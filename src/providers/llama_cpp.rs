//! LlamaCpp provider — calls a running `llama-server` `/completion` endpoint.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::providers::client::ProviderClient;
use crate::providers::http_error::HttpProviderErrorClassifier;
use crate::providers::types::{
    ProviderError, ProviderErrorKind, ProviderRequest, ProviderResponse, StructuredOutput,
};

/// GBNF grammar that constrains llama.cpp output to a valid JSON object.
///
/// Uses the object-root form because all role outputs are JSON objects.
/// Reference: <https://github.com/ggerganov/llama.cpp/blob/master/grammars/json.gbnf>
///
/// `root` duplicates `object`'s body rather than delegating to it, because
/// `object` (like every other rule here) ends in the reference grammar's
/// recursive `ws`, which is unbounded and has nothing mandatory after it
/// when reached from `root`: a grammar-constrained model can keep sampling
/// whitespace forever after a complete top-level object, running every call
/// to `n_predict` instead of stopping. `object` keeps its trailing `ws`
/// because it also appears as a nested `value`, where the surrounding `,`
/// or closing bracket still bounds it. `root` has no such bound, so it ends
/// at the closing brace with no further grammar-legal tokens.
const JSON_GBNF: &str = r#"root   ::=
  "{" ws (
            string ":" ws value
    ("," ws string ":" ws value)*
  )? "}"
value  ::= object | array | string | number | ("true" | "false" | "null") ws

object ::=
  "{" ws (
            string ":" ws value
    ("," ws string ":" ws value)*
  )? "}" ws

array  ::=
  "[" ws (
            value
    ("," ws value)*
  )? "]" ws

string ::=
  "\"" (
    [^\\"\x7F\x00-\x1F] |
    "\\" (["\\/bfnrt] | "u" [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F] [0-9a-fA-F])
  )* "\"" ws

number ::= ("-"? ([0-9] | [1-9] [0-9]*)) ("." [0-9]+)? (([eE] [-+]? [0-9]+))? ws

ws ::= ([ \t\n] ws)?"#;

/// Calls the llama.cpp server completion API at the given base URL.
pub struct LlamaCppProvider {
    base_url: String,
    agent: ureq::Agent,
}

impl LlamaCppProvider {
    /// Create a new provider targeting `base_url` with a per-request `timeout_secs` deadline.
    pub fn new(base_url: impl Into<String>, timeout_secs: u64) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(timeout_secs))
            .build();
        Self {
            base_url: base_url.into(),
            agent,
        }
    }
}

#[derive(Serialize)]
struct CompletionRequest {
    prompt: String,
    n_predict: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    grammar: Option<String>,
}

#[derive(Deserialize)]
struct CompletionResponse {
    content: Option<String>,
}

/// Resolve the GBNF grammar string to send for a given structured-output
/// request, falling back to [`JSON_GBNF`] for generic JSON mode.
fn resolve_grammar(output_schema: Option<&StructuredOutput>) -> Option<String> {
    output_schema.map(|schema| match schema {
        StructuredOutput::Json => JSON_GBNF.to_string(),
        StructuredOutput::Grammar(grammar) => grammar.clone(),
    })
}

fn map_completion_response(r: CompletionResponse) -> Result<ProviderResponse, ProviderError> {
    match r.content {
        Some(content) => Ok(ProviderResponse {
            content,
            finish_reason: None,
        }),
        None => Err(ProviderError {
            kind: ProviderErrorKind::Terminal,
            message: "content field missing from llama-server reply".to_string(),
        }),
    }
}

impl ProviderClient for LlamaCppProvider {
    fn call(&self, request: ProviderRequest) -> Result<ProviderResponse, ProviderError> {
        let url = format!("{}/completion", self.base_url);
        let grammar = resolve_grammar(request.output_schema.as_ref());
        let body = CompletionRequest {
            prompt: request.prompt,
            n_predict: request.max_tokens,
            grammar,
        };

        let http_response = self
            .agent
            .post(&url)
            .send_json(&body)
            .map_err(|err| match err {
                ureq::Error::Status(status, _) => ProviderError {
                    kind: HttpProviderErrorClassifier::classify_status(status),
                    message: format!("HTTP {status}"),
                },
                ureq::Error::Transport(transport_err) => ProviderError {
                    kind: HttpProviderErrorClassifier::classify_transport(&transport_err),
                    message: format!("connection error: {transport_err}"),
                },
            })?;

        let parsed: CompletionResponse =
            http_response.into_json().map_err(|err| ProviderError {
                kind: ProviderErrorKind::Terminal,
                message: format!("invalid JSON response: {err}"),
            })?;

        map_completion_response(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llama_cpp_response_parses_content() {
        let json = r#"{"content":"hello world"}"#;
        let parsed: CompletionResponse = serde_json::from_str(json).unwrap();
        let result = map_completion_response(parsed).unwrap();
        assert_eq!(result.content, "hello world");
        assert_eq!(result.finish_reason, None);
    }

    #[test]
    fn llama_cpp_response_missing_content_is_terminal() {
        let json = r#"{"other_field":"value"}"#;
        let parsed: CompletionResponse = serde_json::from_str(json).unwrap();
        let err = map_completion_response(parsed).unwrap_err();
        assert_eq!(err.kind, ProviderErrorKind::Terminal);
    }

    #[test]
    fn llama_cpp_classify_status() {
        let cases = [
            (429, ProviderErrorKind::Retryable),
            (500, ProviderErrorKind::Retryable),
            (503, ProviderErrorKind::Retryable),
            (404, ProviderErrorKind::Terminal),
            (400, ProviderErrorKind::Terminal),
        ];
        for (status, expected) in cases {
            assert_eq!(
                HttpProviderErrorClassifier::classify_status(status),
                expected,
                "status {status}"
            );
        }
    }

    // Regression test for a runaway-generation bug: `root` used to delegate
    // to `object`, whose trailing `ws` is unbounded and, at the top level,
    // had nothing mandatory after it — a grammar-constrained model could
    // keep sampling whitespace forever after a complete object, running
    // every call to n_predict. `root` must end at the closing brace.
    #[test]
    fn json_gbnf_rejects_trailing_whitespace_after_closing_brace() {
        use crate::roles::policy::gbnf_check::Grammar;

        let grammar = Grammar::parse(JSON_GBNF);
        assert!(grammar.accepts(r#"{"a":1,"b":[1,2,{"c":true}]}"#));
        assert!(!grammar.accepts("{\"a\":1} \n"));
    }

    #[test]
    fn llama_provider_resolve_grammar() {
        let cases = [
            (Some(StructuredOutput::Json), Some(JSON_GBNF.to_string())),
            (None, None),
            (
                Some(StructuredOutput::Grammar("root ::= \"x\"".to_string())),
                Some("root ::= \"x\"".to_string()),
            ),
        ];
        for (output_schema, expected) in cases {
            let req = ProviderRequest {
                prompt: "test".to_string(),
                max_tokens: 512,
                output_schema,
            };
            let grammar = resolve_grammar(req.output_schema.as_ref());
            assert_eq!(grammar, expected);
        }
    }

    #[test]
    fn completion_request_grammar_serialization() {
        let cases = [(None, false), (Some(JSON_GBNF.to_string()), true)];
        for (grammar, expect_present) in cases {
            let body = CompletionRequest {
                prompt: "test".to_string(),
                n_predict: 128,
                grammar,
            };
            let json = serde_json::to_string(&body).unwrap();
            assert_eq!(json.contains("\"grammar\""), expect_present);
        }
    }

    #[test]
    fn is_timeout_source_classification() {
        let timed_out = std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out");
        let refused = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let timed_out_boxed: Box<dyn std::error::Error + 'static> = Box::new(timed_out);
        let refused_boxed: Box<dyn std::error::Error + 'static> = Box::new(refused);

        assert!(HttpProviderErrorClassifier::is_timeout_source(Some(
            timed_out_boxed.as_ref()
        )));
        assert!(!HttpProviderErrorClassifier::is_timeout_source(Some(
            refused_boxed.as_ref()
        )));
        assert!(!HttpProviderErrorClassifier::is_timeout_source(None));
    }
}
