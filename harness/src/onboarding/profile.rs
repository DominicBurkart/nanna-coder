use thiserror::Error;

pub const DEFAULT_RUST_VERSION: &str = "1.84.0";

const COMMAND_BLOCKLIST: &[&str] = &[
    "publish", "deploy", "push", "rm -rf", "drop", "delete", "destroy",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildSystem {
    Cargo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCategory {
    Build,
    Test,
    Lint,
    Format,
    Check,
}

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: String,
    pub command: String,
    pub description: String,
    pub category: ToolCategory,
}

impl ToolSpec {
    pub fn new(
        name: impl Into<String>,
        command: impl Into<String>,
        description: impl Into<String>,
        category: ToolCategory,
    ) -> Result<Self, ProfileError> {
        let command = command.into();
        let command_tokens: Vec<&str> = command.split_whitespace().collect();
        for blocked in COMMAND_BLOCKLIST {
            let blocked_tokens: Vec<&str> = blocked.split_whitespace().collect();
            let window_size = blocked_tokens.len();
            if window_size > 0
                && command_tokens
                    .windows(window_size)
                    .any(|w| w == blocked_tokens.as_slice())
            {
                return Err(ProfileError::BlocklistedCommand(command));
            }
        }
        Ok(Self {
            name: name.into(),
            command,
            description: description.into(),
            category,
        })
    }
}

#[derive(Debug, Clone)]
pub struct ProjectProfile {
    pub project_name: String,
    pub build_system: BuildSystem,
    pub tools: Vec<ToolSpec>,
    pub nix_packages: Vec<String>,
    pub rust_version: Option<String>,
    pub extra_env_vars: Vec<(String, String)>,
}

#[derive(Debug, Error)]
pub enum ProfileError {
    #[error("command contains blocklisted term: {0}")]
    BlocklistedCommand(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_spec_rejects_blocklisted_commands() {
        assert!(ToolSpec::new(
            "publish",
            "cargo publish",
            "Publish to crates.io",
            ToolCategory::Build
        )
        .is_err());
        assert!(ToolSpec::new(
            "deploy",
            "run deploy production",
            "Deploy to production",
            ToolCategory::Build
        )
        .is_err());
        assert!(ToolSpec::new(
            "push",
            "git push origin main",
            "Push to remote",
            ToolCategory::Build
        )
        .is_err());
        assert!(ToolSpec::new(
            "clean",
            "rm -rf ./target",
            "Clean build artifacts",
            ToolCategory::Build
        )
        .is_err());
    }

    #[test]
    fn tool_spec_accepts_valid_commands() {
        assert!(ToolSpec::new(
            "build",
            "cargo build",
            "Build the project",
            ToolCategory::Build
        )
        .is_ok());
        assert!(ToolSpec::new("test", "cargo test", "Run tests", ToolCategory::Test).is_ok());
        assert!(ToolSpec::new(
            "clippy",
            "cargo clippy -- -D warnings",
            "Run clippy",
            ToolCategory::Lint
        )
        .is_ok());
        assert!(ToolSpec::new(
            "fmt",
            "cargo fmt --check",
            "Check formatting",
            ToolCategory::Format
        )
        .is_ok());
    }

    #[test]
    fn tool_spec_allows_blocklist_substring_within_word() {
        assert!(ToolSpec::new(
            "run-publisher",
            "cargo run --bin publisher",
            "Run the publisher binary",
            ToolCategory::Build
        )
        .is_ok());
    }

    #[test]
    fn all_default_cargo_tools_have_valid_categories() {
        let tools = [
            ToolSpec::new("build", "cargo build", "Build", ToolCategory::Build).unwrap(),
            ToolSpec::new("test", "cargo test", "Test", ToolCategory::Test).unwrap(),
            ToolSpec::new(
                "clippy",
                "cargo clippy -- -D warnings",
                "Lint",
                ToolCategory::Lint,
            )
            .unwrap(),
            ToolSpec::new(
                "fmt",
                "cargo fmt --check",
                "Format check",
                ToolCategory::Format,
            )
            .unwrap(),
        ];
        assert_eq!(tools[0].category, ToolCategory::Build);
        assert_eq!(tools[1].category, ToolCategory::Test);
        assert_eq!(tools[2].category, ToolCategory::Lint);
        assert_eq!(tools[3].category, ToolCategory::Format);
    }
}
