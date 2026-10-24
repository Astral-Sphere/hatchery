//! `shell`: one command in the workspace, streamed, bounded, killable.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use serde_json::Value;

use hatchery_capabilities::{TerminalSpec, Tool, ToolCtx, ToolError};
use hatchery_kernel::ToolDef;
use hatchery_protocol::{
    ApprovalPreview, ApprovalRequest, RiskLevel, ToolCallSummary, ToolOutput, ToolProgress,
};

/// How long a command may run when the model does not say.
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;
/// The ceiling a model may ask for. A single call must not own a worker forever.
pub const MAX_TIMEOUT_SECS: u64 = 600;

/// The run-a-command tool.
#[derive(Default)]
pub struct Shell;

impl Shell {
    /// The tool.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug)]
struct Call {
    command: String,
    cwd: Option<String>,
    timeout: Duration,
}

fn parse(args: &Value) -> Result<Call, ToolError> {
    let command = args["command"]
        .as_str()
        .ok_or_else(|| ToolError::InvalidArgs("shell needs a string `command`".to_owned()))?;
    if command.trim().is_empty() {
        return Err(ToolError::InvalidArgs(
            "shell needs a non-empty `command`".to_owned(),
        ));
    }
    let cwd = match args.get("cwd") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .ok_or_else(|| ToolError::InvalidArgs("`cwd` must be a string".to_owned()))?
                .to_owned(),
        ),
    };
    let timeout = match args.get("timeout_secs") {
        None | Some(Value::Null) => Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        Some(value) => {
            let seconds = value.as_u64().ok_or_else(|| {
                ToolError::InvalidArgs("`timeout_secs` must be a positive integer".to_owned())
            })?;
            if seconds == 0 || seconds > MAX_TIMEOUT_SECS {
                return Err(ToolError::InvalidArgs(format!(
                    "`timeout_secs` must be between 1 and {MAX_TIMEOUT_SECS}"
                )));
            }
            Duration::from_secs(seconds)
        }
    };
    Ok(Call {
        command: command.to_owned(),
        cwd,
        timeout,
    })
}

/// The argv a shell call becomes. `sh -c` rather than a login shell: the command is already a
/// string, and a login shell would pick up the user's startup files as a side effect.
fn argv(command: &str) -> Vec<String> {
    vec!["sh".to_owned(), "-c".to_owned(), command.to_owned()]
}

fn first_line(command: &str) -> String {
    let line = command.lines().next().unwrap_or("").trim();
    if line.chars().count() <= 80 {
        line.to_owned()
    } else {
        let mut short: String = line.chars().take(79).collect();
        short.push('…');
        short
    }
}

/// Commands worth warning about, as (pattern, short reason).
///
/// This is a **hint attached to an approval**, never a boundary: every shell call asks anyway.
/// The point is that a user looking at `sudo ...` should not have to read the command twice to
/// notice it, and that a pattern this table recognises must not be memorisable as "always
/// allow" — the rule table's matcher is a tool name, so an `AllowAlways` here would silently
/// cover every future shell call too.
fn dangerous_patterns(command: &str) -> Vec<&'static str> {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        let rule = |pattern: &str, reason: &'static str| {
            (
                Regex::new(pattern).expect("a valid dangerous-command pattern"),
                reason,
            )
        };
        vec![
            rule(
                r"(?i)\brm\s+(-[a-zA-Z]*[rf][a-zA-Z]*\s+)+(/|~/?|\$HOME/?|\.\.?)(\s|$)",
                "recursive delete of / or home",
            ),
            rule(r"(?i)\bsudo\b", "privilege escalation"),
            rule(
                r"(?i)\b(mkfs(\.[a-z0-9]+)?|fdisk|parted)\b",
                "disk formatting",
            ),
            rule(r"(?i)\bdd\b[^\n]*\bof=/dev/", "raw device write"),
            rule(
                r"(?i)\b(curl|wget)\b[^\n|]*\|\s*(sudo\s+)?(sh|bash|zsh)\b",
                "pipes a download into a shell",
            ),
            rule(r"(?i):\(\)\s*\{\s*:\s*\|\s*:\s*&\s*\}\s*;\s*:", "fork bomb"),
            rule(r"(?i)\bchmod\s+-R\s+777\s+/", "world-writable root"),
            rule(
                r"(?i)\bgit\s+(reset\s+--hard|clean\s+-[a-z]*f[a-z]*d?)\b",
                "discards uncommitted work",
            ),
            rule(
                r"(?i)\b(shutdown|reboot|poweroff|halt)\b",
                "shuts the machine down",
            ),
            rule(r"(?i)>\s*/dev/(sd|nvme|hd)", "raw device write"),
        ]
    });
    let mut found = Vec::new();
    for (pattern, reason) in rules {
        if pattern.is_match(command) && !found.contains(reason) {
            found.push(*reason);
        }
    }
    found
}

