//! The assembly path the daemon will call: `chat_tools()` into a `ToolRegistry`, seen through
//! the kernel's `ToolHost` seam only (docs/design/capabilities.md §4).

use std::sync::Arc;

use hatchery_capabilities::{Backends, ToolRegistry};
use hatchery_kernel::ToolHost;
use hatchery_testkit::MemoryFs;
use tokio_util::sync::CancellationToken;

fn registry_on(fs: Arc<MemoryFs>) -> ToolRegistry {
    let backends = Backends {
        fs,
        terminal: Arc::new(hatchery_capabilities::NoTerminal),
    };
    let mut registry = ToolRegistry::new(backends);
    for tool in hatchery_tools::chat_tools() {
        registry.register(tool);
    }
    registry
}

#[test]
fn the_chat_catalogue_is_sorted_and_matches_the_dispatch_table() {
    let registry = registry_on(Arc::new(MemoryFs::new()));
    let snapshot = registry.snapshot();
    let names: Vec<&str> = snapshot.iter().map(|def| def.name.as_str()).collect();
    assert_eq!(
        names,
        ["glob", "grep", "read_file"],
        "sorted for prompt-cache stability"
    );

    // The advertised catalogue and the dispatch table must be the same list (kernel.md: the
    // model cannot call a tool it was not shown, and must not fail to call one it was).
    for def in &snapshot {
        assert!(
            registry.get(&def.name).is_some(),
            "{} advertised but absent",
            def.name
        );
    }
    assert_eq!(snapshot.len(), 3);
}

#[tokio::test]
async fn a_turn_round_trip_through_toolhost_reads_a_file() {
    let fs = MemoryFs::new();
    fs.file("guide.md", "# Title\nBody\n");
    let registry = registry_on(Arc::new(fs));

    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
    let invocation = registry
        .invoke(
            "read_file",
            serde_json::json!({"path": "guide.md"}),
            CancellationToken::new(),
            progress_tx,
        )
        .await
        .expect("dispatched");
    assert!(!invocation.is_error);
    assert_eq!(invocation.output.text, "1: # Title\n2: Body\n");
    assert!(
        progress_rx.try_recv().is_err(),
        "read_file is silent while it runs"
    );
}

#[tokio::test]
async fn glob_and_grep_see_the_same_workspace_through_the_same_backend() {
    let fs = MemoryFs::new();
    fs.file("src/lib.rs", "pub fn f() {}\n")
        .file("src/main.rs", "fn main() { f(); }\n");
    let registry = registry_on(Arc::new(fs));

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let found = registry
        .invoke(
            "glob",
            serde_json::json!({"pattern": "**/*.rs"}),
            CancellationToken::new(),
            tx.clone(),
        )
        .await
        .expect("glob runs");
    assert!(
        found.output.text.contains("src/lib.rs"),
        "{}",
        found.output.text
    );

    let searched = registry
        .invoke(
            "grep",
            serde_json::json!({"pattern": "fn main", "include": "*.rs"}),
            CancellationToken::new(),
            tx,
        )
        .await
        .expect("grep runs");
    assert!(
        searched.output.text.starts_with("src/main.rs:1:"),
        "{}",
        searched.output.text
    );
}

#[tokio::test]
async fn chat_mode_approves_nothing_and_refuses_unknown_tools() {
    let registry = registry_on(Arc::new(MemoryFs::new()));

    // ADR-0005: Chat has no approvals — every registered tool answers None for any arguments.
    for name in ["read_file", "glob", "grep"] {
        assert!(
            registry
                .approval_for(name, &serde_json::json!({}))
                .is_none(),
            "{name} must not demand approval in Chat mode"
        );
    }

    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let error = registry
        .invoke("shell", serde_json::json!({}), CancellationToken::new(), tx)
        .await
        .expect_err("not a Chat tool");
    assert!(error.to_string().contains("shell"), "{error}");
}

#[test]
fn summaries_come_back_human_readable_for_every_chat_tool() {
    let registry = registry_on(Arc::new(MemoryFs::new()));
    let cases = [
        (
            "read_file",
            serde_json::json!({"path": "a.txt"}),
            "read_file a.txt",
        ),
        (
            "glob",
            serde_json::json!({"pattern": "**/*.rs"}),
            "glob **/*.rs",
        ),
        ("grep", serde_json::json!({"pattern": "todo"}), "grep todo"),
    ];
    for (name, args, expected) in cases {
        assert_eq!(registry.summarize(name, &args).title, expected, "{name}");
    }
}
