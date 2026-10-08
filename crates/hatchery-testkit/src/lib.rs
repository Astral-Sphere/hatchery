//! Test infrastructure for the hatchery workspace (docs/design/testing.md §2).
//!
//! Dev-only crate, never published: referenced as a `dev-dependency` by the crates under test.
//! Fakes live here so they are written once instead of per crate, and so that a fake which drifts
//! from the trait it implements breaks in one place.
//!
//! The M0b subset is the kernel's four seams plus the store's reference model:
//!
//! * [`ScriptedProvider`] — a provider that replays scripted rounds and records what it was asked;
//! * [`MemoryHistory`] — a conversation the test controls;
//! * [`RecordingSink`] — keeps every event, so a test can assert an exact sequence;
//! * [`ScriptedToolHost`] + [`answer_approvals`] — scripted tools and a frontend that answers
//!   approvals;
//! * [`ReferenceTree`] — an independently written item tree for the store's property tests;
//! * [`Gate`] — releases a gated fake step by step, so interrupt tests have a window to act in;
//! * the [`events`] helpers — read a recorded turn's outcome, items and states back out.
//!
//! `MemoryTerminal`, `TempWorkspace`, `TestDaemon` and `ClientProbe` land with their consumers in
//! M1–M2; `MemoryFs` and the wire double ([`MockWire`]) arrived with the llm crate's SSE replay.

pub mod daemon;
pub mod events;
pub mod fs;
pub mod gate;
pub mod history;
pub mod model;
pub mod provider;
pub mod sink;
pub mod tools;
pub mod wire;

pub use daemon::{ClientProbe, TestDaemon};
pub use events::{completion, error, finished_items, kinds, reason, states, tool_result_texts};
pub use fs::{MemoryFs, TempWorkspace, UserRepoState, user_repo_state};
pub use gate::Gate;
pub use history::MemoryHistory;
pub use model::{ChainShape, ReferenceTree};
pub use provider::{RecordedRequest, ScriptedProvider};
pub use sink::RecordingSink;
pub use tools::{RecordedCall, ScriptedApproval, ScriptedToolHost, answer_approvals};
pub use wire::{MockWire, RecordedWireRequest, json_fixture, sse_fixture};
