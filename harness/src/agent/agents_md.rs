//! Repository-level agent guidance loader.
//!
//! Reads an `AGENTS.md` (preferred) or `CLAUDE.md` (fallback) file at the root
//! of an onboarded / working-directory repository and returns its contents so
//! the harness can inject them into the session system prompt and the entity
//! store at session start.
//!
//! Closes issue #231. `AGENTS.md` is the emerging cross-vendor convention for
//! per-repo agent guidance (see the AGENTS.md community spec); `CLAUDE.md` is
//! Claude Code's legacy equivalent. When both are present `AGENTS.md` wins, as
//! the industry convention.
//!
//! # Behavior summary
//!
//! - Missing file → `Ok(None)`, no side effects, no tracing output.
//! - Oversize file (> [`MAX_AGENTS_MD_BYTES`]) → truncated to the cap and a
//!   `tracing::warn!` is emitted naming the file and observed length. The
//!   caller still receives usable content.
//! - Non-UTF8 bytes → `Err(io::Error)`; the caller logs and continues without
//!   injection.
//! - Both `AGENTS.md` and `CLAUDE.md` present → only `AGENTS.md` is loaded.
//!
//! # Why not merge it into `.nanna/prompt.md`?
//!
//! `.nanna/prompt.md` is nanna-specific per-project prompt configuration owned
//! by the onboarded repo's maintainers. `AGENTS.md` is a cross-tool contract
//! surfaced by the onboarded repo as-is — we do not parse its body. Keeping the
//! two loaders distinct lets `AGENTS.md` evolve with the community spec without
//! entangling it with nanna's front-matter schema.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Cap on the number of bytes read from an AGENTS.md / CLAUDE.md file.
///
/// 64 KiB matches the cap used for `.nanna/prompt.md`
/// (`project_prompt::MAX_PROMPT_FILE_BYTES`) and is generous enough for any
/// reasonable per-repo guidance document while preventing a runaway file from
/// blowing out the model context window.
pub const MAX_AGENTS_MD_BYTES: u64 = 64 * 1024;

/// Relative path of the preferred guidance file within a repository root.
pub const AGENTS_MD_FILENAME: &str = "AGENTS.md";

/// Relative path of the legacy Claude Code guidance file; consulted only when
/// `AGENTS.md` is absent.
pub const CLAUDE_MD_FILENAME: &str = "CLAUDE.md";

/// Which of the two known filenames was loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentsMdSource {
    /// The preferred `AGENTS.md` file was found and loaded.
    AgentsMd,
    /// `AGENTS.md` was missing; the legacy `CLAUDE.md` file was loaded instead.
    ClaudeMd,
}

impl AgentsMdSource {
    /// Filename relative to the repository root.
    pub fn filename(&self) -> &'static str {
        match self {
            AgentsMdSource::AgentsMd => AGENTS_MD_FILENAME,
            AgentsMdSource::ClaudeMd => CLAUDE_MD_FILENAME,
        }
    }
}

/// Loaded guidance document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsMdDoc {
    /// Which file was loaded. Callers can surface this to downstream events.
    pub source: AgentsMdSource,
    /// Absolute path of the loaded file.
    pub path: PathBuf,
    /// UTF-8 body, truncated to [`MAX_AGENTS_MD_BYTES`] when the on-disk file
    /// exceeds the cap (see [`AgentsMdDoc::truncated`]).
    pub body: String,
    /// `true` iff the on-disk file exceeded [`MAX_AGENTS_MD_BYTES`] and `body`
    /// is a prefix of the file.
    pub truncated: bool,
}

/// Load the repo-level agent guidance file from `workspace_root`, if any.
///
/// Prefers `AGENTS.md` over `CLAUDE.md`. Returns `Ok(None)` when neither file
/// is present. Oversize files are truncated to the byte cap and a tracing
/// warning is emitted so operators can spot it.
pub fn load(workspace_root: &Path) -> io::Result<Option<AgentsMdDoc>> {
    if let Some(doc) = try_load_one(workspace_root, AgentsMdSource::AgentsMd)? {
        return Ok(Some(doc));
    }
    if let Some(doc) = try_load_one(workspace_root, AgentsMdSource::ClaudeMd)? {
        return Ok(Some(doc));
    }
    Ok(None)
}

fn try_load_one(workspace_root: &Path, source: AgentsMdSource) -> io::Result<Option<AgentsMdDoc>> {
    let path = workspace_root.join(source.filename());

    let metadata = match fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };

    // Directory entries with the same name should not be interpreted as
    // guidance files. A symlink pointing at a regular file is fine — `metadata`
    // follows symlinks.
    if !metadata.is_file() {
        return Ok(None);
    }

    let observed_len = metadata.len();
    let (raw, truncated) = if observed_len > MAX_AGENTS_MD_BYTES {
        warn!(
            path = %path.display(),
            observed_bytes = observed_len,
            max_bytes = MAX_AGENTS_MD_BYTES,
            "AGENTS.md exceeds size cap; truncating"
        );
        let mut buf = fs::read(&path)?;
        buf.truncate(MAX_AGENTS_MD_BYTES as usize);
        (buf, true)
    } else {
        (fs::read(&path)?, false)
    };

    let body = String::from_utf8(raw).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not valid UTF-8: {e}", path.display()),
        )
    })?;

    Ok(Some(AgentsMdDoc {
        source,
        path,
        body,
        truncated,
    }))
}

