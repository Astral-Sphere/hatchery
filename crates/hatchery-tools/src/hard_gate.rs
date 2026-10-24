//! The in-workspace paths a write may never be allowed to remember (invariant 5).
//!
//! Design §5 lists four hard gates: `~/.ssh`, `~/.config/hatchery`, `.git/hooks` and `.env*`.
//! Only the last two are reachable through a tool: paths are workspace-relative, and `LocalFs`
//! refuses every path that resolves outside the workspace before a tool ever sees a file. A gate
//! for `~/.ssh` here would therefore never fire — a false claim of protection, and the kind of
//! dead branch ADR-0009 says nobody has tested.
//!
//! This is also the resolution of the `RiskLevel::is_hard_gate` gap: that predicate recognises
//! only `WritesOutside`, while `.env*` and `.git/hooks` live *inside* the workspace, and adding
//! a variant would be a protocol major bump. `ApprovalRequest::once_only()` already says
//! "cannot be remembered" by omitting the always options, so the tools force that instead.

/// The reason a workspace-relative path is a hard gate, or `None` when it is an ordinary write.
///
/// The result is the short phrase the approval digest carries, so the user is told *why* the
/// always options are missing.
#[must_use]
pub(crate) fn hard_gate(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == ".env" || name.starts_with(".env.") {
        return Some("environment file");
    }

    // `.git/hooks` is a component sequence, not a substring: `.github/` and `a.git/` are ordinary
    // directories, and only a nested `.git` followed directly by `hooks` is the hook directory.
    let components: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    if components.windows(2).any(|pair| pair == [".git", "hooks"]) {
        return Some("git hook");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::hard_gate;

    #[test]
    fn environment_files_are_gated() {
        for path in [".env", "a/.env", ".env.local", "config/.env.production"] {
            assert_eq!(hard_gate(path), Some("environment file"), "{path}");
        }
    }

    #[test]
    fn git_hook_directories_are_gated() {
        for path in [".git/hooks", ".git/hooks/pre-commit", "x/.git/hooks/y"] {
            assert_eq!(hard_gate(path), Some("git hook"), "{path}");
        }
    }

    #[test]
    fn lookalikes_are_not_gated() {
        for path in [
            "env",
            "src/.envrc",
            ".github/workflows/x",
            "a.git/hooks",
            "docs/.git/hooks.md",
        ] {
            assert_eq!(hard_gate(path), None, "{path}");
        }
    }
}
