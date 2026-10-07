use serde::{Deserialize, Serialize};
use std::path::Path;

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

/// A side-effecting step, as opposed to a code change.
///
/// ```
/// use harness::impact::Action;
/// use serde_json::json;
///
/// let action = Action::from_tool_call("sandbox_deploy", &json!({"environment": "staging"}));
/// assert_eq!(action, Some(Action::SandboxDeploy { environment: "staging".into() }));
/// assert_eq!(Action::from_tool_call("ci_trigger", &json!({})), Some(Action::CiTrigger));
/// assert_eq!(Action::from_tool_call("read_file", &json!({})), None);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    SandboxDeploy { environment: String },
    Rollout { environment: String },
    CiTrigger,
}

impl Action {
    /// The action a tool call performs, if it is one of the deploy or CI tools.
    pub fn from_tool_call(tool: &str, arguments: &serde_json::Value) -> Option<Self> {
        let environment = |default: &str| {
            arguments
                .get("environment")
                .and_then(|value| value.as_str())
                .unwrap_or(default)
                .to_string()
        };
        match tool {
            "sandbox_deploy" => Some(Action::SandboxDeploy {
                environment: environment("sandbox"),
            }),
            "prod_rollout" | "rollout" => Some(Action::Rollout {
                environment: environment("production"),
            }),
            "ci_trigger" => Some(Action::CiTrigger),
            _ => None,
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

/// A proposed change: modified files and actions to run.
///
/// ```
/// use harness::impact::Change;
///
/// let diff = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n keep\n-old\n+new\n";
/// let change = Change::from_unified_diff(diff);
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

    /// Parse a unified diff such as `git diff` prints.
    ///
    /// Deleted files are attributed to their old path; binary files and mode
    /// changes carry no lines but still count as changed files.
    ///
    /// ```
    /// use harness::impact::Change;
    ///
    /// let diff = "diff --git a/gone.sql b/gone.sql\ndeleted file mode 100644\n--- a/gone.sql\n+++ /dev/null\n@@ -1 +0,0 @@\n-DROP TABLE x;\n";
    /// let change = Change::from_unified_diff(diff);
    /// assert_eq!(change.files[0].path, "gone.sql");
    /// assert_eq!(change.files[0].removed, ["DROP TABLE x;"]);
    /// assert!(Change::from_unified_diff("").files.is_empty());
    /// ```
    pub fn from_unified_diff(diff: &str) -> Self {
        let mut files: Vec<ChangedFile> = Vec::new();
        let mut old_remaining = 0u32;
        let mut new_remaining = 0u32;
        let mut new_line = 0u32;
        let mut old_path: Option<String> = None;
        for line in diff.lines() {
            if old_remaining > 0 || new_remaining > 0 {
                if let Some(text) = line.strip_prefix('+') {
                    if let Some(file) = files.last_mut() {
                        file.added.push(AddedLine {
                            number: new_line,
                            text: text.to_string(),
                        });
                    }
                    new_line += 1;
                    new_remaining = new_remaining.saturating_sub(1);
                } else if let Some(text) = line.strip_prefix('-') {
                    if let Some(file) = files.last_mut() {
                        file.removed.push(text.to_string());
                    }
                    old_remaining = old_remaining.saturating_sub(1);
                } else if line.starts_with('\\') {
                } else {
                    new_line += 1;
                    new_remaining = new_remaining.saturating_sub(1);
                    old_remaining = old_remaining.saturating_sub(1);
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("diff --git ") {
                let path = rest
                    .rsplit_once(" b/")
                    .map(|(_, new)| new.to_string())
                    .unwrap_or_else(|| rest.to_string());
                files.push(ChangedFile::new(path));
                old_path = None;
            } else if let Some(rest) = line.strip_prefix("--- ") {
                old_path = rest.strip_prefix("a/").map(str::to_string);
            } else if let Some(rest) = line.strip_prefix("+++ ") {
                let resolved = match rest.strip_prefix("b/") {
                    Some(new) => Some(new.to_string()),
                    None if rest == "/dev/null" => old_path.take(),
                    None => None,
                };
                if let (Some(path), Some(file)) = (resolved, files.last_mut()) {
                    file.path = normalize_path(&path);
                }
            } else if let Some(header) = line.strip_prefix("@@ ") {
                let (old, new) = parse_hunk_header(header);
                old_remaining = old.1;
                new_remaining = new.1;
                new_line = new.0;
            }
        }
        Self {
            files,
            actions: Vec::new(),
        }
    }

    /// Fill each file's `content` from `root` where the file exists.
    ///
    /// ```
    /// use harness::impact::Change;
    ///
    /// let dir = tempfile::tempdir().unwrap();
    /// std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
    /// let mut change = Change::from_paths(["a.txt", "missing.txt"]);
    /// change.load_content(dir.path());
    /// assert_eq!(change.files[0].content.as_deref(), Some("hi"));
    /// assert_eq!(change.files[1].content, None);
    /// ```
    pub fn load_content(&mut self, root: &Path) {
        for file in &mut self.files {
            if file.content.is_none() {
                file.content = std::fs::read_to_string(root.join(&file.path)).ok();
            }
        }
    }
}

fn parse_hunk_header(header: &str) -> ((u32, u32), (u32, u32)) {
    let range = |token: &str| {
        let token = token.trim_start_matches(['-', '+']);
        let (start, len) = token.split_once(',').unwrap_or((token, "1"));
        (start.parse().unwrap_or(0), len.parse().unwrap_or(0))
    };
    let mut parts = header.split_whitespace();
    let old = parts.next().map(range).unwrap_or((0, 0));
    let new = parts.next().map(range).unwrap_or((0, 0));
    (old, new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_multiple_files_and_hunks_with_new_line_numbers() {
        let diff = "diff --git a/a.rs b/a.rs\nindex 1..2 100644\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,4 @@\n one\n+two\n three\n four\n@@ -10,2 +11,2 @@ fn ctx\n-old\n+new\n same\ndiff --git a/b.rs b/b.rs\nnew file mode 100644\n--- /dev/null\n+++ b/b.rs\n@@ -0,0 +1,2 @@\n+x\n+y\n";
        let change = Change::from_unified_diff(diff);
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
        let change = Change::from_unified_diff(diff);
        assert_eq!(change.files[0].path, "a.md");
        assert_eq!(change.files[0].added[0].text, "++ not a header");
    }

    #[test]
    fn binary_and_mode_only_files_have_no_lines() {
        let diff = "diff --git a/bin.dat b/bin.dat\nBinary files a/bin.dat and b/bin.dat differ\ndiff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n";
        let change = Change::from_unified_diff(diff);
        assert_eq!(change.files.len(), 2);
        assert!(change.files.iter().all(|f| f.added.is_empty()));
        assert_eq!(change.files[1].path, "run.sh");
    }

    #[test]
    fn no_newline_marker_is_ignored() {
        let diff = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\n\\ No newline at end of file\n+y\n\\ No newline at end of file\n";
        let change = Change::from_unified_diff(diff);
        assert_eq!(change.files[0].removed, ["x"]);
        assert_eq!(change.files[0].added[0].text, "y");
    }

    #[test]
    fn garbage_yields_no_files() {
        assert!(Change::from_unified_diff("not a diff\nat all")
            .files
            .is_empty());
    }

    #[test]
    fn paths_are_normalised() {
        assert_eq!(ChangedFile::new("./a\\b.rs").path, "a/b.rs");
    }

    #[test]
    fn tool_call_mapping() {
        assert_eq!(
            Action::from_tool_call("prod_rollout", &json!({})),
            Some(Action::Rollout {
                environment: "production".into()
            })
        );
        assert_eq!(
            Action::from_tool_call("sandbox_deploy", &json!({})),
            Some(Action::SandboxDeploy {
                environment: "sandbox".into()
            })
        );
        assert_eq!(Action::CiTrigger.label(), "ci_trigger");
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
    fn diff_header_without_b_prefix_uses_the_whole_remainder() {
        let change = Change::from_unified_diff("diff --git weird\n");
        assert_eq!(change.files.len(), 1);
        assert_eq!(change.files[0].path, "weird");
    }

    #[test]
    fn deleted_file_takes_its_path_from_the_old_side() {
        let diff = "diff --git a/gone.rs b/gone.rs\ndeleted file mode 100644\n--- a/gone.rs\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-bye\n";
        let change = Change::from_unified_diff(diff);
        assert_eq!(change.files[0].path, "gone.rs");
        assert_eq!(change.files[0].removed, ["bye"]);
        assert!(change.files[0].added.is_empty());
    }

    #[test]
    fn unrecognised_new_side_keeps_the_header_path() {
        let diff = "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ elsewhere\n";
        let change = Change::from_unified_diff(diff);
        assert_eq!(change.files[0].path, "x.rs");
    }
}
