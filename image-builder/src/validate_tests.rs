//! Extended tests for image validation edge cases
//!
//! The existing tests cover nonexistent paths and JSON files.
//! These tests add coverage for:
//! - directory validation (empty vs non-empty)
//! - non-JSON file content (first byte != '{')

#[cfg(test)]
mod tests {
    use crate::validate_image;
    use std::io::Write;

    #[test]
    fn validate_image_nonempty_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("layer.tar"), b"data").unwrap();
        assert!(validate_image(dir.path()).unwrap());
    }

    #[test]
    fn validate_image_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        // Empty directory should return false
        assert!(!validate_image(dir.path()).unwrap());
    }

    #[test]
    fn validate_image_non_json_file() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"not json content").unwrap();
        // First byte is 'n', not '{', so should return false
        assert!(!validate_image(f.path()).unwrap());
    }

    #[test]
    fn validate_image_empty_file() {
        let f = tempfile::NamedTempFile::new().unwrap();
        // Empty file: read returns 0 bytes
        assert!(!validate_image(f.path()).unwrap());
    }

    #[test]
    fn validate_image_json_array_file() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"[{\"layer\": 1}]").unwrap();
        // First byte is '[', not '{', so should return false
        assert!(!validate_image(f.path()).unwrap());
    }
}
