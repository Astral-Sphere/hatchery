//! The prompt assembly pipeline (docs/design/platform.md §2.1).
//!
//! Sections, in order: `identity`, `mode_<id>`, `project_context` (only when a project file is
//! found), `environment`, `tool_discipline` (only when the session has tools), `safety_gate`.
//! Each is a markdown file beside this module, embedded with `include_str!` at compile time and
//! overridable per section from `~/.config/hatchery/prompts/<id>.md` — except `tool_discipline`
//! and `safety_gate`, whose override attempts are refused with a warning (invariant 5). Section
//! ids are also the override file names, so the Code variant is `mode_code` (`mode_code.md`),
//! not the `mode-code.md` spelling that platform.md §2.2's example still shows. `{{var}}`
//! placeholders interpolate the runtime facts; unknown variables are left visible rather than
//! silently emptied, because a template that drifted from its variables should be loud.
//!
//! The assembled prompt is what `prompt/render` hands back, section by section with sources, so
//! "why did the model see this" is always answerable.

use std::ffi::OsStr;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use hatchery_protocol::SessionModeId;
use hatchery_protocol::method::PromptSection;

use super::template::{Vars, interpolate};

/// The `identity` section: persona plus the PRECEDENCE declaration.
pub const IDENTITY: &str = include_str!("../prompts/identity.md");
/// The Chat-mode variant: read-only tools, no workspace discipline.
pub const MODE_CHAT: &str = include_str!("../prompts/mode-chat.md");
/// The Code-mode variant: workspace discipline, write tools, checkpoints.
pub const MODE_CODE: &str = include_str!("../prompts/mode-code.md");
/// The runtime-facts section, full of `{{vars}}`.
pub const ENVIRONMENT: &str = include_str!("../prompts/environment.md");
/// The tool-table discipline; its `{{tools}}` placeholder is filled from the turn's snapshot.
pub const TOOL_DISCIPLINE: &str = include_str!("../prompts/tool-discipline.md");
/// The safety gate: not overridable, by anyone, ever.
pub const SAFETY_GATE: &str = include_str!("../prompts/safety-gate.md");

/// How much project context reaches the prompt, in bytes.
///
/// The cap keeps a repository's instruction files from crowding out the runtime facts and the
/// safety gate; the nearest file is retained first because it is the most specific.
const PROJECT_CONTEXT_LIMIT: usize = 32 * 1024;

/// What the runtime knows that the prompt should mention.
#[derive(Clone, Debug, Default)]
pub struct Environment {
    /// Working directory of the session.
    pub cwd: String,
    /// The bound workspace root, when there is one. `project_context` is discovered from here.
    pub workspace: Option<PathBuf>,
    /// `std::env::consts::OS` with a friendlier spelling.
    pub platform: String,
    /// Today, as `YYYY-MM-DD`.
    pub date: String,
    /// One line about the workspace's git state, from [`git_summary`].
    pub git_status: String,
    /// The configured reply language, if one is set.
    pub response_language: String,
}

/// The runtime facts behind one session's prompt.
///
/// One builder for both callers — `prompt/render` and the runtime assembly — because they must
/// not drift: the transparency answer and the text a request carries are supposed to be the same
/// text.
#[must_use]
pub fn environment(workspace: Option<&Path>, response_language: Option<&str>) -> Environment {
    let cwd = match workspace {
        Some(workspace) => workspace.display().to_string(),
        None => std::env::current_dir()
            .unwrap_or_default()
            .display()
            .to_string(),
    };
    Environment {
        cwd,
        workspace: workspace.map(Path::to_path_buf),
        platform: std::env::consts::OS.to_owned(),
        date: super::clock::humantime_date(),
        git_status: git_summary(workspace),
        response_language: response_language.unwrap_or_default().to_owned(),
    }
}

/// Where the per-section override files live, from the two variables that can say.
///
/// The pure half of [`default_prompts_dir`]: the sibling of the user config layer
/// (`config::LoadPaths::detect`), so `$XDG_CONFIG_HOME/hatchery/prompts`, else
/// `~/.config/hatchery/prompts`.
#[must_use]
pub fn prompts_dir(config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    config_home
        .map(PathBuf::from)
        .or_else(|| home.map(|home| Path::new(home).join(".config")))
        .map(|dir| dir.join("hatchery").join("prompts"))
}

