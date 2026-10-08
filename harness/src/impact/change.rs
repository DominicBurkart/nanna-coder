use super::error::ImpactError;
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

/// A line added by a diff, with its 1-based number in the new file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddedLine {
    pub number: u32,
    pub text: String,
}

/// One file in a change: what was added and removed, and optionally the full
/// post-change content for extractors that need surrounding context.
///
/// ```
/// use harness::impact::ChangedFile;
///
/// let file = ChangedFile::new("api/src/lib.rs");
/// assert_eq!(file.path, "api/src/lib.rs");
/// assert!(file.added.is_empty() && file.content.is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    pub added: Vec<AddedLine>,
    pub removed: Vec<String>,
    pub content: Option<String>,
}

impl ChangedFile {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: normalize_path(&path.into()),
            added: Vec::new(),
            removed: Vec::new(),
            content: None,
        }
    }

    /// A file that was written whole: every line of `content` is added.
    ///
    /// ```
    /// use harness::impact::ChangedFile;
    ///
    /// let file = ChangedFile::whole("a.sql", "one\ntwo\n");
    /// assert_eq!(file.added.len(), 2);
    /// assert_eq!(file.added[1].number, 2);
    /// assert_eq!(file.content.as_deref(), Some("one\ntwo\n"));
    /// ```
    pub fn whole(path: impl Into<String>, content: impl Into<String>) -> Self {
        let content = content.into();
        let mut file = Self::new(path);
        for (i, text) in content.lines().enumerate() {
            let number = i as u32 + 1;
            let text = text.to_string();
            file.added.push(AddedLine { number, text });
        }
        file.content = Some(content);
        file
    }

    pub fn added_text(&self) -> String {
        join_lines(self.added.iter().map(|line| line.text.as_str()))
    }

    pub fn removed_text(&self) -> String {
        join_lines(self.removed.iter().map(String::as_str))
    }
}

fn join_lines<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    lines.collect::<Vec<_>>().join("\n")
}

pub(crate) fn normalize_path(path: &str) -> String {
    let mut path = path.replace('\\', "/");
    while let Some(rest) = path.strip_prefix("./") {
        path = rest.to_string();
    }
    path
}

/// Why a tool call could not be turned into an [`Action`] or a changed path.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ActionError {
    #[error("argument `{field}` must be present and a string")]
    BadArgument { field: &'static str },
}

/// A side-effecting step, as opposed to a code change.
///
/// ```
/// use harness::impact::Action;
/// use serde_json::json;
///
/// let action = Action::from_tool_call("sandbox_deploy", &json!({"environment": "staging"}));
/// assert_eq!(action, Ok(Some(Action::SandboxDeploy { environment: "staging".into() })));
/// assert_eq!(Action::from_tool_call("ci_trigger", &json!({})), Ok(Some(Action::CiTrigger)));
/// assert_eq!(Action::from_tool_call("read_file", &json!({})), Ok(None));
/// assert!(Action::from_tool_call("sandbox_deploy", &json!({})).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    SandboxDeploy { environment: String },
    Rollout { environment: String },
    CiTrigger,
}

impl Action {
    /// The action a tool call performs, if it is one of the deploy or CI tools.
    ///
    /// A deploy or rollout must name its `environment` as a string; there is
    /// no default environment.
    pub fn from_tool_call(
        tool: &str,
        arguments: &serde_json::Value,
    ) -> Result<Option<Self>, ActionError> {
        let environment = || {
            arguments
                .get("environment")
                .and_then(|value| value.as_str())
                .map(str::to_string)
                .ok_or(ActionError::BadArgument {
                    field: "environment",
                })
        };
        match tool {
            "sandbox_deploy" => Ok(Some(Action::SandboxDeploy {
                environment: environment()?,
            })),
            "prod_rollout" | "rollout" => Ok(Some(Action::Rollout {
                environment: environment()?,
            })),
            "ci_trigger" => Ok(Some(Action::CiTrigger)),
            _ => Ok(None),
        }
    }

    pub fn label(&self) -> String {
        match self {
            Action::SandboxDeploy { environment } => format!("sandbox_deploy({environment})"),
            Action::Rollout { environment } => format!("rollout({environment})"),
            Action::CiTrigger => "ci_trigger".to_string(),
        }
    }
}

