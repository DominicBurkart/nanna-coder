use super::lineage::SessionId;
use super::summary::Summary;
use crate::pr_tools::{redact_resilient, strip_unprintable};
use crate::task::TaskId;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::fmt;

/// The largest librarian reply the hub will look at, in bytes.
///
/// ```
/// assert_eq!(harness::hub::query::MAX_RAW_BYTES, 16 * 1024);
/// ```
pub const MAX_RAW_BYTES: usize = 16 * 1024;

/// The longest session id accepted from a librarian reply, in characters.
///
/// ```
/// assert_eq!(harness::hub::query::MAX_ID_CHARS, 128);
/// ```
pub const MAX_ID_CHARS: usize = 128;

/// The most matches a query may ask for.
///
/// ```
/// assert_eq!(harness::hub::query::MAX_RESULTS, 10);
/// ```
pub const MAX_RESULTS: u8 = 10;

/// Why a librarian reply was refused. No variant carries any of the reply's text.
///
/// ```
/// use harness::hub::query::ParseError;
///
/// assert_eq!(ParseError::Malformed.to_string(), "librarian reply refused: not the expected JSON object");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("librarian reply refused: larger than the size cap")]
    TooLarge,
    #[error("librarian reply refused: not the expected JSON object")]
    Malformed,
    #[error("librarian reply refused: a match names a session outside the view")]
    UnknownSession,
    #[error("librarian reply refused: a session is named twice")]
    Duplicate,
    #[error("librarian reply refused: more matches than requested")]
    TooManyMatches,
    #[error("librarian reply refused: a relevance is outside 0 to 1")]
    BadRelevance,
    #[error("librarian reply refused: a reason is empty, too long, unprintable or unredacted")]
    BadWhy,
    #[error("librarian reply refused: the requested match count is outside 1 to 10")]
    BadLimit,
}

/// The final text of a librarian run, size-capped at construction and never shown by `Debug`.
///
/// ```
/// use harness::hub::query::{ParseError, RawLibrarianAnswer, MAX_RAW_BYTES};
///
/// let raw = RawLibrarianAnswer::new("{\"matches\":[]}".to_string()).unwrap();
/// assert_eq!(format!("{raw:?}"), "RawLibrarianAnswer { bytes: 14 }");
/// let big = "x".repeat(MAX_RAW_BYTES + 1);
/// assert_eq!(RawLibrarianAnswer::new(big).unwrap_err(), ParseError::TooLarge);
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct RawLibrarianAnswer(String);

impl RawLibrarianAnswer {
    /// Wraps the final librarian text; refuses more than [`MAX_RAW_BYTES`] bytes.
    ///
    /// ```
    /// use harness::hub::query::{RawLibrarianAnswer, MAX_RAW_BYTES};
    ///
    /// assert!(RawLibrarianAnswer::new("x".repeat(MAX_RAW_BYTES)).is_ok());
    /// assert!(RawLibrarianAnswer::new("x".repeat(MAX_RAW_BYTES + 1)).is_err());
    /// ```
    pub fn new(text: String) -> Result<Self, ParseError> {
        ensure(text.len() <= MAX_RAW_BYTES, ParseError::TooLarge)?;
        Ok(Self(text))
    }
}

impl fmt::Debug for RawLibrarianAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bytes = self.0.len();
        write!(f, "RawLibrarianAnswer {{ bytes: {bytes} }}")
    }
}

/// One validated match: a session id from the known set, a relevance in `[0, 1]` and a reason.
/// `Debug` shows the relevance and the reason's length, never its text.
///
/// ```
/// use harness::hub::lineage::SessionId;
/// use harness::hub::query::{parse_strict, RawLibrarianAnswer};
/// use harness::task::TaskId;
/// use std::collections::BTreeSet;
///
/// let id = SessionId::from_task(TaskId("t-1".into()));
/// let known = BTreeSet::from([id.clone()]);
/// let raw = RawLibrarianAnswer::new(r#"{"matches":[{"id":"t-1","relevance":0.5,"why":"edits the parser"}]}"#.into()).unwrap();
/// let found = parse_strict(&raw, &known, 5).unwrap();
/// assert_eq!(found[0].id(), &id);
/// assert_eq!(found[0].why(), "edits the parser");
/// assert_eq!(format!("{:?}", found[0]), "LibrarianMatch { relevance: 0.5, why_chars: 16 }");
/// ```
#[derive(Clone, PartialEq)]
pub struct LibrarianMatch {
    id: SessionId,
    relevance: f32,
    why: String,
}

