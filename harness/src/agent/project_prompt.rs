//! Per-project prompt configuration loader.
//!
//! Reads an optional `.nanna/prompt.md` file at the workspace root and parses
//! it into a [`ProjectPromptDoc`] consisting of optional TOML front-matter plus
//! a Markdown body. This is the first of several phases implementing
//! domain-specific system prompts (see issue #194, Phase A).
//!
//! # Relationship to the existing hardcoded system prompt
//!
//! The current hardcoded system prompt lives in two call sites that will be
//! rewritten in Phase D of the issue-194 roll-out:
//!
//! - `harness/src/main.rs:425` (`AgentConfig { system_prompt: ... }` in the
//!   CLI entry point)
//! - `harness/src/task.rs:369` (same string in the task-dispatch path)
//!
//! Phase A (this module) only introduces the loader - neither call site is
//! modified here. Phases C (assembler) and D (wiring) will replace those
//! hardcoded strings with the layered, detector-augmented assembly produced
//! from a [`ProjectPromptDoc`].
//!
//! # File format
//!
//! ```text
//! +++
//! language_override = "rust"
//! frameworks = ["tokio", "nextest"]
//! max_tokens = 1200
//! +++
//!
//! # Project guidance
//! Use `nix develop --command cargo nextest run` for tests.
//! ```
//!
//! Front-matter is optional. When absent, the entire file is treated as the
//! body and `frontmatter` is [`Default`]. Unknown keys in the front-matter
//! are rejected (`#[serde(deny_unknown_fields)]`) so typos surface early.

use serde::Deserialize;
use std::fs;
use std::io;
use std::path::Path;

/// Maximum allowed size of `.nanna/prompt.md` on disk, in bytes.
pub const MAX_PROMPT_FILE_BYTES: u64 = 64 * 1024;

/// Relative path of the per-project prompt file within a workspace.
pub const PROMPT_FILE_REL_PATH: &str = ".nanna/prompt.md";

/// Parsed representation of `.nanna/prompt.md`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectPromptDoc {
    /// Structured hints from the TOML front-matter.
    pub frontmatter: ProjectPromptFrontMatter,
    /// Markdown body (everything after the closing `+++` delimiter, or the
    /// whole file if no front-matter is present).
    pub body: String,
}

/// Structured front-matter hints.
///
/// Unknown keys are rejected at parse time to catch typos early. Extending
/// the schema therefore requires adding a field here.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ProjectPromptFrontMatter {
    /// Force a language and skip auto-detection in Phase B.
    pub language_override: Option<String>,
    /// Additional framework hints to merge with detected frameworks.
    pub frameworks: Vec<String>,
    /// Optional soft cap on assembled prompt tokens (consumed in Phase C).
    pub max_tokens: Option<usize>,
}

/// Load and parse `.nanna/prompt.md` from `workspace_root`, if it exists.
///
/// Returns `Ok(None)` when the file is absent - a fresh checkout should not
/// produce warnings. All other errors (oversize file, control chars in body,
/// malformed TOML front-matter, unknown front-matter keys) are surfaced as
/// [`io::Error`] with a descriptive message.
pub fn load(workspace_root: &Path) -> io::Result<Option<ProjectPromptDoc>> {
    let path = workspace_root.join(PROMPT_FILE_REL_PATH);

    let metadata = match fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };

    if metadata.len() > MAX_PROMPT_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} exceeds maximum size of {} bytes (actual: {})",
                path.display(),
                MAX_PROMPT_FILE_BYTES,
                metadata.len()
            ),
        ));
    }

    let raw = fs::read_to_string(&path)?;
    parse(&raw).map(Some)
}

/// Parse a raw prompt-file string into a [`ProjectPromptDoc`].
///
/// Exposed for unit tests; callers should prefer [`load`].
fn parse(raw: &str) -> io::Result<ProjectPromptDoc> {
    let (frontmatter_str, body) = split_frontmatter(raw);

    let frontmatter = match frontmatter_str {
        Some(fm) => toml::from_str::<ProjectPromptFrontMatter>(fm).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("failed to parse TOML front-matter: {e}"),
            )
        })?,
        None => ProjectPromptFrontMatter::default(),
    };

    reject_disallowed_control_chars(body)?;

    Ok(ProjectPromptDoc {
        frontmatter,
        body: body.to_string(),
    })
}

/// If `raw` starts with a `+++` fenced block, return `(Some(inner), rest)`;
/// otherwise `(None, raw)`.
///
/// The opening fence must be at byte offset 0 and the closing fence must be
/// on its own line. Unterminated front-matter is treated as "no front-matter"
/// so a stray `+++` at the top of an otherwise-valid markdown file doesn't
/// eat the body.
fn split_frontmatter(raw: &str) -> (Option<&str>, &str) {
    let Some(after_open) = raw.strip_prefix("+++\n") else {
        return (None, raw);
    };

    // Look for a line consisting solely of `+++` (optionally followed by \n).
    let mut search_start = 0usize;
    while let Some(pos) = after_open[search_start..].find("+++") {
        let absolute = search_start + pos;
        // The fence must start at beginning-of-line: either offset 0 or
        // preceded by '\n'.
        let at_line_start = absolute == 0 || after_open.as_bytes()[absolute - 1] == b'\n';
        // The fence must be followed by EOL or EOF.
        let end = absolute + 3;
        let followed_by_eol = match after_open.as_bytes().get(end) {
            None => true,
            Some(&b'\n') => true,
            Some(&b'\r') => matches!(after_open.as_bytes().get(end + 1), Some(&b'\n')),
            _ => false,
        };

        if at_line_start && followed_by_eol {
            let inner = &after_open[..absolute];
            // Trim the trailing newline that belongs to the "+++" line above.
            let inner = inner.strip_suffix('\n').unwrap_or(inner);
            // Skip past the closing fence and its line terminator.
            let mut body_start = end;
            if after_open.as_bytes().get(body_start) == Some(&b'\r') {
                body_start += 1;
            }
            if after_open.as_bytes().get(body_start) == Some(&b'\n') {
                body_start += 1;
            }
            let body = &after_open[body_start..];
            return (Some(inner), body);
        }

        search_start = absolute + 3;
    }

    (None, raw)
}

