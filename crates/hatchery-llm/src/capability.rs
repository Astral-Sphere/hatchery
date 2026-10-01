//! The capability table: what each model family can do on the wire (docs/design/llm.md §3).
//!
//! Every provider quirk the adapter honours — which field carries "think harder", whether
//! reasoning may be echoed back, whether signatures exist — is a row here rather than an
//! `if model.contains(...)` somewhere. Rows are data: the built-in table is a starting point the
//! daemon's config can override wholesale, and `hatchery doctor --provider` exists to check the
//! table against reality (its findings go back into the built-ins).

use hatchery_protocol::ReasoningEffort;
use serde::{Deserialize, Serialize};

/// How a model family expresses "how much should I reason".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReasoningWire {
    /// Plain `reasoning_effort` passthrough, the OpenAI spelling generic gateways accept.
    Effort,
    /// Qwen: `enable_thinking: bool` plus a `thinking_budget` tier for Low..Max.
    QwenThinking,
    /// DeepSeek: a `thinking` object — `{"type": "enabled" | "disabled"}`. The current
    /// generation of DeepSeek models are hybrid: thinking is on by default and this switch
    /// turns it off; the model never changes (2026-09-30 recalibration, worklog/llm.md).
    ThinkingSwitch,
    /// DeepSeek (historic): no knob — the effort picked the model, `off` naming the
    /// non-reasoning one. Kept as a config-overridable row for older deployments/gateways.
    ModelSwitch {
        /// The model that does not reason.
        off_model: String,
        /// The model that does.
        thinking_model: String,
    },
    /// The wire has no reasoning knob; an explicit effort is ignored with one warning.
    None,
}

/// What one model can do (docs/design/llm.md §3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCapabilities {
    /// The reasoning knob this family speaks.
    pub reasoning: ReasoningWire,
    /// Whether assistant `reasoning_content` may be written back into request history. When
    /// false, stored reasoning is dropped on the way out — the provider regenerates its own.
    pub echo_reasoning: bool,
    /// Whether the provider issues opaque signature blocks that must be replayed verbatim.
    /// Chat Completions has no such field; this is reserved for the Responses wire.
    pub signature_blocks: bool,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            reasoning: ReasoningWire::Effort,
            echo_reasoning: false,
            signature_blocks: false,
        }
    }
}

impl ModelCapabilities {
    /// The model name a request should carry for this effort, when the family switches models.
    #[must_use]
    pub fn model_for_effort(&self, model: &str, effort: Option<ReasoningEffort>) -> String {
        match (&self.reasoning, effort) {
            (ReasoningWire::ModelSwitch { off_model, .. }, Some(ReasoningEffort::Off)) => {
                off_model.clone()
            }
            (ReasoningWire::ModelSwitch { thinking_model, .. }, Some(_)) => thinking_model.clone(),
            // No effort requested: the caller's model stands, whatever it is.
            _ => model.to_owned(),
        }
    }
}

/// `thinking_budget` tiers for the Qwen family. Provisional until `hatchery doctor` probes the
/// current generation of models; rows are config-overridable precisely so a correction never
/// needs a release.
#[must_use]
pub fn qwen_thinking_budget(effort: ReasoningEffort) -> Option<u32> {
    match effort {
        ReasoningEffort::Off => None,
        ReasoningEffort::Low => Some(2_048),
        ReasoningEffort::Medium => Some(8_192),
        ReasoningEffort::High => Some(32_768),
        ReasoningEffort::Max => Some(65_536),
    }
}

/// Model-family rows, longest-prefix match wins, built-ins at the bottom.
///
/// A prefix (`"deepseek"`) rather than an exact name (`"deepseek-flash"`) because providers
/// keep adding models to a family and the family's wire behaviour is what the table describes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CapabilityTable {
    overrides: Vec<(String, ModelCapabilities)>,
}

impl CapabilityTable {
    /// A table with no overrides, falling back to the built-ins for every model.
    #[must_use]
    pub fn builtin() -> Self {
        Self::default()
    }