impl LibrarianMatch {
    /// The matched session, always a member of the known set.
    ///
    /// ```
    /// use harness::hub::lineage::SessionId;
    /// use harness::hub::query::{parse_strict, RawLibrarianAnswer};
    /// use harness::task::TaskId;
    /// use std::collections::BTreeSet;
    ///
    /// let id = SessionId::from_task(TaskId("t-1".into()));
    /// let known = BTreeSet::from([id.clone()]);
    /// let text = r#"{"matches":[{"id":"t-1","relevance":1,"why":"same files"}]}"#;
    /// let found = parse_strict(&RawLibrarianAnswer::new(text.into()).unwrap(), &known, 1).unwrap();
    /// assert_eq!(found[0].id(), &id);
    /// ```
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// The relevance, a finite number in `[0, 1]` that is never negative zero.
    ///
    /// ```
    /// use harness::hub::lineage::SessionId;
    /// use harness::hub::query::{parse_strict, RawLibrarianAnswer};
    /// use harness::task::TaskId;
    /// use std::collections::BTreeSet;
    ///
    /// let known = BTreeSet::from([SessionId::from_task(TaskId("t-1".into()))]);
    /// let text = r#"{"matches":[{"id":"t-1","relevance":-0.0,"why":"same files"}]}"#;
    /// let found = parse_strict(&RawLibrarianAnswer::new(text.into()).unwrap(), &known, 1).unwrap();
    /// assert!(found[0].relevance().is_sign_positive());
    /// ```
    pub fn relevance(&self) -> f32 {
        self.relevance
    }

    /// The reason: non-empty, at most 179 characters, printable and unchanged by redaction.
    ///
    /// ```
    /// use harness::hub::lineage::SessionId;
    /// use harness::hub::query::{parse_strict, RawLibrarianAnswer};
    /// use harness::task::TaskId;
    /// use std::collections::BTreeSet;
    ///
    /// let known = BTreeSet::from([SessionId::from_task(TaskId("t-1".into()))]);
    /// let text = r#"{"matches":[{"id":"t-1","relevance":1,"why":"same files"}]}"#;
    /// let found = parse_strict(&RawLibrarianAnswer::new(text.into()).unwrap(), &known, 1).unwrap();
    /// assert_eq!(found[0].why(), "same files");
    /// ```
    pub fn why(&self) -> &str {
        &self.why
    }

    /// Splits the match into `(id, relevance, why)`.
    ///
    /// ```
    /// use harness::hub::lineage::SessionId;
    /// use harness::hub::query::{parse_strict, RawLibrarianAnswer};
    /// use harness::task::TaskId;
    /// use std::collections::BTreeSet;
    ///
    /// let id = SessionId::from_task(TaskId("t-1".into()));
    /// let known = BTreeSet::from([id.clone()]);
    /// let text = r#"{"matches":[{"id":"t-1","relevance":1,"why":"same files"}]}"#;
    /// let found = parse_strict(&RawLibrarianAnswer::new(text.into()).unwrap(), &known, 1).unwrap();
    /// let (got, relevance, why) = found[0].clone().into_fields();
    /// assert_eq!((got, relevance, why.as_str()), (id, 1.0, "same files"));
    /// ```
    pub fn into_fields(self) -> (SessionId, f32, String) {
        (self.id, self.relevance, self.why)
    }
}

