//! Best-effort secret redaction for log payloads.
//!
//! Two orthogonal passes, both applied by [`redact_secrets`]:
//!
//! 1. **Field-name allowlist.** Substrings that look like `"<field>":"<value>"`
//!    or `<field>=<value>` where `<field>` matches a known-sensitive name
//!    (see [`SENSITIVE_FIELD_NAMES`]) have their value replaced with
//!    [`REDACTED`].
//!
//! 2. **Regex defense in depth.** The module carries a static set of
//!    high-precision patterns for secret *shapes* (OpenAI-style `sk-`,
//!    GitHub PAT, AWS access key id, bearer tokens, long hex, JWT-like
//!    three-part base64url). A match anywhere in the input is replaced with
//!    [`REDACTED`].
//!
//! Redaction is intentionally **idempotent**: applying `redact_secrets` twice
//! is equivalent to applying it once (the [`REDACTED`] marker itself does not
//! contain any pattern-matching character that would cause re-entry).

use std::borrow::Cow;
use std::sync::OnceLock;

use regex::{Regex, RegexSet};

/// Replacement string written wherever a secret is detected.
pub const REDACTED: &str = "***REDACTED***";

/// Field names whose values are always redacted regardless of shape.
///
/// Matched case-insensitively. Both JSON-style (`"api_key":"<value>"`) and
/// key=value style (`api_key=<value>`) are covered.
pub const SENSITIVE_FIELD_NAMES: &[&str] = &[
    "password",
    "api_key",
    "apikey",
    "token",
    "secret",
    "authorization",
    "bearer",
    "gh_token",
    "github_token",
    "access_key",
    "access_token",
    "refresh_token",
    "private_key",
    "client_secret",
];

/// Regex patterns used by the shape-based redaction pass.
///
/// Each entry is an anchored-enough pattern to be unambiguous. Order is only
/// significant for cosmetics (longer matches first reduce churn on
/// overlapping candidates).
const SHAPE_PATTERNS: &[&str] = &[
    // OpenAI / Anthropic / generic `sk-` prefixed keys (20+ chars of body)
    r"sk-[A-Za-z0-9_\-]{20,}",
    // GitHub personal access tokens
    r"ghp_[A-Za-z0-9]{20,}",
    r"github_pat_[A-Za-z0-9_]{20,}",
    // AWS access key id
    r"AKIA[0-9A-Z]{16}",
    // HTTP Bearer tokens
    r"Bearer\s+[A-Za-z0-9._\-]{8,}",
    // JWT-like three-segment base64url string
    r"ey[A-Za-z0-9_\-]+\.ey[A-Za-z0-9_\-]+\.[A-Za-z0-9_\-]+",
    // Long hex strings (32+ chars) — hash/digest/raw-key shape
    r"\b[0-9a-fA-F]{32,}\b",
];

fn shape_regex_set() -> &'static RegexSet {
    static SET: OnceLock<RegexSet> = OnceLock::new();
    SET.get_or_init(|| RegexSet::new(SHAPE_PATTERNS).expect("valid shape patterns"))
}

fn combined_shape_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let joined = SHAPE_PATTERNS
            .iter()
            .map(|p| format!("(?:{p})"))
            .collect::<Vec<_>>()
            .join("|");
        Regex::new(&joined).expect("valid combined shape pattern")
    })
}

fn field_name_alternation() -> String {
    SENSITIVE_FIELD_NAMES
        .iter()
        .map(|n| regex::escape(n))
        .collect::<Vec<_>>()
        .join("|")
}

fn json_field_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Matches `"password":"..."` or `"password": "..."` with escaped quotes.
        let pat = format!(
            r#"(?i)"(?:{names})"\s*:\s*"(?:[^"\\]|\\.)*""#,
            names = field_name_alternation()
        );
        Regex::new(&pat).expect("valid json field regex")
    })
}

fn kv_field_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Matches `password=...`, `password="..."`, `password: ...` up to the
        // next whitespace / comma / closing bracket.
        let pat = format!(
            r#"(?i)\b(?:{names})\s*[=:]\s*(?:"(?:[^"\\]|\\.)*"|[^\s,;)\]]+)"#,
            names = field_name_alternation()
        );
        Regex::new(&pat).expect("valid kv field regex")
    })
}

