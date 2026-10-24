//! `{{var}}` interpolation for the prompt templates.
//!
//! A placeholder that has no variable keeps its braces in the output: a template that drifted
//! from its variables must be visible in `prompt/render`, not silently emptied into the model's
//! instructions.

use std::collections::BTreeMap;

use super::prompt::Environment;

/// The variables one render may use.
#[derive(Default)]
pub struct Vars {
    values: BTreeMap<String, String>,
}

impl Vars {
    /// The variables of one session's environment.
    #[must_use]
    pub fn new(env: &Environment) -> Self {
        let mut values = BTreeMap::new();
        values.insert("cwd".to_owned(), env.cwd.clone());
        values.insert("platform".to_owned(), env.platform.clone());
        values.insert("date".to_owned(), env.date.clone());
        values.insert("git_status".to_owned(), env.git_status.clone());
        values.insert(
            "response_language".to_owned(),
            if env.response_language.is_empty() {
                "the user's language".to_owned()
            } else {
                env.response_language.clone()
            },
        );
        Self { values }
    }

    /// One variable, for tests and overrides.
    pub fn insert(&mut self, name: &str, value: impl Into<String>) {
        self.values.insert(name.to_owned(), value.into());
    }
}

/// Replaces every `{{name}}` with its variable, leaving unknown placeholders alone.
#[must_use]
pub fn interpolate(template: &str, vars: &Vars) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let name = after[..end].trim();
                match vars.values.get(name) {
                    Some(value) => out.push_str(value),
                    None => {
                        // Unknown: keep the braces so a human sees the drift.
                        out.push_str("{{");
                        out.push_str(name);
                        out.push_str("}}");
                    }
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_variables_are_replaced_and_unknown_are_kept_visible() {
        let mut vars = Vars::default();
        vars.insert("platform", "linux");
        let out = interpolate(
            "on {{platform}} at {{time_of_day}} and {{{platform}}}",
            &vars,
        );
        assert_eq!(
            out, "on linux at {{time_of_day}} and {{{platform}}}",
            "a malformed placeholder is drift to be seen, not silently rewritten"
        );
    }

    #[test]
    fn an_unclosed_brace_is_left_alone() {
        let vars = Vars::default();
        assert_eq!(interpolate("hello {{name", &vars), "hello {{name");
    }

    #[test]
    fn names_are_trimmed_of_incidental_whitespace() {
        let mut vars = Vars::default();
        vars.insert("a", "A");
        assert_eq!(interpolate("{{ a }}", &vars), "A");
    }
}
