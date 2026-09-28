//! Crash recovery: a writer killed with no chance to clean up must leave committed work behind.
//!
//! The child process is this same test binary, re-entered with an environment variable and the
//! name of an ignored test. That was chosen over a dedicated `[[bin]]` target or a helper crate:
//! nothing extra is built, nothing extra is shipped, and the child gets the real `TursoStore` code
//! rather than a copy of it that could drift.
//!
//! `Child::kill` is `SIGKILL` on unix and `TerminateProcess` on Windows, so the child dies without
//! running a destructor, without a final `shutdown()`, and without a WAL checkpoint — which is
//! exactly the crash the store claims to survive.
//!
//! What is *not* covered here: a power cut. The M0 spike measured that `PRAGMA synchronous` has no
//! measurable effect in this engine, so nothing below the process boundary is proven
//! (`docs/design/storage.md` open question 4).

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use hatchery_protocol::{
    Content, Item, ItemId, ItemKind, ModelRef, Session, SessionId, SessionModeId, SessionStatus,
    StopReason, Timestamp, TurnCompletion, TurnId, Usage,
};
use hatchery_store::{SessionStore, TursoStore};

/// Set on the child so it knows what to do before hanging.
const PROBE_SPEC: &str = "HATCHERY_CRASH_PROBE";
/// The database file the child writes to.
const PROBE_DB: &str = "HATCHERY_CRASH_DB";
/// How long the parent waits for the child to report that it has committed.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// What the child did, so the parent knows what to expect after the kill.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ProbeReport {
    session: SessionId,
    /// Every item the child committed, in order.
    items: Vec<ItemId>,
    /// The branch head the child left behind.
    head: Option<ItemId>,
    /// How many items a deleted branch took with it.
    deleted: Option<u64>,
    /// The turn the child started, when it started one.
    turn: Option<TurnId>,
}

/// What the child should do before it hangs.
fn operations(spec: &str) -> Vec<Operation> {
    match spec {
        "append" => vec![Operation::Append(5)],
        "edit_fork" => vec![Operation::Append(3), Operation::EditFork],
        "switch_branch" => vec![Operation::Append(3), Operation::SwitchToFirst],
        "delete_branch" => vec![
            Operation::Append(3),
            Operation::SwitchToFirst,
            Operation::DeleteSecond,
        ],
        "finish_turn" => vec![
            Operation::StartTurn,
            Operation::Append(1),
            Operation::FinishTurn,
        ],
        other => panic!("unknown probe spec {other:?}"),
    }
}

#[derive(Clone, Copy, Debug)]
enum Operation {
    /// Appends n chained messages.
    Append(usize),
    /// Starts a turn.
    StartTurn,
    /// Forks the second item.
    EditFork,
    /// Points the head at the first item.
    SwitchToFirst,
    /// Deletes the subtree rooted at the second item.
    DeleteSecond,
    /// Finishes the turn successfully.
    FinishTurn,
}

// ------------------------------------------------------------------- the child

