//! Headless exec: one prompt in, a stream out, an exit code that tells the truth.
//!
//! Plain mode prints assistant text as it arrives and keeps tool chatter on stderr, so
//! stdout stays pipeable. `--json` prints the item-level protocol events — items, deltas,
//! tool calls, the turn terminal events — one JSON object per line, for scripts and SDKs.
//! Exit codes: 0 completed, 1 failed, 2 cancelled (Ctrl-C cancels the turn server-side).

use hatchery_daemon::discover::StateDir;
use hatchery_protocol::method as m;
use hatchery_protocol::{EventStream, ServerEvent, SessionEvent, SessionId};

use crate::args::ExecArgs;
use crate::attach;

/// Where exec writes what it sees; the seam that makes golden tests possible.
pub trait ExecOut {
    /// A chunk of assistant text (plain mode only).
    fn text(&mut self, text: &str);
    /// One JSON line (json mode only).
    fn json_line(&mut self, line: &str);
    /// Human-visible commentary, never part of the piped payload.
    fn note(&mut self, note: &str);
}

/// The real sinks: text and JSON to stdout, notes to stderr.
pub struct StdoutOut;

impl ExecOut for StdoutOut {
    fn text(&mut self, text: &str) {
        use std::io::Write;
        print!("{text}");
        let _ = std::io::stdout().flush();
    }

    fn json_line(&mut self, line: &str) {
        use std::io::Write;
        println!("{line}");
        let _ = std::io::stdout().flush();
    }

    fn note(&mut self, note: &str) {
        eprintln!("{note}");
    }
}

/// Why exec exited the way it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The turn finished normally.
    Completed,
    /// The turn failed; the message went to stderr.
    Failed,
    /// The user (or the connection) interrupted the turn.
    Cancelled,
}

impl Outcome {
    /// The process exit code.
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Completed => 0,
            Self::Failed => 1,
            Self::Cancelled => 2,
        }
    }
}

/// Runs one prompt headlessly; returns the exit code the process should carry.
pub async fn run(state: StateDir, args: &ExecArgs, out: &mut dyn ExecOut) -> Outcome {
    let attached = match attach::attach_or_spawn(state, None, args.workspace.as_deref()).await {
        Ok(attached) => attached,
        Err(error) => {
            out.note(&format!("hatchery: {error}"));
            return Outcome::Failed;
        }
    };
    run_attached(attached, args, out).await
}

/// The exec core on an already-attached client (the test seam for the attach step).
pub async fn run_attached(
    attached: attach::Attached,
    args: &ExecArgs,
    out: &mut dyn ExecOut,
) -> Outcome {
    // Resume or create; either subscribing call rides the events connection, so the stream is
    // live from here on.
    let (mut events, session, _reply) = match open_subscribed(
        &attached,
        args.session,
        args.model.as_deref(),
        args.workspace.as_deref(),
    )
    .await
    {
        Ok(pair) => pair,
        Err(error) => {
            out.note(&format!("hatchery: {error}"));
            return Outcome::Failed;
        }
    };
    if !args.json {
        out.note(&format!(
            "session {} on {} ({})",
            session,
            attached.info.uds_path,
            if attached.spawned {
                "spawned"
            } else {
                "attached"
            }
        ));
    }

    let prompt = m::SessionPromptParams {
        session_id: session,
        content: hatchery_protocol::Content::text(args.prompt.clone()),
        generation: None,
    };
    if let Err(error) = attached
        .client
        .call::<_, m::SessionPromptResult>(m::SESSION_PROMPT, &prompt)
        .await
    {
        out.note(&format!("hatchery: the turn was not accepted: {error}"));
        return Outcome::Failed;
    }

    // Ctrl-C cancels the turn server-side, then exec finishes as cancelled — the daemon runs
    // the cancellation, the client only asks for it and reports.
    let mut interrupted = Box::pin(tokio::signal::ctrl_c());

    #[allow(unused_assignments)]
    let mut outcome = Outcome::Failed;
    loop {
        let event = tokio::select! {
            event = events.next() => event,
            _ = &mut interrupted => {
                let params = m::SessionCancelParams { session_id: session };
                if let Err(error) = attached
                    .client
                    .call::<_, m::SessionCancelResult>(m::SESSION_CANCEL, &params)
                    .await
                {
                    out.note(&format!("hatchery: the cancel request failed: {error}"));
                }
                outcome = Outcome::Cancelled;
                break;
            }
        };
        let Some(event) = event else {
            out.note("hatchery: the event stream ended before the turn did");
            outcome = Outcome::Failed;
            break;
        };
        match step(&event, args.json, out) {
            Step::Continue => {}
            Step::Terminal(end) => {
                outcome = end;
                break;
            }
        }
    }
    if !args.json {
        out.text("\n");
    }
    outcome
}

