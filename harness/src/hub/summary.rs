use crate::pr_tools::{redact_resilient, strip_unprintable};
use serde::{Deserialize, Serialize};

/// Why a string was refused as a [`Summary`].
///
/// ```
/// use harness::hub::summary::{Summary, SummaryError};
///
/// assert_eq!(Summary::try_from("a  b".to_string()), Err(SummaryError::NotCollapsed));
/// assert_eq!(Summary::try_from("x".repeat(180)), Err(SummaryError::TooLong(180)));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SummaryError {
    #[error("summary is {0} characters; the maximum is 179")]
    TooLong(usize),
    #[error("summary contains unprintable or bidirectional-control characters")]
    Unprintable,
    #[error("summary contains text that redaction would change")]
    Unredacted,
    #[error("summary is not whitespace-collapsed")]
    NotCollapsed,
}

/// A one-line, redacted, printable summary of at most 179 characters, built through
/// [`Summary::compose`] or validated by `TryFrom<String>` and deserialization.
///
/// ```
/// use harness::hub::summary::Summary;
///
/// let from_json: Summary = serde_json::from_str("\"coder [running] fix the parser\"").unwrap();
/// assert_eq!(from_json, Summary::compose("coder", "running", "fix the parser"));
/// assert!(serde_json::from_str::<Summary>("\"token=abc\"").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct Summary(String);

fn violation(text: &str) -> Option<SummaryError> {
    let chars = text.chars().count();
    if chars > Summary::MAX_CHARS {
        Some(SummaryError::TooLong(chars))
    } else if strip_unprintable(text) != text {
        Some(SummaryError::Unprintable)
    } else if redact_resilient(text) != text {
        Some(SummaryError::Unredacted)
    } else if collapse_whitespace(text) != text {
        Some(SummaryError::NotCollapsed)
    } else {
        None
    }
}

impl TryFrom<String> for Summary {
    type Error = SummaryError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match violation(&value) {
            Some(error) => Err(error),
            None => Ok(Summary(value)),
        }
    }
}

const ELLIPSIS: char = '\u{2026}';
const WITHHELD: &str = "[summary withheld]";

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cut_plan(chars: usize) -> Option<usize> {
    if chars <= Summary::MAX_CHARS {
        None
    } else {
        Some(Summary::MAX_CHARS - 1)
    }
}

fn truncate_chars(text: &str) -> Option<String> {
    match cut_plan(text.chars().count()) {
        None => Some(text.to_string()),
        Some(keep) => (0..=keep)
            .rev()
            .map(|k| {
                text.chars()
                    .take(k)
                    .chain(std::iter::once(ELLIPSIS))
                    .collect::<String>()
            })
            .find(|candidate| violation(candidate).is_none()),
    }
}

fn pipeline(raw: &str) -> Option<String> {
    let stripped = strip_unprintable(raw);
    let redacted = redact_resilient(&stripped);
    truncate_chars(&collapse_whitespace(&redacted))
}

fn finish(candidate: Option<String>) -> String {
    candidate
        .filter(|c| violation(c).is_none())
        .unwrap_or_else(|| WITHHELD.to_string())
}

impl Summary {
    /// The largest number of characters a summary may hold (the spec bound is "fewer than 180").
    pub const MAX_CHARS: usize = 179;

    /// Builds `"{identity} [{status_label}] {text}"`, then strips unprintable characters, redacts
    /// secrets, collapses whitespace and truncates to [`Summary::MAX_CHARS`] characters, ending in
    /// an ellipsis when cut. The result never contains text that redaction would change.
    ///
    /// ```
    /// use harness::hub::summary::Summary;
    ///
    /// let s = Summary::compose("coder", "running", "fix\n  the\tparser");
    /// assert_eq!(s.as_str(), "coder [running] fix the parser");
    ///
    /// let long = Summary::compose("coder", "running", &"\u{e9}".repeat(500));
    /// assert_eq!(long.as_str().chars().count(), Summary::MAX_CHARS);
    /// assert!(long.as_str().ends_with('\u{2026}'));
    ///
    /// let hidden = Summary::compose("coder", "running", "api_key=hunter2");
    /// assert!(!hidden.as_str().contains("hunter2"));
    /// ```
    pub fn compose(identity: &str, status_label: &str, text: &str) -> Summary {
        let raw = format!("{identity} [{status_label}] {text}");
        Summary(finish(pipeline(&raw)))
    }