/// Build a system-prompt fragment from a loaded guidance document.
///
/// The fragment is delimited by machine-parseable markers so downstream log
/// inspection can identify the repo-level guidance block. The source filename
/// is named explicitly so the model knows which convention the guidance came
/// from.
pub fn format_system_prompt_fragment(doc: &AgentsMdDoc) -> String {
    format!(
        "<repo-guidance source=\"{}\">\n{}\n</repo-guidance>",
        doc.source.filename(),
        doc.body.trim_end()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_file(dir: &Path, name: &str, contents: &[u8]) {
        fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn missing_both_files_returns_none() {
        let dir = tempdir().unwrap();
        assert!(load(dir.path()).unwrap().is_none());
    }

    #[test]
    fn agents_md_is_loaded_when_present() {
        let dir = tempdir().unwrap();
        let body = "# Project rules\n\nUse nextest.\n";
        write_file(dir.path(), AGENTS_MD_FILENAME, body.as_bytes());

        let doc = load(dir.path()).unwrap().expect("should load AGENTS.md");
        assert_eq!(doc.source, AgentsMdSource::AgentsMd);
        assert_eq!(doc.body, body);
        assert!(!doc.truncated);
        assert_eq!(doc.path, dir.path().join(AGENTS_MD_FILENAME));
    }

    #[test]
    fn claude_md_is_fallback_when_agents_md_missing() {
        let dir = tempdir().unwrap();
        let body = "# Legacy Claude rules\n";
        write_file(dir.path(), CLAUDE_MD_FILENAME, body.as_bytes());

        let doc = load(dir.path()).unwrap().expect("should load CLAUDE.md");
        assert_eq!(doc.source, AgentsMdSource::ClaudeMd);
        assert_eq!(doc.body, body);
    }

    #[test]
    fn agents_md_wins_over_claude_md_when_both_present() {
        let dir = tempdir().unwrap();
        write_file(dir.path(), AGENTS_MD_FILENAME, b"agents content");
        write_file(dir.path(), CLAUDE_MD_FILENAME, b"claude content");

        let doc = load(dir.path()).unwrap().unwrap();
        assert_eq!(doc.source, AgentsMdSource::AgentsMd);
        assert_eq!(doc.body, "agents content");
    }

    #[test]
    fn oversize_file_is_truncated_with_warning() {
        let dir = tempdir().unwrap();
        // Write (MAX + 1024) bytes of ASCII.
        let big = vec![b'a'; (MAX_AGENTS_MD_BYTES as usize) + 1024];
        write_file(dir.path(), AGENTS_MD_FILENAME, &big);

        let doc = load(dir.path()).unwrap().unwrap();
        assert!(doc.truncated);
        assert_eq!(doc.body.len(), MAX_AGENTS_MD_BYTES as usize);
        assert!(doc.body.chars().all(|c| c == 'a'));
    }

    #[test]
    fn non_utf8_file_returns_error() {
        let dir = tempdir().unwrap();
        // 0xFF is not valid UTF-8.
        write_file(dir.path(), AGENTS_MD_FILENAME, &[0x48, 0xFF, 0x49]);

        let err = load(dir.path()).expect_err("non-UTF8 must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("not valid UTF-8"));
    }

    #[test]
    fn directory_named_agents_md_is_ignored() {
        // A repo could plausibly have a folder named `AGENTS.md/` (e.g. a
        // documentation site). Treating it as "no guidance" is safer than
        // panicking in `fs::read_to_string`.
        let dir = tempdir().unwrap();
        fs::create_dir(dir.path().join(AGENTS_MD_FILENAME)).unwrap();
        write_file(dir.path(), CLAUDE_MD_FILENAME, b"claude content");

        let doc = load(dir.path()).unwrap().unwrap();
        assert_eq!(doc.source, AgentsMdSource::ClaudeMd);
    }

    #[test]
    fn source_filename_matches_constants() {
        assert_eq!(AgentsMdSource::AgentsMd.filename(), AGENTS_MD_FILENAME);
        assert_eq!(AgentsMdSource::ClaudeMd.filename(), CLAUDE_MD_FILENAME);
    }

    #[test]
    fn format_system_prompt_fragment_includes_source_and_body() {
        let doc = AgentsMdDoc {
            source: AgentsMdSource::AgentsMd,
            path: PathBuf::from("/tmp/x/AGENTS.md"),
            body: "Use nextest.\n\n".to_string(),
            truncated: false,
        };
        let fragment = format_system_prompt_fragment(&doc);
        assert!(fragment.starts_with("<repo-guidance source=\"AGENTS.md\">"));
        assert!(fragment.ends_with("</repo-guidance>"));
        // Trailing blank line trimmed so the fragment is compact.
        assert!(fragment.contains("Use nextest."));
        assert!(!fragment.contains("Use nextest.\n\n"));
    }

    #[test]
    fn format_system_prompt_fragment_names_claude_md_when_fallback() {
        let doc = AgentsMdDoc {
            source: AgentsMdSource::ClaudeMd,
            path: PathBuf::from("/tmp/x/CLAUDE.md"),
            body: "legacy".to_string(),
            truncated: false,
        };
        let fragment = format_system_prompt_fragment(&doc);
        assert!(fragment.contains("source=\"CLAUDE.md\""));
    }

    #[test]
    fn oversize_claude_md_is_also_truncated() {
        // Exercise the fallback branch with an oversize file so coverage stays
        // at 100% on the `ClaudeMd` path as well.
        let dir = tempdir().unwrap();
        let big = vec![b'c'; (MAX_AGENTS_MD_BYTES as usize) + 7];
        write_file(dir.path(), CLAUDE_MD_FILENAME, &big);

        let doc = load(dir.path()).unwrap().unwrap();
        assert_eq!(doc.source, AgentsMdSource::ClaudeMd);
        assert!(doc.truncated);
        assert_eq!(doc.body.len(), MAX_AGENTS_MD_BYTES as usize);
    }
}