/// The re-entered child: commits, reports, then hangs until it is killed.
///
/// Ignored so a normal test run never executes it; the parent names it explicitly.
#[tokio::test]
#[ignore = "re-entered as a child process by the parent test"]
async fn crash_probe_child() {
    let spec = std::env::var(PROBE_SPEC).expect("the parent sets the probe spec");
    let path = std::env::var(PROBE_DB).expect("the parent sets the database path");
    let store = TursoStore::open(&path).await.expect("open the database");

    let now = Timestamp::now();
    let session = store
        .create_session(Session {
            id: SessionId::new(),
            title: Some("crash probe".to_owned()),
            mode: SessionModeId::code(),
            workspace: None,
            model: ModelRef::new("deepseek", "deepseek-chat"),
            config_patch: None,
            created_at: now,
            updated_at: now,
            active_branch_head: None,
            generation: 0,
            status: SessionStatus::Idle,
        })
        .await
        .expect("create the session");

    let mut report = ProbeReport {
        session: session.id,
        items: Vec::new(),
        head: None,
        deleted: None,
        turn: None,
    };
    let mut parent = None;

    for operation in operations(&spec) {
        match operation {
            Operation::Append(count) => {
                for index in 0..count {
                    let item = Item::new(
                        session.id,
                        ItemKind::UserMessage(Content::text(format!("item {index}"))),
                    );
                    let item = match parent {
                        Some(parent) => item.with_parent(parent),
                        None => item,
                    };
                    store.append_item(item.clone()).await.expect("append");
                    parent = Some(item.id);
                    report.items.push(item.id);
                }
            }
            Operation::StartTurn => {
                let turn = TurnId::new();
                store
                    .start_turn(session.id, turn, Timestamp::now())
                    .await
                    .expect("start the turn");
                report.turn = Some(turn);
            }
            Operation::EditFork => {
                let target = report.items[1];
                let forked = store
                    .edit_fork(session.id, target, Content::text("edited"))
                    .await
                    .expect("fork");
                report.items.push(forked.id);
                parent = Some(forked.id);
            }
            Operation::SwitchToFirst => {
                store
                    .switch_branch(session.id, report.items[0])
                    .await
                    .expect("switch");
            }
            Operation::DeleteSecond => {
                let deleted = store
                    .delete_branch(session.id, report.items[1])
                    .await
                    .expect("delete");
                report.deleted = Some(deleted);
            }
            Operation::FinishTurn => {
                let turn = report.turn.expect("a turn was started");
                store
                    .finish_turn(
                        session.id,
                        turn,
                        Some(
                            TurnCompletion::new(StopReason::ModelDone).with_usage(Usage {
                                prompt_tokens: Some(4),
                                completion_tokens: Some(2),
                                reasoning_tokens: None,
                                requests: 1,
                            }),
                        ),
                    )
                    .await
                    .expect("finish the turn");
            }
        }
    }

    let session = store.session(session.id).await.expect("read back");
    report.head = session.active_branch_head;

    // Flush stdout before hanging: the parent is waiting for exactly this line.
    println!(
        "ready {}",
        serde_json::to_string(&report).expect("a serialisable report")
    );
    use std::io::Write as _;
    std::io::stdout().flush().expect("flush stdout");

    // No shutdown, no drop of the store through a graceful path: the parent kills this process.
    std::thread::sleep(Duration::from_secs(600));
}

// ------------------------------------------------------------------ the parent

/// A killed child, and the database it left behind.
struct Recovered {
    store: TursoStore,
    report: ProbeReport,
    path: PathBuf,
    _dir: tempfile::TempDir,
}

impl Recovered {
    /// The messages on the active branch, in order.
    async fn chain_texts(&self) -> Vec<String> {
        self.store
            .rebuild_chain(self.report.session, None)
            .await
            .expect("rebuild after the crash")
            .into_iter()
            .filter_map(|item| match item.kind {
                ItemKind::UserMessage(content) | ItemKind::AssistantMessage(content) => {
                    Some(content.text)
                }
                _ => None,
            })
            .collect()
    }
}

/// Kills a pooled child even when a test fails before it gets to.
struct ChildGuard {
    child: Child,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs the child, waits for it to report, kills it, and reopens the database.
async fn kill_and_reopen(spec: &str) -> Recovered {
    let dir = tempfile::tempdir().expect("a tempdir");
    let path = dir.path().join("hatchery.db");

    let child = Command::new(std::env::current_exe().expect("the test binary's path"))
        // The child is a test in this very binary: name it exactly, and let ignored tests run for
        // this one invocation.
        .args(["--exact", "crash_probe_child", "--ignored", "--nocapture"])
        .env(PROBE_SPEC, spec)
        .env(PROBE_DB, &path)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn the probe child");
    let mut guard = ChildGuard { child };

    let stdout = guard.child.stdout.take().expect("stdout is piped");
    let mut reader = BufReader::new(stdout);
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut report = None;
    let mut line = String::new();
    while Instant::now() < deadline {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if let Some(json) = line.trim().strip_prefix("ready ") {
                    report = Some(
                        serde_json::from_str::<ProbeReport>(json)
                            .unwrap_or_else(|error| panic!("unreadable report {json:?}: {error}")),
                    );
                    break;
                }
                // Anything else the child printed, e.g. libtest's own output.
            }
            Err(error) => panic!("reading the child's stdout failed: {error}"),
        }
    }
    let report = report
        .unwrap_or_else(|| panic!("the probe child never reported ready within {READY_TIMEOUT:?}"));

    // The kill is the experiment: no destructor, no shutdown, no checkpoint.
    guard.child.kill().expect("kill the child");
    let status = guard.child.wait().expect("reap the child");
    assert!(
        !status.success(),
        "the child was supposed to be killed, not to exit cleanly: {status}"
    );

    let store = TursoStore::open(&path)
        .await
        .expect("reopen the database after the crash");
    Recovered {
        store,
        report,
        path,
        _dir: dir,
    }
}