    /// The validated text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pr_tools::{redact_resilient, strip_unprintable};
    use proptest::prelude::*;

    fn invariants(s: &Summary) {
        let text = s.as_str();
        assert!(text.chars().count() < 180, "{} chars", text.chars().count());
        assert!(text.chars().count() <= Summary::MAX_CHARS);
        assert_eq!(strip_unprintable(text), text);
        assert_eq!(redact_resilient(text), text);
        assert_eq!(text, text.split_whitespace().collect::<Vec<_>>().join(" "));
        let json = serde_json::to_string(s).unwrap();
        let back: Summary = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, s);
    }

    #[test]
    fn max_chars_is_one_under_the_spec_bound() {
        assert_eq!(Summary::MAX_CHARS, 179);
    }

    #[test]
    fn happy_path_formats_identity_status_and_text() {
        let s = Summary::compose("coder", "running", "fix the parser");
        assert_eq!(s.as_str(), "coder [running] fix the parser");
        invariants(&s);
    }

    #[test]
    fn empty_inputs_still_yield_a_valid_summary() {
        invariants(&Summary::compose("", "", ""));
        invariants(&Summary::compose("a", "b", ""));
    }

    #[test]
    fn whitespace_and_newlines_collapse_to_single_spaces() {
        let s = Summary::compose("a", "b", "  one\n\n two\t\tthree \r\n ");
        assert_eq!(s.as_str(), "a [b] one two three");
    }

    #[test]
    fn exactly_179_chars_is_kept_whole_and_180_is_cut_with_an_ellipsis() {
        let prefix = "a [b] ";
        let fit = "x".repeat(179 - prefix.len());
        let s = Summary::compose("a", "b", &fit);
        assert_eq!(s.as_str().chars().count(), 179);
        assert!(!s.as_str().ends_with('\u{2026}'));
        let over = "x".repeat(180 - prefix.len());
        let s = Summary::compose("a", "b", &over);
        assert_eq!(s.as_str().chars().count(), 179);
        assert!(s.as_str().ends_with('\u{2026}'));
        assert!(s.as_str().starts_with("a [b] xxx"));
    }

    #[test]
    fn multibyte_input_is_cut_on_char_boundaries_under_the_limit() {
        for unit in [
            "\u{e9}",
            "\u{4e2d}",
            "\u{1f600}",
            "\u{10ffff}",
            "\u{1f468}\u{200d}\u{1f469}",
        ] {
            let s = Summary::compose("id", "st", &unit.repeat(400));
            invariants(&s);
        }
    }

    #[test]
    fn combining_mark_floods_cannot_break_the_limit_or_leave_dangling_marks() {
        let flood = format!("e{}", "\u{301}".repeat(500));
        invariants(&Summary::compose("id", "st", &flood));
        let spread = "e\u{301}".repeat(300);
        invariants(&Summary::compose("id", "st", &spread));
    }

    #[test]
    fn bidi_and_invisible_controls_are_removed() {
        let hostile = "a\u{202e}b\u{2066}c\u{200b}d\u{feff}e\u{0}f\u{1b}g\u{2028}h";
        let s = Summary::compose("i\u{202e}d", "s\u{202d}t", hostile);
        invariants(&s);
        for c in [
            '\u{202e}', '\u{2066}', '\u{200b}', '\u{feff}', '\u{0}', '\u{1b}', '\u{202d}',
        ] {
            assert!(!s.as_str().contains(c), "{c:?}");
        }
    }

    #[test]
    fn secrets_are_redacted_in_every_argument() {
        let s = Summary::compose(
            "sk-abcdefghijklmnopqrstuvwxyz",
            "token=hunter2",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789 password: x",
        );
        invariants(&s);
        assert!(!s.as_str().contains("hunter2"));
        assert!(!s.as_str().contains("ghp_abcdef"));
        assert!(!s.as_str().contains("sk-abcdef"));
        assert!(s.as_str().contains("<redacted:"));
    }

    #[test]
    fn truncation_cannot_resurrect_a_secret_split_by_the_cut() {
        let filler = "y".repeat(150);
        let s = Summary::compose("a", "b", &format!("{filler} api_key=SECRETVALUE123"));
        invariants(&s);
        assert!(!s.as_str().contains("SECRETVALUE"));
    }

    #[test]
    fn compose_is_idempotent_on_its_own_output() {
        let s = Summary::compose("a", "b", &"\u{e9} token=abc ".repeat(40));
        let again = Summary::compose("", "", s.as_str());
        let reparsed: Summary = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(reparsed, s);
        invariants(&again);
    }

    #[test]
    fn deserialize_accepts_exactly_179_chars_and_rejects_180() {
        let ok = format!("\"{}\"", "x".repeat(179));
        assert!(serde_json::from_str::<Summary>(&ok).is_ok());
        let long = format!("\"{}\"", "x".repeat(180));
        assert!(serde_json::from_str::<Summary>(&long).is_err());
        let multibyte = format!("\"{}\"", "\u{e9}".repeat(180));
        assert!(serde_json::from_str::<Summary>(&multibyte).is_err());
        let multibyte_ok = format!("\"{}\"", "\u{e9}".repeat(179));
        assert!(serde_json::from_str::<Summary>(&multibyte_ok).is_ok());
    }

    #[test]
    fn deserialize_rejects_unprintable_unredacted_and_uncollapsed_input() {
        for bad in [
            "\"a\\u202eb\"",
            "\"a\\u200bb\"",
            "\"a\\u0000b\"",
            "\"a\\nb\"",
            "\"a  b\"",
            "\" a\"",
            "\"token=abc\"",
            "\"sk-abcdefghijklmnopqrstuvwxyz\"",
        ] {
            assert!(serde_json::from_str::<Summary>(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn try_from_reports_the_specific_reason() {
        assert_eq!(
            Summary::try_from("x".repeat(180)),
            Err(SummaryError::TooLong(180))
        );
        assert_eq!(
            Summary::try_from("a\u{202e}b".to_string()),
            Err(SummaryError::Unprintable)
        );
        assert_eq!(
            Summary::try_from("token=abc".to_string()),
            Err(SummaryError::Unredacted)
        );
        assert_eq!(
            Summary::try_from("a  b".to_string()),
            Err(SummaryError::NotCollapsed)
        );
    }

    #[test]
    fn finish_withholds_every_kind_of_invalid_candidate_and_keeps_valid_ones() {
        for bad in [
            "x".repeat(180),
            "a\u{202e}b".to_string(),
            "token=abc".to_string(),
            "a  b".to_string(),
        ] {
            assert_eq!(finish(Some(bad)), WITHHELD);
        }
        assert_eq!(finish(Some("fine text".to_string())), "fine text");
        assert_eq!(finish(None), WITHHELD);
        assert!(violation(WITHHELD).is_none());
        assert_eq!(
            Summary::try_from(WITHHELD.to_string()).unwrap().as_str(),
            WITHHELD
        );
    }

    #[test]
    fn the_lone_ellipsis_never_violates_so_the_shortest_cut_always_exists() {
        assert_eq!(violation(&ELLIPSIS.to_string()), None);
    }

    #[test]
    fn cut_plan_boundaries() {
        assert_eq!(cut_plan(0), None);
        assert_eq!(cut_plan(179), None);
        assert_eq!(cut_plan(180), Some(178));
        assert_eq!(cut_plan(usize::MAX), Some(178));
    }

    #[test]
    fn truncate_chars_keeps_whole_text_at_the_limit_and_cuts_one_over() {
        let at = "\u{e9}".repeat(179);
        assert_eq!(truncate_chars(&at), Some(at.clone()));
        let over = "\u{e9}".repeat(180);
        let cut = truncate_chars(&over).unwrap();
        assert_eq!(cut.chars().count(), 179);
        assert!(cut.ends_with(ELLIPSIS));
        assert_eq!(cut.chars().filter(|c| *c == '\u{e9}').count(), 178);
    }

    #[test]
    fn truncation_that_exposes_a_dangling_mark_still_yields_a_valid_summary() {
        for marks in 0..6 {
            let text = format!("{}e{}tail", "x".repeat(170), "\u{301}".repeat(marks));
            let s = Summary::compose("a", "b", &text);
            invariants(&s);
            assert_eq!(Summary::try_from(s.as_str().to_string()).as_ref(), Ok(&s));
        }
    }

    fn is_bidi_control(c: char) -> bool {
        matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}')
    }

    fn nasty_char() -> impl Strategy<Value = char> {
        prop_oneof![
            4 => any::<char>(),
            2 => prop::sample::select(vec![
                '\u{202e}', '\u{2066}', '\u{200b}', '\u{200d}', '\u{fe0f}', '\u{20e3}',
                '\u{301}', '\u{feff}', '\u{0}', '\u{1b}', '\r', '\n', '\t', ' ', '\u{2028}',
                '\u{e0041}', '\u{1f600}', '\u{10ffff}', '=', ':', '_', '-', '\u{2026}',
            ]),
            1 => prop::sample::select("abcdefghijklmnopqrstuvwxyz0123456789".chars().collect::<Vec<_>>()),
        ]
    }

    fn nasty_string() -> impl Strategy<Value = String> {
        prop_oneof![
            3 => prop::collection::vec(nasty_char(), 0..400).prop_map(|v| v.into_iter().collect()),
            1 => prop::collection::vec(
                prop::sample::select(vec![
                    "token=", "sk-", "ghp_", "password: ", "Bearer ", "AKIA", "https://u:p@h/",
                    "abcdefghijklmnopqrstuvwxyz", "\u{301}", "\u{202e}", " ", "\u{e9}", "[", "]",
                ]),
                0..40
            ).prop_map(|v| v.concat()),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn compose_always_satisfies_every_invariant(
            identity in nasty_string(),
            status in nasty_string(),
            text in nasty_string(),
        ) {
            let s = Summary::compose(&identity, &status, &text);
            let out = s.as_str();
            prop_assert!(out.chars().count() < 180);
            prop_assert_eq!(strip_unprintable(out), out);
            prop_assert_eq!(redact_resilient(out), out);
            prop_assert!(!out.chars().any(is_bidi_control));
            prop_assert_eq!(out, out.split_whitespace().collect::<Vec<_>>().join(" "));
            let json = serde_json::to_string(&s).unwrap();
            prop_assert_eq!(serde_json::from_str::<Summary>(&json).unwrap(), s.clone());
            prop_assert_eq!(Summary::try_from(out.to_string()), Ok(s.clone()));
        }

        #[test]
        fn deserialize_never_accepts_what_compose_would_change(raw in nasty_string()) {
            if let Ok(s) = serde_json::from_value::<Summary>(serde_json::Value::String(raw.clone())) {
                prop_assert!(s.as_str().chars().count() < 180);
                prop_assert_eq!(strip_unprintable(&raw), raw.clone());
                prop_assert_eq!(redact_resilient(&raw), raw);
            }
        }
    }
}

#[cfg(kani)]
mod kani_proofs {
    use super::{cut_plan, Summary};

    #[kani::proof]
    fn cut_plan_output_is_within_the_limit_and_never_over_slices() {
        let chars: usize = kani::any();
        let out = match cut_plan(chars) {
            None => chars,
            Some(keep) => {
                assert!(keep <= chars);
                keep + 1
            }
        };
        assert!(out <= Summary::MAX_CHARS);
        assert!(out < 180);
    }
}
