//! JSON-RPC 2.0 frames and the newline-delimited transport codec.
//!
//! The daemon and every frontend speak the same frames over UDS or stdio
//! (`docs/design/protocol.md` §1): one JSON value per line, `\n` terminated. No gRPC, no
//! length-prefixing — a human can `nc` the socket and read it.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::Value;

/// The only version this crate speaks, spelled the way the spec spells it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum JsonRpcVersion {
    /// JSON-RPC 2.0.
    #[default]
    #[serde(rename = "2.0")]
    V2,
}

/// A request or response id: a number or a string, per the spec.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    /// Numeric id.
    Number(i64),
    /// String id.
    String(String),
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(number) => write!(f, "{number}"),
            Self::String(text) => f.write_str(text),
        }
    }
}

impl From<i64> for Id {
    fn from(number: i64) -> Self {
        Self::Number(number)
    }
}

impl From<String> for Id {
    fn from(text: String) -> Self {
        Self::String(text)
    }
}

impl From<&str> for Id {
    fn from(text: &str) -> Self {
        Self::String(text.to_owned())
    }
}

/// A call that expects a response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// Correlation id, echoed back in the [`Response`].
    pub id: Id,
    /// Method name; see [`crate::method`].
    pub method: String,
    /// Parameters. Absent for methods that take none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    /// A request with no parameters.
    #[must_use]
    pub fn new(id: impl Into<Id>, method: impl Into<String>) -> Self {
        Self {
            jsonrpc: JsonRpcVersion::V2,
            id: id.into(),
            method: method.into(),
            params: None,
        }
    }

    /// A request whose parameters are the struct's fields.
    ///
    /// # Errors
    ///
    /// Fails only if the parameter type is not representable as JSON, which no type in this crate
    /// is.
    pub fn with_params<T: Serialize>(mut self, params: &T) -> Result<Self, FrameError> {
        self.params = Some(serde_json::to_value(params).map_err(FrameError::from)?);
        Ok(self)
    }

    /// Decodes the parameters into a typed struct.
    ///
    /// # Errors
    ///
    /// [`FrameError`] when the parameters are missing or do not fit `T` — a malformed call must
    /// be answered with `InvalidParams`, not guessed at.
    pub fn params_as<T: DeserializeOwned>(&self) -> Result<T, FrameError> {
        let params = self
            .params
            .as_ref()
            .ok_or_else(|| FrameError::MissingParams(self.method.clone()))?;
        T::deserialize(params).map_err(FrameError::from)
    }
}

/// A call that does not expect a response (a hint, a cancellation, a notification).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// Method name.
    pub method: String,
    /// Parameters. Absent for methods that take none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    /// A notification with no parameters.
    #[must_use]
    pub fn new(method: impl Into<String>) -> Self {
        Self {
            jsonrpc: JsonRpcVersion::V2,
            method: method.into(),
            params: None,
        }
    }

    /// A notification whose parameters are the struct's fields.
    ///
    /// # Errors
    ///
    /// See [`Request::with_params`].
    pub fn with_params<T: Serialize>(mut self, params: &T) -> Result<Self, FrameError> {
        self.params = Some(serde_json::to_value(params).map_err(FrameError::from)?);
        Ok(self)
    }
}

/// The answer to a [`Request`]: exactly one of `result` or `error`.
///
/// A `null` result is indistinguishable from an absent one after deserialization. Every hatchery
/// method returns a JSON object (an empty one when there is nothing to say), so the ambiguity is
/// not reachable on our own wire; [`Response::is_ok`] therefore keys off `error`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// The id of the request being answered.
    pub id: Id,
    /// Success payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Failure payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::ErrorObject>,
}

impl Response {
    /// A successful response.
    pub fn ok<T: Serialize>(id: impl Into<Id>, result: &T) -> Result<Self, FrameError> {
        Ok(Self {
            jsonrpc: JsonRpcVersion::V2,
            id: id.into(),
            result: Some(serde_json::to_value(result).map_err(FrameError::from)?),
            error: None,
        })
    }

    /// A failed response.
    #[must_use]
    pub fn err(id: impl Into<Id>, error: crate::ErrorObject) -> Self {
        Self {
            jsonrpc: JsonRpcVersion::V2,
            id: id.into(),
            result: None,
            error: Some(error),
        }
    }

