//! Error codes and the two error shapes on the wire.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC error code.
///
/// The number behind each variant is part of the protocol: clients branch on it, so a released
/// number must never change meaning. `error_codes_are_stable` pins the table.
///
/// Two shapes use this type. A JSON-RPC **response** carries the number
/// ([`ErrorObject`]), because that is what the spec mandates and an unknown number from a newer
/// daemon must survive. An in-event error carries the readable name
/// ([`EventError`]), because those land in golden fixtures and logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    // -- JSON-RPC 2.0 reserved (spec §5.1) --
    /// `-32700`: the frame was not valid JSON.
    ParseError,
    /// `-32600`: valid JSON, but not a JSON-RPC request.
    InvalidRequest,
    /// `-32601`: no such method.
    MethodNotFound,
    /// `-32602`: the method exists, the parameters do not fit.
    InvalidParams,
    /// `-32603`: an error inside the daemon that has no better code.
    InternalError,

    // -- hatchery application codes (server-error range, -32000..-32099) --
    /// `-32000`: no session with that id.
    SessionNotFound,
    /// `-32001`: the client's generation does not match the session's (invariant 1: a stale
    /// runtime or frontend must not act on a session it no longer owns).
    GenerationMismatch,
    /// `-32002`: a turn is already running for this session.
    TurnInProgress,
    /// `-32003`: the user refused the action.
    ApprovalDenied,
    /// `-32004`: nobody answered the approval request in time (fail-closed).
    ApprovalTimedOut,
    /// `-32005`: the storage layer failed.
    StoreError,
    /// `-32006`: the provider call failed. Whether it can be retried is in
    /// `EventError::retryable`.
    LlmError,
    /// `-32007`: configuration is unusable for this operation.
    ConfigError,
    /// `-32008`: the client's protocol version is outside the supported range.
    UnsupportedProtocolVersion,
}

impl ErrorCode {
    /// Every code, in declaration order.
    pub const ALL: &'static [Self] = &[
        Self::ParseError,
        Self::InvalidRequest,
        Self::MethodNotFound,
        Self::InvalidParams,
        Self::InternalError,
        Self::SessionNotFound,
        Self::GenerationMismatch,
        Self::TurnInProgress,
        Self::ApprovalDenied,
        Self::ApprovalTimedOut,
        Self::StoreError,
        Self::LlmError,
        Self::ConfigError,
        Self::UnsupportedProtocolVersion,
    ];

    /// The numeric code sent in a JSON-RPC error object.
    #[must_use]
    pub const fn as_i64(self) -> i64 {
        match self {
            Self::ParseError => -32700,
            Self::InvalidRequest => -32600,
            Self::MethodNotFound => -32601,
            Self::InvalidParams => -32602,
            Self::InternalError => -32603,
            Self::SessionNotFound => -32000,
            Self::GenerationMismatch => -32001,
            Self::TurnInProgress => -32002,
            Self::ApprovalDenied => -32003,
            Self::ApprovalTimedOut => -32004,
            Self::StoreError => -32005,
            Self::LlmError => -32006,
            Self::ConfigError => -32007,
            Self::UnsupportedProtocolVersion => -32008,
        }
    }

    /// The readable name used inside events, in `snake_case`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ParseError => "parse_error",
            Self::InvalidRequest => "invalid_request",
            Self::MethodNotFound => "method_not_found",
            Self::InvalidParams => "invalid_params",
            Self::InternalError => "internal_error",
            Self::SessionNotFound => "session_not_found",
            Self::GenerationMismatch => "generation_mismatch",
            Self::TurnInProgress => "turn_in_progress",
            Self::ApprovalDenied => "approval_denied",
            Self::ApprovalTimedOut => "approval_timed_out",
            Self::StoreError => "store_error",
            Self::LlmError => "llm_error",
            Self::ConfigError => "config_error",
            Self::UnsupportedProtocolVersion => "unsupported_protocol_version",
        }
    }

    /// Looks up a code by number. `None` for a code this build does not know, which happens
    /// legitimately when a newer daemon answers an older client.
    #[must_use]
    pub fn from_i64(code: i64) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|known| known.as_i64() == code)
    }

    /// Parses the readable name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|known| known.as_str() == name)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ErrorCode {
    type Err = UnknownErrorCode;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::from_name(name).ok_or_else(|| UnknownErrorCode(name.to_owned()))
    }
}

/// Returned when an error name is not one this build knows.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unknown error code {0:?}")]
pub struct UnknownErrorCode(String);

