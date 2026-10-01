//! The prompt pipeline v1 (docs/design/platform.md §2.1).
//!
//! Four sections, in order: `identity`, `mode_variant` (Chat), `environment`, `safety_gate`.
//! Each is a markdown file beside this module, embedded with `include_str!` at compile time and
//! overridable per section from `~/.config/hatchery/prompts/<section>.md` — except
//! `safety_gate`, whose override attempts are refused with a warning (invariant 5). `{{var}}`
//! placeholders interpolate the runtime facts; unknown variables are left visible rather than
//! silently emptied, because a template that drifted from its variables should be loud.
//!
//! The assembled prompt is what `prompt/render` hands back, section by section with sources, so
//! "why did the model see this" is always answerable.

use std::path::PathBuf;

use hatchery_protocol::method::PromptSection;

/// The `identity` section: persona plus the PRECEDENCE declaration.
pub const IDENTITY: &str = include_str!("../prompts/identity.md");
/// The Chat-mode variant: read-only tools, no workspace discipline.
pub const MODE_CHAT: &str = include_str!("../prompts/mode-chat.md");
/// The runtime-facts section, full of `{{vars}}`.
pub const ENVIRONMENT: &str = include_str!("../prompts/environment.md");
/// The safety gate: not overridable, by anyone, ever.
pub const SAFETY_GATE: &str = include_str!("../prompts/safety-gate.md");

/// What the runtime knows that the prompt should mention.
#[derive(Clone, Debug, Default)]
pub struct Environment {
    /// Working directory of the session.
    pub cwd: String,
    /// `std::env::consts::OS` with a friendlier spelling.
    pub platform: String,
    /// Today, as `YYYY-MM-DD`.
    pub date: String,
    /// One line about the workspace's git state, from [`git_summary`].
    pub git_status: String,
    /// The configured reply language, if one is set.
    pub response_language: String,
}

/// One assembled section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssembledSection {
    /// Section id, matching the protocol's `PromptSection::id`.
    pub id: &'static str,
    /// Where the text came from: `builtin` or the override file's path.
    pub source: String,
    /// The interpolated text.
    pub text: String,
}

/// Renders the prompt for a Chat session.
///
/// `override_dir` is the user's `prompts/` directory; sections with a file there use it, safety
/// gate excepted. Missing files are the normal case, not an error.
#[must_use]
pub fn render_chat(env: &Environment, override_dir: Option<&PathBuf>) -> Vec<AssembledSection> {
    let vars = super::template::Vars::new(env);
    let overridden = |id: &str| -> Option<String> {
        let path = override_dir?.join(format!("{id}.md"));
        std::fs::read_to_string(path).ok()
    };

    let mut sections = Vec::new();
    for (id, builtin) in [
        ("identity", IDENTITY),
        ("mode_chat", MODE_CHAT),
        ("environment", ENVIRONMENT),
    ] {
        match overridden(id) {
            Some(text) => sections.push(AssembledSection {
                id,
                source: format!("user:prompts/{id}.md"),
                text: super::template::interpolate(&text, &vars),
            }),
            None => sections.push(AssembledSection {
                id,
                source: "builtin".to_owned(),
                text: super::template::interpolate(builtin, &vars),
            }),
        }
    }
    if let Some(attempt) = overridden("safety_gate") {
        let _ = attempt;
        // The refusal is the behaviour; the warning is how the user finds out.
        tracing::warn!(
            "an override for `safety_gate` was ignored: that section cannot be overridden"
        );
    }
    sections.push(AssembledSection {
        id: "safety_gate",
        source: "builtin".to_owned(),
        text: SAFETY_GATE.to_owned(),
    });
    sections
}