/// Why a unified diff could not be understood.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DiffError {
    #[error("line {line}: `{text}` does not belong in a unified diff here")]
    UnexpectedLine { line: usize, text: String },
    #[error("line {line}: malformed hunk header `{text}`")]
    HunkHeader { line: usize, text: String },
    #[error(
        "line {line}: unrecognised path in `{text}` (expected `a/<path>`, `b/<path>` or /dev/null)"
    )]
    PathHeader { line: usize, text: String },
    #[error("the diff ends inside a hunk of `{path}`")]
    TruncatedHunk { path: String },
}

/// A proposed change: modified files and actions to run.
///
/// ```
/// use harness::impact::Change;
///
/// let diff = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n keep\n-old\n+new\n";
/// let change = Change::from_unified_diff(diff).unwrap();
/// assert_eq!(change.files.len(), 1);
/// assert_eq!(change.files[0].added[0].number, 2);
/// assert_eq!(change.files[0].removed, ["old"]);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub files: Vec<ChangedFile>,
    pub actions: Vec<Action>,
}

impl Change {
    pub fn from_actions(actions: impl IntoIterator<Item = Action>) -> Self {
        Self {
            files: Vec::new(),
            actions: actions.into_iter().collect(),
        }
    }

    pub fn from_paths<I, S>(paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            files: paths.into_iter().map(ChangedFile::new).collect(),
            actions: Vec::new(),
        }
    }

    /// Parse a unified diff such as `git diff` prints, with `a/` and `b/` path
    /// prefixes.
    ///
    /// Deleted files are attributed to their old path; renames count as a
    /// change to both paths; binary files and mode changes carry no lines but
    /// still count as changed files. Quoted paths and paths containing spaces
    /// are understood. Anything that is not a well-formed diff is an error.
    ///
    /// ```
    /// use harness::impact::Change;
    ///
    /// let diff = "diff --git a/gone.sql b/gone.sql\ndeleted file mode 100644\n--- a/gone.sql\n+++ /dev/null\n@@ -1 +0,0 @@\n-DROP TABLE x;\n";
    /// let change = Change::from_unified_diff(diff).unwrap();
    /// assert_eq!(change.files[0].path, "gone.sql");
    /// assert_eq!(change.files[0].removed, ["DROP TABLE x;"]);
    /// assert!(Change::from_unified_diff("").unwrap().files.is_empty());
    /// assert!(Change::from_unified_diff("not a diff").is_err());
    /// ```
    pub fn from_unified_diff(diff: &str) -> Result<Self, DiffError> {
        let mut parser = DiffParser::default();
        for (index, line) in diff.lines().enumerate() {
            parser.feed(index + 1, line)?;
        }
        parser.finish()
    }

    /// Fill each file's `content` from `root` where the file exists.
    ///
    /// A file that no longer exists (deleted), is not UTF-8 text (binary) or
    /// is a directory (a submodule) has no content; any other read failure is
    /// an error.
    ///
    /// ```
    /// use harness::impact::Change;
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
    /// let mut change = Change::from_paths(["a.txt", "missing.txt"]);
    /// change.load_content(dir.path()).unwrap();
    /// assert_eq!(change.files[0].content.as_deref(), Some("hi"));
    /// assert_eq!(change.files[1].content, None);
    /// ```
    pub fn load_content(&mut self, root: &Path) -> Result<(), ImpactError> {
        for file in &mut self.files {
            if file.content.is_some() {
                continue;
            }
            let path = root.join(&file.path);
            match std::fs::read_to_string(&path) {
                Ok(content) => file.content = Some(content),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound
                            | std::io::ErrorKind::InvalidData
                            | std::io::ErrorKind::IsADirectory
                    ) => {}
                Err(source) => return Err(ImpactError::Io { path, source }),
            }
        }
        Ok(())
    }
}

#[derive(Default)]
struct DiffParser {
    files: Vec<ChangedFile>,
    old_remaining: u32,
    new_remaining: u32,
    new_line: u32,
    rename_from: Option<String>,
}

const IGNORED_HEADERS: [&str; 9] = [
    "index ",
    "old mode ",
    "new mode ",
    "deleted file mode ",
    "new file mode ",
    "similarity index ",
    "dissimilarity index ",
    "Binary files ",
    "copy from ",
];

impl DiffParser {
    fn unexpected(line: usize, text: &str) -> DiffError {
        DiffError::UnexpectedLine {
            line,
            text: text.to_string(),
        }
    }

