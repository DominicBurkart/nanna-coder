use regex::Regex;
use std::sync::OnceLock;

/// Replacement marker for a matched secret of `kind`.
pub fn marker(kind: &str) -> String {
    format!("<redacted:{kind}>")
}

fn patterns() -> &'static [(Regex, &'static str)] {
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            (r"\b(?:gh[pousr]_[A-Za-z0-9]{16,}|github_pat_[A-Za-z0-9_]{16,})", "<redacted:github-token>"),
            (r"\bsk-[A-Za-z0-9_\-]{16,}", "<redacted:api-key>"),
            (r"\bAKIA[0-9A-Z]{16}\b", "<redacted:aws-key>"),
            (r"(?i)\bbearer\s+[A-Za-z0-9\-._~+/]+=*", "Bearer <redacted:bearer>"),
            (r#"(?i)(token|secret|password|passwd|api[_\-]?key)(["']?\s*[=:]\s*["']?)[^\s"'&,;]+"#, "$1$2<redacted:credential>"),
            (r"\b([a-zA-Z][a-zA-Z0-9+.\-]*://)[^/\s@:]+(?::[^/\s@]*)?@", "$1<redacted:userinfo>@"),
        ]
        .into_iter()
        .map(|(pattern, replacement)| (Regex::new(pattern).expect("static redaction pattern"), replacement))
        .collect()
    })
}

/// Mask common secret shapes in `text`: GitHub tokens (`ghp_…`, `gho_…`,
/// `ghu_…`, `ghs_…`, `ghr_…`, `github_pat_…`), OpenAI-style `sk-…` keys,
/// AWS access key ids (`AKIA…`), `Bearer …` credentials, `token=`, `secret=`,
/// `password=` and `api_key=` style assignments (`:` accepted as separator)
/// and the user-info part of URLs. Each match becomes `<redacted:<kind>>`.
///
/// This is defence in depth, not a security boundary: it is applied to every
/// escalation body before it leaves the process (see issue #232, which this
/// helper addresses for escalations only). Redaction is idempotent.
///
/// ```
/// use harness::escalation::redact;
///
/// assert_eq!(redact("push failed with ghp_abcdefghijklmnopqrstuvwxyz0123456789"), "push failed with <redacted:github-token>");
/// assert_eq!(redact("Authorization: Bearer eyJhbGci.payload.sig"), "Authorization: Bearer <redacted:bearer>");
/// assert_eq!(redact("AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE"), "AWS_ACCESS_KEY_ID=<redacted:aws-key>");
/// assert_eq!(redact("password=hunter2 and token: t0ps3cret"), "password=<redacted:credential> and token: <redacted:credential>");
/// assert_eq!(redact("git clone https://alice:pw@example.invalid/r.git"), "git clone https://<redacted:userinfo>@example.invalid/r.git");
/// assert_eq!(redact("nothing to hide"), "nothing to hide");
/// ```
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for (pattern, replacement) in patterns() {
        out = pattern.replace_all(&out, *replacement).into_owned();
    }
    out
}

