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
        let rel = self.relevance;
        let n = self.why.chars().count();
        write!(f, "LibrarianMatch {{ relevance: {rel}, why_chars: {n} }}")
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
    let chars = text.chars().count();
    ensure(chars <= MAX_ID_CHARS, ParseError::UnknownSession)?;
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
    let chars = text.chars().count();
    ensure(chars <= Summary::MAX_CHARS, ParseError::BadWhy)?;
    ensure(!text.chars().any(char::is_control), ParseError::BadWhy)?;
    ensure(strip_unprintable(&text) == text, ParseError::BadWhy)?;
    ensure(redact_resilient(&text) == text, ParseError::BadWhy)?;
    Ok(text)
}

fn rank(a: &LibrarianMatch, b: &LibrarianMatch) -> std::cmp::Ordering {
    let by_relevance = b.relevance.total_cmp(&a.relevance);
    by_relevance.then_with(|| a.id.cmp(&b.id))
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
    let wanted = usize::from(max);
    ensure(wire.matches.len() <= wanted, ParseError::TooManyMatches)?;
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
    const UNKNOWN: ParseError = ParseError::UnknownSession;
    const BAD_WHY: ParseError = ParseError::BadWhy;
    const BAD_REL: ParseError = ParseError::BadRelevance;
    const MALFORMED: ParseError = ParseError::Malformed;

    fn sid(text: &str) -> SessionId {
        SessionId::from_task(TaskId(text.to_string()))
    }

    fn arb_known() -> BTreeSet<SessionId> {
        let names = ["a", "b", "c", "id-1", "id-2"];
        names.iter().map(|id| sid(id)).collect()
    }

    fn raw(text: &str) -> RawLibrarianAnswer {
        RawLibrarianAnswer::new(text.to_string()).unwrap()
    }

    fn parse(text: &str, max: u8) -> Result<Vec<LibrarianMatch>, ParseError> {
        parse_strict(&raw(text), &arb_known(), max)
    }

    fn entry(id: &str, relevance: f64, why: &str) -> Value {
        json!({"id": id, "relevance": relevance, "why": why})
    }

    fn answer(items: &[(&str, f64, &str)]) -> String {
        let entries: Vec<Value> = items.iter().map(|i| entry(i.0, i.1, i.2)).collect();
        json!({"matches": entries}).to_string()
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

    fn expect_refused(text: &str, expected: ParseError, label: &str) {
        assert_eq!(refused(text), expected, "{label}");
    }

    fn texts(error: ParseError) -> String {
        format!("{error} {error:?}")
    }

    fn why_list(found: &[LibrarianMatch]) -> Vec<String> {
        found.iter().map(|m| m.why().to_string()).collect()
    }

    fn ids(found: &[LibrarianMatch]) -> Vec<String> {
        found.iter().map(|m| m.id().to_string()).collect()
    }

    #[test]
    fn accepts_the_documented_shape() {
        let found = parse(&with_why("edits the parser"), 5).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id(), &sid("a"));
        assert_eq!(found[0].relevance(), 0.5);
        assert_eq!(found[0].why(), "edits the parser");
        let (id, relevance, why) = found[0].clone().into_fields();
        assert_eq!(id, sid("a"));
        assert_eq!(relevance, 0.5);
        assert_eq!(why, "edits the parser");
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
        expect_refused("\u{a0}{\"matches\":[]}", MALFORMED, "leading");
        expect_refused("{\"matches\":[]}\u{2003}", MALFORMED, "trailing");
    }

    #[test]
    fn raw_answer_cap_is_exact() {
        let pad = " ".repeat(MAX_RAW_BYTES - 14);
        let at_cap = format!("{{\"matches\":[]{pad}}}");
        assert_eq!(at_cap.len(), MAX_RAW_BYTES);
        assert!(parse(&at_cap, 5).is_ok());
        let over = format!("{{\"matches\":[] {pad}}}");
        assert_eq!(over.len(), MAX_RAW_BYTES + 1);
        let error = RawLibrarianAnswer::new(over).unwrap_err();
        assert_eq!(error, ParseError::TooLarge);
    }

    #[test]
    fn why_cap_counts_characters_not_bytes() {
        for ch in ["a", "é", "日"] {
            let at_cap = ch.repeat(179);
            assert!(parse(&with_why(&at_cap), 5).is_ok(), "{ch}");
            let over = ch.repeat(180);
            expect_refused(&with_why(&over), BAD_WHY, ch);
        }
        assert_eq!("é".repeat(179).len(), 358);
    }

    #[test]
    fn why_cap_matches_the_summary_cap() {
        assert_eq!(Summary::MAX_CHARS, 179);
    }

    #[test]
    fn why_of_one_character_is_accepted_and_empty_is_not() {
        assert!(parse(&with_why("x"), 5).is_ok());
        expect_refused(&with_why(""), BAD_WHY, "empty");
        expect_refused(&with_why("   "), BAD_WHY, "blank");
    }

    #[test]
    fn why_with_control_characters_is_refused() {
        for bad in ["a\nb", "a\tb", "a\u{0}b", "a\u{7}b", "a\u{85}b", "a\rb"] {
            expect_refused(&with_why(bad), BAD_WHY, bad);
        }
    }

    const HIDDEN: [&str; 7] = [
        "\u{202e}",
        "\u{200d}",
        "\u{200b}",
        "\u{feff}",
        "\u{ad}",
        "\u{e0041}",
        "\u{2066}",
    ];

    #[test]
    fn why_with_hidden_or_bidi_characters_is_refused() {
        for part in HIDDEN {
            let why = format!("fine{part}text");
            expect_refused(&with_why(&why), BAD_WHY, part);
        }
    }

    #[test]
    fn why_with_unicode_escapes_for_bidi_is_refused() {
        let text = "{\"matches\":[{\"id\":\"a\",\"relevance\":0.5,\"why\":\"x\\u202ey\"}]}";
        expect_refused(text, BAD_WHY, "escaped");
    }

    #[test]
    fn why_that_redaction_would_change_is_refused() {
        expect_refused(&with_why(SECRET), BAD_WHY, "bare");
        let spaced = format!("see {SECRET} now");
        expect_refused(&with_why(&spaced), BAD_WHY, "spaced");
        let joined = SECRET.replace('_', "_\u{200d}");
        expect_refused(&with_why(&joined), BAD_WHY, "joined");
    }

    #[test]
    fn multibyte_and_combining_text_is_accepted_when_printable() {
        let plain = "naïve café 日本語 résumé";
        assert_eq!(parse(&with_why(plain), 5).unwrap()[0].why(), plain);
        let combining = "e\u{301} cafe\u{301}";
        assert_eq!(parse(&with_why(combining), 5).unwrap()[0].why(), combining);
    }

    #[test]
    fn why_may_quote_json_as_plain_text() {
        let quoted = r#"{"matches":[]} and "quotes""#;
        assert_eq!(parse(&with_why(quoted), 5).unwrap()[0].why(), quoted);
    }

    const GOOD_RELEVANCE: [&str; 9] = ["0", "0.0", "1", "1.0", "0.5", "1e0", "0e5", "-0.0", "5e-1"];
    const BAD_RELEVANCE: [&str; 7] = [
        "1.0000001",
        "-0.0000001",
        "2",
        "-1",
        "1e300",
        "-1e300",
        "1.000000000001",
    ];
    const NOT_A_NUMBER: [&str; 10] = [
        "1e999",
        "-1e999",
        "NaN",
        "Infinity",
        "-Infinity",
        "\"0.5\"",
        "null",
        "true",
        "[0.5]",
        "{}",
    ];

    #[test]
    fn relevance_bounds_are_exact() {
        for good in GOOD_RELEVANCE {
            assert!(parse(&with_relevance(good), 5).is_ok(), "{good}");
        }
        for bad in BAD_RELEVANCE {
            expect_refused(&with_relevance(bad), BAD_REL, bad);
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
        for bad in NOT_A_NUMBER {
            expect_refused(&with_relevance(bad), MALFORMED, bad);
        }
    }

    #[test]
    fn match_count_cap_is_exact() {
        let text = answer(&[("a", 0.5, "ok"), ("b", 0.5, "ok"), ("c", 0.5, "ok")]);
        assert_eq!(parse(&text, 3).unwrap().len(), 3);
        assert_eq!(parse(&text, 2).unwrap_err(), ParseError::TooManyMatches);
        assert_eq!(parse(&text, 1).unwrap_err(), ParseError::TooManyMatches);
    }

    #[test]
    fn requested_limit_must_be_one_to_ten() {
        let text = r#"{"matches":[]}"#;
        assert_eq!(parse(text, 0).unwrap_err(), ParseError::BadLimit);
        assert!(parse(text, 1).is_ok());
        assert!(parse(text, MAX_RESULTS).is_ok());
        let eleven = parse(text, MAX_RESULTS + 1).unwrap_err();
        assert_eq!(eleven, ParseError::BadLimit);
        assert_eq!(parse(text, u8::MAX).unwrap_err(), ParseError::BadLimit);
    }

    #[test]
    fn ten_matches_are_accepted_at_the_largest_limit() {
        let names: Vec<String> = (0..10).map(|n| format!("s{n}")).collect();
        let set: BTreeSet<SessionId> = names.iter().map(|n| sid(n)).collect();
        let entries: Vec<Value> = names.iter().map(|n| entry(n, 0.5, "ok")).collect();
        let text = json!({"matches": entries}).to_string();
        assert_eq!(parse_strict(&raw(&text), &set, 10).unwrap().len(), 10);
        let nine = parse_strict(&raw(&text), &set, 9).unwrap_err();
        assert_eq!(nine, ParseError::TooManyMatches);
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
        assert_eq!(error, UNKNOWN);
    }

    const LOOK_ALIKES: [&str; 9] = ["zz", "", "A", " a", "a ", "a\u{0}", "\u{430}", "aa", "../a"];

    #[test]
    fn unknown_ids_are_refused_including_look_alikes() {
        for id in LOOK_ALIKES {
            expect_refused(&reply(id, json!(0.5), "ok"), UNKNOWN, id);
        }
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let text = answer(&[("a", 0.9, "one"), ("a", 0.1, "two")]);
        expect_refused(&text, ParseError::Duplicate, "dup");
    }

    #[test]
    fn one_bad_entry_refuses_the_whole_answer() {
        let mixed = answer(&[("a", 0.9, "fine"), ("b", 2.0, "bad"), ("c", 0.1, "fine")]);
        expect_refused(&mixed, BAD_REL, "relevance");
        let unknown = answer(&[("a", 0.9, "fine"), ("zz", 0.5, "fine")]);
        expect_refused(&unknown, UNKNOWN, "unknown");
        let bad_why = answer(&[("a", 0.9, "fine"), ("b", 0.5, "\u{202e}")]);
        expect_refused(&bad_why, BAD_WHY, "why");
    }

    #[test]
    fn matches_are_ordered_by_relevance_then_id() {
        let shuffled = [
            ("c", 0.9, "third"),
            ("b", 0.2, "fourth"),
            ("a", 0.9, "first"),
        ];
        let text = answer(&shuffled);
        let found = parse(&text, 5).unwrap();
        assert_eq!(why_list(&found), ["first", "third", "fourth"]);
    }

    #[test]
    fn lower_relevance_never_precedes_higher() {
        let text = answer(&[("a", 0.1, "x"), ("b", 0.2, "x"), ("c", 0.3, "x")]);
        assert_eq!(ids(&parse(&text, 5).unwrap()), ["c", "b", "a"]);
    }

    const HOSTILE: [(&str, &str); 31] = [
        ("empty", ""),
        ("null", "null"),
        ("array", "[]"),
        ("string", "\"{}\""),
        ("number", "7"),
        ("trailing prose", "{\"matches\":[]} trailing"),
        ("two objects", "{\"matches\":[]}{\"matches\":[]}"),
        ("leading prose", "Here: {\"matches\":[]}"),
        ("code fence", "```json\n{\"matches\":[]}\n```"),
        ("bare fence", "```\n{\"matches\":[]}\n```"),
        ("bom", "\u{feff}{\"matches\":[]}"),
        ("dup top key", "{\"matches\":[],\"matches\":[]}"),
        (
            "dup match key",
            r#"{"matches":[{"id":"a","id":"b","relevance":1,"why":"w"}]}"#,
        ),
        ("extra top key", "{\"matches\":[],\"x\":1}"),
        (
            "extra match key",
            r#"{"matches":[{"id":"a","relevance":1,"why":"w","n":1}]}"#,
        ),
        ("missing matches", "{}"),
        ("missing id", r#"{"matches":[{"relevance":1,"why":"w"}]}"#),
        ("missing why", r#"{"matches":[{"id":"a","relevance":1}]}"#),
        ("missing relevance", r#"{"matches":[{"id":"a","why":"w"}]}"#),
        ("matches object", r#"{"matches":{}}"#),
        ("matches string", r#"{"matches":"a"}"#),
        ("null entry", r#"{"matches":[null]}"#),
        ("array entry", r#"{"matches":[[]]}"#),
        (
            "numeric id",
            r#"{"matches":[{"id":1,"relevance":1,"why":"w"}]}"#,
        ),
        (
            "object why",
            r#"{"matches":[{"id":"a","relevance":1,"why":{"a":1}}]}"#,
        ),
        (
            "array why",
            r#"{"matches":[{"id":"a","relevance":1,"why":["x"]}]}"#,
        ),
        ("single quotes", "{'matches':[]}"),
        ("trailing comma", "{\"matches\":[],}"),
        ("comment", "{\"matches\":[]} // done"),
        ("nul byte", "{\"matches\":[]}\u{0}"),
        ("lone brace", "{"),
    ];

    #[test]
    fn hostile_reply_table_is_refused_whole() {
        assert!(HOSTILE.len() >= 20);
        for (name, text) in HOSTILE {
            expect_refused(text, MALFORMED, name);
        }
    }

    #[test]
    fn deeply_nested_replies_are_refused_without_overflow() {
        expect_refused(&"[".repeat(10_000), MALFORMED, "deep array");
        let open = "[".repeat(200);
        let close = "]".repeat(200);
        let nested = format!("{{\"matches\":[],\"x\":{open}{close}}}");
        expect_refused(&nested, MALFORMED, "nested extra key");
        let object = format!("{{\"matches\":{}{}}}", "[".repeat(5_000), "]".repeat(5_000));
        expect_refused(&object, MALFORMED, "nested matches");
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
        let bidi = format!("{TOKEN}\u{202e}");
        let secret = format!("{TOKEN} {SECRET}");
        let key = format!("{{\"{TOKEN}\":1,\"matches\":[]}}");
        let inner = format!("{{\"matches\":[{{\"{TOKEN}\":1}}]}}");
        let prose = format!("prose {TOKEN} {{\"matches\":[]}}");
        let tail = format!("{{\"matches\":[]}} {TOKEN}");
        let fence = format!("```{TOKEN}```");
        let mut attempts = vec![key, inner, prose, tail, fence];
        attempts.push(reply(TOKEN, json!(0.5), "ok"));
        attempts.push(reply("a", json!(0.5), &long));
        attempts.push(reply("a", json!(0.5), &bidi));
        attempts.push(reply("a", json!(0.5), &secret));
        attempts.push(reply("a", json!(TOKEN), "ok"));
        for text in attempts {
            let shown = texts(refused(&text));
            assert!(!shown.contains(TOKEN), "{shown}");
            assert!(!shown.contains("ghp_"), "{shown}");
        }
        let over = format!("{TOKEN}{}", "x".repeat(MAX_RAW_BYTES));
        let error = RawLibrarianAnswer::new(over).unwrap_err();
        assert!(!texts(error).contains(TOKEN));
    }

    const ALL_ERRORS: [ParseError; 8] = [
        ParseError::TooLarge,
        ParseError::Malformed,
        ParseError::UnknownSession,
        ParseError::Duplicate,
        ParseError::TooManyMatches,
        ParseError::BadRelevance,
        ParseError::BadWhy,
        ParseError::BadLimit,
    ];

    #[test]
    fn every_error_text_is_fixed_and_distinct() {
        let shown: BTreeSet<String> = ALL_ERRORS.iter().map(|e| e.to_string()).collect();
        assert_eq!(shown.len(), ALL_ERRORS.len());
        for text in &shown {
            assert!(text.starts_with("librarian reply refused: "), "{text}");
        }
    }

    fn check_invariants(found: &[LibrarianMatch], max: u8) {
        let set = arb_known();
        assert!(found.len() <= usize::from(max));
        let unique: BTreeSet<&SessionId> = found.iter().map(|m| m.id()).collect();
        assert_eq!(unique.len(), found.len());
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
    const ALPHABET: &str = "aeé日ß -_./x1";

    fn why_strategy() -> impl Strategy<Value = String> {
        let letters = proptest::sample::select(ALPHABET.chars().collect::<Vec<char>>());
        let lengths = prop_oneof![1usize..=20, 178usize..=179];
        let text = lengths.prop_flat_map(move |n| proptest::collection::vec(letters.clone(), n));
        let joined = text.prop_map(|chars| chars.into_iter().collect::<String>());
        joined.prop_filter("not blank", |text| !text.trim().is_empty())
    }

    fn set_field(entries: &mut [Value], at: usize, key: &str, value: Value) {
        entries[at][key] = value;
    }

    fn apply(defect: Defect, entries: &mut [Value], top: &mut serde_json::Map<String, Value>) {
        match defect {
            Defect::ExtraTopKey => drop(top.insert("extra".into(), json!(1))),
            Defect::ExtraMatchKey => set_field(entries, 0, "extra", json!(1)),
            Defect::UnknownId => set_field(entries, 0, "id", json!("not-in-view")),
            Defect::Duplicate => set_field(entries, 1, "id", entries[0]["id"].clone()),
            Defect::WhyTooLong => set_field(entries, 0, "why", json!("é".repeat(180))),
            Defect::WhyBadChar => set_field(entries, 0, "why", json!("fine\u{202e}text")),
            Defect::WhyEmpty => set_field(entries, 0, "why", json!("")),
            Defect::WhySecret => set_field(entries, 0, "why", json!(SECRET)),
            Defect::RelevanceHigh => set_field(entries, 0, "relevance", json!(1.0000001)),
            Defect::RelevanceLow => set_field(entries, 0, "relevance", json!(-0.0000001)),
            Defect::RelevanceString => set_field(entries, 0, "relevance", json!("0.5")),
            Defect::MissingKey => drop(entries[0].as_object_mut().map(|o| o.remove("why"))),
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

    type Item = (String, f64, String);

    fn beats(a: &Item, b: &Item) -> bool {
        let (rel_a, rel_b) = (a.1 as f32, b.1 as f32);
        rel_a > rel_b || (rel_a == rel_b && a.0 < b.0)
    }

    fn expected_order(items: &[Item]) -> Vec<String> {
        let mut left: Vec<&Item> = items.iter().collect();
        let mut order = Vec::new();
        while !left.is_empty() {
            let mut best = 0;
            for (at, item) in left.iter().enumerate() {
                if beats(item, left[best]) {
                    best = at;
                }
            }
            order.push(left.remove(best).2.clone());
        }
        order
    }

    fn build_items(picks: &[&str], relevances: &[f64], whys: &[String]) -> Vec<Item> {
        let mut items = Vec::new();
        for (at, id) in picks.iter().enumerate() {
            items.push((id.to_string(), relevances[at], whys[at].clone()));
        }
        items
    }

    fn echo(found: &[LibrarianMatch]) -> String {
        let mut entries = Vec::new();
        for item in found {
            let id = item.id().to_string();
            let rel = f64::from(item.relevance());
            entries.push(entry(&id, rel, item.why()));
        }
        json!({"matches": entries}).to_string()
    }

    fn render(items: &[Item], defect: Defect) -> String {
        let mut entries: Vec<Value> = items.iter().map(|i| entry(&i.0, i.1, &i.2)).collect();
        let mut top = serde_json::Map::new();
        apply(defect, &mut entries, &mut top);
        top.insert("matches".into(), Value::Array(entries));
        decorate(defect, Value::Object(top).to_string())
    }

    fn limit_for(defect: Defect, count: usize, slack: u8) -> u8 {
        let count = count as u8;
        if defect == Defect::TooMany {
            count - 1
        } else {
            (count + slack).min(10)
        }
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
            let items = build_items(&picks, &relevances, &whys);
            let text = render(&items, defect);
            let max = limit_for(defect, items.len(), slack);
            let outcome = parse(&text, max);
            prop_assert_eq!(outcome.is_ok(), defect == Defect::Clean, "{:?}", defect);
            if let Ok(found) = outcome {
                prop_assert_eq!(why_list(&found), expected_order(&items));
                check_invariants(&found, max);
                let again = parse(&echo(&found), max).unwrap();
                prop_assert_eq!(again, found);
            }
        }

        #[test]
        fn why_length_boundary_with_multibyte_text(
            len in 170usize..190,
            ch in prop_oneof![Just('a'), Just('é'), Just('日')],
        ) {
            let why = ch.to_string().repeat(len);
            let accepted = parse(&with_why(&why), 5).is_ok();
            prop_assert_eq!(accepted, len < 180);
        }
    }
}
