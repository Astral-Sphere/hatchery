//! Message content and provider signature blocks.

use serde::{Deserialize, Serialize};

/// The body of a user or assistant message: text plus any attachments.
///
/// A struct rather than an enum because both halves coexist — a user can type a question and
/// attach a screenshot — and because new part kinds must not change the shape of existing text
/// messages.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Content {
    /// The text. Empty when the message is purely attachments.
    pub text: String,
    /// Attachments, in the order the user supplied them. Absent from the wire form when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<ContentPart>,
}

impl Content {
    /// Text-only content.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            parts: Vec::new(),
        }
    }

    /// True when there is neither text nor attachments.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.parts.is_empty()
    }

    /// The text, but only when nothing else rides along.
    ///
    /// Callers that must flatten content for a provider request use this to detect the common
    /// case; anything else needs a real translation step.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        self.parts.is_empty().then_some(self.text.as_str())
    }

    /// Appends an attachment.
    pub fn push_part(&mut self, part: ContentPart) {
        self.parts.push(part);
    }
}

impl From<String> for Content {
    fn from(text: String) -> Self {
        Self::text(text)
    }
}

impl From<&str> for Content {
    fn from(text: &str) -> Self {
        Self::text(text)
    }
}

/// One non-text part of a message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    /// An inline image. `data` is base64 so the wire form stays JSON.
    Image {
        /// MIME type, e.g. `image/png`.
        mime_type: String,
        /// Base64-encoded bytes.
        data: String,
    },
    /// A resource the user attached by reference: a file, a URL, a snippet.
    Resource {
        /// Where the resource lives. A path or a URL, whatever the user attached.
        uri: String,
        /// MIME type when known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        /// Text inlined at attach time, when the frontend could read it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
}

/// An opaque provider signature that must be replayed byte-exactly.
///
/// Three ecosystems produce these and none of them documents the contents: Anthropic's
/// `signature`, OpenAI Responses' `encrypted_content` and Gemini's `thoughtSignature`. ADR-0007's
/// rule is that whatever the provider emitted is stored and sent back verbatim — no trimming, no
/// normalising, no re-encoding — because the provider uses it to validate (and cache) the
/// reasoning it produced.
///
/// `scheme` names the mechanism rather than the model, and is a `String` so that adding a fourth
/// ecosystem needs no protocol change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureBlock {
    /// Which mechanism produced the block; see the associated constants.
    pub scheme: String,
    /// The opaque payload, verbatim.
    pub data: String,
}

impl SignatureBlock {
    /// Anthropic's thinking-block signature.
    pub const ANTHROPIC_SIGNATURE: &'static str = "anthropic-signature";
    /// OpenAI Responses' encrypted reasoning content.
    pub const OPENAI_ENCRYPTED_CONTENT: &'static str = "openai-encrypted-content";
    /// Gemini's thought signature.
    pub const GEMINI_THOUGHT_SIGNATURE: &'static str = "gemini-thought-signature";

    /// Pairs a scheme with its payload.
    #[must_use]
    pub fn new(scheme: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            scheme: scheme.into(),
            data: data.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_only_content_has_a_minimal_wire_form() {
        let content = Content::text("hello");
        assert_eq!(
            serde_json::to_string(&content).expect("serialize"),
            r#"{"text":"hello"}"#,
            "an absent parts list must not appear as `\"parts\":[]`"
        );
        assert_eq!(content.as_text(), Some("hello"));
    }

    #[test]
    fn attachments_survive_a_roundtrip() {
        let mut content = Content::text("look");
        content.push_part(ContentPart::Image {
            mime_type: "image/png".to_owned(),
            data: "aGk=".to_owned(),
        });
        content.push_part(ContentPart::Resource {
            uri: "/tmp/notes.md".to_owned(),
            mime_type: Some("text/markdown".to_owned()),
            text: None,
        });

        let json = serde_json::to_string(&content).expect("serialize");
        assert!(json.contains(r#""type":"image""#), "{json}");
        assert!(json.contains(r#""type":"resource""#), "{json}");
        assert!(!json.contains("mime_type\":null"), "{json}");
        assert_eq!(
            serde_json::from_str::<Content>(&json).expect("deserialize"),
            content
        );
    }

    #[test]
    fn content_with_parts_has_no_single_text_form() {
        let mut content = Content::text("look");
        content.push_part(ContentPart::Resource {
            uri: "x".to_owned(),
            mime_type: None,
            text: None,
        });
        assert_eq!(content.as_text(), None);
        assert!(!content.is_empty());
        assert!(Content::default().is_empty());
    }

    #[test]
    fn signature_blocks_keep_their_payload_byte_for_byte() {
        // Trailing newline, leading spaces and a unicode escape sequence: exactly the shapes that
        // a well-meaning `trim()` would destroy (ADR-0007).
        let payload = " \n\t𝔰𝔦𝔤\\u00e9 \"quoted\" \n";
        let block = SignatureBlock::new(SignatureBlock::OPENAI_ENCRYPTED_CONTENT, payload);
        let json = serde_json::to_string(&block).expect("serialize");
        let back: SignatureBlock = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.data, payload);
        assert_eq!(back.scheme, "openai-encrypted-content");
    }
}