    fn feed(&mut self, number: usize, line: &str) -> Result<(), DiffError> {
        if self.old_remaining > 0 || self.new_remaining > 0 {
            return self.hunk_line(number, line);
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let path = header_new_path(rest).ok_or_else(|| DiffError::PathHeader {
                line: number,
                text: line.to_string(),
            })?;
            self.files.push(ChangedFile::new(path));
            self.rename_from = None;
            return Ok(());
        }
        if line.is_empty() || line.starts_with('\\') {
            return Ok(());
        }
        if self.files.is_empty() {
            return Err(Self::unexpected(number, line));
        }
        if let Some(rest) = line.strip_prefix("--- ") {
            check_side(number, line, rest, "a/")?;
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            check_side(number, line, rest, "b/")?;
        } else if let Some(rest) = line.strip_prefix("rename from ") {
            self.rename_from =
                Some(unquote_or_plain(rest).ok_or_else(|| DiffError::PathHeader {
                    line: number,
                    text: line.to_string(),
                })?);
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            self.rename_to(number, line, rest)?;
        } else if let Some(rest) = line.strip_prefix("copy to ") {
            let path = unquote_or_plain(rest).ok_or_else(|| DiffError::PathHeader {
                line: number,
                text: line.to_string(),
            })?;
            if let Some(file) = self.files.last_mut() {
                file.path = normalize_path(&path);
            }
        } else if let Some(header) = line.strip_prefix("@@ ") {
            let ((_, old_len), (new_start, new_len)) =
                parse_hunk_header(header).ok_or_else(|| DiffError::HunkHeader {
                    line: number,
                    text: line.to_string(),
                })?;
            self.old_remaining = old_len;
            self.new_remaining = new_len;
            self.new_line = new_start;
        } else if !IGNORED_HEADERS
            .iter()
            .any(|prefix| line.starts_with(prefix))
        {
            return Err(Self::unexpected(number, line));
        }
        Ok(())
    }

    fn rename_to(&mut self, number: usize, line: &str, rest: &str) -> Result<(), DiffError> {
        let new = unquote_or_plain(rest).ok_or_else(|| DiffError::PathHeader {
            line: number,
            text: line.to_string(),
        })?;
        let old = self
            .rename_from
            .take()
            .ok_or_else(|| Self::unexpected(number, line))?;
        let last = self.files.len() - 1;
        self.files[last].path = normalize_path(&new);
        self.files.insert(last, ChangedFile::new(old));
        Ok(())
    }

    fn hunk_line(&mut self, number: usize, line: &str) -> Result<(), DiffError> {
        let file = self
            .files
            .last_mut()
            .ok_or_else(|| Self::unexpected(number, line))?;
        if let Some(text) = line.strip_prefix('+') {
            if self.new_remaining == 0 {
                return Err(Self::unexpected(number, line));
            }
            file.added.push(AddedLine {
                number: self.new_line,
                text: text.to_string(),
            });
            self.new_line += 1;
            self.new_remaining -= 1;
        } else if let Some(text) = line.strip_prefix('-') {
            if self.old_remaining == 0 {
                return Err(Self::unexpected(number, line));
            }
            file.removed.push(text.to_string());
            self.old_remaining -= 1;
        } else if line.starts_with('\\') {
        } else if line.is_empty() || line.starts_with(' ') {
            if self.old_remaining == 0 || self.new_remaining == 0 {
                return Err(Self::unexpected(number, line));
            }
            self.new_line += 1;
            self.new_remaining -= 1;
            self.old_remaining -= 1;
        } else {
            return Err(Self::unexpected(number, line));
        }
        Ok(())
    }

    fn finish(self) -> Result<Change, DiffError> {
        if self.old_remaining > 0 || self.new_remaining > 0 {
            let last = self.files.last().expect("a hunk header implies a file");
            return Err(DiffError::TruncatedHunk {
                path: last.path.clone(),
            });
        }
        Ok(Change {
            files: self.files,
            actions: Vec::new(),
        })
    }
}

