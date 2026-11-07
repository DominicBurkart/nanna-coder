use crate::protected::{declaration_text, ProtectedConfigError};
use serde::Deserialize;
use std::path::Path;
use thiserror::Error;

pub const POLICY_FILE: &str = ".nanna/policy.toml";

pub const MAX_POLICY_BYTES: usize = 64 * 1024;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("`{POLICY_FILE}` is unreadable: {0}")]
    Unreadable(String),
    #[error("`{POLICY_FILE}` is malformed: {0}")]
    Malformed(String),
    #[error("`{POLICY_FILE}` is larger than {MAX_POLICY_BYTES} bytes")]
    TooLarge,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HumanGate {
    #[default]
    Github,
    Solo,
}

impl HumanGate {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Solo => "solo",
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    #[serde(default)]
    human_gate: HumanGate,
}

/// ```
/// use harness::policy::{HumanGate, RepoPolicy};
///
/// assert_eq!(RepoPolicy::default().human_gate(), HumanGate::Github);
/// let solo = RepoPolicy::parse("human_gate = \"solo\"").unwrap();
/// assert!(solo.is_solo());
/// assert!(RepoPolicy::parse("human_gate = \"duo\"").is_err());
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RepoPolicy {
    human_gate: HumanGate,
}

impl RepoPolicy {
    pub fn solo() -> Self {
        Self {
            human_gate: HumanGate::Solo,
        }
    }

    pub fn parse(text: &str) -> Result<Self, PolicyError> {
        if text.len() > MAX_POLICY_BYTES {
            return Err(PolicyError::TooLarge);
        }
        let file: PolicyFile =
            toml::from_str(text).map_err(|e| PolicyError::Malformed(e.to_string()))?;
        Ok(Self {
            human_gate: file.human_gate,
        })
    }

    pub fn load(base: &Path) -> Result<Self, ProtectedConfigError> {
        let text = declaration_text(base, POLICY_FILE).map_err(PolicyError::Unreadable)?;
        match text {
            Some(text) => Ok(Self::parse(&text)?),
            None => Ok(Self::default()),
        }
    }

    pub fn human_gate(&self) -> HumanGate {
        self.human_gate
    }

    pub fn is_solo(&self) -> bool {
        self.human_gate == HumanGate::Solo
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn write_policy(base: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(base.join(".nanna")).unwrap();
        std::fs::write(base.join(POLICY_FILE), bytes).unwrap();
    }

    fn load_err(base: &Path) -> String {
        format!("{:?}", RepoPolicy::load(base).unwrap_err())
    }

    fn padded_to(len: usize) -> String {
        let head = "human_gate = \"solo\"\n";
        format!("{head}{}", "\n".repeat(len - head.len()))
    }

    #[test]
    fn a_missing_policy_file_means_github_semantics() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(RepoPolicy::load(dir.path()), Ok(RepoPolicy::default()));
        assert_eq!(RepoPolicy::default().human_gate(), HumanGate::Github);
        assert!(!RepoPolicy::default().is_solo());
        std::fs::create_dir_all(dir.path().join(".nanna")).unwrap();
        assert_eq!(RepoPolicy::load(dir.path()), Ok(RepoPolicy::default()));
    }

    #[test]
    fn both_gates_parse_and_an_empty_file_is_github() {
        assert_eq!(
            RepoPolicy::parse("human_gate = \"github\"")
                .unwrap()
                .human_gate(),
            HumanGate::Github
        );
        let solo = RepoPolicy::parse("human_gate = \"solo\"").unwrap();
        assert_eq!(solo.human_gate(), HumanGate::Solo);
        assert!(solo.is_solo());
        assert_eq!(solo, RepoPolicy::solo());
        assert_eq!(RepoPolicy::parse("").unwrap(), RepoPolicy::default());
        assert_eq!(HumanGate::Solo.as_str(), "solo");
        assert_eq!(HumanGate::Github.as_str(), "github");
    }

    #[test]
    fn a_solo_file_on_disk_loads_as_solo() {
        let dir = tempfile::tempdir().unwrap();
        write_policy(dir.path(), b"human_gate = \"solo\"\n");
        assert_eq!(RepoPolicy::load(dir.path()), Ok(RepoPolicy::solo()));
    }

    #[test]
    fn unknown_values_keys_and_types_fail_closed() {
        for text in [
            "human_gate = \"duo\"",
            "human_gate = \"Solo\"",
            "human_gate = \"\"",
            "human_gate = 1",
            "human_gate = true",
            "human_gate = [\"solo\"]",
            "human_gates = \"solo\"",
            "human_gate = \"solo\"\nextra = []",
            "human_gate = \"solo\"\nhuman_gate = \"github\"",
            "human_gate = ",
            "[human_gate]",
        ] {
            assert!(
                matches!(RepoPolicy::parse(text), Err(PolicyError::Malformed(_))),
                "{text}"
            );
        }
    }

    #[test]
    fn a_bad_file_on_disk_refuses_with_a_policy_error_naming_the_file() {
        let dir = tempfile::tempdir().unwrap();
        write_policy(dir.path(), b"human_gate = \"duo\"");
        assert!(load_err(dir.path()).starts_with("Policy(Malformed("));
        let shown = RepoPolicy::load(dir.path()).unwrap_err().to_string();
        assert!(shown.contains(".nanna/policy.toml"), "{shown}");
        assert!(shown.contains("is malformed"), "{shown}");
    }

    #[test]
    fn non_utf8_and_directories_and_symlinks_in_place_are_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        write_policy(dir.path(), &[0xff, 0xfe, b'h']);
        assert!(load_err(dir.path()).starts_with("Policy(Unreadable("));

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(POLICY_FILE)).unwrap();
        assert!(load_err(dir.path()).starts_with("Policy(Unreadable("));

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere.toml");
        std::fs::write(&target, "human_gate = \"github\"").unwrap();
        std::fs::create_dir_all(dir.path().join(".nanna")).unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(POLICY_FILE)).unwrap();
        assert!(load_err(dir.path()).starts_with("Policy(Unreadable("));

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".nanna"), "x").unwrap();
        assert!(load_err(dir.path()).starts_with("Policy(Unreadable("));

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, "x").unwrap();
        assert!(load_err(&file).starts_with("Policy(Unreadable("));
    }

    #[test]
    fn a_file_of_exactly_the_size_cap_loads_and_one_byte_more_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        write_policy(dir.path(), padded_to(MAX_POLICY_BYTES).as_bytes());
        assert_eq!(RepoPolicy::load(dir.path()), Ok(RepoPolicy::solo()));
        write_policy(dir.path(), padded_to(MAX_POLICY_BYTES + 1).as_bytes());
        assert_eq!(load_err(dir.path()), "Policy(TooLarge)");
        assert_eq!(
            RepoPolicy::parse(&padded_to(MAX_POLICY_BYTES + 1)),
            Err(PolicyError::TooLarge)
        );
        assert!(PolicyError::TooLarge.to_string().contains("65536 bytes"));
    }

    #[test]
    fn the_cap_matches_the_protected_declaration_cap() {
        assert_eq!(MAX_POLICY_BYTES, crate::protected::MAX_EXTRAS_BYTES);
    }
}
