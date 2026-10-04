//! Tool-name patterns used by identity scopes.

use glob::Pattern;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// Reason a string is not a valid [`ToolPattern`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ToolPatternError {
    /// The pattern is the empty string.
    #[error("tool pattern is empty")]
    Empty,
    /// The pattern contains a character that can never appear in a tool name or wildcard.
    #[error("tool pattern `{pattern}` contains {ch:?}; allowed: ASCII letters, digits, `_`, `-`, `*`, `?`, `[`, `]`, `!`")]
    InvalidCharacter {
        /// The offending pattern.
        pattern: String,
        /// The first disallowed character.
        ch: char,
    },
    /// The pattern is not valid glob syntax (for example an unclosed `[`).
    #[error("tool pattern `{pattern}` is not a valid glob: {reason}")]
    InvalidGlob {
        /// The offending pattern.
        pattern: String,
        /// The glob parser's explanation.
        reason: String,
    },
}

/// A glob over tool names, such as `cargo_*` or the exact name `read_file`.
///
/// `*` matches any run of characters, `?` a single character and `[abc]` a
/// character class; tool names contain no path separators, so `*` may span
/// the whole name.
///
/// ```
/// use harness::identity::ToolPattern;
///
/// let cargo: ToolPattern = "cargo_*".parse().unwrap();
/// assert!(cargo.matches("cargo_check"));
/// assert!(!cargo.matches("git_status"));
///
/// let exact: ToolPattern = "read_file".parse().unwrap();
/// assert!(exact.matches("read_file"));
/// assert!(!exact.matches("read_file_v2"));
/// assert!("cargo [".parse::<ToolPattern>().is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ToolPattern(Pattern);

impl ToolPattern {
    /// Validate and compile `pattern`.
    pub fn new(pattern: &str) -> Result<Self, ToolPatternError> {
        if pattern.is_empty() {
            return Err(ToolPatternError::Empty);
        }
        if let Some(ch) = pattern.chars().find(|ch| !is_allowed(*ch)) {
            let pattern = pattern.to_string();
            return Err(ToolPatternError::InvalidCharacter { pattern, ch });
        }
        Pattern::new(pattern)
            .map(Self)
            .map_err(|e| ToolPatternError::InvalidGlob {
                pattern: pattern.to_string(),
                reason: e.msg.to_string(),
            })
    }

    /// The pattern exactly as written.
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Whether `tool_name` is covered by this pattern.
    pub fn matches(&self, tool_name: &str) -> bool {
        self.0.matches(tool_name)
    }
}

fn is_allowed(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '*' | '?' | '[' | ']' | '!')
}

impl fmt::Display for ToolPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ToolPattern {
    type Err = ToolPatternError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for ToolPattern {
    type Error = ToolPatternError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(&value)
    }
}

impl From<ToolPattern> for String {
    fn from(pattern: ToolPattern) -> Self {
        pattern.as_str().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn matching_table() {
        let cases: &[(&str, &str, bool)] = &[
            ("cargo_*", "cargo_check", true),
            ("cargo_*", "cargo_", true),
            ("cargo_*", "cargo", false),
            ("cargo_*", "git_status", false),
            ("read_file", "read_file", true),
            ("read_file", "read_files", false),
            ("read_file", "xread_file", false),
            ("*", "anything", true),
            ("*", "", true),
            ("git_*", "github_pr_status", false),
            ("git*", "github_pr_status", true),
            ("read_?ile", "read_file", true),
            ("read_?ile", "read_ile", false),
            ("[rw]*_file", "write_file", true),
            ("[rw]*_file", "list_file", false),
            ("cargo-*", "cargo-deny", true),
            ("CARGO_*", "cargo_check", false),
        ];
        for (pattern, name, expected) in cases {
            let compiled = ToolPattern::new(pattern).unwrap();
            assert_eq!(compiled.matches(name), *expected, "{pattern} vs {name}");
        }
    }

    #[test]
    fn rejects_empty_pattern() {
        assert_eq!(ToolPattern::new(""), Err(ToolPatternError::Empty));
        assert_eq!(
            ToolPattern::new("").unwrap_err().to_string(),
            "tool pattern is empty"
        );
    }

    #[test]
    fn rejects_characters_outside_tool_name_alphabet() {
        for (pattern, ch) in [
            ("cargo *", ' '),
            ("git/status", '/'),
            ("read.file", '.'),
            ("é", 'é'),
        ] {
            let err = ToolPattern::new(pattern).unwrap_err();
            assert_eq!(
                err,
                ToolPatternError::InvalidCharacter {
                    pattern: pattern.to_string(),
                    ch
                }
            );
            assert!(err.to_string().contains(pattern), "{err}");
        }
    }

    #[test]
    fn rejects_invalid_glob_syntax() {
        let err = ToolPattern::new("cargo_[").unwrap_err();
        match &err {
            ToolPatternError::InvalidGlob { pattern, reason } => {
                assert_eq!(pattern, "cargo_[");
                assert!(!reason.is_empty());
            }
            other => panic!("unexpected error {other:?}"),
        }
        assert!(err
            .to_string()
            .starts_with("tool pattern `cargo_[` is not a valid glob: "));
    }

    #[test]
    fn display_parse_and_string_conversions_preserve_text() {
        let pattern: ToolPattern = "cargo_*".parse().unwrap();
        assert_eq!(pattern.as_str(), "cargo_*");
        assert_eq!(pattern.to_string(), "cargo_*");
        assert_eq!(String::from(pattern.clone()), "cargo_*");
        assert_eq!(ToolPattern::try_from("cargo_*".to_string()), Ok(pattern));
        assert!(ToolPattern::try_from(String::new()).is_err());
    }

    #[test]
    fn serde_round_trips_as_a_plain_string() {
        let pattern: ToolPattern = "git_*".parse().unwrap();
        let json = serde_json::to_string(&pattern).unwrap();
        assert_eq!(json, "\"git_*\"");
        let back: ToolPattern = serde_json::from_str(&json).unwrap();
        assert_eq!(back, pattern);
        assert!(serde_json::from_str::<ToolPattern>("\"bad pattern\"").is_err());
    }

    proptest! {
        #[test]
        fn literal_patterns_match_only_themselves(name in "[a-z][a-z0-9_]{0,15}", other in "[a-z][a-z0-9_]{0,15}") {
            let pattern = ToolPattern::new(&name).unwrap();
            prop_assert!(pattern.matches(&name));
            prop_assert_eq!(pattern.matches(&other), name == other);
        }

        #[test]
        fn prefix_wildcard_matches_every_extension(prefix in "[a-z][a-z0-9_]{0,8}", suffix in "[a-z0-9_]{0,8}") {
            let pattern = ToolPattern::new(&format!("{prefix}*")).unwrap();
            let name = format!("{prefix}{suffix}");
            prop_assert!(pattern.matches(&name));
        }
    }
}
