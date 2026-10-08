//! The assembly path the daemon will call: `chat_tools()` into a `ToolRegistry`, seen through
//! the kernel's `ToolHost` seam only (docs/design/capabilities.md §4).

use std::sync::Arc;

use hatchery_capabilities::{Backends, ToolRegistry};
use hatchery_kernel::{CheckpointCollector, ToolHost};
use hatchery_testkit::MemoryFs;
use tokio_util::sync::CancellationToken;

/// The same assembly, but over a real tempdir on the real disk: the only coverage the tools
/// get against actual symlinks, permissions and case behaviour (MemoryFs pins the logic; this
/// pins the disk).
fn registry_on_disk(root: &std::path::Path) -> ToolRegistry {
    let backends = Backends {
        fs: Arc::new(hatchery_capabilities::LocalFs::new(root).expect("a valid workspace root")),
        terminal: Arc::new(hatchery_capabilities::NoTerminal),
        // None, as in a Chat session: these tools only read, so there is nothing to checkpoint.
        // The write path's checkpoint wiring is covered in `hatchery-capabilities`.
        checkpointer: None,
    };
    let mut registry = ToolRegistry::new(backends);
    for tool in hatchery_tools::chat_tools() {
        registry.register(tool);
    }
    registry
}

fn registry_on(fs: Arc<MemoryFs>) -> ToolRegistry {
    let backends = Backends {
        fs,
        terminal: Arc::new(hatchery_capabilities::NoTerminal),
        checkpointer: None,
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
            CheckpointCollector::new(),
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
            CheckpointCollector::new(),
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
            CheckpointCollector::new(),
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
        .invoke(
            "shell",
            serde_json::json!({}),
            CancellationToken::new(),
            tx,
            CheckpointCollector::new(),
        )
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

#[tokio::test]
async fn the_disk_backend_serves_the_tools_and_refuses_a_symlink_escape() {
    let ws = hatchery_testkit::TempWorkspace::new();
    ws.write("src/lib.rs", "pub fn real() {}\n");
    // A symlink pointing outside the workspace: classic escape attempt.
    std::os::unix::fs::symlink("/etc/hostname", ws.root().join("outside")).expect("symlink");

    let registry = registry_on_disk(ws.root());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

    let inside = registry
        .invoke(
            "read_file",
            serde_json::json!({"path": "src/lib.rs"}),
            CancellationToken::new(),
            tx.clone(),
            CheckpointCollector::new(),
        )
        .await
        .expect("read inside");
    assert!(!inside.is_error);
    assert!(inside.output.text.contains("pub fn real()"));

    let escaped = registry
        .invoke(
            "read_file",
            serde_json::json!({"path": "outside"}),
            CancellationToken::new(),
            tx,
            CheckpointCollector::new(),
        )
        .await
        .expect("dispatched");
    assert!(
        escaped.is_error,
        "a symlink out of the workspace must be refused, got: {}",
        escaped.output.text
    );
}

#[tokio::test]
async fn glob_and_grep_walk_the_real_disk() {
    let ws = hatchery_testkit::TempWorkspace::new();
    ws.write("src/one.rs", "fn needle() {}\n");
    ws.write("src/two.rs", "fn other() {}\n");
    ws.write("docs/readme.md", "no code here\n");

    let registry = registry_on_disk(ws.root());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

    let globbed = registry
        .invoke(
            "glob",
            serde_json::json!({"pattern": "src/*.rs"}),
            CancellationToken::new(),
            tx.clone(),
            CheckpointCollector::new(),
        )
        .await
        .expect("glob");
    assert!(
        globbed.output.text.contains("src/one.rs"),
        "{}",
        globbed.output.text
    );
    assert!(globbed.output.text.contains("src/two.rs"));

    let grepped = registry
        .invoke(
            "grep",
            serde_json::json!({"pattern": "needle", "include": "*.rs"}),
            CancellationToken::new(),
            tx,
            CheckpointCollector::new(),
        )
        .await
        .expect("grep");
    assert!(
        grepped.output.text.contains("src/one.rs:1: fn needle()"),
        "{}",
        grepped.output.text
    );
}

#[tokio::test]
async fn a_symlink_loop_is_skipped_not_fatal_for_the_walk() {
    let ws = hatchery_testkit::TempWorkspace::new();
    ws.write("plain.txt", "content\n");
    std::os::unix::fs::symlink(ws.root().join("loop"), ws.root().join("loop"))
        .expect("self-symlink");

    let registry = registry_on_disk(ws.root());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let found = registry
        .invoke(
            "glob",
            serde_json::json!({"pattern": "**/*.txt"}),
            CancellationToken::new(),
            tx,
            CheckpointCollector::new(),
        )
        .await
        .expect("the walk survives the loop");
    assert!(
        found.output.text.contains("plain.txt"),
        "{}",
        found.output.text
    );
}