/// A JSON-RPC error object, as it appears inside a response frame.
///
/// The code stays an `i64` here on purpose: a client talking to a newer daemon must be able to
/// report an error it cannot name, rather than fail to parse the response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorObject {
    /// Numeric code; see [`ErrorCode::as_i64`].
    pub code: i64,
    /// Human-readable description. Not for parsing.
    pub message: String,
    /// Structured detail, when the code has some.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl fmt::Display for ErrorObject {
    /// Renders as `code message`, with the readable name when the code is known — so a log line
    /// reads `session_not_found (-32000): no such session` instead of a bare number.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.error_code() {
            Some(code) => write!(f, "{} ({}): {}", code.as_str(), self.code, self.message),
            None => write!(f, "{}: {}", self.code, self.message),
        }
    }
}

impl ErrorObject {
    /// An error object for a known code.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code.as_i64(),
            message: message.into(),
            data: None,
        }
    }

    /// Attaches structured detail.
    #[must_use]
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// The known code, when this build recognises the number.
    #[must_use]
    pub fn error_code(&self) -> Option<ErrorCode> {
        ErrorCode::from_i64(self.code)
    }
}

/// An error reported inside a server event, e.g. `TurnFailed`.
///
/// Carries the readable code and, for provider failures, whether retrying could help (ADR-0007:
/// the daemon retries retryable failures itself and emits `RateLimited`; the frontend only
/// displays).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventError {
    /// What went wrong.
    pub code: ErrorCode,
    /// Human-readable description.
    pub message: String,
    /// Set for provider failures: `Some(true)` when a retry may succeed, `Some(false)` when it
    /// will not, `None` when retrying is not a meaningful question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

impl EventError {
    /// An error with no retry verdict.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: None,
        }
    }

    /// A provider failure, marking whether a retry could succeed.
    #[must_use]
    pub fn llm(message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: ErrorCode::LlmError,
            message: message.into(),
            retryable: Some(retryable),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes_are_stable_and_unique() {
        // IETF/JSON-RPC reserved values, then our own range. Changing any of these is a
        // protocol break: clients branch on the number.
        assert_eq!(ErrorCode::ParseError.as_i64(), -32700);
        assert_eq!(ErrorCode::InvalidRequest.as_i64(), -32600);
        assert_eq!(ErrorCode::MethodNotFound.as_i64(), -32601);
        assert_eq!(ErrorCode::InvalidParams.as_i64(), -32602);
        assert_eq!(ErrorCode::InternalError.as_i64(), -32603);
        assert_eq!(ErrorCode::SessionNotFound.as_i64(), -32000);
        assert_eq!(ErrorCode::UnsupportedProtocolVersion.as_i64(), -32008);

        let numbers: Vec<i64> = ErrorCode::ALL.iter().map(|c| c.as_i64()).collect();
        let mut unique = numbers.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(numbers.len(), unique.len(), "duplicate error codes");

        for code in ErrorCode::ALL {
            assert!(
                code.as_i64() <= -32000,
                "{} is outside the JSON-RPC error range",
                code.as_str()
            );
            let json = serde_json::to_string(code).expect("serialize");
            assert_eq!(json, format!("\"{}\"", code.as_str()));
            assert_eq!(ErrorCode::from_i64(code.as_i64()), Some(*code));
            assert_eq!(ErrorCode::from_name(code.as_str()), Some(*code));
        }
    }

    #[test]
    fn an_unknown_number_survives_the_round_trip() {
        let object = ErrorObject {
            code: -31999,
            message: "from a newer daemon".to_owned(),
            data: None,
        };
        assert_eq!(
            object.error_code(),
            None,
            "unknown codes are not guessed at"
        );
        let json = serde_json::to_string(&object).expect("serialize");
        let back: ErrorObject = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.code, -31999);
    }

    #[test]
    fn a_known_error_object_carries_its_number() {
        let object = ErrorObject::new(ErrorCode::SessionNotFound, "no such session")
            .with_data(serde_json::json!({"session_id": "x"}));
        assert_eq!(object.code, -32000);
        assert_eq!(object.error_code(), Some(ErrorCode::SessionNotFound));
        assert!(object.data.is_some());
    }

    #[test]
    fn an_event_error_only_mentions_retryability_when_it_is_a_question() {
        let plain = EventError::new(ErrorCode::StoreError, "disk on fire");
        assert_eq!(
            serde_json::to_string(&plain).expect("serialize"),
            r#"{"code":"store_error","message":"disk on fire"}"#
        );
        let llm = EventError::llm("429 after 4 retries", true);
        assert_eq!(llm.retryable, Some(true));
        let json = serde_json::to_string(&llm).expect("serialize");
        assert!(json.contains(r#""retryable":true"#), "{json}");
    }
}
