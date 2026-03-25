pub mod detect;
pub mod flake_template;
pub mod profile;

use async_trait::async_trait;
use detect::scan_project;
use flake_template::generate_flake;
use profile::ProjectProfile;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OnboardingError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse error: {0}")]
    ParseError(String),
    #[error("project already has a flake.nix")]
    AlreadyOnboarded,
    #[error("no Cargo.toml found; project is not a pure Cargo project")]
    NotCargoProject,
    #[error("ambiguous build system: BUILD or BUILD.bazel found alongside Cargo.toml")]
    AmbiguousBuildSystem,
    #[error("profile error: {0}")]
    ProfileError(String),
    #[error("image builder error: {0}")]
    ImageBuilder(String),
}

impl From<image_builder::ImageBuilderError> for OnboardingError {
    fn from(e: image_builder::ImageBuilderError) -> Self {
        OnboardingError::ImageBuilder(e.to_string())
    }
}

pub struct OnboardingResult {
    pub profile: ProjectProfile,
    pub flake_content: String,
    pub flake_path: PathBuf,
}

impl std::fmt::Debug for OnboardingResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnboardingResult")
            .field("project_name", &self.profile.project_name)
            .field("flake_path", &self.flake_path)
            .finish()
    }
}

#[async_trait]
pub trait Onboarder: Send + Sync {
    async fn onboard(&self, source: &Path) -> Result<OnboardingResult, OnboardingError>;
}

pub struct DeterministicOnboarder;

#[async_trait]
impl Onboarder for DeterministicOnboarder {
    async fn onboard(&self, source: &Path) -> Result<OnboardingResult, OnboardingError> {
        let signals = scan_project(source)?;

        if signals.has_flake_nix {
            return Err(OnboardingError::AlreadyOnboarded);
        }

        if signals.cargo_toml.is_none() {
            return Err(OnboardingError::NotCargoProject);
        }

        if signals.has_build_file {
            return Err(OnboardingError::AmbiguousBuildSystem);
        }

        let profile = signals.to_cargo_profile()?;
        let flake_content = generate_flake(&profile)?;
        let flake_path = source.join("flake.nix");
        std::fs::write(&flake_path, &flake_content)?;

        Ok(OnboardingResult {
            profile,
            flake_content,
            flake_path,
        })
    }
}

pub async fn ensure_dev_container(
    source: &Path,
    onboarder: &dyn Onboarder,
) -> Result<PathBuf, OnboardingError> {
    if !source.join("flake.nix").exists() {
        onboarder.onboard(source).await?;
    }
    image_builder::build_dev_container(source).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_file(dir: &TempDir, name: &str, content: &str) {
        fs::write(dir.path().join(name), content).unwrap();
    }

    fn minimal_cargo_toml(name: &str) -> String {
        format!(
            r#"
[package]
name = "{}"
version = "0.1.0"
edition = "2021"
"#,
            name
        )
    }

    #[tokio::test]
    async fn deterministic_onboarder_errors_on_missing_cargo_toml() {
        let dir = TempDir::new().unwrap();
        let onboarder = DeterministicOnboarder;
        let err = onboarder.onboard(dir.path()).await.unwrap_err();
        assert!(matches!(err, OnboardingError::NotCargoProject));
    }

    #[tokio::test]
    async fn deterministic_onboarder_errors_when_flake_exists() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "flake.nix", "{}");
        write_file(&dir, "Cargo.toml", &minimal_cargo_toml("myapp"));
        let onboarder = DeterministicOnboarder;
        let err = onboarder.onboard(dir.path()).await.unwrap_err();
        assert!(matches!(err, OnboardingError::AlreadyOnboarded));
    }

    #[tokio::test]
    async fn deterministic_onboarder_errors_on_ambiguous_build_system() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "Cargo.toml", &minimal_cargo_toml("myapp"));
        write_file(&dir, "BUILD", "");
        let onboarder = DeterministicOnboarder;
        let err = onboarder.onboard(dir.path()).await.unwrap_err();
        assert!(matches!(err, OnboardingError::AmbiguousBuildSystem));
    }

    #[tokio::test]
    async fn deterministic_onboarder_writes_flake_nix() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "Cargo.toml", &minimal_cargo_toml("myapp"));
        let onboarder = DeterministicOnboarder;
        let result = onboarder.onboard(dir.path()).await.unwrap();
        assert!(result.flake_path.exists());
        assert!(result.flake_path.ends_with("flake.nix"));
        let written = fs::read_to_string(&result.flake_path).unwrap();
        assert_eq!(written, result.flake_content);
    }

    #[tokio::test]
    async fn ensure_dev_container_skips_onboarding_when_flake_exists() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "flake.nix", "{}");

        struct NeverOnboarder;
        #[async_trait]
        impl Onboarder for NeverOnboarder {
            async fn onboard(&self, _: &Path) -> Result<OnboardingResult, OnboardingError> {
                panic!("onboard should not be called when flake.nix exists");
            }
        }

        let result = ensure_dev_container(dir.path(), &NeverOnboarder).await;
        assert!(result.is_err());
    }
}
