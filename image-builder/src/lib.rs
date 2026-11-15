//! Container image building utilities for nanna-coder
//!
//! This module provides integration with Nix for building container images
//! for different environments (dev, sandbox, release).

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

/// Type of container image to build
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageType {
    /// Development container with full toolchain
    Dev,
    /// Sandbox container for isolated execution
    Sandbox,
    /// Release container with minimal dependencies
    Release,
}

/// Configuration for building a container image
#[derive(Debug, Clone)]
pub struct ImageBuildConfig {
    /// Type of image to build
    pub image_type: ImageType,
    /// Source path for the build
    pub source_path: PathBuf,
    /// Output path for the built image
    pub output_path: PathBuf,
    /// Additional Nix build arguments
    pub nix_args: Vec<String>,
}

impl Default for ImageBuildConfig {
    fn default() -> Self {
        Self {
            image_type: ImageType::Dev,
            source_path: PathBuf::from("."),
            output_path: PathBuf::from("./result"),
            nix_args: vec![],
        }
    }
}

pub fn build_image(config: &ImageBuildConfig) -> ImageBuilderResult<PathBuf> {
    match config.image_type {
        ImageType::Dev => build_dev_container(&config.source_path),
        _ => Err(ImageBuilderError::InvalidConfig(
            "only Dev image type is currently supported".to_string(),
        )),
    }
}

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

pub fn build_sandbox_container(_source: &Path) -> ImageBuilderResult<PathBuf> {
    unimplemented!(
        "Sandbox container building requires further problem definition. \
         This should create an isolated execution environment."
    )
}

pub fn promote_to_release(_dev_image: &Path) -> ImageBuilderResult<PathBuf> {
    unimplemented!(
        "Container promotion requires further problem definition. \
         This should take a tested dev container and create a minimal release image."
    )
}

/// Validate that a built image artifact exists and is plausibly non-empty.
///
/// This is a structural sanity check, not a semantic one: it does not parse
/// the image manifest nor verify Nix store contents. It exists so that
/// callers can fail fast before handing an empty or absent path to a
/// container runtime.
///
/// # Invariants
///
/// * A path that does not exist returns `Ok(false)`.
/// * A regular file returns `Ok(true)` iff its first byte is `b'{'`
///   (the leading byte of a JSON image manifest tarball stream entry, the
///   shape `nix build` emits when materializing OCI images to a single file).
///   An empty file, a file beginning with any other byte, and a non-readable
///   file all return `Ok(false)` (the last surfaces as `Err` only when the
///   read itself fails).
/// * A directory returns `Ok(true)` iff it contains at least one entry.
///   An empty directory returns `Ok(false)`.
/// * Other filesystem object kinds (sockets, devices, broken symlinks
///   resolved to nothing) fall through to `Ok(false)`.
///
/// # Errors
///
/// Returns `Err(ImageBuilderError::Io)` only when the underlying open /
/// read / directory enumeration syscall fails (e.g. permission denied).
///
/// # Examples
///
/// A nonexistent path is treated as invalid, never as an error:
///
/// ```
/// use image_builder::validate_image;
/// use std::path::Path;
///
/// assert!(!validate_image(Path::new("/definitely/not/here")).unwrap());
/// ```
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
    fn test_image_types() {
        assert_eq!(ImageType::Dev, ImageType::Dev);
        assert_ne!(ImageType::Dev, ImageType::Sandbox);
        assert_ne!(ImageType::Sandbox, ImageType::Release);
    }

    #[test]
    fn test_image_build_config_default() {
        let config = ImageBuildConfig::default();
        assert_eq!(config.image_type, ImageType::Dev);
        assert_eq!(config.source_path, PathBuf::from("."));
        assert_eq!(config.output_path, PathBuf::from("./result"));
        assert!(config.nix_args.is_empty());
    }

    #[test]
    fn test_image_build_config_custom() {
        let config = ImageBuildConfig {
            image_type: ImageType::Release,
            source_path: PathBuf::from("/path/to/source"),
            output_path: PathBuf::from("/path/to/output"),
            nix_args: vec!["--arg".to_string(), "value".to_string()],
        };
        assert_eq!(config.image_type, ImageType::Release);
        assert_eq!(config.nix_args.len(), 2);
    }

    #[test]
    fn test_build_image_non_dev_type() {
        let config = ImageBuildConfig {
            image_type: ImageType::Sandbox,
            ..ImageBuildConfig::default()
        };
        let result = build_image(&config);
        assert!(matches!(result, Err(ImageBuilderError::InvalidConfig(_))));
    }

    #[test]
    fn test_build_dev_container_missing_flake() {
        let dir = tempfile::tempdir().unwrap();
        let result = build_dev_container(dir.path());
        assert!(matches!(result, Err(ImageBuilderError::InvalidConfig(_))));
    }

    #[test]
    #[should_panic(expected = "Sandbox container building requires further problem definition")]
    fn test_build_sandbox_container_unimplemented() {
        let source = PathBuf::from(".");
        let _ = build_sandbox_container(&source);
    }

    #[test]
    #[should_panic(expected = "Container promotion requires further problem definition")]
    fn test_promote_to_release_unimplemented() {
        let dev_image = PathBuf::from("./dev-image");
        let _ = promote_to_release(&dev_image);
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

    /// Invariant: a file whose first byte is not `b'{'` is not a valid image
    /// manifest, even if it is non-empty. Guards against blindly accepting
    /// arbitrary build output (e.g. a stray text file at the result path).
    #[test]
    fn test_validate_image_file_with_non_brace_leading_byte_is_invalid() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"not a manifest").unwrap();
        assert!(
            !validate_image(f.path()).unwrap(),
            "non-{{ leading byte must yield false"
        );
    }

    /// Invariant: an empty file is invalid (no leading byte to inspect).
    /// Catches the case where a build creates the output path but writes
    /// nothing to it.
    #[test]
    fn test_validate_image_empty_file_is_invalid() {
        let f = tempfile::NamedTempFile::new().unwrap();
        assert!(
            !validate_image(f.path()).unwrap(),
            "empty file must yield false"
        );
    }

    /// Invariant: a directory with at least one entry is treated as a valid
    /// image (matches the `nix build` "store path directory" output shape).
    #[test]
    fn test_validate_image_non_empty_directory_is_valid() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("manifest.json"), b"{}").unwrap();
        assert!(
            validate_image(dir.path()).unwrap(),
            "non-empty directory must yield true"
        );
    }

    /// Invariant: an empty directory is invalid. Otherwise a stale `mkdir`
    /// at the output path would silently pass validation.
    #[test]
    fn test_validate_image_empty_directory_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            !validate_image(dir.path()).unwrap(),
            "empty directory must yield false"
        );
    }

    /// Invariant: validation is total over the "absent" case — a missing
    /// path never returns `Err`, only `Ok(false)`. This lets callers use a
    /// single `?` site and treat absence as a normal negative result.
    #[test]
    fn test_validate_image_missing_path_is_ok_false_not_err() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("never-created");
        let result = validate_image(&missing);
        assert!(result.is_ok(), "missing path must not raise Err");
        assert!(!result.unwrap(), "missing path must yield false");
    }
}