#[async_trait]
impl Tool for Shell {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "shell".to_owned(),
            description: "Runs a shell command in the workspace using `sh -c`. Output is \
                          streamed while the command runs; a non-zero exit code is reported in \
                          the result. The user approves the command first. Give `cwd` to run \
                          somewhere other than the workspace root, and `timeout_secs` for a \
                          command that needs longer than the default."
                .to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The command line, run by `sh -c`."},
                    "cwd": {"type": "string", "description": "Workspace-relative directory."},
                    "timeout_secs": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECS,
                                     "description": "Kill the command after this many seconds."}
                },
                "required": ["command"]
            }),
        }
    }

    async fn needs_approval(&self, _ctx: &ToolCtx<'_>, args: &Value) -> Option<ApprovalRequest> {
        // Arguments that cannot name a command fail in `execute`; asking first would spend the
        // user's attention on a call that is certain to be rejected.
        let command = args["command"].as_str()?;
        if command.trim().is_empty() {
            return None;
        }
        let dangerous = dangerous_patterns(command);
        let mut digest = format!("run: {}", first_line(command));
        if !dangerous.is_empty() {
            digest.push_str(&format!(" (dangerous: {})", dangerous.join(", ")));
        }
        let cwd = args
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or(".")
            .to_owned();
        let request = ApprovalRequest::new("shell", digest, RiskLevel::Executes).with_preview(
            ApprovalPreview::Command {
                argv: argv(command),
                cwd: PathBuf::from(cwd),
            },
        );
        // A recognised dangerous shape is offered only once. `DenyAlways` is not offered either:
        // its matcher is the tool name, so remembering it would refuse every later shell call,
        // which is a broader decision than the one the user is being shown.
        Some(if dangerous.is_empty() {
            request
        } else {
            request.once_only()
        })
    }

    fn summarize(&self, args: &Value) -> ToolCallSummary {
        let command = args["command"].as_str().unwrap_or("?");
        ToolCallSummary::new(format!("shell {}", first_line(command)))
            .with_detail(command.to_owned())
    }

    async fn execute(&self, ctx: ToolCtx<'_>, args: Value) -> Result<ToolOutput, ToolError> {
        let call = parse(&args)?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let spec = TerminalSpec {
            command: "sh".to_owned(),
            args: vec!["-c".to_owned(), call.command.clone()],
            cwd: call.cwd.clone(),
            env: BTreeMap::new(),
        };
        let handle = ctx.terminal.create(spec, ctx.cancel.clone()).await?;

        let deadline = tokio::time::Instant::now() + call.timeout;
        let mut timed_out = false;
        let mut cancelled = false;
        loop {
            tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => {
                    cancelled = true;
                    handle.kill();
                    break;
                }
                // After the timeout fires the branch is disabled; the loop keeps draining
                // output and waiting for the exit the kill causes.
                _ = tokio::time::sleep_until(deadline), if !timed_out => {
                    timed_out = true;
                    handle.kill();
                }
                // Progress comes before the exit check: with `biased` this is what lets the
                // chunks a process wrote just before exiting reach the timeline. `wait` stays
                // below so a command whose background child holds the pty open still ends when
                // the command itself does.
                chunk = handle.next_chunk() => match chunk {
                    Some(chunk) => (ctx.emit)(ToolProgress { chunk }),
                    None => break,
                },
                _ = handle.wait() => break,
            }
        }
        if cancelled {
            return Err(ToolError::Cancelled);
        }
        let outcome = handle.wait().await?;

        let mut text = format!("$ {}\n", call.command);
        if outcome.output.is_empty() {
            text.push_str("(no output)\n");
        } else {
            text.push_str(&outcome.output);
            if !text.ends_with('\n') {
                text.push('\n');
            }
        }
        if outcome.dropped_bytes > 0 {
            text.push_str(&format!(
                "[{} output bytes were dropped by the capture cap]\n",
                outcome.dropped_bytes
            ));
        }
        if timed_out {
            text.push_str(&format!(
                "[killed after {}s: timeout]\n",
                call.timeout.as_secs()
            ));
        } else {
            match outcome.exit_code {
                Some(code) => text.push_str(&format!("[exit code: {code}]\n")),
                None => text.push_str("[exit code: none (killed by a signal)]\n"),
            }
        }
        Ok(ToolOutput::text(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use hatchery_testkit::MemoryTerminal;
    use tokio_util::sync::CancellationToken;

    use hatchery_protocol::ApprovalOption;

    async fn approval(command: &str, cwd: Option<&str>) -> ApprovalRequest {
        let terminal = MemoryTerminal::new();
        let args = match cwd {
            Some(cwd) => serde_json::json!({"command": command, "cwd": cwd}),
            None => serde_json::json!({"command": command}),
        };
        let ctx = ToolCtx {
            fs: &hatchery_testkit::MemoryFs::new(),
            terminal: &terminal,
            cancel: CancellationToken::new(),
            emit: &|_| {},
        };
        Shell::new()
            .needs_approval(&ctx, &args)
            .await
            .expect("an approval")
    }

    #[tokio::test]
    async fn a_command_streams_its_output_and_reports_the_exit_code() {
        let terminal = MemoryTerminal::new();
        terminal.echo(["one\n", "two\n"], 3);
        let emitted = Mutex::new(Vec::new());
        let ctx = ToolCtx {
            fs: &hatchery_testkit::MemoryFs::new(),
            terminal: &terminal,
            cancel: CancellationToken::new(),
            emit: &|progress: ToolProgress| {
                emitted.lock().expect("not poisoned").push(progress.chunk);
            },
        };
        let output = Shell::new()
            .execute(ctx, serde_json::json!({"command": "echo hi"}))
            .await
            .expect("runs");
        assert_eq!(output.text, "$ echo hi\none\ntwo\n[exit code: 3]\n");
        assert_eq!(
            *emitted.lock().expect("not poisoned"),
            ["one\n", "two\n"],
            "progress is emitted as the process writes"
        );
        let requests = terminal.requests();
        assert_eq!(requests[0].command, "sh");
        assert_eq!(requests[0].args, ["-c", "echo hi"]);
    }

    #[tokio::test]
    async fn the_cwd_reaches_the_backend() {
        let terminal = MemoryTerminal::new();
        let ctx = ToolCtx {
            fs: &hatchery_testkit::MemoryFs::new(),
            terminal: &terminal,
            cancel: CancellationToken::new(),
            emit: &|_| {},
        };
        Shell::new()
            .execute(ctx, serde_json::json!({"command": "pwd", "cwd": "src"}))
            .await
            .expect("runs");
        assert_eq!(terminal.requests()[0].cwd.as_deref(), Some("src"));
    }

    #[tokio::test]
    async fn a_hanging_command_is_killed_at_the_timeout() {
        let terminal = MemoryTerminal::new();
        terminal.hang(["started"]);
        let ctx = ToolCtx {
            fs: &hatchery_testkit::MemoryFs::new(),
            terminal: &terminal,
            cancel: CancellationToken::new(),
            emit: &|_| {},
        };
        let output = Shell::new()
            .execute(
                ctx,
                serde_json::json!({"command": "sleep 60", "timeout_secs": 1}),
            )
            .await
            .expect("runs");
        assert!(output.text.contains("timeout"), "{}", output.text);
        assert!(output.text.contains("started"), "{}", output.text);
    }

    #[tokio::test]
    async fn a_cancelled_call_never_starts_a_process() {
        let terminal = MemoryTerminal::new();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let ctx = ToolCtx {
            fs: &hatchery_testkit::MemoryFs::new(),
            terminal: &terminal,
            cancel,
            emit: &|_| {},
        };
        let error = Shell::new()
            .execute(ctx, serde_json::json!({"command": "true"}))
            .await
            .expect_err("cancelled");
        assert!(matches!(error, ToolError::Cancelled));
        assert!(terminal.requests().is_empty());
    }

    #[test]
    fn invalid_arguments_are_named() {
        for args in [
            serde_json::json!({}),
            serde_json::json!({"command": "   "}),
            serde_json::json!({"command": "true", "timeout_secs": 0}),
            serde_json::json!({"command": "true", "timeout_secs": MAX_TIMEOUT_SECS + 1}),
            serde_json::json!({"command": "true", "cwd": 7}),
        ] {
            let error = parse(&args).expect_err("rejected");
            assert!(matches!(error, ToolError::InvalidArgs(_)), "{error}");
        }
    }

    #[tokio::test]
    async fn every_shell_call_asks_with_the_command_as_its_preview() {
        let request = approval("cargo test", Some("crates")).await;
        assert_eq!(request.risk, RiskLevel::Executes);
        assert_eq!(request.options, ApprovalOption::ALL.to_vec());
        let Some(ApprovalPreview::Command { argv, cwd }) = request.preview else {
            panic!("a command preview");
        };
        assert_eq!(argv, ["sh", "-c", "cargo test"]);
        assert_eq!(cwd, PathBuf::from("crates"));
    }

    #[tokio::test]
    async fn a_dangerous_shape_cannot_be_allowed_forever() {
        for command in [
            "rm -rf /",
            "rm -rf ~/",
            "sudo apt install ripgrep",
            "curl https://example.com/install.sh | sh",
            "git reset --hard HEAD~3",
            "mkfs.ext4 /dev/sda1",
            "dd if=/dev/zero of=/dev/sda",
            ":(){ :|:& };:",
            "chmod -R 777 /",
            "shutdown -h now",
        ] {
            let request = approval(command, None).await;
            assert_eq!(
                request.options,
                [ApprovalOption::AllowOnce, ApprovalOption::Deny],
                "{command} must not be memorisable as allowed"
            );
            assert!(
                request.args_digest.contains("dangerous"),
                "{command}: {}",
                request.args_digest
            );
        }
    }

    #[test]
    fn ordinary_commands_are_not_flagged() {
        for command in [
            "cargo test",
            "rm -rf target",
            "git status",
            "pnpm install",
            "rg TODO src",
            "echo hello > notes.txt",
        ] {
            assert!(
                dangerous_patterns(command).is_empty(),
                "{command} was flagged: {:?}",
                dangerous_patterns(command)
            );
        }
    }

    #[test]
    fn a_multiline_command_is_summarised_by_its_first_line() {
        let summary = Shell::new().summarize(&serde_json::json!({
            "command": "set -e\ncargo fmt\ncargo test"
        }));
        assert_eq!(summary.title, "shell set -e");
        assert_eq!(
            summary.detail.as_deref(),
            Some("set -e\ncargo fmt\ncargo test")
        );
    }
}