impl fmt::Debug for LibrarianMatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let relevance = self.relevance;
        let why_chars = self.why.chars().count();
        write!(
            f,
            "LibrarianMatch {{ relevance: {relevance}, why_chars: {why_chars} }}"
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireAnswer {
    matches: Vec<WireMatch>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireMatch {
    id: String,
    relevance: f64,
    why: String,
}

fn ensure(ok: bool, error: ParseError) -> Result<(), ParseError> {
    if ok {
        Ok(())
    } else {
        Err(error)
    }
}

fn decode(text: &str) -> Result<WireAnswer, ParseError> {
    serde_json::from_str(text).map_err(|_| ParseError::Malformed)
}

fn known_id(text: &str, known: &BTreeSet<SessionId>) -> Result<SessionId, ParseError> {
    ensure(
        text.chars().count() <= MAX_ID_CHARS,
        ParseError::UnknownSession,
    )?;
    let id = SessionId::from_task(TaskId(text.to_string()));
    ensure(known.contains(&id), ParseError::UnknownSession)?;
    Ok(id)
}

fn checked_relevance(value: f64) -> Result<f32, ParseError> {
    ensure((0.0..=1.0).contains(&value), ParseError::BadRelevance)?;
    Ok((value as f32).abs())
}

fn checked_why(text: String) -> Result<String, ParseError> {
    ensure(!text.trim().is_empty(), ParseError::BadWhy)?;
    ensure(
        text.chars().count() <= Summary::MAX_CHARS,
        ParseError::BadWhy,
    )?;
    ensure(!text.chars().any(char::is_control), ParseError::BadWhy)?;
    ensure(strip_unprintable(&text) == text, ParseError::BadWhy)?;
    ensure(redact_resilient(&text) == text, ParseError::BadWhy)?;
    Ok(text)
}

fn rank(a: &LibrarianMatch, b: &LibrarianMatch) -> std::cmp::Ordering {
    b.relevance
        .total_cmp(&a.relevance)
        .then_with(|| a.id.cmp(&b.id))
}

/// Parses a librarian reply strictly: the whole text (JSON whitespace allowed at the ends, nothing else) is one JSON object
/// `{"matches":[{"id","relevance","why"}]}` with no other key at either level.
/// Every id must be in `known`, appear once and be at most [`MAX_ID_CHARS`] long; there are at
/// most `max` matches (1 to [`MAX_RESULTS`]); a relevance is a number in `[0, 1]`; a reason is
/// non-empty, at most 179 characters, printable and unchanged by redaction. Anything else is
/// refused as a whole with a content-free [`ParseError`]; there is never a partial result.
/// Matches come back by relevance, highest first, then by id.
///
/// ```
/// use harness::hub::lineage::SessionId;
/// use harness::hub::query::{parse_strict, ParseError, RawLibrarianAnswer};
/// use harness::task::TaskId;
/// use std::collections::BTreeSet;
///
/// let known = BTreeSet::from([SessionId::from_task(TaskId("a".into()))]);
/// let ok = RawLibrarianAnswer::new(r#"{"matches":[]}"#.into()).unwrap();
/// assert!(parse_strict(&ok, &known, 5).unwrap().is_empty());
/// let fenced = RawLibrarianAnswer::new("```json\n{\"matches\":[]}\n```".into()).unwrap();
/// assert_eq!(parse_strict(&fenced, &known, 5).unwrap_err(), ParseError::Malformed);
/// let foreign = RawLibrarianAnswer::new(r#"{"matches":[{"id":"zz","relevance":1,"why":"x"}]}"#.into()).unwrap();
/// assert_eq!(parse_strict(&foreign, &known, 5).unwrap_err(), ParseError::UnknownSession);
/// ```
pub fn parse_strict(
    raw: &RawLibrarianAnswer,
    known: &BTreeSet<SessionId>,
    max: u8,
) -> Result<Vec<LibrarianMatch>, ParseError> {
    ensure((1..=MAX_RESULTS).contains(&max), ParseError::BadLimit)?;
    let wire = decode(&raw.0)?;
    ensure(
        wire.matches.len() <= usize::from(max),
        ParseError::TooManyMatches,
    )?;
    let mut seen = BTreeSet::new();
    let mut found = Vec::new();
    for entry in wire.matches {
        let id = known_id(&entry.id, known)?;
        ensure(seen.insert(id.clone()), ParseError::Duplicate)?;
        let relevance = checked_relevance(entry.relevance)?;
        let why = checked_why(entry.why)?;
        found.push(LibrarianMatch { id, relevance, why });
    }
    found.sort_by(rank);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::{json, Value};

    const TOKEN: &str = "PLANTEDxq7Zk";
    const SECRET: &str = "ghp_aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5";

    fn sid(text: &str) -> SessionId {
        SessionId::from_task(TaskId(text.to_string()))
    }

    fn known(ids: &[&str]) -> BTreeSet<SessionId> {
        ids.iter().map(|id| sid(id)).collect()
    }

    fn raw(text: &str) -> RawLibrarianAnswer {
        RawLibrarianAnswer::new(text.to_string()).unwrap()
    }

    fn parse(text: &str, max: u8) -> Result<Vec<LibrarianMatch>, ParseError> {
        parse_strict(&raw(text), &arb_known(), max)
    }

    fn reply(id: &str, relevance: Value, why: &str) -> String {
        json!({"matches": [{"id": id, "relevance": relevance, "why": why}]}).to_string()
    }

    fn with_why(why: &str) -> String {
        reply("a", json!(0.5), why)
    }

    fn with_relevance(relevance: &str) -> String {
        format!(r#"{{"matches":[{{"id":"a","relevance":{relevance},"why":"ok"}}]}}"#)
    }

    fn refused(text: &str) -> ParseError {
        parse(text, 5).unwrap_err()
    }

    fn texts(error: ParseError) -> String {
        format!("{error} {error:?}")
    }

    #[test]
    fn accepts_the_documented_shape() {
        let found = parse(&with_why("edits the parser"), 5).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id(), &sid("a"));
        assert_eq!(found[0].relevance(), 0.5);
        assert_eq!(found[0].why(), "edits the parser");
        let (id, relevance, why) = found[0].clone().into_fields();
        assert_eq!(
            (id, relevance, why.as_str()),
            (sid("a"), 0.5, "edits the parser")
        );
    }

    #[test]
    fn empty_matches_is_a_valid_answer() {
        assert!(parse(r#"{"matches":[]}"#, 5).unwrap().is_empty());
    }

    #[test]
    fn json_whitespace_around_the_object_is_allowed() {
        assert!(parse("\n  {\"matches\":[]} \t\r\n", 5).is_ok());
    }

    #[test]
    fn unicode_whitespace_around_the_object_is_refused() {
        assert_eq!(refused("\u{a0}{\"matches\":[]}"), ParseError::Malformed);
        assert_eq!(refused("{\"matches\":[]}\u{2003}"), ParseError::Malformed);
    }

    #[test]
    fn raw_answer_cap_is_exact() {
        let pad = " ".repeat(MAX_RAW_BYTES - 14);
        let at_cap = format!("{{\"matches\":[]{pad}}}");
        assert_eq!(at_cap.len(), MAX_RAW_BYTES);
        assert!(parse(&at_cap, 5).is_ok());
        let over = format!("{{\"matches\":[] {pad}}}");
        assert_eq!(over.len(), MAX_RAW_BYTES + 1);
        assert_eq!(
            RawLibrarianAnswer::new(over).unwrap_err(),
            ParseError::TooLarge
        );
    }

    #[test]
    fn why_cap_counts_characters_not_bytes() {
        let ascii = "a".repeat(179);
        assert!(parse(&with_why(&ascii), 5).is_ok());
        assert_eq!(refused(&with_why(&"a".repeat(180))), ParseError::BadWhy);
        let wide = "é".repeat(179);
        assert_eq!(wide.len(), 358);
        assert!(parse(&with_why(&wide), 5).is_ok());
        assert_eq!(refused(&with_why(&"é".repeat(180))), ParseError::BadWhy);
        let cjk = "日".repeat(179);
        assert!(parse(&with_why(&cjk), 5).is_ok());
        assert_eq!(refused(&with_why(&"日".repeat(180))), ParseError::BadWhy);
    }

    #[test]
    fn why_cap_matches_the_summary_cap() {
        assert_eq!(Summary::MAX_CHARS, 179);
    }

    #[test]
    fn why_of_one_character_is_accepted_and_empty_is_not() {
        assert!(parse(&with_why("x"), 5).is_ok());
        assert_eq!(refused(&with_why("")), ParseError::BadWhy);
        assert_eq!(refused(&with_why("   ")), ParseError::BadWhy);
    }

    #[test]
    fn why_with_control_characters_is_refused() {
        for bad in ["a\nb", "a\tb", "a\u{0}b", "a\u{7}b", "a\u{85}b", "a\rb"] {
            assert_eq!(refused(&with_why(bad)), ParseError::BadWhy, "{bad:?}");
        }
    }

    #[test]
    fn why_with_hidden_or_bidi_characters_is_refused() {
        let bad = [
            "\u{202e}",
            "\u{200d}",
            "\u{200b}",
            "\u{feff}",
            "\u{ad}",
            "\u{e0041}",
            "\u{2066}",
        ];
        for part in bad {
            let why = format!("fine{part}text");
            assert_eq!(refused(&with_why(&why)), ParseError::BadWhy, "{part:?}");
        }
    }

    #[test]
    fn why_with_unicode_escapes_for_bidi_is_refused() {
        let text = "{\"matches\":[{\"id\":\"a\",\"relevance\":0.5,\"why\":\"x\\u202ey\"}]}";
        assert_eq!(refused(text), ParseError::BadWhy);
    }

    #[test]
    fn why_that_redaction_would_change_is_refused() {
        assert_eq!(refused(&with_why(SECRET)), ParseError::BadWhy);
        let spaced = format!("see {SECRET} now");
        assert_eq!(refused(&with_why(&spaced)), ParseError::BadWhy);
        let joined = SECRET.replace('_', "_\u{200d}");
        assert_eq!(refused(&with_why(&joined)), ParseError::BadWhy);
    }

    #[test]
    fn multibyte_and_combining_text_is_accepted_when_printable() {
        let found = parse(&with_why("naïve café 日本語 résumé"), 5).unwrap();
        assert_eq!(found[0].why(), "naïve café 日本語 résumé");
        let combining = "e\u{301} cafe\u{301}";
        assert_eq!(parse(&with_why(combining), 5).unwrap()[0].why(), combining);
    }

    #[test]
    fn why_may_quote_json_as_plain_text() {
        let found = parse(&with_why(r#"{"matches":[]} and "quotes""#), 5).unwrap();
        assert_eq!(found[0].why(), r#"{"matches":[]} and "quotes""#);
    }

    #[test]
    fn relevance_bounds_are_exact() {
        for good in ["0", "0.0", "1", "1.0", "0.5", "1e0", "0e5", "-0.0", "5e-1"] {
            assert!(parse(&with_relevance(good), 5).is_ok(), "{good}");
        }
        for bad in [
            "1.0000001",
            "-0.0000001",
            "2",
            "-1",
            "1e300",
            "-1e300",
            "1.000000000001",
        ] {
            assert_eq!(
                refused(&with_relevance(bad)),
                ParseError::BadRelevance,
                "{bad}"
            );
        }
    }

    #[test]
    fn negative_zero_is_normalised() {
        let found = parse(&with_relevance("-0.0"), 5).unwrap();
        assert!(found[0].relevance().is_sign_positive());
        assert_eq!(found[0].relevance(), 0.0);
    }

    #[test]
    fn non_finite_and_non_numeric_relevance_is_malformed() {
        for bad in [
            "1e999",
            "-1e999",
            "NaN",
            "Infinity",
            "-Infinity",
            "\"0.5\"",
            "null",
            "true",
        ] {
            assert_eq!(
                refused(&with_relevance(bad)),
                ParseError::Malformed,
                "{bad}"
            );
        }
        assert_eq!(refused(&with_relevance("[0.5]")), ParseError::Malformed);
        assert_eq!(refused(&with_relevance("{\"v\":1}")), ParseError::Malformed);
    }

    #[test]
    fn match_count_cap_is_exact() {
        let ids = ["a", "b", "c"];
        let entries: Vec<Value> = ids.iter().map(|id| entry(id, 0.5, "ok")).collect();
        let text = json!({"matches": entries}).to_string();
        assert_eq!(parse(&text, 3).unwrap().len(), 3);
        assert_eq!(parse(&text, 2).unwrap_err(), ParseError::TooManyMatches);
        assert_eq!(parse(&text, 1).unwrap_err(), ParseError::TooManyMatches);
    }

    fn entry(id: &str, relevance: f64, why: &str) -> Value {
        json!({"id": id, "relevance": relevance, "why": why})
    }

    #[test]
    fn requested_limit_must_be_one_to_ten() {
        let text = r#"{"matches":[]}"#;
        assert_eq!(parse(text, 0).unwrap_err(), ParseError::BadLimit);
        assert!(parse(text, 1).is_ok());
        assert!(parse(text, MAX_RESULTS).is_ok());
        assert_eq!(
            parse(text, MAX_RESULTS + 1).unwrap_err(),
            ParseError::BadLimit
        );
        assert_eq!(parse(text, u8::MAX).unwrap_err(), ParseError::BadLimit);
    }

    #[test]
    fn ten_matches_are_accepted_at_the_largest_limit() {
        let names: Vec<String> = (0..10).map(|n| format!("s{n}")).collect();
        let set: BTreeSet<SessionId> = names.iter().map(|n| sid(n)).collect();
        let entries: Vec<Value> = names.iter().map(|n| entry(n, 0.5, "ok")).collect();
        let text = json!({"matches": entries}).to_string();
        assert_eq!(parse_strict(&raw(&text), &set, 10).unwrap().len(), 10);
        assert_eq!(
            parse_strict(&raw(&text), &set, 9).unwrap_err(),
            ParseError::TooManyMatches
        );
    }

    #[test]
    fn id_length_cap_is_exact_and_ids_must_be_known() {
        let at_cap = "i".repeat(MAX_ID_CHARS);
        let over = "i".repeat(MAX_ID_CHARS + 1);
        let set = BTreeSet::from([sid(&at_cap), sid(&over)]);
        let accepted = reply(&at_cap, json!(0.5), "ok");
        assert_eq!(parse_strict(&raw(&accepted), &set, 5).unwrap().len(), 1);
        let rejected = reply(&over, json!(0.5), "ok");
        let error = parse_strict(&raw(&rejected), &set, 5).unwrap_err();
        assert_eq!(error, ParseError::UnknownSession);
    }

    #[test]
    fn unknown_ids_are_refused_including_look_alikes() {
        for id in ["zz", "", "A", " a", "a ", "a\u{0}", "\u{430}", "aa", "../a"] {
            assert_eq!(
                refused(&reply(id, json!(0.5), "ok")),
                ParseError::UnknownSession,
                "{id:?}"
            );
        }
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let two = vec![entry("a", 0.9, "one"), entry("a", 0.1, "two")];
        let text = json!({"matches": two}).to_string();
        assert_eq!(refused(&text), ParseError::Duplicate);
    }

    #[test]
    fn one_bad_entry_refuses_the_whole_answer() {
        let mixed = vec![
            entry("a", 0.9, "fine"),
            entry("b", 2.0, "bad"),
            entry("c", 0.1, "fine"),
        ];
        let text = json!({"matches": mixed}).to_string();
        assert_eq!(refused(&text), ParseError::BadRelevance);
        let unknown = vec![entry("a", 0.9, "fine"), entry("zz", 0.5, "fine")];
        let text = json!({"matches": unknown}).to_string();
        assert_eq!(refused(&text), ParseError::UnknownSession);
        let bad_why = vec![entry("a", 0.9, "fine"), entry("b", 0.5, "\u{202e}")];
        let text = json!({"matches": bad_why}).to_string();
        assert_eq!(refused(&text), ParseError::BadWhy);
    }

    #[test]
    fn matches_are_ordered_by_relevance_then_id() {
        let shuffled = vec![
            entry("c", 0.9, "third"),
            entry("b", 0.2, "fourth"),
            entry("a", 0.9, "first"),
        ];
        let text = json!({"matches": shuffled}).to_string();
        let found = parse(&text, 5).unwrap();
        let order: Vec<&str> = found.iter().map(|m| m.why()).collect();
        assert_eq!(order, ["first", "third", "fourth"]);
    }

    #[test]
    fn lower_relevance_never_precedes_higher() {
        let rising = vec![
            entry("a", 0.1, "x"),
            entry("b", 0.2, "x"),
            entry("c", 0.3, "x"),
        ];
        let text = json!({"matches": rising}).to_string();
        let found = parse(&text, 5).unwrap();
        let ids: Vec<String> = found.iter().map(|m| m.id().to_string()).collect();
        assert_eq!(ids, ["c", "b", "a"]);
    }

    #[test]
    fn hostile_reply_table_is_refused_whole() {
        let cases: Vec<(&str, String, ParseError)> = vec![
            ("empty", String::new(), ParseError::Malformed),
            ("null", "null".into(), ParseError::Malformed),
            ("array", "[]".into(), ParseError::Malformed),
            ("string", "\"{}\"".into(), ParseError::Malformed),
            ("number", "7".into(), ParseError::Malformed),
            (
                "trailing prose",
                "{\"matches\":[]} trailing".into(),
                ParseError::Malformed,
            ),
            (
                "two objects",
                "{\"matches\":[]}{\"matches\":[]}".into(),
                ParseError::Malformed,
            ),
            (
                "leading prose",
                "Here: {\"matches\":[]}".into(),
                ParseError::Malformed,
            ),
            (
                "code fence",
                "```json\n{\"matches\":[]}\n```".into(),
                ParseError::Malformed,
            ),
            (
                "bare fence",
                "```\n{\"matches\":[]}\n```".into(),
                ParseError::Malformed,
            ),
            (
                "bom",
                "\u{feff}{\"matches\":[]}".into(),
                ParseError::Malformed,
            ),
            (
                "dup top key",
                "{\"matches\":[],\"matches\":[]}".into(),
                ParseError::Malformed,
            ),
            ("dup match key", dup_match_key(), ParseError::Malformed),
            (
                "extra top key",
                "{\"matches\":[],\"x\":1}".into(),
                ParseError::Malformed,
            ),
            ("extra match key", extra_match_key(), ParseError::Malformed),
            ("missing matches", "{}".into(), ParseError::Malformed),
            (
                "missing id",
                r#"{"matches":[{"relevance":1,"why":"w"}]}"#.into(),
                ParseError::Malformed,
            ),
            (
                "missing why",
                r#"{"matches":[{"id":"a","relevance":1}]}"#.into(),
                ParseError::Malformed,
            ),
            (
                "missing relevance",
                r#"{"matches":[{"id":"a","why":"w"}]}"#.into(),
                ParseError::Malformed,
            ),
            (
                "matches object",
                r#"{"matches":{}}"#.into(),
                ParseError::Malformed,
            ),
            (
                "matches string",
                r#"{"matches":"a"}"#.into(),
                ParseError::Malformed,
            ),
            (
                "null entry",
                r#"{"matches":[null]}"#.into(),
                ParseError::Malformed,
            ),
            (
                "array entry",
                r#"{"matches":[[]]}"#.into(),
                ParseError::Malformed,
            ),
            (
                "numeric id",
                r#"{"matches":[{"id":1,"relevance":1,"why":"w"}]}"#.into(),
                ParseError::Malformed,
            ),
            (
                "object why",
                r#"{"matches":[{"id":"a","relevance":1,"why":{"a":1}}]}"#.into(),
                ParseError::Malformed,
            ),
            (
                "array why",
                r#"{"matches":[{"id":"a","relevance":1,"why":["x"]}]}"#.into(),
                ParseError::Malformed,
            ),
            (
                "single quotes",
                "{'matches':[]}".into(),
                ParseError::Malformed,
            ),
            (
                "trailing comma",
                "{\"matches\":[],}".into(),
                ParseError::Malformed,
            ),
            (
                "comment",
                "{\"matches\":[]} // done".into(),
                ParseError::Malformed,
            ),
            (
                "nul byte",
                "{\"matches\":[]}\u{0}".into(),
                ParseError::Malformed,
            ),
            ("deep array", "[".repeat(10_000), ParseError::Malformed),
            (
                "deep nesting in extra key",
                deep_extra_key(),
                ParseError::Malformed,
            ),
            ("escaped bidi", with_why("x\u{202e}y"), ParseError::BadWhy),
            (
                "huge number",
                with_relevance("1e999"),
                ParseError::Malformed,
            ),
        ];
        assert!(cases.len() >= 20);
        for (name, text, expected) in cases {
            assert_eq!(parse(&text, 5).unwrap_err(), expected, "{name}");
        }
    }

    fn dup_match_key() -> String {
        r#"{"matches":[{"id":"a","id":"b","relevance":1,"why":"w"}]}"#.to_string()
    }

    fn extra_match_key() -> String {
        r#"{"matches":[{"id":"a","relevance":1,"why":"w","note":"n"}]}"#.to_string()
    }

    fn deep_extra_key() -> String {
        let open = "[".repeat(200);
        let close = "]".repeat(200);
        format!("{{\"matches\":[],\"x\":{open}{close}}}")
    }

    #[test]
    fn debug_and_errors_never_print_text() {
        let why = format!("note {TOKEN} here");
        let found = parse(&with_why(&why), 5).unwrap();
        let shown = format!("{found:?} {:?}", found[0]);
        assert!(!shown.contains(TOKEN));
        assert!(shown.contains("why_chars"));
        let answer = raw(&with_why(&why));
        assert!(!format!("{answer:?}").contains(TOKEN));
    }

    #[test]
    fn planted_token_never_reaches_an_error() {
        let long = format!("{TOKEN}{}", "z".repeat(200));
        let attempts = vec![
            reply(TOKEN, json!(0.5), "ok"),
            reply("a", json!(0.5), &long),
            reply("a", json!(0.5), &format!("{TOKEN}\u{202e}")),
            reply("a", json!(0.5), &format!("{TOKEN} {SECRET}")),
            reply("a", json!(TOKEN), "ok"),
            format!("{{\"{TOKEN}\":1,\"matches\":[]}}"),
            format!("{{\"matches\":[{{\"{TOKEN}\":1}}]}}"),
            format!("prose {TOKEN} {{\"matches\":[]}}"),
            format!("{{\"matches\":[]}} {TOKEN}"),
            format!("```{TOKEN}```"),
        ];
        for text in attempts {
            let error = refused(&text);
            let shown = texts(error);
            assert!(!shown.contains(TOKEN), "{shown}");
            assert!(!shown.contains("ghp_"), "{shown}");
        }
        let over = format!("{TOKEN}{}", "x".repeat(MAX_RAW_BYTES));
        let error = RawLibrarianAnswer::new(over).unwrap_err();
        assert!(!texts(error).contains(TOKEN));
    }

    #[test]
    fn every_error_text_is_fixed_and_distinct() {
        let all = [
            ParseError::TooLarge,
            ParseError::Malformed,
            ParseError::UnknownSession,
            ParseError::Duplicate,
            ParseError::TooManyMatches,
            ParseError::BadRelevance,
            ParseError::BadWhy,
            ParseError::BadLimit,
        ];
        let shown: BTreeSet<String> = all.iter().map(|e| e.to_string()).collect();
        assert_eq!(shown.len(), all.len());
        for text in &shown {
            assert!(text.starts_with("librarian reply refused: "), "{text}");
        }
    }

    fn arb_known() -> BTreeSet<SessionId> {
        known(&["a", "b", "c", "id-1", "id-2"])
    }

    fn check_invariants(found: &[LibrarianMatch], max: u8) {
        let set = arb_known();
        assert!(found.len() <= usize::from(max));
        let ids: BTreeSet<&SessionId> = found.iter().map(|m| m.id()).collect();
        assert_eq!(ids.len(), found.len());
        for item in found {
            assert!(set.contains(item.id()));
            assert!((0.0..=1.0).contains(&item.relevance()));
            assert!(item.relevance().is_sign_positive());
            assert!(item.why().chars().count() <= 179);
            assert!(!item.why().trim().is_empty());
            assert!(!item.why().chars().any(char::is_control));
        }
        for pair in found.windows(2) {
            assert!(pair[0].relevance() >= pair[1].relevance());
        }
    }

    fn survives(text: String, max: u8) {
        let answer = RawLibrarianAnswer::new(text);
        if let Ok(answer) = answer {
            let outcome = parse_strict(&answer, &arb_known(), max);
            if let Ok(found) = outcome {
                check_invariants(&found, max);
            }
        }
    }

    fn structured(id: String, relevance: f64, why: String) -> String {
        let number = serde_json::Number::from_f64(relevance);
        let value = number.map_or(Value::Null, Value::Number);
        json!({"matches": [{"id": id, "relevance": value, "why": why}]}).to_string()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn never_panics_on_arbitrary_utf8(text in any::<String>(), max in 0u8..=12) {
            survives(text, max);
        }

        #[test]
        fn never_panics_on_arbitrary_bytes_as_string(
            bytes in proptest::collection::vec(any::<u8>(), 0..3000),
            max in 0u8..=12,
        ) {
            survives(String::from_utf8_lossy(&bytes).into_owned(), max);
        }

        #[test]
        fn never_panics_on_arbitrary_fields_inside_a_valid_frame(
            id in prop_oneof![Just("a".to_string()), any::<String>()],
            relevance in any::<f64>(),
            why in any::<String>(),
        ) {
            survives(structured(id, relevance, why), 5);
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    enum Defect {
        Clean,
        ExtraTopKey,
        ExtraMatchKey,
        UnknownId,
        Duplicate,
        WhyTooLong,
        WhyBadChar,
        WhyEmpty,
        WhySecret,
        RelevanceHigh,
        RelevanceLow,
        RelevanceString,
        TooMany,
        ProsePrefix,
        CodeFence,
        TrailingText,
        MissingKey,
    }

    const DEFECTS: [Defect; 17] = [
        Defect::Clean,
        Defect::ExtraTopKey,
        Defect::ExtraMatchKey,
        Defect::UnknownId,
        Defect::Duplicate,
        Defect::WhyTooLong,
        Defect::WhyBadChar,
        Defect::WhyEmpty,
        Defect::WhySecret,
        Defect::RelevanceHigh,
        Defect::RelevanceLow,
        Defect::RelevanceString,
        Defect::TooMany,
        Defect::ProsePrefix,
        Defect::CodeFence,
        Defect::TrailingText,
        Defect::MissingKey,
    ];

    const POOL: [&str; 5] = ["a", "b", "c", "id-1", "id-2"];

    fn why_strategy() -> impl Strategy<Value = String> {
        let alphabet: Vec<char> = "aeé日ß -_./x1".chars().collect();
        let letters = proptest::sample::select(alphabet);
        let lengths = prop_oneof![1usize..=20, 178usize..=179];
        lengths
            .prop_flat_map(move |n| proptest::collection::vec(letters.clone(), n))
            .prop_map(|chars| chars.into_iter().collect::<String>())
            .prop_filter("not blank", |text| !text.trim().is_empty())
    }

    fn apply(defect: Defect, entries: &mut [Value], top: &mut serde_json::Map<String, Value>) {
        let first = 0;
        match defect {
            Defect::ExtraTopKey => {
                top.insert("extra".into(), json!(1));
            }
            Defect::ExtraMatchKey => {
                entries[first]["extra"] = json!(1);
            }
            Defect::UnknownId => entries[first]["id"] = json!("not-in-view"),
            Defect::Duplicate => entries[1]["id"] = entries[first]["id"].clone(),
            Defect::WhyTooLong => entries[first]["why"] = json!("é".repeat(180)),
            Defect::WhyBadChar => entries[first]["why"] = json!("fine\u{202e}text"),
            Defect::WhyEmpty => entries[first]["why"] = json!(""),
            Defect::WhySecret => entries[first]["why"] = json!(SECRET),
            Defect::RelevanceHigh => entries[first]["relevance"] = json!(1.0000001),
            Defect::RelevanceLow => entries[first]["relevance"] = json!(-0.0000001),
            Defect::RelevanceString => entries[first]["relevance"] = json!("0.5"),
            Defect::MissingKey => {
                entries[first].as_object_mut().unwrap().remove("why");
            }
            _ => {}
        }
    }

    fn decorate(defect: Defect, body: String) -> String {
        match defect {
            Defect::ProsePrefix => format!("Sure, here it is: {body}"),
            Defect::CodeFence => format!("```json\n{body}\n```"),
            Defect::TrailingText => format!("{body} done"),
            _ => body,
        }
    }

    fn expected_order(items: &[(String, f64, String)]) -> Vec<String> {
        let mut left: Vec<&(String, f64, String)> = items.iter().collect();
        let mut order = Vec::new();
        while !left.is_empty() {
            let mut best = 0;
            for (at, item) in left.iter().enumerate() {
                let current = left[best];
                let rel_now = item.1 as f32;
                let rel_best = current.1 as f32;
                if rel_now > rel_best || (rel_now == rel_best && item.0 < current.0) {
                    best = at;
                }
            }
            order.push(left.remove(best).2.clone());
        }
        order
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(1024))]

        #[test]
        fn accepts_exactly_the_defect_free_answers(
            picks in proptest::sample::subsequence(POOL.to_vec(), 2..=5),
            relevances in proptest::collection::vec(0.0f64..=1.0, 5),
            whys in proptest::collection::vec(why_strategy(), 5),
            slack in 0u8..=5,
            defect in proptest::sample::select(DEFECTS.to_vec()),
        ) {
            let items: Vec<(String, f64, String)> = picks
                .iter()
                .zip(relevances.iter().zip(whys.iter()))
                .map(|(id, (rel, why))| (id.to_string(), *rel, why.clone()))
                .collect();
            let n = items.len();
            let mut entries: Vec<Value> = items.iter().map(|i| entry(&i.0, i.1, &i.2)).collect();
            let mut top = serde_json::Map::new();
            apply(defect, &mut entries, &mut top);
            top.insert("matches".into(), Value::Array(entries));
            let text = decorate(defect, Value::Object(top).to_string());
            let max = if defect == Defect::TooMany { n as u8 - 1 } else { (n as u8 + slack).min(10) };
            let outcome = parse(&text, max);
            prop_assert_eq!(outcome.is_ok(), defect == Defect::Clean, "{:?}", defect);
            if let Ok(found) = outcome {
                let got: Vec<String> = found.iter().map(|m| m.why().to_string()).collect();
                prop_assert_eq!(got, expected_order(&items));
                check_invariants(&found, max);
                let echoed: Vec<Value> = found
                    .iter()
                    .map(|m| entry(&m.id().to_string(), f64::from(m.relevance()), m.why()))
                    .collect();
                let again = parse(&json!({"matches": echoed}).to_string(), max).unwrap();
                prop_assert_eq!(again, found);
            }
        }

        #[test]
        fn why_length_boundary_with_multibyte_text(len in 170usize..190, ch in prop_oneof![Just('a'), Just('é'), Just('日')]) {
            let why = ch.to_string().repeat(len);
            let accepted = parse(&with_why(&why), 5).is_ok();
            prop_assert_eq!(accepted, len < 180);
        }
    }
}