/// Run the redaction pipeline against `input`.
///
/// Returns `Cow::Borrowed` when the input did not match any pattern, so
/// callers on the log fast-path pay no allocation for non-sensitive events.
///
/// The function is total and infallible. Any panic here would bubble through
/// the logging infrastructure, so it is careful to only use regexes built
/// from the constants in this module.
pub fn redact_secrets(input: &str) -> Cow<'_, str> {
    // Short-circuit: if nothing matches any of the shape patterns *and* no
    // sensitive field name appears (case-insensitive substring check), the
    // input is clean.
    let shapes_hit = shape_regex_set()
        .matches(input)
        .into_iter()
        .next()
        .is_some();
    let lower = input.to_ascii_lowercase();
    let any_name = SENSITIVE_FIELD_NAMES
        .iter()
        .any(|n| lower.contains(&n.to_ascii_lowercase()));

    if !shapes_hit && !any_name {
        return Cow::Borrowed(input);
    }

    let mut out: String = input.to_string();

    // Pass 1: shape-based defense in depth. Done first so that values like
    // `Bearer <token>` are redacted as a unit before the field-name passes
    // have a chance to slice them at whitespace boundaries.
    out = combined_shape_regex()
        .replace_all(&out, REDACTED)
        .into_owned();

    // Pass 2: JSON-style `"key":"value"` replacements.
    out = json_field_regex()
        .replace_all(&out, |caps: &regex::Captures<'_>| {
            // Preserve the key by extracting up to the first `:`, then append
            // the marker as a JSON string.
            let whole = caps.get(0).unwrap().as_str();
            if let Some(colon) = whole.find(':') {
                let key = &whole[..colon];
                format!("{key}:\"{REDACTED}\"")
            } else {
                REDACTED.to_string()
            }
        })
        .into_owned();

    // Pass 3: `key=value` / `key: value` (non-JSON, logfmt-style).
    out = kv_field_regex()
        .replace_all(&out, |caps: &regex::Captures<'_>| {
            let whole = caps.get(0).unwrap().as_str();
            // Split on first `=` or `:` (the only separator per the regex).
            let sep_idx = whole.find(['=', ':']).unwrap_or(whole.len());
            let (key, rest) = whole.split_at(sep_idx);
            let sep = rest.chars().next().unwrap_or('=');
            format!("{key}{sep}{REDACTED}")
        })
        .into_owned();

    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_plain_text_untouched() {
        let input = "hello world, nothing to redact here";
        let out = redact_secrets(input);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out, input);
    }

    #[test]
    fn redacts_openai_sk_key() {
        let input = "using sk-ABCDEFGHIJKLMNOPQRSTUVWX0123456789 now";
        let out = redact_secrets(input);
        assert!(!out.contains("sk-ABCDE"));
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn redacts_github_pat() {
        let input = "token=ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let out = redact_secrets(input);
        assert!(!out.contains("ghp_abcdef"));
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn redacts_github_pat_new_format() {
        let input = "creds: github_pat_11AAAAAA0aBcDeFgHiJkLmNoPqRsTuVwXyZ012345678_abcdef ok";
        let out = redact_secrets(input);
        assert!(!out.contains("github_pat_11AAAAAA"));
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn redacts_aws_access_key_id() {
        let input = "key AKIAIOSFODNN7EXAMPLE seen";
        let out = redact_secrets(input);
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn redacts_bearer_token_with_dots_and_dashes() {
        let input = "Authorization: Bearer abc.def-ghi_jklmnop";
        let out = redact_secrets(input);
        assert!(!out.contains("abc.def-ghi_jklmnop"));
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn redacts_jwt_like_triplet() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTYifQ.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let input = format!("jwt: {jwt}");
        let out = redact_secrets(&input);
        assert!(!out.contains(jwt));
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn redacts_long_hex_string() {
        let input = "sha256=deadbeefcafebabe0123456789abcdef0123456789abcdef0123456789abcdef";
        let out = redact_secrets(input);
        assert!(!out.contains("deadbeefcafebabe"));
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn does_not_redact_short_hex() {
        // 16 hex chars — below the threshold.
        let input = "short=deadbeefcafebabe";
        let out = redact_secrets(input);
        assert!(out.contains("deadbeefcafebabe"));
    }

    #[test]
    fn redacts_json_field_password() {
        let input = r#"{"user":"alice","password":"hunter2"}"#;
        let out = redact_secrets(input);
        assert!(!out.contains("hunter2"));
        assert!(out.contains(REDACTED));
        assert!(out.contains("\"user\":\"alice\""));
    }

    #[test]
    fn redacts_logfmt_api_key() {
        let input = "user=alice api_key=ABCDEF1234 outcome=ok";
        let out = redact_secrets(input);
        assert!(!out.contains("ABCDEF1234"));
        assert!(out.contains(REDACTED));
        assert!(out.contains("user=alice"));
        assert!(out.contains("outcome=ok"));
    }

    #[test]
    fn redacts_authorization_header_style() {
        let input = r#"headers: {"authorization":"Bearer abc.def.ghi123456"}"#;
        let out = redact_secrets(input);
        assert!(!out.contains("abc.def.ghi123456"));
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn redaction_is_idempotent() {
        let input = "sk-ABCDEFGHIJKLMNOPQRSTUVWX0123456789 and password=hunter2";
        let once = redact_secrets(input).into_owned();
        let twice = redact_secrets(&once).into_owned();
        assert_eq!(once, twice, "redaction should be idempotent");
    }

    #[test]
    fn field_name_match_is_case_insensitive() {
        let input = r#"{"API_KEY":"abcdef"}"#;
        let out = redact_secrets(input);
        assert!(!out.contains("abcdef"));
        assert!(out.contains(REDACTED));
    }
}
