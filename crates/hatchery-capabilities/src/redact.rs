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
//! or an assignment whose value is a literal (quoted, or unquoted with a digit/uppercase) is
//! evidence enough to act on. The real defence is that the secret never reaches the model in the
//! first place — this is the last line, not the first.

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
            // Whole PEM blocks first: the body would otherwise be scanned line by line. The
            // optional `BLOCK` covers PGP's `PGP PRIVATE KEY BLOCK` spelling.
            rule(
                r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----.*?-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----",
                "[redacted private key]",
            ),
            // `Authorization: Bearer/Basic/Digest <token>` — keep the scheme, drop the value.
            rule(
                r"(?i)\b(Authorization\s*:\s*(?:Bearer|Basic|Digest)\s+)\S+",
                "${1}[redacted]",
            ),
            // Prefixes that are credentials by construction.
            rule(
                r"\b(?:sk-proj-[A-Za-z0-9_-]{20,}|sk-[A-Za-z0-9_-]{20,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|xox[abprs]-[A-Za-z0-9-]{10,}|(?:AKIA|ASIA)[A-Z0-9]{16}|AIza[A-Za-z0-9_-]{30,}|glpat-[A-Za-z0-9_-]{20,})\b",
                "[redacted credential]",
            ),
            // `postgres://user:password@host` — the userinfo password. The user may be empty
            // (`redis://:password@host`) and the password may itself contain `@`; the greedy
            // password group backtracks to the last `@` that leaves a host.
            rule(
                r"(\b[a-zA-Z][a-zA-Z0-9+.-]*://[^/\s:@]*:)[^/\s]+@",
                "${1}[redacted]@",
            ),
        ]
    })
}

/// The assignment shape, matched separately because whether to redact depends on the **value**,
/// and `regex` has no look-ahead to express that in one pattern (measured: the crate rejects
/// `(?=…)`). The pattern stops at the separator; [`assignment_value`] then reads the value with
/// quote awareness, which is what makes `DB_PASSWORD=…`, `AWS_SECRET_ACCESS_KEY=…` and
/// `password = "a value with spaces"` reachable at all.
fn assignment() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"(?i)\b[A-Za-z0-9_]*(?:password|passwd|secret(?:[_-]?(?:access[_-]?)?key)?|api[_-]?key|access[_-]?key|private[_-]?key|access[_-]?token|auth[_-]?token|client[_-]?secret|token)\s*[:=]\s*",
        )
        .expect("a valid assignment pattern")
    })
}

/// Reads the value that follows a matched `key = ` prefix.
///
/// Returns the quote (if the value was quoted), the byte range of the value itself, and the
/// value. A quoted value runs to its closing quote, so spaces and `:` are allowed inside; an
/// unquoted one ends at whitespace, a comma, a semicolon or a quote.
fn assignment_value(text: &str, start: usize) -> Option<(Option<char>, usize, usize, &str)> {
    let bytes = text.as_bytes();
    let mut index = start;
    while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
        index += 1;
    }
    if index >= bytes.len() {
        return None;
    }
    match bytes[index] {
        b'"' | b'\'' => {
            let quote = bytes[index] as char;
            let value_start = index + 1;
            let mut end = value_start;
            while end < bytes.len() && bytes[end] != quote as u8 {
                end += 1;
            }
            (end < bytes.len()).then(|| {
                (
                    Some(quote),
                    value_start,
                    end,
                    text.get(value_start..end).unwrap_or(""),
                )
            })
        }
        _ => {
            let value_start = index;
            let mut end = index;
            while end < bytes.len() {
                let byte = bytes[end];
                if byte.is_ascii_whitespace() || matches!(byte, b',' | b';' | b'"' | b'\'') {
                    break;
                }
                end += 1;
            }
            Some((None, value_start, end, text.get(value_start..end)?))
        }
    }
}