    /// Overrides a family, exact prefix semantics, replacing any earlier override of it.
    #[must_use]
    pub fn with_override(
        mut self,
        model_prefix: impl Into<String>,
        capabilities: ModelCapabilities,
    ) -> Self {
        let prefix = model_prefix.into();
        self.overrides.retain(|(existing, _)| *existing != prefix);
        self.overrides.push((prefix, capabilities));
        self
    }

    /// The capabilities of one model.
    #[must_use]
    pub fn capabilities(&self, model: &str) -> ModelCapabilities {
        let overrides = self
            .overrides
            .iter()
            .map(|(prefix, caps)| (prefix.as_str(), caps));
        let builtins = BUILTIN.iter().map(|(prefix, caps)| (*prefix, caps));
        let mut best: Option<(&str, &ModelCapabilities)> = None;
        for (prefix, caps) in overrides.chain(builtins) {
            if model.starts_with(prefix)
                && best.is_none_or(|(longest, _)| prefix.len() > longest.len())
            {
                best = Some((prefix, caps));
            }
        }
        best.map_or_else(ModelCapabilities::default, |(_, caps)| caps.clone())
    }
}

/// The shipped table. DeepSeek and Qwen rows come from their current API docs; both are
/// corrected through live probing as part of M1 (docs/worklog/llm.md).
static BUILTIN: std::sync::LazyLock<Vec<(&str, ModelCapabilities)>> =
    std::sync::LazyLock::new(|| {
        vec![
            (
                "deepseek",
                ModelCapabilities {
                    reasoning: ReasoningWire::ThinkingSwitch,
                    echo_reasoning: false,
                    signature_blocks: false,
                },
            ),
            (
                "qwen",
                ModelCapabilities {
                    reasoning: ReasoningWire::QwenThinking,
                    echo_reasoning: false,
                    signature_blocks: false,
                },
            ),
            (
                // Every unlisted family gets the generic OpenAI-compatible row.
                "",
                ModelCapabilities {
                    reasoning: ReasoningWire::Effort,
                    echo_reasoning: false,
                    signature_blocks: false,
                },
            ),
        ]
    });

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_switches_thinking_mode_rather_than_models() {
        let table = CapabilityTable::builtin();
        // The family row covers every current DeepSeek model name.
        for model in ["deepseek-flash", "deepseek-chat"] {
            assert_eq!(
                table.capabilities(model).reasoning,
                ReasoningWire::ThinkingSwitch,
                "{model}"
            );
            assert_eq!(
                table
                    .capabilities(model)
                    .model_for_effort(model, Some(ReasoningEffort::Off)),
                model,
                "the model never changes; the switch is a field"
            );
        }
    }

    #[test]
    fn qwen_uses_budgets_and_off_disables_thinking() {
        let table = CapabilityTable::builtin();
        let caps = table.capabilities("qwen3-max");
        assert_eq!(caps.reasoning, ReasoningWire::QwenThinking);
        assert_eq!(qwen_thinking_budget(ReasoningEffort::Off), None);
        assert!(qwen_thinking_budget(ReasoningEffort::High).is_some_and(|b| b > 8_192));
    }

    #[test]
    fn an_unlisted_model_gets_the_generic_row() {
        let table = CapabilityTable::builtin();
        let caps = table.capabilities("some-gateway-model");
        assert_eq!(caps.reasoning, ReasoningWire::Effort);
        assert!(!caps.echo_reasoning);
    }

    #[test]
    fn an_override_wins_by_longest_prefix_and_replaces_its_own_kind() {
        let table = CapabilityTable::builtin().with_override(
            "qwen3.8-flash",
            ModelCapabilities {
                echo_reasoning: true,
                ..ModelCapabilities::default()
            },
        );
        assert!(table.capabilities("qwen3.8-flash").echo_reasoning);
        assert!(!table.capabilities("qwen-other-model").echo_reasoning);

        let replaced = table
            .with_override("qwen3.8-flash", ModelCapabilities::default())
            .capabilities("qwen3.8-flash");
        assert!(
            !replaced.echo_reasoning,
            "the last override of a prefix wins"
        );
    }
}