/// [`prompts_dir`] against this process's environment.
///
/// `None` only when neither variable is set. A directory that does not exist is the normal case,
/// not an error: [`render`] reads each section's file and falls back to the builtin when the
/// read fails.
#[must_use]
pub fn default_prompts_dir() -> Option<PathBuf> {
    prompts_dir(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// One assembled section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssembledSection {
    /// Section id, matching the protocol's `PromptSection::id`.
    ///
    /// An owned string because a user-declared mode contributes its own id (`mode_<name>`), not
    /// only the built-in pair.
    pub id: String,
    /// Where the text came from: `builtin`, the override file's path, or `project:<path>`.
    pub source: String,
    /// The interpolated text.
    pub text: String,
}

/// Renders the prompt for a session in `mode`.
///
/// `tool_names` is the session's tool snapshot, in any order: it becomes the `tool_discipline`
/// section, and an empty snapshot emits no such section (Chat without tools, in the tests).
/// `override_dir` is the user's `prompts/` directory; sections with a file there use it, except
/// `tool_discipline` and `safety_gate`. Missing files are the normal case, not an error.
#[must_use]
pub fn render(
    mode: &SessionModeId,
    tool_names: &[String],
    env: &Environment,
    override_dir: Option<&PathBuf>,
) -> Vec<AssembledSection> {
    let vars = Vars::new(env);
    let mut sections = Vec::new();

    sections.push(overridable_section(
        "identity",
        IDENTITY,
        override_dir,
        &vars,
    ));

    // A mode without a variant of its own still gets a section id, so an override can give it
    // one; the text meanwhile falls back to Chat's read-only discipline rather than inventing
    // write permission the tool table may not grant.
    let (mode_id, mode_text) = match mode.as_str() {
        SessionModeId::CHAT => (format!("mode_{}", SessionModeId::CHAT), MODE_CHAT),
        SessionModeId::CODE => (format!("mode_{}", SessionModeId::CODE), MODE_CODE),
        custom => {
            tracing::warn!(
                mode = custom,
                "no prompt variant for this mode; falling back to the Chat variant"
            );
            (format!("mode_{custom}"), MODE_CHAT)
        }
    };
    sections.push(overridable_section(
        &mode_id,
        mode_text,
        override_dir,
        &vars,
    ));

    if let Some(project) = project_context(env.workspace.as_deref()) {
        sections.push(with_override(project, override_dir, &vars));
    }

    sections.push(overridable_section(
        "environment",
        ENVIRONMENT,
        override_dir,
        &vars,
    ));

    if !tool_names.is_empty() {
        sections.push(refuse_override(tool_discipline(tool_names), override_dir));
    }

    sections.push(refuse_override(
        AssembledSection {
            id: "safety_gate".to_owned(),
            source: "builtin".to_owned(),
            text: SAFETY_GATE.to_owned(),
        },
        override_dir,
    ));
    sections
}

/// Renders the prompt for a Chat session with no tools.
///
/// A thin wrapper over [`render`] for callers that have only an [`Environment`]; a session with
/// a tool snapshot or another mode should call [`render`] directly.
#[must_use]
pub fn render_chat(env: &Environment, override_dir: Option<&PathBuf>) -> Vec<AssembledSection> {
    render(&SessionModeId::chat(), &[], env, override_dir)
}

/// The `tool_discipline` section: the snapshot's tool table, spelled out.
///
/// The list is sorted so the same snapshot always renders the same bytes — prompt stability is
/// what keeps the provider's prefix cache warm.
fn tool_discipline(tool_names: &[String]) -> AssembledSection {
    let mut names: Vec<&str> = tool_names.iter().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    let mut vars = Vars::default();
    vars.insert("tools", names.join(", "));
    AssembledSection {
        id: "tool_discipline".to_owned(),
        source: "builtin".to_owned(),
        text: interpolate(TOOL_DISCIPLINE, &vars),
    }
}

/// The `project_context` section: `AGENTS.md`/`HATCHERY.md` from the workspace up to the root.
///
/// At each directory `AGENTS.md` wins over `HATCHERY.md` (platform.md open question 1). Files are
/// joined root-most first so the nearest file is last: later text wins by position, and the
/// nearest file is the most specific. The cap is applied nearest-first for the same reason — the
/// root-most content is what gets shortened.
fn project_context(workspace: Option<&Path>) -> Option<AssembledSection> {
    let workspace = workspace?;
    if workspace.as_os_str().is_empty() {
        return None;
    }

    // Nearest first. `parent()` walks as given, so a symlinked workspace is not chased past the
    // path the session actually bound; the loop ends at the filesystem root.
    let mut found: Vec<(String, String)> = Vec::new();
    let mut truncated = false;
    let mut warned_both = false;
    let mut cursor = Some(workspace);
    while let Some(dir) = cursor {
        let agents = dir.join("AGENTS.md");
        let hatchery = dir.join("HATCHERY.md");
        let path = match (agents.is_file(), hatchery.is_file()) {
            (true, true) => {
                if !warned_both {
                    tracing::warn!(
                        directory = %dir.display(),
                        "both AGENTS.md and HATCHERY.md exist here; using AGENTS.md"
                    );
                    warned_both = true;
                }
                Some(agents)
            }
            (true, false) => Some(agents),
            (false, true) => Some(hatchery),
            (false, false) => None,
        };
        if let Some(path) = path {
            match read_capped(&path) {
                Ok((text, was_truncated)) => {
                    truncated |= was_truncated;
                    let display = path
                        .strip_prefix(workspace)
                        .unwrap_or(path.as_path())
                        .display()
                        .to_string();
                    found.push((display, text));
                }
                Err(error) => tracing::warn!(
                    path = %path.display(),
                    %error,
                    "skipping unreadable project context file"
                ),
            }
        }
        cursor = dir.parent();
    }
    if found.is_empty() {
        return None;
    }

    // Spend the budget nearest-first; the file that crosses it is shortened, and anything
    // further out is dropped.
    let mut remaining = PROJECT_CONTEXT_LIMIT;
    let mut kept: Vec<(String, String)> = Vec::new();
    for (display, text) in found {
        if remaining == 0 {
            truncated = true;
            break;
        }
        if text.len() <= remaining {
            remaining -= text.len();
            kept.push((display, text));
        } else {
            let cut = floor_char_boundary(&text, remaining);
            if cut > 0 {
                kept.push((display, text[..cut].to_owned()));
            }
            truncated = true;
            break;
        }
    }

    // Root-most first, nearest last: the near file is the one the model should weigh.
    let source = format!(
        "project:{}",
        kept.iter()
            .rev()
            .map(|(display, _)| display.as_str())
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut text = kept
        .iter()
        .rev()
        .map(|(_, text)| text.trim_end())
        .collect::<Vec<_>>()
        .join("\n\n");
    if truncated {
        text.push_str(
            "\n\n[project context truncated: only the nearest files within 32 KiB are shown]",
        );
    }
    Some(AssembledSection {
        id: "project_context".to_owned(),
        source,
        text,
    })
}

/// Reads at most [`PROJECT_CONTEXT_LIMIT`] bytes, reporting whether the file was cut.
fn read_capped(path: &Path) -> std::io::Result<(String, bool)> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take((PROJECT_CONTEXT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > PROJECT_CONTEXT_LIMIT;
    let bytes = if truncated {
        &bytes[..PROJECT_CONTEXT_LIMIT]
    } else {
        &bytes[..]
    };
    // A byte cap can land inside a multi-byte character; drop that partial tail.
    let end = std::str::from_utf8(bytes)
        .map(|_| bytes.len())
        .unwrap_or_else(|error| error.valid_up_to());
    Ok((
        String::from_utf8_lossy(&bytes[..end]).into_owned(),
        truncated,
    ))
}

/// The largest character boundary at or below `limit`.
fn floor_char_boundary(text: &str, limit: usize) -> usize {
    let mut end = limit.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// The override text for `id`, when the user has one there.
fn override_text(override_dir: Option<&PathBuf>, id: &str) -> Option<String> {
    let path = override_dir?.join(format!("{id}.md"));
    std::fs::read_to_string(path).ok()
}

/// A builtin section, interpolated with the session's variables.
fn builtin_section(id: &str, builtin: &str, vars: &Vars) -> AssembledSection {
    AssembledSection {
        id: id.to_owned(),
        source: "builtin".to_owned(),
        text: interpolate(builtin, vars),
    }
}

/// The section, replaced whole by the user's file when one exists.
fn with_override(
    builtin: AssembledSection,
    override_dir: Option<&PathBuf>,
    vars: &Vars,
) -> AssembledSection {
    let Some(text) = override_text(override_dir, &builtin.id) else {
        return builtin;
    };
    AssembledSection {
        id: builtin.id.clone(),
        source: format!("user:prompts/{}.md", builtin.id),
        text: interpolate(&text, vars),
    }
}

/// [`builtin_section`] with [`with_override`] applied.
fn overridable_section(
    id: &str,
    builtin: &str,
    override_dir: Option<&PathBuf>,
    vars: &Vars,
) -> AssembledSection {
    with_override(builtin_section(id, builtin, vars), override_dir, vars)
}

/// A section the user may not replace: the attempt is ignored and reported (invariant 5).
fn refuse_override(section: AssembledSection, override_dir: Option<&PathBuf>) -> AssembledSection {
    if override_text(override_dir, &section.id).is_some() {
        // The refusal is the behaviour; the warning is how the user finds out.
        tracing::warn!(
            section = section.id.as_str(),
            "an override was ignored: that section cannot be overridden"
        );
    }
    section
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
                id: section.id.clone(),
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
            workspace: None,
            platform: "linux".to_owned(),
            date: "2026-10-01".to_owned(),
            git_status: "not a git repository".to_owned(),
            response_language: "简体中文".to_owned(),
        }
    }

    fn ids(sections: &[AssembledSection]) -> Vec<&str> {
        sections.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn all_four_sections_assemble_in_order() {
        let sections = render_chat(&env(), None);
        assert_eq!(
            ids(&sections),
            ["identity", "mode_chat", "environment", "safety_gate"]
        );
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
    fn code_render_uses_the_code_variant_and_its_tool_snapshot() {
        let tools = [
            "write_file".to_owned(),
            "read_file".to_owned(),
            "edit".to_owned(),
        ];
        let sections = render(&SessionModeId::code(), &tools, &env(), None);
        assert_eq!(
            ids(&sections),
            [
                "identity",
                "mode_code",
                "environment",
                "tool_discipline",
                "safety_gate"
            ]
        );
        let mode = sections
            .iter()
            .find(|s| s.id == "mode_code")
            .expect("mode_code");
        assert_eq!(mode.text, MODE_CODE, "the embedded file is what is sent");
        assert!(mode.text.contains("checkpointed"));
    }

    #[test]
    fn an_unknown_mode_keeps_its_id_but_falls_back_to_the_chat_variant() {
        let custom = SessionModeId::new("review-only");
        let sections = render(&custom, &[], &env(), None);
        assert_eq!(
            ids(&sections),
            ["identity", "mode_review-only", "environment", "safety_gate"]
        );
        let mode = sections
            .iter()
            .find(|s| s.id == "mode_review-only")
            .expect("the custom mode still names its section");
        assert_eq!(
            mode.text, MODE_CHAT,
            "no invented discipline for a custom mode"
        );
    }

    #[test]
    fn tool_discipline_lists_every_name_and_cannot_be_overridden() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("tool_discipline.md"), "there are no tools").expect("write");
        let tools = [
            "write_file".to_owned(),
            "edit".to_owned(),
            "read_file".to_owned(),
        ];

        let sections = render(
            &SessionModeId::code(),
            &tools,
            &env(),
            Some(&dir.path().to_path_buf()),
        );
        let discipline = sections
            .iter()
            .find(|s| s.id == "tool_discipline")
            .expect("tool_discipline");
        assert_eq!(discipline.source, "builtin", "the override is refused");
        assert!(
            discipline.text.contains("edit, read_file, write_file"),
            "every supplied name is listed, sorted: {}",
            discipline.text
        );
        assert!(
            discipline.text.contains("complete set"),
            "the list is declared complete: {}",
            discipline.text
        );
        assert!(
            !join(&sections).contains("there are no tools"),
            "the attempted override must not leak into the prompt"
        );
    }

    #[test]
    fn project_context_prefers_the_nearest_file_and_places_it_last() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("crates").join("app");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::write(dir.path().join("AGENTS.md"), "root rules").expect("write");
        std::fs::write(nested.join("AGENTS.md"), "nearest rules").expect("write");

        let section = project_context(Some(&nested)).expect("section");
        assert_eq!(section.id, "project_context");
        assert_eq!(
            section.source,
            format!(
                "project:{},AGENTS.md",
                dir.path().join("AGENTS.md").display()
            ),
            "the file above the workspace can only be absolute; the nearest is relative"
        );
        assert!(
            section.text.ends_with("nearest rules"),
            "the nearest file is last: {}",
            section.text
        );
        let root = section.text.find("root rules").expect("root rules");
        let nearest = section.text.find("nearest rules").expect("nearest rules");
        assert!(root < nearest, "root-most first, nearest last");
    }

    #[test]
    fn hatchery_md_is_used_when_agents_md_is_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("HATCHERY.md"), "hatchery rules").expect("write");

        let section = project_context(Some(dir.path())).expect("section");
        assert_eq!(section.source, "project:HATCHERY.md");
        assert!(section.text.contains("hatchery rules"));
    }

    #[test]
    fn agents_md_wins_when_both_names_exist_in_one_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("AGENTS.md"), "agents rules").expect("write");
        std::fs::write(dir.path().join("HATCHERY.md"), "hatchery rules").expect("write");

        let section = project_context(Some(dir.path())).expect("section");
        assert_eq!(section.source, "project:AGENTS.md");
        assert!(section.text.contains("agents rules"));
        assert!(
            !section.text.contains("hatchery rules"),
            "the losing file is not read: {}",
            section.text
        );
    }

    #[test]
    fn no_project_file_means_no_section() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(project_context(Some(dir.path())).is_none());
        assert!(project_context(None).is_none());

        let sections = render(&SessionModeId::chat(), &[], &env(), None);
        assert!(!sections.iter().any(|s| s.id == "project_context"));
    }

    #[test]
    fn project_context_sits_between_the_mode_and_the_environment() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("AGENTS.md"), "project rules").expect("write");
        let env = Environment {
            workspace: Some(dir.path().to_path_buf()),
            ..env()
        };
        let tools = ["read_file".to_owned()];

        let sections = render(&SessionModeId::code(), &tools, &env, None);
        assert_eq!(
            ids(&sections),
            [
                "identity",
                "mode_code",
                "project_context",
                "environment",
                "tool_discipline",
                "safety_gate"
            ],
            "the safety gate is always last"
        );
    }

    #[test]
    fn the_project_context_cap_shortens_the_root_most_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("nested");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::write(dir.path().join("AGENTS.md"), "r".repeat(33 * 1024)).expect("write");
        std::fs::write(nested.join("AGENTS.md"), "nearest rules").expect("write");

        let section = project_context(Some(&nested)).expect("section");
        assert!(
            section.text.contains("nearest rules"),
            "the near file survives whole: {}",
            section.text
        );
        assert!(
            section.text.len() <= PROJECT_CONTEXT_LIMIT + 128,
            "the cap bounds the near file plus the truncation note: {}",
            section.text.len()
        );
        assert!(
            section.text.ends_with("are shown]"),
            "the model is told the context was cut, after the content: {}",
            section.text
        );
    }

    #[test]
    fn identity_and_environment_overrides_still_apply() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("identity.md"), "identity from the user").expect("write");
        std::fs::write(dir.path().join("environment.md"), "cwd is {{cwd}}").expect("write");

        let sections = render(
            &SessionModeId::chat(),
            &[],
            &env(),
            Some(&dir.path().to_path_buf()),
        );
        let identity = sections
            .iter()
            .find(|s| s.id == "identity")
            .expect("identity");
        assert_eq!(identity.source, "user:prompts/identity.md");
        assert_eq!(identity.text, "identity from the user");
        let environment = sections
            .iter()
            .find(|s| s.id == "environment")
            .expect("environment");
        assert_eq!(environment.source, "user:prompts/environment.md");
        assert_eq!(environment.text, "cwd is /home/jimmy/demo");
    }

    #[test]
    fn a_missing_workspace_reads_as_not_bound() {
        assert_eq!(git_summary(None), "not bound (Chat without a workspace)");
    }

    #[test]
    fn the_prompts_dir_is_the_user_config_layer_sibling() {
        assert_eq!(
            prompts_dir(Some(OsStr::new("/xdg")), Some(OsStr::new("/home/u"))),
            Some(PathBuf::from("/xdg/hatchery/prompts")),
            "XDG_CONFIG_HOME wins when it is set"
        );
        assert_eq!(
            prompts_dir(None, Some(OsStr::new("/home/u"))),
            Some(PathBuf::from("/home/u/.config/hatchery/prompts")),
            "the documented fallback"
        );
        assert_eq!(prompts_dir(None, None), None, "no home, no overrides");
    }

    #[test]
    fn the_environment_builder_supplies_every_variable_a_render_interpolates() {
        let env = environment(None, Some("简体中文"));
        assert_eq!(env.git_status, "not bound (Chat without a workspace)");
        assert_eq!(env.response_language, "简体中文");
        assert_eq!(env.platform, std::env::consts::OS);
        assert!(env.workspace.is_none());
        assert!(!env.cwd.is_empty(), "the daemon's cwd stands in");
        let date = env.date.clone();
        assert_eq!(date.len(), "YYYY-MM-DD".len(), "{date}");
        assert!(
            !join(&render_chat(&env, None)).contains("{{"),
            "nothing is left uninterpolated"
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let bound = environment(Some(dir.path()), None);
        assert_eq!(bound.cwd, dir.path().display().to_string());
        assert_eq!(bound.workspace.as_deref(), Some(dir.path()));
        assert!(!bound.git_status.is_empty(), "a workspace is described");
        assert_eq!(bound.response_language, "", "no configured language");
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