#[tokio::test]
async fn committed_appends_survive_a_killed_writer() {
    let recovered = kill_and_reopen("append").await;
    let chain = recovered
        .store
        .rebuild_chain(recovered.report.session, None)
        .await
        .expect("rebuild");

    assert_eq!(chain.len(), 5, "every committed item is still there");
    assert_eq!(
        chain.iter().map(|item| item.id).collect::<Vec<_>>(),
        recovered.report.items,
        "and in the order they were committed"
    );
    assert_eq!(
        recovered
            .store
            .session(recovered.report.session)
            .await
            .expect("session")
            .active_branch_head,
        recovered.report.head
    );

    // The WAL is what made that possible: a clean shutdown would have checkpointed it away.
    let wal = recovered.path.with_extension("db-wal");
    assert!(
        wal.exists(),
        "expected the write-ahead log next to the database at {}",
        wal.display()
    );
}

#[tokio::test]
async fn a_forked_branch_survives_a_killed_writer() {
    let recovered = kill_and_reopen("edit_fork").await;
    let chain = recovered.chain_texts().await;
    assert_eq!(
        chain,
        vec!["item 0", "edited"],
        "the branch the child left active is intact"
    );
    assert_eq!(
        recovered
            .store
            .branch_tree(recovered.report.session)
            .await
            .expect("tree")
            .nodes
            .len(),
        4,
        "and so is the branch it forked away from"
    );
}

#[tokio::test]
async fn a_branch_switch_survives_a_killed_writer() {
    let recovered = kill_and_reopen("switch_branch").await;
    let session = recovered
        .store
        .session(recovered.report.session)
        .await
        .expect("session");
    assert_eq!(
        session.active_branch_head,
        Some(recovered.report.items[0]),
        "the head the child moved is where it left it"
    );
    assert_eq!(
        recovered.chain_texts().await,
        vec!["item 0"],
        "and it decides what the branch is"
    );
}

#[tokio::test]
async fn a_branch_deletion_survives_a_killed_writer() {
    let recovered = kill_and_reopen("delete_branch").await;
    assert_eq!(
        recovered.report.deleted,
        Some(2),
        "the child deleted two items"
    );
    assert_eq!(
        recovered.chain_texts().await,
        vec!["item 0"],
        "the surviving branch is what the cascade left"
    );
    assert!(matches!(
        recovered
            .store
            .item(recovered.report.session, recovered.report.items[2])
            .await,
        Err(hatchery_store::StoreError::ItemNotFound(_))
    ));
}

#[tokio::test]
async fn a_finished_turn_survives_a_killed_writer() {
    let recovered = kill_and_reopen("finish_turn").await;
    let turn = recovered.report.turn.expect("the child started a turn");

    // Read the row through the engine: no store API exposes turn statistics yet.
    let text = recovered.path.to_string_lossy().into_owned();
    let database = turso::Builder::new_local(&text)
        .build()
        .await
        .expect("open directly");
    let conn = database.connect().expect("connect");
    let mut rows = conn
        .query(
            "SELECT ended_at, stop_reason FROM turns WHERE id = ?1",
            [turn.to_string()],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("step").expect("the turn row");
    assert!(
        matches!(row.get_value(0), Ok(turso::Value::Integer(_))),
        "the turn is marked as ended"
    );
    assert_eq!(
        row.get_value(1).expect("a value"),
        turso::Value::Text("model_done".to_owned()),
        "and its stop reason came through"
    );
}

#[tokio::test]
async fn the_store_is_usable_after_recovering_from_a_crash() {
    // A recovery that leaves the database read-only, or with a stale lock, would be no recovery at
    // all: the daemon has to be able to carry on.
    let recovered = kill_and_reopen("append").await;
    let session = recovered
        .store
        .session(recovered.report.session)
        .await
        .expect("session");
    let next = Item::new(
        session.id,
        ItemKind::AssistantMessage(Content::text("after the crash")),
    )
    .with_parent(recovered.report.head.expect("a head to chain onto"));
    recovered.store.append_item(next).await.expect("append");

    assert_eq!(
        recovered.chain_texts().await,
        vec![
            "item 0",
            "item 1",
            "item 2",
            "item 3",
            "item 4",
            "after the crash"
        ]
    );
    recovered.store.shutdown().await.expect("shutdown");
}
