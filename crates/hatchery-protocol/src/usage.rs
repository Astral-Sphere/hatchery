//! Token accounting and how a turn ended.

use std::fmt;

use serde::{Deserialize, Serialize};

/// What a turn or a single provider round cost.
///
/// Every token field is optional because providers omit them unevenly — some report a completion
/// count but no reasoning breakdown, gateways sometimes report nothing at all. "Unknown" and
/// "zero" are different facts and the UI shows them differently.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Tokens in the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    /// Tokens the model generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    /// The subset of `completion_tokens` spent on reasoning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    /// How many provider requests this accounts for; a turn with tool calls makes several.
    #[serde(default)]
    pub requests: u32,
}

impl Usage {
    /// True when nothing was reported at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.prompt_tokens.is_none()
            && self.completion_tokens.is_none()
            && self.reasoning_tokens.is_none()
            && self.requests == 0
    }

    /// Folds another measurement in.
    ///
    /// Counts add; a field that is unknown on either side stays unknown only if it is unknown on
    /// both — one provider reporting a count is still information.
    pub fn merge(&mut self, other: &Self) {
        self.prompt_tokens = add_optional(self.prompt_tokens, other.prompt_tokens);
        self.completion_tokens = add_optional(self.completion_tokens, other.completion_tokens);
        self.reasoning_tokens = add_optional(self.reasoning_tokens, other.reasoning_tokens);
        self.requests = self.requests.saturating_add(other.requests);
    }

    /// This usage plus `other`, as a new value.
    #[must_use]
    pub fn merged(mut self, other: &Self) -> Self {
        self.merge(other);
        self
    }
}

fn add_optional(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.saturating_add(right)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

/// Why a turn ended normally.
///
/// Failures are not a stop reason: a failed turn is reported as `TurnFailed` with its error, and
/// `turns.stop_reason` stays NULL while `turns.ended_at` is set — so "still running" and
/// "failed" are distinguishable without inventing a pseudo-reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished answering without asking for another round.
    ModelDone,
    /// The `max_rounds` fuse tripped: the loop was cut off on purpose.
    MaxRounds,
    /// The provider hit its output-token limit.
    MaxTokens,
    /// The user interrupted.
    Interrupted,
}

impl StopReason {
    /// True when the turn ended because a limit or the user cut it short, which the UI flags.
    #[must_use]
    pub const fn is_truncated(self) -> bool {
        matches!(self, Self::MaxRounds | Self::MaxTokens | Self::Interrupted)
    }

    /// The wire and database spelling (`snake_case`), which is what `turns.stop_reason` holds.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelDone => "model_done",
            Self::MaxRounds => "max_rounds",
            Self::MaxTokens => "max_tokens",
            Self::Interrupted => "interrupted",
        }
    }
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a turn ended, with what it cost.
///
/// Carried by `TurnFinished` and written to the `turns` row. A failed turn is not here: it is a
/// `TurnFailed` event, and its row keeps `stop_reason` NULL (see [`StopReason`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnCompletion {
    /// Why the turn ended.
    pub reason: StopReason,
    /// Tokens and request counts, when the provider reported them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

impl TurnCompletion {
    /// A completion with no usage figures.
    #[must_use]
    pub const fn new(reason: StopReason) -> Self {
        Self {
            reason,
            usage: None,
        }
    }

    /// Attaches usage figures.
    #[must_use]
    pub const fn with_usage(mut self, usage: Usage) -> Self {
        self.usage = Some(usage);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_measurement_is_omitted_not_zeroed() {
        let usage = Usage::default();
        assert!(usage.is_empty());
        assert_eq!(
            serde_json::to_string(&usage).expect("serialize"),
            r#"{"requests":0}"#,
            "absent token counts must not be written as zeros; the request count is always known"
        );
        let partial: Usage = serde_json::from_str(r#"{"requests":2}"#).expect("deserialize");
        assert_eq!(partial.requests, 2);
        assert_eq!(partial.prompt_tokens, None);
    }

    #[test]
    fn merging_adds_counts_and_keeps_lone_measurements() {
        let mut total = Usage {
            prompt_tokens: Some(100),
            completion_tokens: None,
            reasoning_tokens: Some(20),
            requests: 1,
        };
        total.merge(&Usage {
            prompt_tokens: Some(50),
            completion_tokens: Some(7),
            reasoning_tokens: None,
            requests: 1,
        });
        assert_eq!(
            total,
            Usage {
                prompt_tokens: Some(150),
                completion_tokens: Some(7),
                reasoning_tokens: Some(20),
                requests: 2,
            }
        );
    }

    #[test]
    fn merging_two_unknowns_leaves_them_unknown() {
        let mut total = Usage::default();
        total.merge(&Usage::default());
        assert!(total.prompt_tokens.is_none());
    }

    #[test]
    fn stop_reasons_roundtrip_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&StopReason::MaxRounds).expect("serialize"),
            "\"max_rounds\""
        );
        assert!(StopReason::Interrupted.is_truncated());
        assert!(!StopReason::ModelDone.is_truncated());
        assert_eq!(
            serde_json::from_str::<StopReason>("\"model_done\"").expect("deserialize"),
            StopReason::ModelDone
        );
    }

    #[test]
    fn the_display_form_is_what_the_database_stores() {
        for (reason, spelling) in [
            (StopReason::ModelDone, "model_done"),
            (StopReason::MaxRounds, "max_rounds"),
            (StopReason::MaxTokens, "max_tokens"),
            (StopReason::Interrupted, "interrupted"),
        ] {
            assert_eq!(reason.to_string(), spelling);
            assert_eq!(
                serde_json::to_string(&reason).expect("serialize"),
                format!("\"{spelling}\""),
                "Display and the wire spelling must not drift"
            );
        }
    }
}
