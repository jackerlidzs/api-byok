//! Defines the provider interface and exports provider implementations.
mod anthropic;
mod attempt;
mod event;
pub(crate) mod keys;
mod normalize;
mod openai_chat;
mod openai_responses;
mod recorder;
mod request_template;
mod router;

use std::pin::Pin;

use futures_util::Stream;
use tokio_util::sync::CancellationToken;

use crate::{model::ModelInvocation, Result};

pub use anthropic::AnthropicProvider;
pub use event::*;
pub use openai_chat::OpenAiChatProvider;
pub use openai_responses::OpenAiResponsesProvider;
pub use recorder::CallRecorder;
pub(crate) use router::custom_headers;

pub(crate) fn native_usage(
    kind: crate::model::ProviderType,
    value: &serde_json::Value,
) -> crate::model::Usage {
    match kind {
        crate::model::ProviderType::OpenAiChat => openai_chat::openai_usage(value),
        crate::model::ProviderType::OpenAiResponses => openai_responses::responses_usage(value),
        crate::model::ProviderType::Anthropic => anthropic::anthropic_usage(value),
        crate::model::ProviderType::Plugin => {
            unreachable!("plugin calls have no native HTTP protocol")
        }
    }
}
pub use router::{build as build_provider, ProviderRouter};

pub type ProviderStream = Pin<Box<dyn Stream<Item = Result<ModelEvent>> + Send>>;

pub trait Provider: Send + Sync {
    fn stream(
        &self,
        invocation: ModelInvocation,
        cancellation: CancellationToken,
    ) -> ProviderStream;
}

fn map_sse_error(
    label: &str,
    error: eventsource_stream::EventStreamError<crate::Error>,
) -> crate::Error {
    match error {
        eventsource_stream::EventStreamError::Transport(error) => error,
        eventsource_stream::EventStreamError::Utf8(error) => {
            crate::Error::Provider(format!("{label} SSE UTF-8 error: {error}"))
        }
        eventsource_stream::EventStreamError::Parser(error) => {
            crate::Error::Provider(format!("{label} SSE parse error: {error}"))
        }
    }
}

fn provider_event_error(label: &str, value: &serde_json::Value) -> Option<crate::Error> {
    let kind = value.get("type").and_then(serde_json::Value::as_str);
    let direct_error = value.get("error").filter(|error| !error.is_null());
    if !matches!(kind, Some("error" | "response.failed")) && direct_error.is_none() {
        return None;
    }

    let message = value
        .get("message")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            value
                .pointer("/error/message")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            value
                .pointer("/response/error/message")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| direct_error.and_then(serde_json::Value::as_str))
        .or_else(|| {
            value
                .pointer("/response/error")
                .and_then(serde_json::Value::as_str)
        })
        .unwrap_or("provider returned an error event without a message");

    let status = value
        .pointer("/error/type")
        .or_else(|| value.pointer("/response/error/code"))
        .and_then(serde_json::Value::as_str)
        .and_then(|kind| error_type_status(kind).map(|status| (status, kind)));
    Some(crate::Error::Provider(match status {
        Some((status, kind)) => format!("{label} error {status} ({kind}): {message}"),
        None => format!("{label} error: {message}"),
    }))
}

/// The HTTP status of an error type sent inside a stream, so the run treats it like the same
/// error returned as a status line: a rejected request stops the run instead of being retried
/// eight times (llm.wiro.ai rejects requests this way with HTTP 200).
fn error_type_status(kind: &str) -> Option<u16> {
    match kind {
        "invalid_request_error" => Some(400),
        "authentication_error" => Some(401),
        "permission_error" => Some(403),
        "not_found_error" => Some(404),
        "request_too_large" => Some(413),
        _ => None,
    }
}

/// Ending of the error a provider adapter raises when the model declined to answer.
const REFUSAL: &str = "declined to answer (stop_reason: refusal)";

/// Reports a refusal as an error: passing it on as an ordinary stop shows the user an empty or
/// cut-off reply without saying why.
fn refusal_error(label: &str) -> crate::Error {
    crate::Error::Provider(format!("{label} {REFUSAL}"))
}

/// The same request is likely to be declined again, so runs stop instead of retrying it.
pub(crate) fn is_refusal(message: &str) -> bool {
    message.ends_with(REFUSAL)
}

/// Removes U+0000 from every string in a request body. Tool output from binary files carries
/// NUL characters, and llm.wiro.ai rejects them with "messages[N].content[M].content must be
/// valid bounded text", which then fails every later turn of the conversation.
pub(crate) fn strip_nul_characters(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) if text.contains('\0') => text.retain(|ch| ch != '\0'),
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_nul_characters),
        serde_json::Value::Object(fields) => fields.values_mut().for_each(strip_nul_characters),
        _ => {}
    }
}