    /// True when the response is not an error.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.error.is_none()
    }

    /// Decodes the success payload into a typed struct.
    ///
    /// # Errors
    ///
    /// [`FrameError`] when the response carries an error or a payload that does not fit `T`.
    pub fn result_as<T: DeserializeOwned>(&self) -> Result<T, FrameError> {
        if let Some(error) = &self.error {
            return Err(FrameError::Remote(error.clone()));
        }
        let absent = Value::Null;
        let result = self.result.as_ref().unwrap_or(&absent);
        T::deserialize(result).map_err(FrameError::from)
    }
}

/// Anything that can arrive on the wire.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    /// A call expecting a response.
    Request(Request),
    /// A call expecting none.
    Notification(Notification),
    /// An answer to something we sent.
    Response(Response),
}

impl Incoming {
    /// True when the frame expects an answer.
    #[must_use]
    pub const fn expects_response(&self) -> bool {
        matches!(self, Self::Request(_))
    }

    /// The method name, for requests and notifications.
    #[must_use]
    pub fn method(&self) -> Option<&str> {
        match self {
            Self::Request(request) => Some(&request.method),
            Self::Notification(notification) => Some(&notification.method),
            Self::Response(_) => None,
        }
    }
}

/// What went wrong while framing or decoding.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum FrameError {
    /// A frame contained no JSON at all.
    #[error("empty frame")]
    Empty,
    /// The line was not a JSON object.
    #[error("frame is not a JSON object")]
    NotAnObject,
    /// The frame carried neither a method nor an id.
    #[error("frame has neither a method nor an id")]
    Unclassifiable,
    /// The frame carried an explicit `"id": null`, which cannot be correlated with anything.
    ///
    /// Kept apart from [`Self::Unclassifiable`]: a null id on a frame that *does* name a method
    /// is a request somebody meant to have answered, and the message names the method so the
    /// faulty client can be found.
    #[error("frame has an explicit null id (method: {})", method.as_deref().unwrap_or("none"))]
    NullId {
        /// The method the frame named, if it named one.
        method: Option<String>,
    },
    /// A request needed parameters and had none.
    #[error("method {0} requires parameters")]
    MissingParams(String),
    /// A single frame exceeded [`FrameDecoder::MAX_FRAME_BYTES`].
    #[error("frame exceeds the {} byte limit", FrameDecoder::MAX_FRAME_BYTES)]
    TooLong,
    /// The bytes were not valid UTF-8.
    #[error("frame is not valid UTF-8")]
    NotUtf8,
    /// The daemon answered with an error.
    #[error("{0}")]
    Remote(crate::ErrorObject),
    /// The JSON did not fit the type.
    #[error("malformed frame: {0}")]
    Malformed(String),
}

impl From<serde_json::Error> for FrameError {
    fn from(error: serde_json::Error) -> Self {
        Self::Malformed(error.to_string())
    }
}

/// Classifies a JSON value into a request, a notification or a response.
///
/// `"id": null` is not an id here. JSON-RPC 2.0 lets a peer answer a request it could not
/// identify with a null-id error response; this build reports such a frame as
/// [`FrameError::NullId`] rather than inventing a correlation for it, because every hatchery id
/// is minted by the caller before the request is written — so a null id is junk, not a reply we
/// lost. Interop with a spec-literal foreign agent is the ACP bridge's job (M3).
///
/// The null is told apart from an *absent* id on purpose. Reading both as "no id" would classify
/// `{"id":null,"method":"session/prompt"}` as a notification: a request silently downgraded to a
/// call nobody answers, with its caller waiting forever. That frame is a client bug — a missing
/// `skip_serializing_if` on an `Option<Id>` produces exactly it — and a bug that hangs the caller
/// has to be reported, not absorbed.
///
/// # Errors
///
/// [`FrameError`] when the value is not a JSON-RPC frame at all — including a wrong or missing
/// `jsonrpc` field, which is a real bug when a client skips it.
pub fn classify(value: Value) -> Result<Incoming, FrameError> {
    if !value.is_object() {
        return Err(FrameError::NotAnObject);
    }

    // The version field is typed, so a missing or wrong one fails here rather than silently
    // producing a frame that no peer will accept. Borrowing keeps the payload out of a clone:
    // a `result` can be a whole session.
    let Probe {
        jsonrpc,
        method,
        id,
        params,
        result,
        error,
    } = Probe::deserialize(&value).map_err(FrameError::from)?;

    let id = match id {
        Some(None) => return Err(FrameError::NullId { method }),
        Some(Some(id)) => Some(id),
        None => None,
    };

    match (method, id) {
        (Some(method), Some(id)) => Ok(Incoming::Request(Request {
            jsonrpc,
            id,
            method,
            params,
        })),
        (Some(method), None) => Ok(Incoming::Notification(Notification {
            jsonrpc,
            method,
            params,
        })),
        (None, Some(id)) => Ok(Incoming::Response(Response {
            jsonrpc,
            id,
            result,
            error,
        })),
        (None, None) => Err(FrameError::Unclassifiable),
    }
}

