//! Container image building utilities for nanna-coder
//!
//! This module provides integration with Nix for building container images.
//! Currently only the development container is supported; sandbox and release
//! image flows are tracked in ARCHITECTURE.md and will be added when their
//! problem definitions and call sites land together.

use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

/// Errors related to image building
#[derive(Error, Debug)]
pub enum ImageBuilderError {
    #[error("Build failed: {0}")]
    BuildFailed(String),
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("Nix error: {0}")]
    NixError(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type ImageBuilderResult<T> = Result<T, ImageBuilderError>;

pub fn build_dev_container(source: &Path) -> ImageBuilderResult<PathBuf> {
    if !source.join("flake.nix").exists() {
        return Err(ImageBuilderError::InvalidConfig(
            "source directory has no flake.nix; use the onboard_repo tool to generate one"
                .to_string(),
        ));
    }

    let source_str = source
        .canonicalize()
        .map_err(ImageBuilderError::Io)?
        .to_string_lossy()
        .into_owned();

    let output = Command::new("nix")
        .args([
            "build",
            &format!("path:{}#devContainerImage", source_str),
            "--print-out-paths",
            "--no-link",
        ])
        .output()
        .map_err(ImageBuilderError::Io)?;

    if !output.status.success() {
        return Err(ImageBuilderError::NixError(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().next().unwrap_or("").trim();
    Ok(PathBuf::from(first_line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_dev_container_missing_flake() {
        let dir = tempfile::tempdir().unwrap();
        let result = build_dev_container(dir.path());
        assert!(matches!(result, Err(ImageBuilderError::InvalidConfig(_))));
    }
}