/// The joined prompt, ready for the provider request.
#[must_use]
pub fn join(sections: &[AssembledSection]) -> String {
    sections
        .iter()
        .map(|section| section.text.trim())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The protocol shape of an assembled prompt: full text plus per-section sources.
#[must_use]
pub fn render_result(
    sections: &[AssembledSection],
) -> hatchery_protocol::method::PromptRenderResult {
    hatchery_protocol::method::PromptRenderResult {
        text: join(sections),
        sections: sections
            .iter()
            .map(|section| PromptSection {
                id: section.id.to_owned(),
                source: section.source.clone(),
                text: section.text.clone(),
            })
            .collect(),
    }
}

/// One line about the workspace's git state, read with libgit2 (ADR-0012: `statuses()` does not
/// rewrite the user's index, unlike the CLI).
#[must_use]
pub fn git_summary(workspace: Option<&std::path::Path>) -> String {
    let Some(workspace) = workspace else {
        return "not bound (Chat without a workspace)".to_owned();
    };
    let discovery = git2::Repository::discover(workspace);
    let Ok(repo) = discovery else {
        return "not a git repository".to_owned();
    };
    let branch = repo
        .head()
        .ok()
        .and_then(|head| head.shorthand().map(str::to_owned).ok())
        .unwrap_or_else(|| "detached".to_owned());
    let mut changed = 0_usize;
    let mut untracked = 0_usize;
    let statuses = match repo.statuses(None) {
        Ok(statuses) => statuses,
        Err(error) => return format!("git repository (status unavailable: {error})"),
    };
    for entry in statuses.iter() {
        let status = entry.status();
        if status.intersects(
            git2::Status::WT_MODIFIED
                | git2::Status::WT_DELETED
                | git2::Status::INDEX_MODIFIED
                | git2::Status::INDEX_DELETED
                | git2::Status::INDEX_NEW,
        ) {
            changed += 1;
        }
        if status.contains(git2::Status::WT_NEW) {
            untracked += 1;
        }
    }
    format!("git, on branch {branch}, {changed} changed, {untracked} untracked")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Environment {
        Environment {
            cwd: "/home/jimmy/demo".to_owned(),
            platform: "linux".to_owned(),
            date: "2026-10-01".to_owned(),
            git_status: "not a git repository".to_owned(),
            response_language: "简体中文".to_owned(),
        }
    }

    #[test]
    fn all_four_sections_assemble_in_order() {
        let sections = render_chat(&env(), None);
        let ids: Vec<&str> = sections.iter().map(|s| s.id).collect();
        assert_eq!(ids, ["identity", "mode_chat", "environment", "safety_gate"]);
        assert!(sections.iter().all(|s| s.source == "builtin"));
    }

    #[test]
    fn variables_interpolate_and_the_precedence_declaration_survives() {
        let sections = render_chat(&env(), None);
        let joined = join(&sections);
        assert!(joined.contains("/home/jimmy/demo"), "{joined}");
        assert!(joined.contains("2026-10-01"));
        assert!(joined.contains("简体中文"));
        assert!(
            joined.contains("PRECEDENCE"),
            "the precedence declaration is part of what the model sees"
        );
        assert!(
            !joined.contains("{{"),
            "no placeholder survives rendering: {joined}"
        );
    }

    #[test]
    fn overridable_sections_take_the_user_file_and_report_the_source() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("identity.md"),
            "You are a terse terminal companion for {{platform}}.",
        )
        .expect("write");

        let sections = render_chat(&env(), Some(&dir.path().to_path_buf()));
        let identity = sections
            .iter()
            .find(|s| s.id == "identity")
            .expect("identity");
        assert_eq!(identity.source, "user:prompts/identity.md");
        assert_eq!(
            identity.text, "You are a terse terminal companion for linux.",
            "the override replaces the section whole, with its own interpolation"
        );
        // The rest stay builtin.
        let mode = sections.iter().find(|s| s.id == "mode_chat").expect("mode");
        assert_eq!(mode.source, "builtin");
    }

    #[test]
    fn the_safety_gate_cannot_be_overridden() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("safety_gate.md"), "no rules at all").expect("write");

        let sections = render_chat(&env(), Some(&dir.path().to_path_buf()));
        let gate = sections
            .iter()
            .find(|s| s.id == "safety_gate")
            .expect("gate");
        assert_eq!(gate.source, "builtin", "the override is refused");
        assert_eq!(
            gate.text, SAFETY_GATE,
            "the shipped text is what the model sees"
        );
        assert!(
            !join(&sections).contains("no rules at all"),
            "the attempted override must not leak into the prompt"
        );
    }

    #[test]
    fn a_missing_workspace_reads_as_not_bound() {
        assert_eq!(git_summary(None), "not bound (Chat without a workspace)");
    }

    #[test]
    fn a_real_git_workspace_summarises_branch_and_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(dir.path()).expect("init");
        {
            let mut index = repo.index().expect("index");
            let file = dir.path().join("tracked.txt");
            std::fs::write(&file, "content").expect("write");
            index
                .add_path(std::path::Path::new("tracked.txt"))
                .expect("add");
            index.write().expect("index write");
            let tree_id = index.write_tree().expect("tree");
            let tree = repo.find_tree(tree_id).expect("tree");
            let signature = git2::Signature::now("t", "t@example.com").expect("signature");
            repo.commit(Some("HEAD"), &signature, &signature, "init", &tree, &[])
                .expect("commit");
        }
        std::fs::write(dir.path().join("tracked.txt"), "changed").expect("modify");
        std::fs::write(dir.path().join("new.txt"), "new").expect("untracked");

        let summary = git_summary(Some(dir.path()));
        assert!(summary.contains("git, on branch"), "{summary}");
        assert!(summary.contains("1 changed"), "{summary}");
        assert!(summary.contains("1 untracked"), "{summary}");
    }
}
