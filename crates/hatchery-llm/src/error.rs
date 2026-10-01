//! Turning wire errors into the kernel's verdict: retryable or fatal (docs/design/llm.md §6).
//!
//! The classification is the adapter's judgement, and the kernel's information: it does not
//! retry, it reports, so `retryable` is what a frontend's "try again" button is allowed to mean.

use hatchery_kernel::LlmError;
use openai_interface::errors::OapiError;

/// The verdict on one failed wire call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Worth another attempt after a backoff.
    Retry,
    /// Repeating the request would fail identically.
    Fatal(LlmError),
}

/// Classifies a failure that happened while *starting* a request.
///
/// Failures midway through a stream are classified by [`mid_stream`] instead: they are never
/// retried transparently, because the round's partial items are already committed.
#[must_use]
pub fn startup(error: OapiError, env_key: &str) -> Verdict {
    match &error {
        OapiError::SendError(_) | OapiError::ResponseError(_) => Verdict::Retry,
        OapiError::ApiError(api) => match api.status {
            429 => Verdict::Retry,
            status if status >= 500 => Verdict::Retry,
            401 | 403 => Verdict::Fatal(LlmError::fatal(format!(
                "{api}; the key in `{env_key}` was rejected — check the variable and the \
                 provider account"
            ))),
            _ => Verdict::Fatal(LlmError::fatal(api.to_string())),
        },
        // A URL that cannot parse or a header that cannot exist is a config bug, and anything
        // else this build does not model is treated as one too: surprising beats silently
        // hammering a provider with retries.
        _ => Verdict::Fatal(LlmError::fatal(error.to_string())),
    }
}

/// Classifies a failure that happened *after* events have flowed.
///
/// Always an error event, never a silent restart (docs/design/llm.md §6): whether it might have
/// worked on a fresh connection is still recorded, because a "try again" button cares.
#[must_use]
pub fn mid_stream(error: &OapiError) -> LlmError {
    let retryable = matches!(
        error,
        OapiError::SendError(_) | OapiError::ResponseError(_) | OapiError::SseParseError(_)
    ) || matches!(error, OapiError::ApiError(api) if api.status >= 500 || api.status == 429);
    let message = match error {
        OapiError::SendError(cause) => format!("the connection failed mid-stream: {cause}"),
        OapiError::SseParseError(cause) => {
            format!("the provider's stream became unreadable: {cause}")
        }
        other => other.to_string(),
    };
    if retryable {
        LlmError::retryable(message)
    } else {
        LlmError::fatal(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openai_interface::errors::ApiError;

    fn api(status: u16) -> OapiError {
        OapiError::ApiError(ApiError {
            status,
            message: "too many requests".to_owned(),
            error_type: Some("rate_limit".to_owned()),
            code: None,
        })
    }

    #[test]
    fn network_and_5xx_and_429_are_worth_retrying() {
        assert_eq!(startup(api(429), "K"), Verdict::Retry);
        assert_eq!(startup(api(503), "K"), Verdict::Retry);
        assert_eq!(
            startup(OapiError::ResponseError("connection reset".to_owned()), "K"),
            Verdict::Retry,
            "an unreadable response is the network's verdict, not the provider's"
        );
    }

    #[test]
    fn a_401_names_the_environment_variable() {
        let Verdict::Fatal(error) = startup(api(401), "DEEPSEEK_API_KEY") else {
            panic!("401 must be fatal");
        };
        assert!(!error.retryable);
        assert!(
            error.message.contains("DEEPSEEK_API_KEY"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_plain_400_is_fatal_without_the_key_hint() {
        let Verdict::Fatal(error) = startup(api(400), "K") else {
            panic!("400 must be fatal");
        };
        assert!(!error.retryable);
        assert!(!error.message.contains("`K`"), "{}", error.message);
    }

    #[test]
    fn mid_stream_keeps_its_retry_verdict_but_never_restarts() {
        let dropped = mid_stream(&OapiError::SseParseError("eof".to_owned()));
        assert!(dropped.retryable);

        let refused = mid_stream(&api(401));
        assert!(!refused.retryable);
    }
}
