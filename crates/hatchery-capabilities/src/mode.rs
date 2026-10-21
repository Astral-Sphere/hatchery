//! Which built-in tools a mode turns on.
//!
//! ADR-0005 defines a mode as a triple — tool set × approval policy × prompt variant. This is the
//! first leg: a declarative subset, not a second registry. The daemon still assembles one
//! [`crate::ToolRegistry`] per session, and the kernel still freezes its snapshot per turn; the
//! policy only decides which constructors feed it.
//!
//! Deliberately no per-tool configuration yet. `docs/design/capabilities.md` §4 sketches one
//! (`shell` timeouts, `web_fetch` domains), but neither tool exists, so a config field would be a
//! value with no reader — the anti-pre-split brake of ADR-0009.

/// An include/exclude subset of the built-in tool catalogue.
///
/// An empty `include` means "everything the catalogue offers"; `exclude` always wins. That makes
/// the two useful declarations short: Code is the default policy, Chat is an include list, and a
/// future read-only custom mode can be Code minus the write pair.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolPolicy {
    include: Vec<String>,
    exclude: Vec<String>,
}

impl ToolPolicy {
    /// Everything the catalogue offers.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    /// Only these tools.
    #[must_use]
    pub fn including(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            include: names.into_iter().map(Into::into).collect(),
            exclude: Vec::new(),
        }
    }

    /// The same policy minus these tools. Exclusions beat inclusions.
    #[must_use]
    pub fn excluding(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.exclude.extend(names.into_iter().map(Into::into));
        self
    }

    /// Whether `name` belongs in the registry this policy assembles.
    #[must_use]
    pub fn allows(&self, name: &str) -> bool {
        if self.exclude.iter().any(|excluded| excluded == name) {
            return false;
        }
        self.include.is_empty() || self.include.iter().any(|included| included == name)
    }

    /// The names this policy allows, in catalogue order.
    #[must_use]
    pub fn enabled<'a>(&self, catalogue: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
        catalogue
            .into_iter()
            .filter(|name| self.allows(name))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATALOGUE: &[&str] = &["read_file", "glob", "grep", "write_file", "edit"];

    #[test]
    fn the_empty_policy_is_the_whole_catalogue() {
        let policy = ToolPolicy::all();
        assert_eq!(policy.enabled(CATALOGUE.iter().copied()), CATALOGUE);
    }

    #[test]
    fn an_include_list_is_the_whole_answer() {
        let policy = ToolPolicy::including(["read_file", "grep"]);
        assert_eq!(
            policy.enabled(CATALOGUE.iter().copied()),
            ["read_file", "grep"]
        );
        assert!(!policy.allows("write_file"));
    }

    #[test]
    fn exclusion_beats_inclusion_and_order_follows_the_catalogue() {
        let policy = ToolPolicy::including(["edit", "write_file", "read_file"]).excluding(["edit"]);
        assert_eq!(
            policy.enabled(CATALOGUE.iter().copied()),
            ["read_file", "write_file"]
        );
        assert!(!policy.allows("edit"));
    }

    #[test]
    fn a_name_the_catalogue_does_not_have_is_simply_never_enabled() {
        let policy = ToolPolicy::including(["does_not_exist"]);
        assert!(policy.enabled(CATALOGUE.iter().copied()).is_empty());
    }
}
