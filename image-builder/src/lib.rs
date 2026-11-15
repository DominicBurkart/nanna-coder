//! Container image building utilities for nanna-coder
//!
//! This module provides integration with Nix for building the dev container
//! image consumed by the harness. Sandbox and release container variants
//! described in [`ARCHITECTURE.md`](../../../ARCHITECTURE.md) (Container
//! Topology) are not yet implemented; when they land they should be added
//! here with real bodies rather than `unimplemented!()` stubs.

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

pub fn validate_image(image_path: &Path) -> ImageBuilderResult<bool> {
    if !image_path.exists() {
        return Ok(false);
    }

    if image_path.is_file() {
        let mut buf = [0u8; 1];
        use std::io::Read;
        let mut f = std::fs::File::open(image_path).map_err(ImageBuilderError::Io)?;
        let n = f.read(&mut buf).map_err(ImageBuilderError::Io)?;
        return Ok(n > 0 && buf[0] == b'{');
    }

    if image_path.is_dir() {
        let mut entries = std::fs::read_dir(image_path).map_err(ImageBuilderError::Io)?;
        return Ok(entries.next().is_some());
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_build_dev_container_missing_flake() {
        let dir = tempfile::tempdir().unwrap();
        let result = build_dev_container(dir.path());
        assert!(matches!(result, Err(ImageBuilderError::InvalidConfig(_))));
    }

    #[test]
    fn test_validate_image_nonexistent() {
        let result = validate_image(Path::new("/nonexistent/path"));
        assert!(!result.unwrap());
    }

    #[test]
    fn test_validate_image_json() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"{}").unwrap();
        let result = validate_image(f.path());
        assert!(result.unwrap());
    }
}