fn parse_hunk_header(header: &str) -> Option<((u32, u32), (u32, u32))> {
    let range = |token: &str, sign: char| {
        let token = token.strip_prefix(sign)?;
        match token.split_once(',') {
            Some((start, len)) => Some((start.parse().ok()?, len.parse().ok()?)),
            None => Some((token.parse().ok()?, 1)),
        }
    };
    let mut parts = header.split(' ');
    let old = range(parts.next()?, '-')?;
    let new = range(parts.next()?, '+')?;
    (parts.next()? == "@@").then_some((old, new))
}

fn take_quoted(text: &str) -> Option<(String, &str)> {
    let body = text.strip_prefix('"')?;
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some((String::from_utf8(out).ok()?, &body[i + 1..])),
            b'\\' => {
                let escaped = *bytes.get(i + 1)?;
                i += 2;
                match escaped {
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'v' => out.push(11),
                    b'\\' | b'"' => out.push(escaped),
                    b'0'..=b'3' => {
                        let digits = bytes.get(i..i + 2)?;
                        if !digits.iter().all(|d| (b'0'..=b'7').contains(d)) {
                            return None;
                        }
                        let value = u32::from(escaped - b'0') * 64
                            + u32::from(digits[0] - b'0') * 8
                            + u32::from(digits[1] - b'0');
                        out.push(u8::try_from(value).ok()?);
                        i += 2;
                    }
                    _ => return None,
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    None
}

fn unquote_or_plain(text: &str) -> Option<String> {
    if text.starts_with('"') {
        let (path, tail) = take_quoted(text)?;
        tail.is_empty().then_some(path)
    } else {
        Some(text.to_string())
    }
}

fn check_side(number: usize, line: &str, rest: &str, prefix: &str) -> Result<(), DiffError> {
    let rest = rest.find('\t').map_or(rest, |tab| &rest[..tab]);
    let valid =
        rest == "/dev/null" || unquote_or_plain(rest).is_some_and(|path| path.starts_with(prefix));
    if valid {
        Ok(())
    } else {
        Err(DiffError::PathHeader {
            line: number,
            text: line.to_string(),
        })
    }
}

fn symmetric_path(rest: &str) -> Option<String> {
    let total = rest.len();
    if total < 5 || (total - 5) % 2 != 0 {
        return None;
    }
    let len = (total - 5) / 2;
    let old = rest.get(2..2 + len)?;
    let new = rest.get(5 + len..)?;
    (rest.get(..2)? == "a/" && rest.get(2 + len..5 + len)? == " b/" && old == new)
        .then(|| new.to_string())
}

fn header_new_path(rest: &str) -> Option<String> {
    let new = if rest.starts_with('"') {
        let (_, tail) = take_quoted(rest)?;
        tail.strip_prefix(' ')?
    } else if let Some(at) = rest.rfind(" \"b/") {
        &rest[at + 1..]
    } else {
        return symmetric_path(rest)
            .or_else(|| rest.rsplit_once(" b/").map(|(_, new)| new.to_string()));
    };
    let new = unquote_or_plain(new)?;
    new.strip_prefix("b/").map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    fn parse(diff: &str) -> Change {
        Change::from_unified_diff(diff).unwrap()
    }

    #[test]
    fn parses_multiple_files_and_hunks_with_new_line_numbers() {
        let diff = "diff --git a/a.rs b/a.rs\nindex 1..2 100644\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,4 @@\n one\n+two\n three\n four\n@@ -10,2 +11,2 @@ fn ctx\n-old\n+new\n same\ndiff --git a/b.rs b/b.rs\nnew file mode 100644\n--- /dev/null\n+++ b/b.rs\n@@ -0,0 +1,2 @@\n+x\n+y\n";
        let change = parse(diff);
        assert_eq!(change.files.len(), 2);
        let a = &change.files[0];
        assert_eq!(a.path, "a.rs");
        assert_eq!(
            a.added.iter().map(|l| l.number).collect::<Vec<_>>(),
            [2, 11]
        );
        assert_eq!(a.removed, ["old"]);
        let b = &change.files[1];
        assert_eq!(b.path, "b.rs");
        assert_eq!(b.added_text(), "x\ny");
    }

    #[test]
    fn plus_plus_plus_inside_hunk_is_content_not_a_header() {
        let diff = "diff --git a/a.md b/a.md\n--- a/a.md\n+++ b/a.md\n@@ -1 +1,2 @@\n keep\n+++ not a header\n";
        let change = parse(diff);
        assert_eq!(change.files[0].path, "a.md");
        assert_eq!(change.files[0].added[0].text, "++ not a header");
    }

    #[test]
    fn binary_and_mode_only_files_have_no_lines() {
        let diff = "diff --git a/bin.dat b/bin.dat\nBinary files a/bin.dat and b/bin.dat differ\ndiff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n";
        let change = parse(diff);
        assert_eq!(change.files.len(), 2);
        assert!(change.files.iter().all(|f| f.added.is_empty()));
        assert_eq!(change.files[1].path, "run.sh");
    }

    #[test]
    fn no_newline_marker_is_ignored() {
        let diff = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n\\ No newline at end of file\n+y\n\\ No newline at end of file\n";
        let change = parse(diff);
        assert_eq!(change.files[0].removed, ["x"]);
        assert_eq!(change.files[0].added[0].text, "y");
    }

    #[test]
    fn blank_context_lines_stripped_of_their_space_are_accepted() {
        let diff = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,3 +1,4 @@\n one\n\n+two\n three\n";
        let change = parse(diff);
        assert_eq!(change.files[0].added[0].number, 3);
    }

    #[test]
    fn hunk_headers_without_counts_default_to_one_line() {
        let diff = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -4 +4 @@ fn ctx\n-x\n+y\n";
        let change = parse(diff);
        assert_eq!(change.files[0].added[0].number, 4);
        assert_eq!(change.files[0].removed, ["x"]);
    }

    #[test]
    fn empty_input_is_an_empty_change() {
        assert!(parse("").files.is_empty());
        assert!(parse("\n\n").files.is_empty());
    }

    #[test]
    fn text_that_is_not_a_diff_is_an_error() {
        assert_eq!(
            Change::from_unified_diff("not a diff\nat all"),
            Err(DiffError::UnexpectedLine {
                line: 1,
                text: "not a diff".into()
            })
        );
        let junk_after_header = "diff --git a/a b/a\nsurprise\n";
        assert_eq!(
            Change::from_unified_diff(junk_after_header),
            Err(DiffError::UnexpectedLine {
                line: 2,
                text: "surprise".into()
            })
        );
    }

    #[test]
    fn a_hunk_header_outside_any_file_is_an_error() {
        assert!(matches!(
            Change::from_unified_diff("@@ -1 +1 @@\n-a\n+b\n"),
            Err(DiffError::UnexpectedLine { line: 1, .. })
        ));
    }

    #[test]
    fn malformed_hunk_headers_are_errors() {
        for header in [
            "@@ -x +1 @@",
            "@@ -1,y +1 @@",
            "@@ -1 +1",
            "@@ -1 @@",
            "@@ 1 +1 @@",
            "@@ -1 1 @@",
        ] {
            let diff = format!("diff --git a/a b/a\n--- a/a\n+++ b/a\n{header}\n-x\n+y\n");
            assert!(
                matches!(
                    Change::from_unified_diff(&diff),
                    Err(DiffError::HunkHeader { line: 4, .. })
                ),
                "{header}"
            );
        }
    }

    #[test]
    fn a_diff_that_ends_inside_a_hunk_is_an_error() {
        let diff = "diff --git a/a.sql b/a.sql\n--- a/a.sql\n+++ b/a.sql\n@@ -1,2 +1,3 @@\n keep\n+DROP TABLE x;\n";
        assert_eq!(
            Change::from_unified_diff(diff),
            Err(DiffError::TruncatedHunk {
                path: "a.sql".into()
            })
        );
    }

    #[test]
    fn hunk_lines_that_contradict_the_header_counts_are_errors() {
        let extra_add = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,2 +1,1 @@\n-x\n-y\n+z\n+w\n";
        assert!(matches!(
            Change::from_unified_diff(extra_add),
            Err(DiffError::UnexpectedLine { line: 8, .. })
        ));
        let extra_remove = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,1 +1,2 @@\n-x\n-y\n";
        assert!(matches!(
            Change::from_unified_diff(extra_remove),
            Err(DiffError::UnexpectedLine { line: 6, .. })
        ));
        let context_after_exhausted_old =
            "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,1 +1,2 @@\n-x\n y\n+z\n";
        assert!(matches!(
            Change::from_unified_diff(context_after_exhausted_old),
            Err(DiffError::UnexpectedLine { line: 6, .. })
        ));
        let stray = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,1 +1,1 @@\n?x\n";
        assert!(matches!(
            Change::from_unified_diff(stray),
            Err(DiffError::UnexpectedLine { line: 5, .. })
        ));
    }

    #[test]
    fn path_headers_without_git_prefixes_are_errors() {
        for diff in [
            "diff --git a/x.rs b/x.rs\n--- c/x.rs\n+++ i/x.rs\n",
            "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ x.rs\n",
            "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ \"b/x.rs\n",
            "diff --git x.rs x.rs\n",
            "diff --git weird\n",
            "diff --git \"a/x.rs\" \"b/x.rs\" extra\n",
        ] {
            assert!(
                matches!(
                    Change::from_unified_diff(diff),
                    Err(DiffError::PathHeader { .. })
                ),
                "{diff:?}"
            );
        }
    }

    #[test]
    fn deleted_file_takes_its_path_from_the_old_side() {
        let diff = "diff --git a/gone.rs b/gone.rs\ndeleted file mode 100644\n--- a/gone.rs\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-bye\n";
        let change = parse(diff);
        assert_eq!(change.files[0].path, "gone.rs");
        assert_eq!(change.files[0].removed, ["bye"]);
        assert!(change.files[0].added.is_empty());
    }

    const GIT_SPACES: &str = "diff --git a/my b/x file.rs b/my b/x file.rs\nindex 45b983b..3222a91 100644\n--- a/my b/x file.rs\t\n+++ b/my b/x file.rs\t\n@@ -1 +1,2 @@\n hi\n+b\n";

    #[test]
    fn paths_with_spaces_and_trailing_tabs_are_read_as_git_prints_them() {
        let change = parse(GIT_SPACES);
        assert_eq!(change.files.len(), 1);
        assert_eq!(change.files[0].path, "my b/x file.rs");
        assert_eq!(change.files[0].added[0].text, "b");
    }

    #[test]
    fn spaces_in_a_file_with_no_content_lines_use_the_symmetric_header() {
        let diff = "diff --git a/my b/run.sh b/my b/run.sh\nold mode 100644\nnew mode 100755\n";
        assert_eq!(parse(diff).files[0].path, "my b/run.sh");
    }

    #[test]
    fn asymmetric_header_without_content_lines_uses_the_last_b_prefix() {
        let diff = "diff --git a/x/old.rs b/x/new.rs\nold mode 100644\nnew mode 100755\n";
        assert_eq!(parse(diff).files[0].path, "x/new.rs");
    }

    const GIT_QUOTED: &str = "diff --git \"a/\\303\\251.sql\" \"b/\\303\\251.sql\"\nindex 7898192..422c2b7 100644\n--- \"a/\\303\\251.sql\"\n+++ \"b/\\303\\251.sql\"\n@@ -1 +1,2 @@\n a\n+b\n";

    #[test]
    fn quoted_non_ascii_paths_are_decoded() {
        let change = parse(GIT_QUOTED);
        assert_eq!(change.files[0].path, "é.sql");
        assert_eq!(change.files[0].added[0].number, 2);
    }

    #[test]
    fn quoted_new_side_of_a_rename_from_an_unquoted_name_is_decoded() {
        let diff = "diff --git a/old.sql \"b/\\303\\251.sql\"\nsimilarity index 100%\nrename from old.sql\nrename to \"\\303\\251.sql\"\n";
        let paths: Vec<_> = parse(diff).files.into_iter().map(|f| f.path).collect();
        assert_eq!(paths, ["old.sql", "é.sql"]);
    }

    #[test]
    fn quoted_header_with_unquoted_new_side_is_decoded() {
        let diff = "diff --git \"a/\\303\\251.sql\" b/plain.sql\nsimilarity index 100%\nrename from \"\\303\\251.sql\"\nrename to plain.sql\n";
        let paths: Vec<_> = parse(diff).files.into_iter().map(|f| f.path).collect();
        assert_eq!(paths, ["é.sql", "plain.sql"]);
    }

    #[test]
    fn a_rename_changes_both_the_old_and_the_new_path() {
        let diff = "diff --git a/old.txt b/new.txt\nsimilarity index 100%\nrename from old.txt\nrename to new.txt\n";
        let paths: Vec<_> = parse(diff).files.into_iter().map(|f| f.path).collect();
        assert_eq!(paths, ["old.txt", "new.txt"]);
    }

    #[test]
    fn a_rename_with_edits_attributes_the_hunk_to_the_new_path() {
        let diff = "diff --git a/old.rs b/new.rs\nsimilarity index 80%\nrename from old.rs\nrename to new.rs\nindex 1..2 100644\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-a\n+b\n";
        let change = parse(diff);
        assert_eq!(change.files.len(), 2);
        assert_eq!(change.files[0].path, "old.rs");
        assert!(change.files[0].added.is_empty());
        assert_eq!(change.files[1].path, "new.rs");
        assert_eq!(change.files[1].added_text(), "b");
    }

    #[test]
    fn a_copy_changes_only_the_destination() {
        let diff = "diff --git a/src.rs b/dst.rs\nsimilarity index 100%\ncopy from src.rs\ncopy to dst.rs\n";
        let paths: Vec<_> = parse(diff).files.into_iter().map(|f| f.path).collect();
        assert_eq!(paths, ["dst.rs"]);
    }

    #[test]
    fn malformed_rename_and_copy_headers_are_errors() {
        let no_from = "diff --git a/a b/b\nrename to b\n";
        assert!(matches!(
            Change::from_unified_diff(no_from),
            Err(DiffError::UnexpectedLine { line: 2, .. })
        ));
        for diff in [
            "diff --git a/a b/b\nrename from \"bad\nrename to b\n",
            "diff --git a/a b/b\nrename from a\nrename to \"bad\n",
            "diff --git a/a b/b\ncopy from a\ncopy to \"bad\n",
        ] {
            assert!(
                matches!(
                    Change::from_unified_diff(diff),
                    Err(DiffError::PathHeader { .. })
                ),
                "{diff:?}"
            );
        }
    }

    #[test]
    fn quoted_strings_decode_every_escape() {
        assert_eq!(
            take_quoted("\"a\\tb\\nc\\\\d\\\"e\\a\\b\\f\\r\\v\\101\" rest"),
            Some(("a\tb\nc\\d\"e\u{7}\u{8}\u{c}\r\u{b}A".to_string(), " rest"))
        );
        assert_eq!(take_quoted("\"\\303\\251\""), Some(("é".to_string(), "")));
    }

    #[test]
    fn invalid_quoted_strings_are_rejected() {
        for text in [
            "plain",
            "\"unterminated",
            "\"dangling\\",
            "\"\\q\"",
            "\"\\4\"",
            "\"\\18x\"",
            "\"\\3\"",
            "\"\\377\"",
        ] {
            assert_eq!(take_quoted(text), None, "{text}");
        }
        assert_eq!(unquote_or_plain("\"x\" trailing"), None);
        assert_eq!(unquote_or_plain("x"), Some("x".to_string()));
    }

    #[test]
    fn paths_are_normalised() {
        assert_eq!(ChangedFile::new("./a\\b.rs").path, "a/b.rs");
    }

    #[test]
    fn tool_call_mapping() {
        assert_eq!(
            Action::from_tool_call("prod_rollout", &json!({"environment": "production"})),
            Ok(Some(Action::Rollout {
                environment: "production".into()
            }))
        );
        assert_eq!(
            Action::from_tool_call("rollout", &json!({"environment": "staging"})),
            Ok(Some(Action::Rollout {
                environment: "staging".into()
            }))
        );
        assert_eq!(
            Action::from_tool_call("sandbox_deploy", &json!({"environment": "sandbox"})),
            Ok(Some(Action::SandboxDeploy {
                environment: "sandbox".into()
            }))
        );
        assert_eq!(
            Action::from_tool_call("ci_trigger", &json!({})),
            Ok(Some(Action::CiTrigger))
        );
        assert_eq!(Action::from_tool_call("write_file", &json!({})), Ok(None));
        assert_eq!(Action::CiTrigger.label(), "ci_trigger");
        assert_eq!(
            Action::Rollout {
                environment: "p".into()
            }
            .label(),
            "rollout(p)"
        );
    }

    #[test]
    fn deploys_and_rollouts_never_default_their_environment() {
        let error = ActionError::BadArgument {
            field: "environment",
        };
        let missing = Err(error.clone());
        for tool in ["sandbox_deploy", "prod_rollout", "rollout"] {
            assert_eq!(Action::from_tool_call(tool, &json!({})), missing, "{tool}");
            assert_eq!(
                Action::from_tool_call(tool, &json!({"environment": 3})),
                missing,
                "{tool}"
            );
            assert_eq!(
                Action::from_tool_call(tool, &json!({"environment": null})),
                missing,
                "{tool}"
            );
        }
        assert_eq!(
            error.to_string(),
            "argument `environment` must be present and a string"
        );
    }

    #[test]
    fn whole_numbers_every_line_and_keeps_content() {
        let file = ChangedFile::whole("a.sql", "one\ntwo\n");
        assert_eq!(file.path, "a.sql");
        assert_eq!(
            file.added
                .iter()
                .map(|l| (l.number, l.text.as_str()))
                .collect::<Vec<_>>(),
            [(1, "one"), (2, "two")]
        );
        assert_eq!(file.content.as_deref(), Some("one\ntwo\n"));
        assert!(ChangedFile::whole("e", "").added.is_empty());
    }

    #[test]
    fn load_content_reads_text_and_skips_deleted_binary_and_directory_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
        std::fs::write(dir.path().join("bin.dat"), [0xff, 0xfe, 0x00]).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        let mut change = Change::from_paths(["a.txt", "gone.txt", "bin.dat", "sub"]);
        change.load_content(dir.path()).unwrap();
        assert_eq!(change.files[0].content.as_deref(), Some("hi"));
        assert_eq!(change.files[1].content, None);
        assert_eq!(change.files[2].content, None);
        assert_eq!(change.files[3].content, None);
    }

    #[test]
    fn load_content_keeps_content_that_is_already_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "disk").unwrap();
        let mut change = Change {
            files: vec![ChangedFile::whole("a.txt", "memory")],
            actions: vec![],
        };
        change.load_content(dir.path()).unwrap();
        assert_eq!(change.files[0].content.as_deref(), Some("memory"));
    }

    #[test]
    fn load_content_surfaces_unreadable_files() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("loop", dir.path().join("loop")).unwrap();
        let mut change = Change::from_paths(["loop"]);
        let error = change.load_content(dir.path()).unwrap_err();
        assert!(matches!(error, ImpactError::Io { .. }));
        assert!(error.to_string().contains("loop"));
    }

    fn render(files: &[(String, Vec<(char, String)>)]) -> String {
        let mut out = String::new();
        for (path, lines) in files {
            out.push_str(&format!(
                "diff --git a/{path} b/{path}\nindex 1..2 100644\n--- a/{path}\n+++ b/{path}\n"
            ));
            let old = lines.iter().filter(|(k, _)| *k != '+').count();
            let new = lines.iter().filter(|(k, _)| *k != '-').count();
            out.push_str(&format!("@@ -1,{old} +1,{new} @@\n"));
            for (kind, text) in lines {
                out.push(*kind);
                out.push_str(text);
                out.push('\n');
            }
        }
        out
    }

    proptest! {
        #[test]
        fn rendered_diffs_parse_back_to_their_added_and_removed_lines(
            files in prop::collection::vec(
                (
                    "[a-z]{1,4}( [a-z]{1,3})?(/[a-z]{1,4}){0,2}\\.rs",
                    prop::collection::vec(
                        (prop::sample::select(vec!['+', '-', ' ']), "[a-zA-Z0-9 {}();+-]{0,12}"),
                        1..8,
                    ),
                ),
                0..4,
            )
        ) {
            let change = Change::from_unified_diff(&render(&files)).unwrap();
            prop_assert_eq!(change.files.len(), files.len());
            for (file, (path, lines)) in change.files.iter().zip(&files) {
                prop_assert_eq!(&file.path, path);
                let mut number = 0u32;
                let mut added = Vec::new();
                let mut removed = Vec::new();
                for (kind, text) in lines {
                    if *kind != '-' {
                        number += 1;
                    }
                    match kind {
                        '+' => added.push((number, text.clone())),
                        '-' => removed.push(text.clone()),
                        _ => {}
                    }
                }
                let parsed: Vec<_> = file.added.iter().map(|l| (l.number, l.text.clone())).collect();
                prop_assert_eq!(parsed, added);
                prop_assert_eq!(&file.removed, &removed);
            }
        }

        #[test]
        fn arbitrary_text_is_parsed_or_rejected_without_panicking(text in "\\PC{0,200}") {
            let _ = Change::from_unified_diff(&text);
            let framed = format!("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n{text}\n");
            let _ = Change::from_unified_diff(&framed);
        }
    }
}
