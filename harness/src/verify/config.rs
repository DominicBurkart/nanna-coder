use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;
use thiserror::Error;

pub const VERIFY_FILE: &str = ".nanna/verify.toml";

pub const MAX_VERIFY_BYTES: usize = 64 * 1024;

pub const MAX_CHECKS: usize = 32;

pub const MAX_NAME_LEN: usize = 64;

pub const MAX_COMMAND_BYTES: usize = 4096;

pub const DEFAULT_TIMEOUT_SECS: u64 = 900;

pub const MAX_TIMEOUT_SECS: u64 = 3600;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum VerifyConfigError {
    #[error("`{VERIFY_FILE}` does not exist, so there is nothing to verify")]
    Missing,
    #[error("`{VERIFY_FILE}` is unreadable: {0}")]
    Unreadable(String),
    #[error("`{VERIFY_FILE}` is malformed: {0}")]
    Malformed(String),
    #[error("`{VERIFY_FILE}` is larger than {MAX_VERIFY_BYTES} bytes")]
    TooLarge,
    #[error("`{VERIFY_FILE}` declares no checks, so verification would pass vacuously")]
    NoChecks,
    #[error("`{VERIFY_FILE}` declares {count} checks, more than the {MAX_CHECKS} allowed")]
    TooManyChecks { count: usize },
    #[error("`{VERIFY_FILE}` declares the invalid check `{name}`: {reason}")]
    InvalidCheck { name: String, reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub command: String,
    pub timeout_secs: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    check: Vec<RawCheck>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCheck {
    name: String,
    command: String,
    timeout_secs: Option<u64>,
}

/// The checks a repository declares in `.nanna/verify.toml`, in the order
/// they run. A missing, empty or malformed declaration is an error, never an
/// empty set, so a verifier cannot pass without running anything.
///
/// ```
/// use harness::verify::VerifyConfig;
///
/// let config = VerifyConfig::parse(r#"
/// [[check]]
/// name = "fmt"
/// command = "cargo fmt --all -- --check"
///
/// [[check]]
/// name = "tests"
/// command = "cargo test --workspace"
/// timeout_secs = 1800
/// "#).unwrap();
/// assert_eq!(config.checks().len(), 2);
/// assert_eq!(config.checks()[0].timeout_secs, 900);
/// assert_eq!(config.checks()[1].timeout_secs, 1800);
/// assert!(VerifyConfig::parse("").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyConfig(Vec<Check>);

fn invalid(name: &str, reason: &str) -> VerifyConfigError {
    VerifyConfigError::InvalidCheck {
        name: name.to_string(),
        reason: reason.to_string(),
    }
}

fn validate(raw: RawCheck) -> Result<Check, VerifyConfigError> {
    let name = raw.name;
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(invalid(&name, "the name must be 1 to 64 characters long"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(invalid(
            &name,
            "the name may only contain ASCII letters, digits, `_`, `-` and `.`",
        ));
    }
    if raw.command.trim().is_empty() {
        return Err(invalid(&name, "the command is empty"));
    }
    if raw.command.len() > MAX_COMMAND_BYTES {
        return Err(invalid(&name, "the command is longer than 4096 bytes"));
    }
    if raw.command.contains('\0') {
        return Err(invalid(&name, "the command contains a NUL byte"));
    }
    let timeout_secs = raw.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS);
    if timeout_secs == 0 || timeout_secs > MAX_TIMEOUT_SECS {
        return Err(invalid(&name, "timeout_secs must be between 1 and 3600"));
    }
    Ok(Check {
        name,
        command: raw.command,
        timeout_secs,
    })
}

fn read_bounded(reader: impl std::io::Read) -> std::io::Result<String> {
    let mut text = String::new();
    let mut bounded = std::io::Read::take(reader, MAX_VERIFY_BYTES as u64 + 1);
    std::io::Read::read_to_string(&mut bounded, &mut text)?;
    Ok(text)
}

impl VerifyConfig {
    pub fn parse(text: &str) -> Result<Self, VerifyConfigError> {
        if text.len() > MAX_VERIFY_BYTES {
            return Err(VerifyConfigError::TooLarge);
        }
        let file: RawFile =
            toml::from_str(text).map_err(|e| VerifyConfigError::Malformed(e.to_string()))?;
        if file.check.is_empty() {
            return Err(VerifyConfigError::NoChecks);
        }
        if file.check.len() > MAX_CHECKS {
            return Err(VerifyConfigError::TooManyChecks {
                count: file.check.len(),
            });
        }
        let checks = file
            .check
            .into_iter()
            .map(validate)
            .collect::<Result<Vec<_>, _>>()?;
        let mut seen = HashSet::new();
        if let Some(duplicate) = checks.iter().find(|c| !seen.insert(c.name.as_str())) {
            return Err(invalid(&duplicate.name, "the name is declared twice"));
        }
        Ok(Self(checks))
    }

    pub fn load(base: &Path) -> Result<Self, VerifyConfigError> {
        let unreadable = |e: &dyn std::fmt::Display| VerifyConfigError::Unreadable(e.to_string());
        let path = base.join(VERIFY_FILE);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(VerifyConfigError::Missing)
            }
            Err(e) => return Err(unreadable(&e)),
            Ok(meta) if !meta.is_file() => return Err(unreadable(&"not a regular file")),
            Ok(_) => {}
        }
        let text = std::fs::File::open(&path)
            .and_then(read_bounded)
            .map_err(|e| unreadable(&e))?;
        Self::parse(&text)
    }

    pub fn checks(&self) -> &[Check] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn one(name: &str, command: &str) -> String {
        format!("[[check]]\nname = {name:?}\ncommand = {command:?}\n")
    }

    #[test]
    fn parses_checks_in_order_with_default_and_explicit_timeouts() {
        let text = format!(
            "{}\n{}timeout_secs = 60\n",
            one("fmt", "cargo fmt --check"),
            one("test", "cargo test")
        );
        let config = VerifyConfig::parse(&text).unwrap();
        let names: Vec<_> = config.checks().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["fmt", "test"]);
        assert_eq!(config.checks()[0].timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert_eq!(config.checks()[1].timeout_secs, 60);
    }

    #[test]
    fn an_empty_or_checkless_file_is_an_error_not_an_empty_set() {
        assert_eq!(VerifyConfig::parse(""), Err(VerifyConfigError::NoChecks));
        assert_eq!(
            VerifyConfig::parse("# nothing"),
            Err(VerifyConfigError::NoChecks)
        );
    }

    #[test]
    fn unknown_keys_are_rejected_at_every_level() {
        assert!(matches!(
            VerifyConfig::parse(&format!("extra = 1\n{}", one("a", "true"))),
            Err(VerifyConfigError::Malformed(_))
        ));
        assert!(matches!(
            VerifyConfig::parse(&format!("{}retries = 3\n", one("a", "true"))),
            Err(VerifyConfigError::Malformed(_))
        ));
        assert!(matches!(
            VerifyConfig::parse("[[check]]\nname = \"a\"\n"),
            Err(VerifyConfigError::Malformed(_))
        ));
        assert!(matches!(
            VerifyConfig::parse("check = ["),
            Err(VerifyConfigError::Malformed(_))
        ));
    }

    #[test]
    fn invalid_names_commands_and_timeouts_are_rejected() {
        let bad_names = [
            "",
            "has space",
            "slash/name",
            "né",
            &"a".repeat(MAX_NAME_LEN + 1),
        ];
        for name in bad_names {
            assert!(
                matches!(
                    VerifyConfig::parse(&one(name, "true")),
                    Err(VerifyConfigError::InvalidCheck { .. })
                ),
                "{name:?}"
            );
        }
        for command in ["", "   ", &"x".repeat(MAX_COMMAND_BYTES + 1)] {
            assert!(
                matches!(
                    VerifyConfig::parse(&one("a", command)),
                    Err(VerifyConfigError::InvalidCheck { .. })
                ),
                "{command:?}"
            );
        }
        let nul = "[[check]]\nname = \"a\"\ncommand = \"a\\u0000b\"\n";
        let err = VerifyConfig::parse(nul).unwrap_err();
        assert!(err.to_string().contains("NUL"), "{err}");
        for timeout in [0, MAX_TIMEOUT_SECS + 1] {
            let text = format!("{}timeout_secs = {timeout}\n", one("a", "true"));
            assert!(matches!(
                VerifyConfig::parse(&text),
                Err(VerifyConfigError::InvalidCheck { .. })
            ));
        }
        let max = format!("{}timeout_secs = {MAX_TIMEOUT_SECS}\n", one("a", "true"));
        assert!(VerifyConfig::parse(&max).is_ok());
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let text = format!("{}\n{}", one("a", "true"), one("a", "false"));
        let err = VerifyConfig::parse(&text).unwrap_err();
        assert!(err.to_string().contains("declared twice"), "{err}");
    }

    #[test]
    fn caps_on_size_and_count_are_enforced() {
        let many: String = (0..=MAX_CHECKS)
            .map(|i| one(&format!("c{i}"), "true"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            VerifyConfig::parse(&many),
            Err(VerifyConfigError::TooManyChecks {
                count: MAX_CHECKS + 1
            })
        );
        let exactly: String = (0..MAX_CHECKS)
            .map(|i| one(&format!("c{i}"), "true"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            VerifyConfig::parse(&exactly).unwrap().checks().len(),
            MAX_CHECKS
        );
        let big = format!("# {}\n{}", "x".repeat(MAX_VERIFY_BYTES), one("a", "true"));
        assert_eq!(VerifyConfig::parse(&big), Err(VerifyConfigError::TooLarge));
    }

    #[test]
    fn load_reads_the_base_checkout_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".nanna")).unwrap();
        std::fs::write(dir.path().join(VERIFY_FILE), one("a", "true")).unwrap();
        assert_eq!(VerifyConfig::load(dir.path()).unwrap().checks().len(), 1);
    }

    #[test]
    fn load_refuses_missing_oversized_irregular_and_malformed_files() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            VerifyConfig::load(dir.path()),
            Err(VerifyConfigError::Missing)
        );

        std::fs::create_dir(dir.path().join(".nanna")).unwrap();
        std::fs::create_dir(dir.path().join(VERIFY_FILE)).unwrap();
        assert!(matches!(
            VerifyConfig::load(dir.path()),
            Err(VerifyConfigError::Unreadable(_))
        ));
        std::fs::remove_dir(dir.path().join(VERIFY_FILE)).unwrap();

        std::fs::write(
            dir.path().join(VERIFY_FILE),
            "x".repeat(MAX_VERIFY_BYTES + 1),
        )
        .unwrap();
        assert_eq!(
            VerifyConfig::load(dir.path()),
            Err(VerifyConfigError::TooLarge)
        );

        std::fs::write(dir.path().join(VERIFY_FILE), "check = [").unwrap();
        assert!(matches!(
            VerifyConfig::load(dir.path()),
            Err(VerifyConfigError::Malformed(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn load_refuses_a_symlinked_declaration_and_an_unreadable_parent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".nanna")).unwrap();
        let target = dir.path().join("real.toml");
        std::fs::write(&target, one("a", "true")).unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(VERIFY_FILE)).unwrap();
        assert!(matches!(
            VerifyConfig::load(dir.path()),
            Err(VerifyConfigError::Unreadable(_))
        ));

        std::fs::write(dir.path().join("file"), "").unwrap();
        assert!(matches!(
            VerifyConfig::load(&dir.path().join("file")),
            Err(VerifyConfigError::Unreadable(_))
        ));
    }

    proptest! {
        #[test]
        fn valid_declarations_round_trip(
            names in prop::collection::hash_set("[A-Za-z0-9_.-]{1,16}", 1..8),
            timeout in 1u64..=MAX_TIMEOUT_SECS,
        ) {
            let text: String = names
                .iter()
                .map(|n| format!("{}timeout_secs = {timeout}\n", one(n, "echo ok")))
                .collect();
            let config = VerifyConfig::parse(&text).unwrap();
            let parsed: Vec<_> = config.checks().iter().map(|c| c.name.clone()).collect();
            let declared: Vec<_> = names.iter().cloned().collect();
            prop_assert_eq!(parsed.len(), declared.len());
            prop_assert!(config.checks().iter().all(|c| c.timeout_secs == timeout));
        }

        #[test]
        fn arbitrary_text_never_panics(text in ".*") {
            let _ = VerifyConfig::parse(&text);
        }
    }
}
