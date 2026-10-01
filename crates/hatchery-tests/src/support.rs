//! Fixtures every e2e test shares: one scripted provider conversation, the config layer that
//! points a `testprov` provider at a MockWire, and the collection helpers that turn an event
//! stream into assertions.
//!
//! The SSE bytes mirror the recorded-fixture shape exactly (full chunk fields, stop and usage
//! as separate chunks) — the wire layer skips chunks it cannot deserialise, so a half-shaped
//! test stream would pass while every real stream failed.

use std::time::Duration;

use hatchery_protocol::method as m;
use hatchery_protocol::{Content, EventStream, ServerEvent, SessionEvent};
use hatchery_testkit::daemon::TestDaemon;
use hatchery_testkit::wire::MockWire;

/// The reasoning the scripted model produces: leading whitespace, unicode, a newline and a
/// trailing tab — exactly the bytes a trim would destroy and the byte-exact checks exist for.
pub const REASONING: &str = "  步骤：⟳ 多行\n中间 尾随 \t";

/// The three reasoning deltas whose concatenation is [`REASONING`].
pub const REASONING_DELTAS: [&str; 3] = ["  步骤：⟳ ", "多行\n中间 ", "尾随 \t"];

/// The answer that follows the reasoning.
pub const ANSWER: &str = "Hello, 世界!";

/// A ChatCompletions stream: role chunk, three reasoning deltas, content, stop, usage, done.
pub const SSE_REASONING_OK: &str = concat!(
    "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}],\"created\":1718345013,\"model\":\"m\",\"object\":\"chat.completion.chunk\",\"usage\":null}\n\n",
    "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"  步骤：⟳ \"},\"finish_reason\":null}],\"created\":1718345013,\"model\":\"m\",\"object\":\"chat.completion.chunk\"}\n\n",
    "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"多行\\n中间 \"},\"finish_reason\":null}],\"created\":1718345013,\"model\":\"m\",\"object\":\"chat.completion.chunk\"}\n\n",
    "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"尾随 \\t\"},\"finish_reason\":null}],\"created\":1718345013,\"model\":\"m\",\"object\":\"chat.completion.chunk\"}\n\n",
    "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello, 世界!\"},\"finish_reason\":null}],\"created\":1718345013,\"model\":\"m\",\"object\":\"chat.completion.chunk\"}\n\n",
    "data: {\"id\":\"1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"created\":1718345013,\"model\":\"m\",\"object\":\"chat.completion.chunk\"}\n\n",
    "data: {\"choices\":[],\"created\":1718345013,\"id\":\"1\",\"model\":\"m\",\"object\":\"chat.completion.chunk\",\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":6,\"total_tokens\":15}}\n\n",
    "data: [DONE]\n\n",
);

/// The `[providers.testprov]` layer pointing at `base_url`, serving model `m` with reasoning
/// echo switched on — the echo comes from this config's capability row, the same table the
/// adapter folds, which is the property `provider_for` must preserve.
#[must_use]
pub fn provider_layer(base_url: &str) -> toml::Table {
    toml::from_str(&format!(
        "[providers.testprov]\n\
         base_url = \"{base_url}\"\n\
         env_key = \"PATH\"\n\
         models = [\"m\"]\n\
         capabilities = [[\"m\", {{ echo_reasoning = true }}]]\n",
    ))
    .expect("the fixture provider layer is valid TOML")
}

/// A daemon whose one provider is `testprov` at the wire's URL.
pub async fn daemon_at(wire: &MockWire) -> TestDaemon {
    TestDaemon::start(vec![(m::ConfigOrigin::User, provider_layer(&wire.url()))]).await
}

/// A chat session on the fixture provider.
pub fn chat_params() -> m::SessionNewParams {
    m::SessionNewParams {
        mode: hatchery_protocol::SessionModeId::chat(),
        workspace: None,
        model: Some(hatchery_protocol::ModelRef::new("testprov", "m")),
        title: Some("e2e".to_owned()),
        config_patch: None,
    }
}

/// A prompt from the caller that last saw `generation`.
pub fn prompt_params(
    session: hatchery_protocol::SessionId,
    text: &str,
    generation: u64,
) -> m::SessionPromptParams {
    m::SessionPromptParams {
        session_id: session,
        content: Content::text(text.to_owned()),
        generation: Some(generation),
    }
}

/// The `"type"` of the server event inside the envelope — what every order assertion reads.
#[must_use]
pub fn event_type(event: &SessionEvent) -> String {
    serde_json::to_value(&event.event).expect("an event serialises")["type"]
        .as_str()
        .expect("tagged")
        .to_owned()
}

/// Collects events until the turn ends (`TurnFinished` or `TurnFailed`), which the collection
/// includes. A missing terminal event is a hang, not a silence — hence the per-event timeout.
///
/// # Panics
///
/// Panics when the stream dies or goes quiet for ten seconds: a scenario that stops producing
/// events mid-turn is the failure the test is looking for.
pub async fn collect_until_terminal(stream: &mut EventStream) -> Vec<SessionEvent> {
    let mut collected = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .expect("an event arrived in time")
            .expect("the event stream stayed alive");
        let terminal = matches!(
            event.event,
            ServerEvent::TurnFinished { .. } | ServerEvent::TurnFailed { .. }
        );
        collected.push(event);
        if terminal {
            return collected;
        }
    }
}

/// The item-level projection of one turn's events: `SessionUpdated` housekeeping stripped, the
/// rest in arrival order. This is the wire-facing half of the kernel's documented sequence.
#[must_use]
pub fn item_story(events: &[SessionEvent]) -> Vec<String> {
    events
        .iter()
        .map(event_type)
        .filter(|kind| kind != "session_updated")
        .collect()
}
