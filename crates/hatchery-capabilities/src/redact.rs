//! Credential redaction at the seam.
//!
//! Where this runs matters more than what it matches. The tool host rewrites the output **once,
//! as it leaves the backend**, before the kernel records it and before the model sees it. That
//! keeps invariant 2 intact: what is stored, what is sent, and what the UI renders are the same
//! bytes (design/capabilities.md §5). Redacting on the way into storage instead would make the
//! stored history differ from the request it is supposed to be able to reproduce.
//!
//! The scanner is deliberately conservative about *shapes* and deliberately ignorant about
//! *entropy*. A generic "long random-looking string" rule would redact git hashes, checksums and
//! base64 in ordinary output, which trains users to ignore the marker; a known credential prefix
//! or an assignment with a value that has a digit or an uppercase letter is evidence enough to
//! act on. The real defence is that the secret never reaches the model in the first place — this
//! is the last line, not the first.

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::Regex;

struct Rule {
    pattern: Regex,
    replacement: &'static str,
}

fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let rule = |pattern: &str, replacement: &'static str| Rule {
            pattern: Regex::new(pattern).expect("a valid redaction pattern"),
            replacement,
        };
        vec![
            // Whole PEM blocks first: the body would otherwise be scanned line by line.
            rule(
                r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
                "[redacted private key]",
            ),
            // `Authorization: Bearer <token>` — keep the header, drop the value.
            rule(r"(?i)\b(Authorization\s*:\s*Bearer\s+)\S+", "${1}[redacted]"),
            // Prefixes that are credentials by construction.
            rule(
                r"\b(?:sk-proj-[A-Za-z0-9_-]{20,}|sk-[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|xox[abprs]-[A-Za-z0-9-]{10,}|(?:AKIA|ASIA)[A-Z0-9]{16}|AIza[A-Za-z0-9_-]{30,}|glpat-[A-Za-z0-9_-]{20,})\b",
                "[redacted credential]",
            ),
            // `postgres://user:password@host` — the userinfo password.
            rule(
                r"(\b[a-zA-Z][a-zA-Z0-9+.-]*://[^/\s:@]+:)[^@\s/]+@",
                "${1}[redacted]@",
            ),
        ]
    })
}

/// The assignment shape, matched separately because whether to redact depends on the **value**,
/// and `regex` has no look-ahead to express that in one pattern (measured: the crate rejects
/// `(?=…)`). Keeping the decision in Rust is also easier to test than a regex that would have
/// to spell "has a digit or an uppercase letter" as a look-ahead.
fn assignment() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(?P<key>password|passwd|secret|api[_-]?key|access[_-]?token|auth[_-]?token|client[_-]?secret)\b(?P<sep>\s*[:=]\s*)(?P<quote>["']?)(?P<value>[^\s"',;]+)"#,
        )
        .expect("a valid assignment pattern")
    })
}

fn looks_like_a_secret(value: &str) -> bool {
    value.chars().count() >= 8
        && value
            .chars()
            .any(|character| character.is_ascii_digit() || character.is_ascii_uppercase())
}

fn redact_assignments(text: &str) -> Option<String> {
    let pattern = assignment();
    let mut output = String::new();
    let mut last = 0;
    let mut changed = false;
    for capture in pattern.captures_iter(text) {
        let value = capture.name("value").expect("the value group").as_str();
        if !looks_like_a_secret(value) {
            continue;
        }
        let whole = capture.get(0).expect("the whole match");
        output.push_str(&text[last..whole.start()]);
        output.push_str(capture.name("key").expect("the key group").as_str());
        output.push_str(capture.name("sep").expect("the separator group").as_str());
        if let Some(quote) = capture.name("quote") {
            output.push_str(quote.as_str());
        }
        output.push_str("[redacted]");
        last = whole.end();
        changed = true;
    }
    if !changed {
        return None;
    }
    output.push_str(&text[last..]);
    Some(output)
}

/// Rewrites recognised credentials in `text`, borrowing when nothing matched.
#[must_use]
pub fn redact(text: &str) -> Cow<'_, str> {
    let mut current: Cow<'_, str> = Cow::Borrowed(text);
    for rule in rules() {
        if rule.pattern.is_match(&current) {
            current = Cow::Owned(
                rule.pattern
                    .replace_all(&current, rule.replacement)
                    .into_owned(),
            );
        }
    }
    if let Some(owned) = redact_assignments(&current) {
        current = Cow::Owned(owned);
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_openai_style_key_is_redacted() {
        let text = "OPENAI_API_KEY=sk-proj-AbCdEfGhIjKlMnOpQrStUvWx";
        // The assignment rule and the prefix rule overlap; either one is enough.
        assert!(!redact(text).contains("AbCdEfGhIjKlMnOpQrStUvWx"), "{text}");
    }

    #[test]
    fn a_private_key_block_is_replaced_whole() {
        let text =
            "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----\nafter";
        assert_eq!(redact(text), "before\n[redacted private key]\nafter");
    }

    #[test]
    fn a_bearer_header_keeps_the_header_and_drops_the_token() {
        let text = "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.secret";
        assert_eq!(redact(text), "Authorization: Bearer [redacted]");
    }

    #[test]
    fn an_assignment_with_a_secret_shaped_value_is_redacted() {
        assert_eq!(
            redact(r#"password = "hunter22""#),
            r#"password = "[redacted]""#
        );
        assert_eq!(redact("api_key=Abcdefgh"), "api_key=[redacted]");
    }

    #[test]
    fn ordinary_code_is_not_redacted() {
        for text in [
            "let password = read_password()?;",
            "token = req.token",
            "password = None",
            "const API_KEY: &str = \"PLACEHOLDER\";",
            "commit 0123456789abcdef0123456789abcdef01234567",
            "the prefix sk- is what OpenAI keys start with",
        ] {
            assert_eq!(redact(text), text, "false positive: {text}");
        }
    }

    #[test]
    fn a_url_password_is_redacted_but_the_user_and_host_survive() {
        assert_eq!(
            redact("postgres://alice:s3cret@db.example/app"),
            "postgres://alice:[redacted]@db.example/app"
        );
    }

    #[test]
    fn text_without_a_match_is_borrowed() {
        assert!(matches!(redact("nothing here"), Cow::Borrowed(_)));
    }
}