/// One line of the transport, decoded.
///
/// # Errors
///
/// [`FrameError`] for an empty line or malformed JSON.
pub fn decode_frame(line: &str) -> Result<Incoming, FrameError> {
    let line = line.trim_end_matches(['\n', '\r']);
    if line.is_empty() {
        return Err(FrameError::Empty);
    }
    classify(serde_json::from_str(line).map_err(FrameError::from)?)
}

/// A frame with its newline delimiter, ready to write to the transport.
///
/// # Errors
///
/// [`FrameError`] only for values JSON cannot represent; no type in this crate is one.
pub fn encode_frame<T: Serialize>(value: &T) -> Result<String, FrameError> {
    let mut line = serde_json::to_string(value).map_err(FrameError::from)?;
    line.push('\n');
    Ok(line)
}

/// The shape used to tell the three frame kinds apart before committing to one.
///
/// Carries every field of all three kinds, so the value is walked once and nothing is cloned: the
/// arms of [`classify`] move the payload out of the probe into the frame they build.
#[derive(Deserialize)]
struct Probe {
    jsonrpc: JsonRpcVersion,
    #[serde(default)]
    method: Option<String>,
    /// Absent (`None`), explicitly null (`Some(None)`) or a value (`Some(Some(_))`).
    #[serde(default, deserialize_with = "absent_null_or_id")]
    id: Option<Option<Id>>,
    #[serde(default)]
    params: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<crate::ErrorObject>,
}

/// Reads an `id` the way [`classify`] needs it: absent, explicit null and a value are three
/// different facts, and serde's own `Option<Id>` collapses the first two into `None`.
fn absent_null_or_id<'de, D>(deserializer: D) -> Result<Option<Option<Id>>, D::Error>
where
    D: Deserializer<'de>,
{
    struct IdProbe;

    impl<'de> serde::de::Visitor<'de> for IdProbe {
        type Value = Option<Option<Id>>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a number, a string, or null")
        }

        fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Some(None))
        }

        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(Some(None))
        }

        fn visit_some<D: Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            Id::deserialize(deserializer).map(|id| Some(Some(id)))
        }
    }

    deserializer.deserialize_option(IdProbe)
}

/// Accumulates bytes and yields complete lines.
///
/// The transport is a stream: a read can end mid-frame, or hand over six frames at once. Decoding
/// happens per *line*, never per chunk, so a multi-byte character split across two reads is not a
/// decoding error — the bytes are simply not decoded until the line is complete.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
    /// How many leading bytes of `buffer` are already known to hold no newline. A frame that
    /// arrives in many chunks is scanned once instead of from byte 0 on every read.
    scanned: usize,
}

impl FrameDecoder {
    /// Largest frame accepted. Tool output is spilled to disk rather than inlined
    /// (`ToolOutput::spilled`), so a frame has no legitimate reason to be huge; the limit keeps a
    /// runaway writer from exhausting memory.
    pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

    /// Feeds a chunk in and returns every line completed by it.
    ///
    /// Blank lines are skipped: a stray newline from a terminal or a logging middleware is noise,
    /// not a protocol violation.
    ///
    /// # Errors
    ///
    /// [`FrameError::TooLong`] when a frame exceeds the limit, terminated or not, or
    /// [`FrameError::NotUtf8`] when a completed line is not valid UTF-8. Both drop the offending
    /// bytes, so the next `push` starts on a clean buffer rather than failing forever.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, FrameError> {
        self.buffer.extend_from_slice(chunk);
        let mut lines = Vec::new();
        let mut consumed = 0;
        let mut scanned = self.scanned;

