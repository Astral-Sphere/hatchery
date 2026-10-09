//! The tool registry: the catalogue the model sees, and the dispatcher calls run through.
//!
//! One registry per session, assembled at session start (docs/design/capabilities.md §4) and
//! immutable afterwards — the kernel freezes its snapshot per turn, so "swap the tools" means
//! assembling a new registry for the next turn, never mutating a live one. Live registration
//! handles (ADR-0009 discipline 3) arrive when a consumer exists: MCP tools are M5.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use hatchery_kernel::{CheckpointCollector, KernelError, ToolHost, ToolInvocation};
use hatchery_protocol::{ToolCallSummary, ToolProgress};

use crate::checkpointed_fs::{CheckpointedFs, Checkpointer};
use crate::fs::FsBackend;
use crate::terminal::TerminalBackend;
use crate::tool::{Tool, ToolCtx, ToolError};

/// The backends a session's tools share.
#[derive(Clone)]
pub struct Backends {
    /// Filesystem access.
    pub fs: Arc<dyn FsBackend>,
    /// Process execution (absent in Chat mode; see [`crate::terminal::TerminalBackend`]).
    pub terminal: Arc<dyn TerminalBackend>,
    /// The undo point taken before each write, absent when nothing in this session can write.
    ///
    /// `None` for a Chat session. For a Code one the daemon supplies an implementation that owns the
    /// budget decision (D9) and delegates to the workspace's [`crate::CheckpointStore`].
    pub checkpointer: Option<Arc<dyn Checkpointer>>,
}

/// The tools of one session, keyed by name.
pub struct ToolRegistry {
    backends: Backends,
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Starts an assembly around one set of backends.
    #[must_use]
    pub fn new(backends: Backends) -> Self {
        Self {
            backends,
            tools: BTreeMap::new(),
        }
    }

    /// Adds a tool. Duplicate names replace — assembly is explicit, and the last word wins.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.def().name, tool);
    }

    /// The tool registered under `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// How many tools are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// True when nothing is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

#[async_trait]
impl ToolHost for ToolRegistry {
    fn snapshot(&self) -> Vec<hatchery_kernel::ToolDef> {
        // BTreeMap order: the catalogue the model sees is sorted by name, so two assemblies of
        // the same toolset advertise it identically — provider-side prompt caches stay warm.
        self.tools.values().map(|tool| tool.def()).collect()
    }

    fn summarize(&self, name: &str, args: &Value) -> ToolCallSummary {
        match self.tools.get(name) {
            Some(tool) => tool.summarize(args),
            // A summary for a call the registry cannot dispatch is still worth rendering; the
            // invoke below will refuse it with the same clarity.
            None => ToolCallSummary::new(name.to_owned()),
        }
    }

    fn approval_for(&self, name: &str, args: &Value) -> Option<hatchery_protocol::ApprovalRequest> {
        self.tools.get(name)?.needs_approval(args)
    }

