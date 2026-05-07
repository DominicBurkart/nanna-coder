//! Branch and invariant coverage for `image-builder`'s public surface.
//!
//! `validate_image` is the gate downstream tooling uses to decide whether a
//! Nix-built artifact looks like an image. Its contract is small but
//! load-bearing:
//!
//! - Nonexistent path → `Ok(false)`.
//! - File path → `Ok(true)` iff the first byte is `b'{'` (matches the JSON
//!   manifest produced by `nix build`).
//! - Directory path → `Ok(true)` iff the directory contains at least one
//!   entry.
//!
//! The crate's existing in-module tests cover only the nonexistent path and
//! a JSON-prefixed file. This file pins the remaining branches and the
//! `build_image` non-Dev rejection so the invariant is observable end-to-end
//! through the crate's public API.
//!
//! The tests are intentionally kept dependency-free (only `tempfile`, already
//! a dev-dep) and avoid invoking `nix`, so they are fast and platform-agnostic.

use image_builder::{build_image, validate_image, ImageBuildConfig, ImageBuilderError, ImageType};
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use tempfile::{tempdir, NamedTempFile};

// ---------------------------------------------------------------------------
// validate_image: file branch
// ---------------------------------------------------------------------------

/// A file whose first byte is not `b'{'` must be rejected. Otherwise any
/// blob-shaped artifact (e.g. a tarball) would silently pass validation.
#[test]
fn validate_image_file_with_non_brace_first_byte_is_invalid() {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"not a manifest").unwrap();
    f.flush().unwrap();
    assert!(!validate_image(f.path()).unwrap());
}

/// An entirely empty file is rejected: `read` returns `n == 0` and the
/// implementation must short-circuit before inspecting `buf[0]` (which would
/// otherwise be the uninitialised default 0u8 and accidentally allow a `\0`
/// header).
#[test]
fn validate_image_empty_file_is_invalid() {
    let f = NamedTempFile::new().unwrap();
    // No write — file exists but has zero bytes.
    assert!(!validate_image(f.path()).unwrap());
}

/// The happy path: a manifest-shaped file (`{...}`) is accepted. Pinned here
/// in addition to the in-module test so the public-surface invariant is
/// expressed against `pub fn validate_image`.
#[test]
fn validate_image_file_starting_with_brace_is_valid() {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(br#"{"manifest": true}"#).unwrap();
    f.flush().unwrap();
    assert!(validate_image(f.path()).unwrap());
}

/// Only the very first byte matters; trailing garbage does not invalidate a
/// manifest-prefixed file. This pins the documented contract — important
/// because changing the rule (e.g. requiring a closing brace) would silently
/// flip downstream gating.
#[test]
fn validate_image_file_only_first_byte_is_examined() {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"{ this rest is ignored \xff\x00 nonsense")
        .unwrap();
    f.flush().unwrap();
    assert!(validate_image(f.path()).unwrap());
}

// ---------------------------------------------------------------------------
// validate_image: directory branch
// ---------------------------------------------------------------------------

/// Empty directory is rejected. Nix's `nix build` output is either a JSON
/// manifest file or a populated store path; an empty directory means the
/// build did not produce anything, and we must not falsely accept it.
#[test]
fn validate_image_empty_directory_is_invalid() {
    let dir = tempdir().unwrap();
    assert!(!validate_image(dir.path()).unwrap());
}

/// A non-empty directory with any single entry counts as valid output. We
/// only check existence of *some* entry — Nix output paths typically contain
/// many files, but a single-file output (e.g. a single tar layer) must also
/// pass.
#[test]
fn validate_image_directory_with_single_file_is_valid() {
    let dir = tempdir().unwrap();
    fs::write(dir.path().join("layer.tar"), b"x").unwrap();
    assert!(validate_image(dir.path()).unwrap());
}

#[test]
fn validate_image_directory_with_subdirectory_is_valid() {
    let dir = tempdir().unwrap();
    fs::create_dir(dir.path().join("nested")).unwrap();
    assert!(validate_image(dir.path()).unwrap());
}

// ---------------------------------------------------------------------------
// validate_image: nonexistent
// ---------------------------------------------------------------------------

/// Nonexistent paths return `Ok(false)` (not an `Err`). Pinned so a future
/// change that converts this to an IO error would be caught by CI rather
/// than silently break callers that use `unwrap_or(false)`.
#[test]
fn validate_image_nonexistent_returns_ok_false_not_err() {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("does-not-exist");
    let result = validate_image(&missing);
    assert!(result.is_ok());
    assert!(!result.unwrap());
}

// ---------------------------------------------------------------------------
// build_image: non-Dev rejection invariant
// ---------------------------------------------------------------------------

/// Both non-Dev variants must surface `InvalidConfig`. The in-module test
/// only exercises `Sandbox`; this pins `Release` so future code that adds a
/// real Release path must update this test (forcing intent to be explicit
/// rather than silently flipping behaviour).
#[test]
fn build_image_release_variant_currently_rejected() {
    let config = ImageBuildConfig {
        image_type: ImageType::Release,
        ..ImageBuildConfig::default()
    };
    let err = build_image(&config).expect_err("Release must be rejected today");
    match err {
        ImageBuilderError::InvalidConfig(msg) => {
            assert!(
                msg.contains("only Dev"),
                "rejection message should name the supported variant, got: {msg}"
            );
        }
        other => panic!("expected InvalidConfig, got {other:?}"),
    }
}

/// `ImageType` Eq must be reflexive, and the three variants pairwise distinct.
/// This is a tiny invariant but `validate_image` and `build_image` dispatch on
/// these values, so a future `#[derive]` change that broke discriminants would
/// be caught here.
#[test]
fn image_type_variants_are_pairwise_distinct() {
    let all = [ImageType::Dev, ImageType::Sandbox, ImageType::Release];
    for (i, a) in all.iter().enumerate() {
        for (j, b) in all.iter().enumerate() {
            if i == j {
                assert_eq!(a, b);
            } else {
                assert_ne!(a, b);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// build_dev_container: missing flake error legibility
// ---------------------------------------------------------------------------

/// When `flake.nix` is missing, the error message must mention the
/// `onboard_repo` tool so an operator (or LLM) reading the error knows the
/// next step. Pinning the message shape catches accidental drift in the
/// remediation hint.
#[test]
fn build_image_dev_without_flake_message_points_at_onboard_repo() {
    let dir = tempdir().unwrap();
    let config = ImageBuildConfig {
        image_type: ImageType::Dev,
        source_path: dir.path().to_path_buf(),
        output_path: PathBuf::from("/tmp/out"),
        nix_args: vec![],
    };
    let err = build_image(&config).expect_err("must error without flake.nix");
    match err {
        ImageBuilderError::InvalidConfig(msg) => {
            assert!(
                msg.contains("flake.nix"),
                "message should name the missing file, got: {msg}"
            );
            assert!(
                msg.contains("onboard_repo"),
                "message should hint at the remediation tool, got: {msg}"
            );
        }
        other => panic!("expected InvalidConfig, got {other:?}"),
    }
}