/// Reject ASCII control characters that have no legitimate use in a prompt
/// body: `\x00-\x08`, `\x0B`, `\x0C`, `\x0E-\x1F`. Tab (`\x09`), LF (`\x0A`)
/// and CR (`\x0D`) are allowed.
fn reject_disallowed_control_chars(body: &str) -> io::Result<()> {
    for (idx, ch) in body.char_indices() {
        let c = ch as u32;
        let disallowed = (c <= 0x08) || c == 0x0B || c == 0x0C || (0x0E..=0x1F).contains(&c);
        if disallowed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "prompt body contains disallowed control character U+{c:04X} at byte offset {idx}"
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_prompt(dir: &Path, contents: &[u8]) {
        let nanna = dir.join(".nanna");
        fs::create_dir_all(&nanna).unwrap();
        fs::write(nanna.join("prompt.md"), contents).unwrap();
    }

    #[test]
    fn missing_file_returns_ok_none() {
        let dir = tempdir().unwrap();
        let result = load(dir.path()).expect("load must not error on missing file");
        assert!(result.is_none());
    }

    #[test]
    fn file_without_frontmatter_has_default_frontmatter() {
        let dir = tempdir().unwrap();
        let body = "# Project guidance\n\nUse nextest.\n";
        write_prompt(dir.path(), body.as_bytes());

        let doc = load(dir.path()).unwrap().expect("should return Some");
        assert_eq!(doc.frontmatter, ProjectPromptFrontMatter::default());
        assert_eq!(doc.body, body);
    }

    #[test]
    fn file_with_frontmatter_parses_fields() {
        let dir = tempdir().unwrap();
        let contents = "+++\nlanguage_override = \"rust\"\nframeworks = [\"tokio\", \"nextest\"]\nmax_tokens = 1200\n+++\n# Body\nHello.\n";
        write_prompt(dir.path(), contents.as_bytes());

        let doc = load(dir.path()).unwrap().unwrap();
        assert_eq!(doc.frontmatter.language_override.as_deref(), Some("rust"));
        assert_eq!(doc.frontmatter.frameworks, vec!["tokio", "nextest"]);
        assert_eq!(doc.frontmatter.max_tokens, Some(1200));
        assert_eq!(doc.body, "# Body\nHello.\n");
    }

    #[test]
    fn frontmatter_only_no_body() {
        let dir = tempdir().unwrap();
        let contents = "+++\nmax_tokens = 42\n+++\n";
        write_prompt(dir.path(), contents.as_bytes());

        let doc = load(dir.path()).unwrap().unwrap();
        assert_eq!(doc.frontmatter.max_tokens, Some(42));
        assert_eq!(doc.body, "");
    }

    #[test]
    fn oversize_file_is_rejected() {
        let dir = tempdir().unwrap();
        // Write (MAX + 1) bytes.
        let big = vec![b'a'; (MAX_PROMPT_FILE_BYTES as usize) + 1];
        write_prompt(dir.path(), &big);

        let err = load(dir.path()).expect_err("oversize file must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("exceeds maximum size"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn disallowed_control_char_is_rejected() {
        let dir = tempdir().unwrap();
        // U+0007 (BEL) is in the \x00-\x08 disallowed range.
        let contents = b"# Heading\nline with bell \x07 here\n";
        write_prompt(dir.path(), contents);

        let err = load(dir.path()).expect_err("control char must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(
            err.to_string().contains("control character"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn tab_lf_cr_are_allowed_in_body() {
        let dir = tempdir().unwrap();
        let contents = "line1\n\tindented\r\nline3\n";
        write_prompt(dir.path(), contents.as_bytes());

        let doc = load(dir.path()).unwrap().unwrap();
        assert_eq!(doc.body, contents);
    }

    #[test]
    fn invalid_toml_frontmatter_errors_descriptively() {
        let dir = tempdir().unwrap();
        let contents = "+++\nthis is = = not toml\n+++\nbody\n";
        write_prompt(dir.path(), contents.as_bytes());

        let err = load(dir.path()).expect_err("invalid TOML must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(
            msg.contains("TOML front-matter"),
            "expected descriptive message, got: {msg}"
        );
    }

    #[test]
    fn unknown_frontmatter_key_is_rejected() {
        // We opted for `deny_unknown_fields` so typos surface early. Documented
        // in the module-level rustdoc.
        let dir = tempdir().unwrap();
        let contents = "+++\nlanguage_overridden = \"rust\"\n+++\nbody\n";
        write_prompt(dir.path(), contents.as_bytes());

        let err = load(dir.path()).expect_err("unknown key must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn unterminated_frontmatter_is_treated_as_body() {
        // A file that begins with `+++\n` but never closes the fence should
        // fall back to "no front-matter" so a stray `+++` at the top of a
        // markdown document doesn't consume the rest of the file.
        let dir = tempdir().unwrap();
        let contents = "+++\nno closing fence here\n";
        write_prompt(dir.path(), contents.as_bytes());

        let doc = load(dir.path()).unwrap().unwrap();
        assert_eq!(doc.frontmatter, ProjectPromptFrontMatter::default());
        assert_eq!(doc.body, contents);
    }
}