    async fn invoke(
        &self,
        name: &str,
        args: Value,
        cancel: CancellationToken,
        progress: UnboundedSender<ToolProgress>,
        checkpoints: CheckpointCollector,
    ) -> Result<ToolInvocation, KernelError> {
        let Some(tool) = self.tools.get(name) else {
            return Err(KernelError::tool(name, "not in this session's tool table"));
        };
        // Wrapped per call, so two calls can never mix their undo points even if the kernel ever
        // runs them concurrently. The collector itself is the kernel's and outlives this future:
        // a cancelled call is dropped without another poll, and its writes still need their
        // checkpoints (see `CheckpointCollector`).
        let checkpointed = self.backends.checkpointer.as_deref().map(|checkpointer| {
            CheckpointedFs::new(self.backends.fs.as_ref(), checkpointer, &checkpoints)
        });
        let fs: &dyn FsBackend = match &checkpointed {
            Some(wrapper) => wrapper,
            None => self.backends.fs.as_ref(),
        };
        let ctx = ToolCtx {
            fs,
            terminal: self.backends.terminal.as_ref(),
            cancel: cancel.clone(),
            emit: &move |chunk: ToolProgress| {
                // An unbounded channel with no receiver means the turn is already gone; the
                // progress was advisory and its loss is not an error.
                let _ = progress.send(chunk);
            },
        };
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ToolError::Cancelled),
            executed = tool.execute(ctx, args) => executed,
        };
        Ok(match result {
            Ok(output) => ToolInvocation::ok(output),
            Err(ToolError::Cancelled) => {
                // The kernel treats the tool as interrupted and records `Cancelled` itself; the
                // message only has to explain if it ever leaks into a result.
                ToolInvocation::failed(hatchery_protocol::ToolOutput::text(
                    "the call was cancelled",
                ))
            }
            Err(error) => {
                ToolInvocation::failed(hatchery_protocol::ToolOutput::text(error.to_string()))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpointed_fs::PreWrite;
    use crate::fs::{FsEntry, FsError, FsMetadata};
    use crate::terminal::{
        TermError, TerminalBackend, TerminalHandle, TerminalOutcome, TerminalSpec,
    };
    use async_trait::async_trait;
    use serde_json::json;

    /// A filesystem that is always empty: the registry tests do not touch it.
    struct NoFs;
    #[async_trait]
    impl FsBackend for NoFs {
        async fn read_text_file(&self, path: &str) -> Result<String, FsError> {
            Err(FsError::NotFound(path.to_owned()))
        }
        async fn read_dir(&self, _path: &str) -> Result<Vec<FsEntry>, FsError> {
            Ok(Vec::new())
        }
        async fn metadata(&self, path: &str) -> Result<FsMetadata, FsError> {
            Err(FsError::NotFound(path.to_owned()))
        }
        async fn write_text_file(&self, path: &str, _contents: &str) -> Result<(), FsError> {
            Err(FsError::NotFound(path.to_owned()))
        }
    }

    /// A terminal that refuses everything: Chat mode's shape.
    struct NoTerminal;
    #[async_trait]
    impl TerminalBackend for NoTerminal {
        async fn create(
            &self,
            _spec: TerminalSpec,
            _cancel: CancellationToken,
        ) -> Result<Box<dyn TerminalHandle>, TermError> {
            Err(TermError::Unavailable("no terminal in this session"))
        }
    }

    /// Echoes its `path` argument, to observe dispatch and summaries.
    struct Echo;
    #[async_trait]
    impl Tool for Echo {
        fn def(&self) -> hatchery_kernel::ToolDef {
            hatchery_kernel::ToolDef {
                name: "zz_echo".to_owned(),
                description: "echoes".to_owned(),
                parameters: json!({}),
            }
        }
        fn needs_approval(&self, _args: &Value) -> Option<hatchery_protocol::ApprovalRequest> {
            None
        }
        fn summarize(&self, args: &Value) -> ToolCallSummary {
            ToolCallSummary::new(format!("zz_echo {}", args["path"].as_str().unwrap_or("?")))
        }
        async fn execute(
            &self,
            _ctx: ToolCtx<'_>,
            args: Value,
        ) -> Result<hatchery_protocol::ToolOutput, ToolError> {
            Ok(hatchery_protocol::ToolOutput::text(
                args["path"].as_str().unwrap_or("?").to_owned(),
            ))
        }
    }

    fn registry() -> ToolRegistry {
        let mut reg = ToolRegistry::new(backends_only());
        reg.register(Arc::new(Echo));
        reg
    }

    fn backends_only() -> Backends {
        Backends {
            fs: Arc::new(NoFs),
            terminal: Arc::new(NoTerminal),
            checkpointer: None,
        }
    }

    #[test]
    fn the_snapshot_is_sorted_by_name_whatever_the_registration_order() {
        let mut reg = ToolRegistry::new(backends_only());
        reg.register(Arc::new(Echo));
        reg.register(Arc::new(Echo)); // replaced by name, not duplicated
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.snapshot()[0].name, "zz_echo");
    }

    #[tokio::test]
    async fn invoke_dispatches_through_the_seam_and_reports_progress() {
        let reg = registry();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let invocation = reg
            .invoke(
                "zz_echo",
                json!({"path": "src/main.rs"}),
                CancellationToken::new(),
                tx,
                CheckpointCollector::new(),
            )
            .await
            .expect("runs");
        assert!(!invocation.is_error);
        assert_eq!(invocation.output.text, "src/main.rs");
        assert!(rx.try_recv().is_err(), "echo emits no progress");
    }

    /// The registry's half of D13: the collector the kernel lends is the one the decorator fills.
    /// Nothing else in the chain can see the write, so if this threading were wrong the checkpoints
    /// would be collected into a buffer nobody drains and every rewind target would silently vanish.
    ///
    /// `NoFs` refuses the write, and the checkpoint is still there — which is the intended order of
    /// operations: the undo point describes the state *before*, and a write that failed halfway has
    /// still left something behind to undo.
    #[tokio::test]
    async fn a_checkpointer_on_the_backends_fills_the_callers_collector() {
        struct Writes;
        #[async_trait]
        impl Tool for Writes {
            fn def(&self) -> hatchery_kernel::ToolDef {
                hatchery_kernel::ToolDef {
                    name: "writes".to_owned(),
                    description: String::new(),
                    parameters: json!({}),
                }
            }
            fn needs_approval(&self, _args: &Value) -> Option<hatchery_protocol::ApprovalRequest> {
                None
            }
            fn summarize(&self, _args: &Value) -> ToolCallSummary {
                ToolCallSummary::new("writes")
            }
            async fn execute(
                &self,
                ctx: ToolCtx<'_>,
                _args: Value,
            ) -> Result<hatchery_protocol::ToolOutput, ToolError> {
                ctx.fs
                    .write_text_file("a.txt", "content")
                    .await
                    .map_err(ToolError::from)?;
                Ok(hatchery_protocol::ToolOutput::text("written"))
            }
        }

        struct OneCheckpoint;
        #[async_trait]
        impl Checkpointer for OneCheckpoint {
            async fn pre_write(&self) -> Result<PreWrite, crate::CheckpointError> {
                Ok(PreWrite::Taken(hatchery_protocol::Checkpoint {
                    commit_id: "shadow-commit".to_owned(),
                    kind: hatchery_protocol::CheckpointKind::PreWrite,
                }))
            }
        }

        let mut reg = ToolRegistry::new(Backends {
            fs: Arc::new(NoFs),
            terminal: Arc::new(NoTerminal),
            checkpointer: Some(Arc::new(OneCheckpoint)),
        });
        reg.register(Arc::new(Writes));

        let collector = CheckpointCollector::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let invocation = reg
            .invoke(
                "writes",
                json!({}),
                CancellationToken::new(),
                tx,
                collector.clone(),
            )
            .await
            .expect("dispatches");

        assert!(invocation.is_error, "NoFs refused the write");
        let collected = collector.drain();
        assert_eq!(collected.len(), 1, "the caller's collector was filled");
        assert_eq!(collected[0].commit_id, "shadow-commit");
    }

    /// Without a checkpointer there is no decorator either, so a Chat session's registry behaves
    /// exactly as it did before the write path existed.
    #[tokio::test]
    async fn no_checkpointer_means_nothing_is_collected() {
        let reg = registry();
        let collector = CheckpointCollector::new();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        reg.invoke(
            "zz_echo",
            json!({"path": "a.txt"}),
            CancellationToken::new(),
            tx,
            collector.clone(),
        )
        .await
        .expect("runs");
        assert!(collector.is_empty());
    }

    #[tokio::test]
    async fn an_unknown_tool_is_a_kernel_tool_error_naming_it() {
        let reg = registry();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let error = reg
            .invoke(
                "nope",
                json!({}),
                CancellationToken::new(),
                tx,
                CheckpointCollector::new(),
            )
            .await
            .expect_err("unknown");
        let KernelError::Tool { name, .. } = &error else {
            panic!("{error}");
        };
        assert_eq!(name, "nope");
    }

    #[test]
    fn summaries_delegate_to_the_tool_and_survive_unknown_names() {
        let reg = registry();
        assert_eq!(
            reg.summarize("zz_echo", &json!({"path": "a.txt"})).title,
            "zz_echo a.txt"
        );
        assert_eq!(reg.summarize("nope", &json!({})).title, "nope");
    }

    #[tokio::test]
    async fn cancellation_beats_execution() {
        struct Slow;
        #[async_trait]
        impl Tool for Slow {
            fn def(&self) -> hatchery_kernel::ToolDef {
                hatchery_kernel::ToolDef {
                    name: "slow".to_owned(),
                    description: String::new(),
                    parameters: json!({}),
                }
            }
            fn needs_approval(&self, _args: &Value) -> Option<hatchery_protocol::ApprovalRequest> {
                None
            }
            fn summarize(&self, _args: &Value) -> ToolCallSummary {
                ToolCallSummary::new("slow")
            }
            async fn execute(
                &self,
                _ctx: ToolCtx<'_>,
                _args: Value,
            ) -> Result<hatchery_protocol::ToolOutput, ToolError> {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                Ok(hatchery_protocol::ToolOutput::text("finished"))
            }
        }
        let mut reg = ToolRegistry::new(backends_only());
        reg.register(Arc::new(Slow));

        let cancel = CancellationToken::new();
        cancel.cancel();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let invocation = reg
            .invoke("slow", json!({}), cancel, tx, CheckpointCollector::new())
            .await
            .expect("returns, does not hang");
        assert!(
            invocation.is_error,
            "a cancelled call did not produce its result"
        );
    }

    // TerminalOutcome is part of the seam's M1 shape; keep it exercised even though no M1 tool
    // runs a process.
    #[test]
    fn terminal_outcome_carries_both_streams_and_the_code() {
        let outcome = TerminalOutcome {
            stdout: "out".to_owned(),
            stderr: "err".to_owned(),
            exit_code: Some(0),
        };
        assert_eq!(outcome.stdout, "out");
        assert_eq!(outcome.exit_code, Some(0));
    }
}
