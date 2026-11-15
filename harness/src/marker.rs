//! The identity marker agent-authored commits and pull requests carry.
//!
//! A commit made by an agent ends with the trailer
//! `Nanna-Identity: <name>`; a pull request body carries the same name in
//! a hidden HTML comment. The CI guard in `docs/ci/protected-paths-guard.yml`
//! and the scheduler read it back with [`parse_identity_from_text`].
//!
//! ```
//! use harness::marker::{parse_identity_from_text, render_html_marker, render_trailer};
//!
//! let commit = format!("fix(api): retry\n\n{}\n", render_trailer("rust-implementer"));
//! assert_eq!(parse_identity_from_text(&commit).as_deref(), Some("rust-implementer"));
//!
//! let body = format!("## Summary\n{}\n", render_html_marker("pr-shepherd"));
//! assert_eq!(body, "## Summary\n<!-- Nanna-Identity: pr-shepherd -->\n");
//! assert_eq!(parse_identity_from_text(&body).as_deref(), Some("pr-shepherd"));
//! assert_eq!(parse_identity_from_text("no marker"), None);
//! ```

use regex::Regex;
use std::sync::OnceLock;

/// The git trailer key naming the identity that authored a commit.
pub const IDENTITY_TRAILER: &str = "Nanna-Identity";

const NAME: &str = "[A-Za-z0-9][A-Za-z0-9._-]*";

/// The trailer line for `name`, to append to a commit message.
pub fn render_trailer(name: &str) -> String {
    format!("{IDENTITY_TRAILER}: {name}")
}

/// The hidden HTML comment for `name`, to embed in a pull request body.
pub fn render_html_marker(name: &str) -> String {
    format!("<!-- {IDENTITY_TRAILER}: {name} -->")
}

fn marker_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        let pattern = format!(
            "(?m)(?:^{IDENTITY_TRAILER}:[ \\t]*({NAME})[ \\t]*$)|(?:<!--\\s*{IDENTITY_TRAILER}:\\s*({NAME})\\s*-->)"
        );
        Regex::new(&pattern).expect("marker regex is valid")
    })
}

/// The identity named by the first trailer line or HTML marker in `text`,
/// or `None` when it carries neither.
pub fn parse_identity_from_text(text: &str) -> Option<String> {
    let captures = marker_regex().captures(text)?;
    let name = captures.get(1).or_else(|| captures.get(2))?;
    Some(name.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn trailer_and_html_marker_render_the_identity() {
        assert_eq!(IDENTITY_TRAILER, "Nanna-Identity");
        assert_eq!(
            render_trailer("rust-implementer"),
            "Nanna-Identity: rust-implementer"
        );
        assert_eq!(
            render_html_marker("pr-shepherd"),
            "<!-- Nanna-Identity: pr-shepherd -->"
        );
    }

    #[test]
    fn a_commit_message_trailer_parses_back() {
        let message = format!(
            "feat(x): do it\n\nBody.\n\n{}\n",
            render_trailer("rust-implementer")
        );
        assert_eq!(
            parse_identity_from_text(&message).as_deref(),
            Some("rust-implementer")
        );
    }

    #[test]
    fn a_pr_body_html_marker_parses_back() {
        let body = format!(
            "## Summary\n\nChanges.\n{}\nCloses #1\n",
            render_html_marker("pr-shepherd")
        );
        assert_eq!(
            parse_identity_from_text(&body).as_deref(),
            Some("pr-shepherd")
        );
        let loose = "<!--   Nanna-Identity:deployer   -->";
        assert_eq!(parse_identity_from_text(loose).as_deref(), Some("deployer"));
    }

    #[test]
    fn text_without_a_marker_or_with_a_malformed_one_yields_none() {
        for text in [
            "",
            "feat: no marker here",
            "Nanna-Identity:",
            "Nanna-Identity: ",
            "see Nanna-Identity: x in the docs",
            "Nanna-Identity: a/b",
            "<!-- Nanna-Identity: -->",
            "<!-- nanna-identity: x -->",
        ] {
            assert_eq!(parse_identity_from_text(text), None, "{text:?}");
        }
    }

    #[test]
    fn the_first_marker_wins() {
        let text = format!(
            "{}\n{}\n",
            render_trailer("first"),
            render_html_marker("second")
        );
        assert_eq!(parse_identity_from_text(&text).as_deref(), Some("first"));
        let text = format!(
            "{}\n{}\n",
            render_html_marker("second"),
            render_trailer("first")
        );
        assert_eq!(parse_identity_from_text(&text).as_deref(), Some("second"));
    }

    proptest! {
        #[test]
        fn markers_round_trip_for_every_valid_name(name in "[A-Za-z0-9][A-Za-z0-9._-]{0,30}") {
            let commit = format!("subject\n\n{}\n", render_trailer(&name));
            prop_assert_eq!(parse_identity_from_text(&commit), Some(name.clone()));
            let body = format!("text {} text", render_html_marker(&name));
            prop_assert_eq!(parse_identity_from_text(&body), Some(name));
        }
    }
}