/// Apply [`redact`] to every string inside a JSON value, in place, including
/// object keys' values nested at any depth.
///
/// ```
/// use harness::escalation::redact_value;
///
/// let mut value = serde_json::json!({"summary": "secret=abc", "evidence": ["sk-abcdefghijklmnopqrstuvwxyz"], "n": 1});
/// redact_value(&mut value);
/// assert_eq!(value["summary"], "secret=<redacted:credential>");
/// assert_eq!(value["evidence"][0], "<redacted:api-key>");
/// assert_eq!(value["n"], 1);
/// ```
pub fn redact_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(s) => *s = redact(s),
        serde_json::Value::Array(items) => items.iter_mut().for_each(redact_value),
        serde_json::Value::Object(map) => map.values_mut().for_each(redact_value),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const GITHUB: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    const GITHUB_PAT: &str = "github_pat_11ABCDEFG0123456789_abcdefghijklmnop";
    const OPENAI: &str = "sk-proj-abcdefghijklmnopqrstuvwxyz";
    const AWS: &str = "AKIAIOSFODNN7EXAMPLE";

    #[test]
    fn masks_every_token_family() {
        assert_eq!(redact(GITHUB), marker("github-token"));
        assert_eq!(
            redact(&format!("gho_{}", &GITHUB[4..])),
            marker("github-token")
        );
        assert_eq!(redact(GITHUB_PAT), marker("github-token"));
        assert_eq!(redact(OPENAI), marker("api-key"));
        assert_eq!(redact(AWS), marker("aws-key"));
        assert_eq!(redact("bearer abc.def-ghi=="), "Bearer <redacted:bearer>");
        assert_eq!(
            redact("GITHUB_TOKEN=abc"),
            "GITHUB_TOKEN=<redacted:credential>"
        );
        assert_eq!(
            redact("api-key: 'k1' passwd=\"p\""),
            "api-key: '<redacted:credential>' passwd=\"<redacted:credential>\""
        );
        assert_eq!(
            redact(r#"{"secret": "abc"}"#),
            r#"{"secret": "<redacted:credential>"}"#
        );
        assert_eq!(
            redact("ssh://bob@host/x"),
            "ssh://<redacted:userinfo>@host/x"
        );
    }

    #[test]
    fn leaves_ordinary_text_and_short_lookalikes_alone() {
        assert_eq!(redact("ghp_short"), "ghp_short");
        assert_eq!(redact("AKIA123"), "AKIA123");
        assert_eq!(redact("the token was rotated"), "the token was rotated");
        assert_eq!(
            redact("https://example.invalid/path"),
            "https://example.invalid/path"
        );
        assert_eq!(redact(""), "");
    }

    #[test]
    fn token_inside_assignment_is_masked_once_and_stays_stable() {
        let once = redact(&format!("token={GITHUB}"));
        assert_eq!(once, "token=<redacted:credential>");
        assert_eq!(redact(&once), once);
        let bearer = redact(&format!("Bearer {GITHUB}"));
        assert_eq!(bearer, "Bearer <redacted:github-token>");
        assert_eq!(redact(&bearer), bearer);
    }

    #[test]
    fn redacts_nested_json_values_only() {
        let mut value =
            serde_json::json!({"a": {"b": [format!("x {AWS}"), 2, null, true]}, "c": "Bearer t"});
        redact_value(&mut value);
        assert_eq!(value["a"]["b"][0], "x <redacted:aws-key>");
        assert_eq!(value["a"]["b"][1], 2);
        assert_eq!(value["a"]["b"][2], serde_json::Value::Null);
        assert_eq!(value["c"], "Bearer <redacted:bearer>");
    }

    fn secret() -> impl Strategy<Value = (String, String)> {
        prop_oneof![
            "ghp_[A-Za-z0-9]{36}".prop_map(|s| (s.clone(), s)),
            "sk-[A-Za-z0-9]{24}".prop_map(|s| (s.clone(), s)),
            "AKIA[0-9A-Z]{16}".prop_map(|s| (s.clone(), s)),
            "[A-Za-z0-9._-]{10,32}".prop_map(|s| (format!("Bearer {s}"), s)),
            ("(token|SECRET|password|api_key)", "[A-Za-z0-9!]{10,20}")
                .prop_map(|(k, v)| (format!("{k}={v}"), v)),
            ("[a-z]{4,8}", "[a-z0-9]{10,12}")
                .prop_map(|(u, p)| (format!("https://{u}:{p}@h.invalid"), p)),
        ]
    }

    proptest! {
        #[test]
        fn redaction_is_idempotent_and_removes_planted_secrets(prefix in "[ -~]{0,24}", (planted, payload) in secret(), suffix in "[ -~]{0,24}") {
            let text = format!("{prefix} {planted} {suffix}");
            let once = redact(&text);
            prop_assert_eq!(redact(&once), once.clone());
            prop_assert!(!once.contains(&payload), "{} still contains {}", once, payload);
        }

        #[test]
        fn redaction_is_idempotent_on_arbitrary_text(text in "[ -~]{0,64}") {
            let once = redact(&text);
            prop_assert_eq!(redact(&once), once);
        }
    }
}