/// Opens (or resumes) the session on a fresh events connection; the reply JSON is the
/// subscribing call's own result (`session/new`'s session or `session/load`'s session+items).
pub(crate) async fn open_subscribed(
    attached: &attach::Attached,
    session: Option<SessionId>,
    model: Option<&str>,
    workspace: Option<&std::path::Path>,
) -> Result<(EventStream, SessionId, serde_json::Value), String> {
    let mut stream = EventStream::connect(&attached.info.uds_path)
        .await
        .map_err(|error| error.to_string())?;
    let reply = if let Some(session) = session {
        let params = m::SessionLoadParams {
            session_id: session,
            replay_from: None,
            generation: None,
        };
        stream
            .subscribe(
                m::SESSION_LOAD,
                serde_json::to_value(&params).expect("params"),
            )
            .await
            .map_err(|error| error.to_string())?;
        (session, serde_json::json!({ "session": { "id": session } }))
    } else {
        let params = m::SessionNewParams {
            mode: hatchery_protocol::SessionModeId::chat(),
            workspace: workspace.map(std::path::Path::to_path_buf),
            model: model.and_then(parse_model),
            title: None,
            config_patch: None,
        };
        let created = stream
            .subscribe(
                m::SESSION_NEW,
                serde_json::to_value(&params).expect("params"),
            )
            .await
            .map_err(|error| error.to_string())?;
        let id = serde_json::from_value::<m::SessionNewResult>(created.clone())
            .map_err(|error| format!("session/new reply: {error}"))?
            .session
            .id;
        (id, created)
    };
    Ok((stream, reply.0, reply.1))
}

/// `provider/model` on the wire; a bare model is passed through and the daemon resolves it.
fn parse_model(text: &str) -> Option<hatchery_protocol::ModelRef> {
    match text.split_once('/') {
        Some((provider, model)) if !provider.is_empty() && !model.is_empty() => {
            Some(hatchery_protocol::ModelRef::new(provider, model))
        }
        None if !text.is_empty() => Some(hatchery_protocol::ModelRef::new("", text)),
        _ => None,
    }
}

/// What one event does to the output.
enum Step {
    Continue,
    Terminal(Outcome),
}

/// The events `--json` forwards: item-level and turn-terminal, not connection housekeeping.
fn is_item_level(event: &ServerEvent) -> bool {
    matches!(
        event,
        ServerEvent::ItemStarted { .. }
            | ServerEvent::TextDelta { .. }
            | ServerEvent::ReasoningDelta { .. }
            | ServerEvent::ItemFinished { .. }
            | ServerEvent::ToolCallStarted { .. }
            | ServerEvent::ToolCallProgress { .. }
            | ServerEvent::ApprovalRequested { .. }
            | ServerEvent::ModeSwitched { .. }
            | ServerEvent::TurnFinished { .. }
            | ServerEvent::TurnFailed { .. }
    )
}

fn step(event: &SessionEvent, json: bool, out: &mut dyn ExecOut) -> Step {
    // One JSON line per kept event, envelope included: session, generation, event.
    if let (true, Some(line)) = (
        json && is_item_level(&event.event),
        serde_json::to_string(&serde_json::json!({
            "session": event.session,
            "generation": event.generation,
            "event": event.event,
        }))
        .ok(),
    ) {
        out.json_line(&line);
    }
    match &event.event {
        ServerEvent::TextDelta { text, .. } if !json => out.text(text),
        ServerEvent::ToolCallStarted { summary, .. } if !json => {
            out.note(&format!("… {}", summary.title));
        }
        ServerEvent::TurnFinished { .. } => return Step::Terminal(Outcome::Completed),
        ServerEvent::TurnFailed { error, .. } => {
            out.note(&format!("hatchery: the turn failed: {}", error.message));
            return Step::Terminal(Outcome::Failed);
        }
        _ => {}
    }
    Step::Continue
}