/// Whether an assignment value looks like a literal secret.
///
/// A quoted value is treated as a literal: `password = "correct horse battery"` is a secret even
/// though every word is lowercase. An unquoted value must contain a digit or an uppercase letter
/// — and must not contain expression punctuation, so `let secret = SecretKey::new();` and
/// `let password = hashPassword2(pw);` are not rewritten.
fn looks_like_a_secret(value: &str, quoted: bool) -> bool {
    let value = value.trim();
    if value.chars().count() < 8 {
        return false;
    }
    if quoted {
        return true;
    }
    if value.chars().any(|character| {
        matches!(
            character,
            '(' | ')' | '{' | '}' | ':' | '<' | '>' | '&' | '|' | '?' | '*'
        )
    }) {
        return false;
    }
    value
        .chars()
        .any(|character| character.is_ascii_digit() || character.is_ascii_uppercase())
}

fn redact_assignments(text: &str) -> Option<String> {
    let pattern = assignment();
    let mut output = String::new();
    let mut last = 0;
    let mut changed = false;
    for whole in pattern.find_iter(text) {
        let Some((quote, value_start, value_end, value)) = assignment_value(text, whole.end())
        else {
            continue;
        };
        if !looks_like_a_secret(value, quote.is_some()) {
            continue;
        }
        output.push_str(&text[last..value_start]);
        output.push_str("[redacted]");
        if let Some(quote) = quote {
            output.push(quote);
            last = value_end + 1;
        } else {
            last = value_end;
        }
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
    fn upper_snake_case_assignments_are_redacted() {
        for text in [
            "DB_PASSWORD=hunter22",
            "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG",
            "DJANGO_SECRET_KEY=Abcdefgh",
            "OPENAI_API_KEY=Abcdefgh",
            "token=Abc12345",
        ] {
            let redacted = redact(text);
            assert!(redacted.contains("[redacted]"), "{text} -> {redacted}");
            assert!(
                !redacted.contains("hunter22")
                    && !redacted.contains("wJalrXUtnFEMI")
                    && !redacted.contains("Abcdefgh")
                    && !redacted.contains("Abc12345"),
                "{text} -> {redacted}"
            );
        }
    }

    #[test]
    fn a_private_key_block_is_replaced_whole() {
        let text =
            "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----\nafter";
        assert_eq!(redact(text), "before\n[redacted private key]\nafter");
    }

    #[test]
    fn a_pgp_private_key_block_is_replaced_whole() {
        let text =
            "-----BEGIN PGP PRIVATE KEY BLOCK-----\nabc\n-----END PGP PRIVATE KEY BLOCK-----";
        assert_eq!(redact(text), "[redacted private key]");
    }

    #[test]
    fn a_bearer_or_basic_header_keeps_the_scheme_and_drops_the_token() {
        assert_eq!(
            redact("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.secret"),
            "Authorization: Bearer [redacted]"
        );
        assert_eq!(
            redact("Authorization: Basic dXNlcjpwYXNz"),
            "Authorization: Basic [redacted]"
        );
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
    fn a_quoted_passphrase_with_spaces_is_redacted() {
        assert_eq!(
            redact(r#"password = "correct horse battery""#),
            r#"password = "[redacted]""#
        );
    }

    #[test]
    fn ordinary_code_is_not_redacted() {
        for text in [
            "let password = read_password()?;",
            "let password = hashPassword2(pw);",
            "let secret = SecretKey::new();",
            "let api_key = std::env::var(\"API_KEY\")?;",
            "secret = &config.secret",
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
        assert_eq!(
            redact("redis://:s3cret@cache:6379"),
            "redis://:[redacted]@cache:6379"
        );
        assert_eq!(
            redact("https://user:p@ss@host/path"),
            "https://user:[redacted]@host/path"
        );
    }

    #[test]
    fn text_without_a_match_is_borrowed() {
        assert!(matches!(redact("nothing here"), Cow::Borrowed(_)));
    }
}