fn merge_extra_params(body: &mut serde_json::Value, extra: &serde_json::Value) -> Result<()> {
    let extra = extra
        .as_object()
        .ok_or_else(|| crate::Error::Config("model extra params must be an object".into()))?;
    let body = body
        .as_object_mut()
        .ok_or_else(|| crate::Error::Provider("provider request body must be an object".into()))?;
    for (name, value) in extra {
        if matches!(
            name.as_str(),
            "model"
                | "stream"
                | "messages"
                | "input"
                | "tools"
                | "system"
                | "instructions"
                | "prompt_cache_key"
        ) {
            return Err(crate::Error::Config(format!(
                "model extra params cannot replace {name}"
            )));
        }
        body.insert(name.clone(), value.clone());
    }
    Ok(())
}

fn apply_body_allowlist(
    body: &mut serde_json::Value,
    allowed: Option<&std::collections::HashSet<String>>,
) -> Result<()> {
    let Some(allowed) = allowed else {
        return Ok(());
    };
    body.as_object_mut()
        .ok_or_else(|| crate::Error::Provider("provider request body must be an object".into()))?
        .retain(|name, _| allowed.contains(name));
    Ok(())
}

fn apply_openai_prompt_cache_key(body: &mut serde_json::Value, model_id: &str) -> Result<()> {
    if !model_id.to_ascii_lowercase().contains("gpt") {
        return Ok(());
    }
    body.as_object_mut()
        .ok_or_else(|| crate::Error::Provider("provider request body must be an object".into()))?
        .insert(
            "prompt_cache_key".into(),
            serde_json::Value::String("cursor-byok".into()),
        );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_transport_errors_are_not_relabelled_as_parse_errors() {
        let error = map_sse_error(
            "test provider",
            eventsource_stream::EventStreamError::Transport(crate::Error::Provider(
                "connection closed".into(),
            )),
        );

        let crate::Error::Provider(message) = error else {
            panic!("transport error category must be preserved");
        };
        assert_eq!(message, "connection closed");
    }

    #[test]
    fn provider_error_events_extract_flat_and_nested_messages() {
        assert_provider_error(
            "OpenAI Responses",
            serde_json::json!({
                "type": "error",
                "message": "Internal error during token generation"
            }),
            "OpenAI Responses error: Internal error during token generation",
        );
        assert_provider_error(
            "OpenAI Chat",
            serde_json::json!({
                "error": {"message": "quota exceeded", "type": "server_error"}
            }),
            "OpenAI Chat error: quota exceeded",
        );
        assert_provider_error(
            "Anthropic",
            serde_json::json!({
                "type": "error",
                "error": {"type": "overloaded_error", "message": "Overloaded"}
            }),
            "Anthropic error: Overloaded",
        );
        assert_provider_error(
            "OpenAI Responses",
            serde_json::json!({
                "type": "response.failed",
                "response": {"error": {"message": "generation failed"}}
            }),
            "OpenAI Responses error: generation failed",
        );
    }

    #[test]
    fn request_errors_inside_a_stream_carry_their_http_status() {
        assert_provider_error(
            "Anthropic",
            serde_json::json!({
                "type": "error",
                "error": {
                    "type": "invalid_request_error",
                    "message": "The model provider rejected this request."
                }
            }),
            "Anthropic error 400 (invalid_request_error): The model provider rejected this request.",
        );
        assert_provider_error(
            "OpenAI Chat",
            serde_json::json!({
                "error": {"type": "authentication_error", "message": "invalid key"}
            }),
            "OpenAI Chat error 401 (authentication_error): invalid key",
        );
    }

    #[test]
    fn successful_provider_events_are_not_errors() {
        assert!(provider_event_error(
            "OpenAI Responses",
            &serde_json::json!({"type": "response.completed", "error": null})
        )
        .is_none());
    }

    #[test]
    fn nul_characters_are_removed_from_every_string() {
        let mut body = serde_json::json!({
            "system": "rules\u{0}",
            "messages": [{
                "role": "user",
                "content": [{"type": "tool_result", "content": "bin\u{0}ary\u{0}"}]
            }],
            "max_tokens": 1
        });
        strip_nul_characters(&mut body);
        assert_eq!(body["system"], "rules");
        assert_eq!(body["messages"][0]["content"][0]["content"], "binary");
        assert_eq!(body["max_tokens"], 1);
        assert!(!body.to_string().contains("\\u0000"));
    }

    fn assert_provider_error(label: &str, value: serde_json::Value, expected: &str) {
        let Some(crate::Error::Provider(message)) = provider_event_error(label, &value) else {
            panic!("expected provider error");
        };
        assert_eq!(message, expected);
    }
}