        while let Some(offset) = self.buffer[scanned..]
            .iter()
            .position(|byte| *byte == b'\n')
        {
            let end = scanned + offset;
            let raw = &self.buffer[consumed..end];
            consumed = end + 1;
            scanned = end + 1;
            if raw.is_empty() {
                continue;
            }
            if raw.len() > Self::MAX_FRAME_BYTES {
                self.buffer.clear();
                self.scanned = 0;
                return Err(FrameError::TooLong);
            }
            // Draining *before* reporting is what keeps the decoder usable: the alternative
            // leaves the bad line buffered and every later push fails on the same bytes.
            let Ok(line) = std::str::from_utf8(raw) else {
                self.buffer.drain(..consumed);
                self.scanned = 0;
                return Err(FrameError::NotUtf8);
            };
            lines.push(line.to_owned());
        }

        if consumed > 0 {
            self.buffer.drain(..consumed);
        }
        if self.buffer.len() > Self::MAX_FRAME_BYTES {
            self.buffer.clear();
            self.scanned = 0;
            return Err(FrameError::TooLong);
        }
        // Everything still buffered was scanned to the end without a newline.
        self.scanned = self.buffer.len();
        Ok(lines)
    }

    /// Bytes held for an incomplete frame.
    #[must_use]
    pub fn buffered_bytes(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ErrorCode, method};

    #[test]
    fn a_request_roundtrips_and_classifies_as_a_request() {
        let request = Request::new(1_i64, method::SESSION_PROMPT)
            .with_params(&serde_json::json!({"session_id": "s"}))
            .expect("params are representable");
        let frame = encode_frame(&request).expect("encode");
        assert!(frame.ends_with('\n'), "frames are newline delimited");

        let incoming = decode_frame(&frame).expect("decode");
        assert!(incoming.expects_response());
        assert_eq!(incoming.method(), Some("session/prompt"));
        match incoming {
            Incoming::Request(back) => {
                assert_eq!(back, request);
                assert_eq!(back.id, Id::Number(1));
            }
            other => panic!("expected a request, got {other:?}"),
        }
    }

    #[test]
    fn a_frame_without_an_id_is_a_notification() {
        let notification = Notification::new("session/cancel");
        let incoming = decode_frame(&encode_frame(&notification).expect("encode")).expect("decode");
        assert!(!incoming.expects_response());
        assert_eq!(incoming, Incoming::Notification(notification));
    }

    #[test]
    fn a_response_carries_either_a_result_or_an_error() {
        let ok = Response::ok("abc", &serde_json::json!({"turn": "t"})).expect("encode result");
        assert!(ok.is_ok());
        assert_eq!(
            ok.result_as::<Value>().expect("typed result"),
            serde_json::json!({"turn": "t"})
        );
        let incoming = decode_frame(&encode_frame(&ok).expect("encode")).expect("decode");
        match incoming {
            Incoming::Response(back) => {
                assert_eq!(back.id, Id::String("abc".to_owned()));
                assert!(back.error.is_none());
            }
            other => panic!("expected a response, got {other:?}"),
        }

        let failed = Response::err(
            1_i64,
            crate::ErrorObject::new(ErrorCode::SessionNotFound, "no such session"),
        );
        assert!(!failed.is_ok());
        let error = failed
            .result_as::<Value>()
            .expect_err("a failed response must not yield a result");
        assert!(matches!(error, FrameError::Remote(_)));
    }

    #[test]
    fn wrong_or_missing_version_is_rejected() {
        let missing = serde_json::json!({"id": 1, "method": "session/new"});
        assert!(matches!(classify(missing), Err(FrameError::Malformed(_))));
        let wrong = serde_json::json!({"jsonrpc": "1.0", "id": 1, "method": "session/new"});
        assert!(matches!(classify(wrong), Err(FrameError::Malformed(_))));
    }

    #[test]
    fn non_frames_are_rejected() {
        assert_eq!(
            classify(serde_json::json!([1, 2])),
            Err(FrameError::NotAnObject)
        );
        assert_eq!(
            classify(serde_json::json!({"jsonrpc": "2.0"})),
            Err(FrameError::Unclassifiable)
        );
        assert_eq!(decode_frame(""), Err(FrameError::Empty));
        assert_eq!(decode_frame("\n"), Err(FrameError::Empty));
        assert!(matches!(
            decode_frame("{not json"),
            Err(FrameError::Malformed(_))
        ));
    }

    #[test]
    fn an_absent_id_and_a_null_id_are_not_the_same_frame() {
        // The null is a client bug with a hang behind it: `Option<Id>` serialized without
        // `skip_serializing_if` produces this frame, and reading it as a notification would leave
        // the caller waiting for an answer that nothing is going to send.
        assert_eq!(
            classify(serde_json::json!({
                "jsonrpc": "2.0", "id": null, "method": "session/prompt"
            })),
            Err(FrameError::NullId {
                method: Some("session/prompt".to_owned())
            }),
            "the message must name the method so the faulty client can be found"
        );
        assert_eq!(
            classify(serde_json::json!({"jsonrpc": "2.0", "id": null, "result": {}})),
            Err(FrameError::NullId { method: None }),
            "and a null-id reply is refused the same way"
        );

        // Absent stays a notification: that is the one spelling the spec gives for "no answer".
        let notification = classify(serde_json::json!({
            "jsonrpc": "2.0", "method": "session/cancel"
        }))
        .expect("a notification");
        assert_eq!(notification.method(), Some("session/cancel"));
        assert!(!notification.expects_response());
    }

    #[test]
    fn typed_params_fail_loudly_instead_of_defaulting() {
        let request = Request::new(1_i64, method::SESSION_CANCEL);
        assert!(matches!(
            request.params_as::<Value>(),
            Err(FrameError::MissingParams(name)) if name == "session/cancel"
        ));

        let wrong_type = Request::new(1_i64, method::SESSION_CANCEL)
            .with_params(&serde_json::json!({"session_id": 5}))
            .expect("encode");
        assert!(matches!(
            wrong_type.params_as::<crate::method::SessionCancelParams>(),
            Err(FrameError::Malformed(_))
        ));
    }

    #[test]
    fn the_decoder_reassembles_a_frame_split_across_chunks() {
        let request = Request::new(7_i64, method::DAEMON_HELLO);
        let frame = encode_frame(&request).expect("encode");
        let bytes = frame.as_bytes();

        let mut decoder = FrameDecoder::default();
        let mut lines = Vec::new();
        for byte in bytes {
            lines.extend(decoder.push(&[*byte]).expect("byte-wise push"));
        }
        assert_eq!(lines.len(), 1);
        assert_eq!(
            decode_frame(&lines[0]).expect("decode"),
            Incoming::Request(request)
        );
        assert_eq!(decoder.buffered_bytes(), 0);
    }

    #[test]
    fn the_decoder_survives_a_multibyte_character_split_across_chunks() {
        let request = Request::new(1_i64, "session/prompt")
            .with_params(&serde_json::json!({"text": "日本語のテキスト"}))
            .expect("encode");
        let bytes = encode_frame(&request).expect("encode").into_bytes();
        let split = bytes.len() / 2;

        let mut decoder = FrameDecoder::default();
        let mut lines = decoder.push(&bytes[..split]).expect("first half");
        lines.extend(decoder.push(&bytes[split..]).expect("second half"));
        assert_eq!(lines.len(), 1);
        assert_eq!(
            decode_frame(&lines[0]).expect("decode"),
            Incoming::Request(request)
        );
    }

    #[test]
    fn the_decoder_yields_several_frames_from_one_chunk_and_skips_blank_lines() {
        let mut payload =
            encode_frame(&Request::new(1_i64, method::SESSION_CANCEL)).expect("encode");
        payload.push('\n');
        payload.push_str(&encode_frame(&Notification::new("x/y")).expect("encode"));
        payload
            .push_str(&encode_frame(&Request::new(2_i64, method::SESSION_CANCEL)).expect("encode"));

        let mut decoder = FrameDecoder::default();
        let lines = decoder.push(payload.as_bytes()).expect("push");
        assert_eq!(
            lines.len(),
            3,
            "two requests plus one notification: {lines:?}"
        );
        assert_eq!(
            lines[0],
            r#"{"jsonrpc":"2.0","id":1,"method":"session/cancel"}"#
        );
    }

    #[test]
    fn a_runaway_frame_is_rejected_rather_than_buffered_forever() {
        let mut decoder = FrameDecoder::default();
        let error = decoder
            .push(&vec![b'a'; FrameDecoder::MAX_FRAME_BYTES + 1])
            .expect_err("an unterminated frame past the limit must fail");
        assert_eq!(error, FrameError::TooLong);
        assert_eq!(decoder.buffered_bytes(), 0, "the buffer is released");
    }

    #[test]
    fn the_limit_also_applies_to_a_frame_that_arrives_terminated() {
        // A terminator must not buy a frame its way past the limit: the check has to see complete
        // lines too, not only the unterminated leftover.
        let mut decoder = FrameDecoder::default();
        let mut chunk = vec![b'a'; FrameDecoder::MAX_FRAME_BYTES + 1];
        chunk.push(b'\n');
        assert_eq!(decoder.push(&chunk), Err(FrameError::TooLong));
        assert_eq!(decoder.buffered_bytes(), 0, "the buffer is released");

        let lines = decoder
            .push(
                encode_frame(&Request::new(1_i64, method::DAEMON_HELLO))
                    .expect("encode")
                    .as_bytes(),
            )
            .expect("the decoder stays usable after refusing an oversized frame");
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn invalid_utf8_in_a_complete_line_is_reported() {
        let mut decoder = FrameDecoder::default();
        let mut bytes = vec![0xff, 0xfe];
        bytes.push(b'\n');
        assert_eq!(decoder.push(&bytes), Err(FrameError::NotUtf8));
    }

    #[test]
    fn the_decoder_recovers_from_an_invalid_utf8_line() {
        // Reporting the error is half of it; the other half is that the bad bytes are gone. A
        // decoder that kept them would fail every later push on the same line, forever.
        let mut decoder = FrameDecoder::default();
        assert_eq!(decoder.push(&[0xff, 0xfe, b'\n']), Err(FrameError::NotUtf8));
        assert_eq!(
            decoder.buffered_bytes(),
            0,
            "the offending line must be dropped rather than re-read"
        );

        let frame = encode_frame(&Request::new(1_i64, method::DAEMON_HELLO)).expect("encode");
        let lines = decoder
            .push(frame.as_bytes())
            .expect("the next push must work");
        assert_eq!(lines.len(), 1);
        assert!(matches!(
            decode_frame(&lines[0]).expect("decode"),
            Incoming::Request(_)
        ));
    }

    #[test]
    fn a_frame_trailing_an_invalid_utf8_line_still_arrives() {
        let mut decoder = FrameDecoder::default();
        let good = encode_frame(&Notification::new("x/y")).expect("encode");
        let mut chunk = vec![0xff, b'\n'];
        chunk.extend_from_slice(good.as_bytes());

        assert_eq!(decoder.push(&chunk), Err(FrameError::NotUtf8));
        let lines = decoder
            .push(&[])
            .expect("only the bad line is discarded, not the whole read");
        assert_eq!(lines, vec![good.trim_end_matches('\n')]);
    }

    #[test]
    fn the_lines_do_not_depend_on_how_the_transport_chunked_them() {
        let mut stream = String::new();
        stream
            .push_str(&encode_frame(&Request::new(1_i64, method::SESSION_CANCEL)).expect("encode"));
        stream.push_str(&encode_frame(&Notification::new("x/y")).expect("encode"));
        stream.push_str(
            &encode_frame(
                &Request::new(2_i64, method::SESSION_PROMPT)
                    .with_params(&serde_json::json!({"text": "日本語"}))
                    .expect("encode"),
            )
            .expect("encode"),
        );

        let mut whole = FrameDecoder::default();
        let expected = whole.push(stream.as_bytes()).expect("one push");
        assert_eq!(expected.len(), 3);

        for size in [1, 2, 3, 7, 64, 4096] {
            let mut decoder = FrameDecoder::default();
            let mut lines = Vec::new();
            for part in stream.as_bytes().chunks(size) {
                lines.extend(decoder.push(part).expect("chunked push"));
            }
            assert_eq!(
                lines, expected,
                "chunking by {size} bytes changed the frames"
            );
            assert_eq!(decoder.buffered_bytes(), 0);
        }
    }

    #[test]
    fn a_long_frame_in_many_chunks_is_scanned_once_not_from_the_start_every_time() {
        // The bound is generous: scanning 2 MiB linearly costs milliseconds even with 65k pushes.
        // What it rules out is rescanning the buffered prefix on every push, which is quadratic in
        // the chunk count and would take tens of seconds here.
        let mut stream = vec![b'a'; 2 * 1024 * 1024];
        stream.push(b'\n');

        let started = std::time::Instant::now();
        let mut decoder = FrameDecoder::default();
        let mut lines = Vec::new();
        for part in stream.chunks(32) {
            lines.extend(decoder.push(part).expect("push"));
        }
        let elapsed = started.elapsed();

        assert_eq!(lines.len(), 1);
        assert_eq!(decoder.buffered_bytes(), 0);
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "decoding took {elapsed:?}; a partial frame must not be rescanned from byte 0"
        );
    }
}
