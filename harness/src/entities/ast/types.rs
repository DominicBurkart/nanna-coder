//! File entity types
//!
//! Represents files in the workspace for RAG-based code understanding.
//! Full AST parsing tracked in issue #23.

use crate::entities::{Entity, EntityMetadata, EntityResult, EntityType};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// File type classification for language-aware processing
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileType {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,
    Java,
    Toml,
    Json,
    Yaml,
    Markdown,
    Shell,
    Dockerfile,
    Nix,
    Other(String),
}

impl FileType {
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "rs" => FileType::Rust,
            "py" => FileType::Python,
            "js" | "mjs" | "cjs" => FileType::JavaScript,
            "ts" | "tsx" => FileType::TypeScript,
            "go" => FileType::Go,
            "java" => FileType::Java,
            "toml" => FileType::Toml,
            "json" => FileType::Json,
            "yaml" | "yml" => FileType::Yaml,
            "md" | "markdown" => FileType::Markdown,
            "sh" | "bash" | "zsh" => FileType::Shell,
            "dockerfile" => FileType::Dockerfile,
            "nix" => FileType::Nix,
            other => FileType::Other(other.to_string()),
        }
    }

    pub fn from_path(path: &std::path::Path) -> Self {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.to_lowercase() == "dockerfile" {
                return FileType::Dockerfile;
            }
        }
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(FileType::from_extension)
            .unwrap_or(FileType::Other("unknown".to_string()))
    }
}

/// A file entity representing a file in the workspace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntity {
    #[serde(flatten)]
    pub metadata: EntityMetadata,
    pub path: PathBuf,
    pub relative_path: String,
    pub file_type: FileType,
    pub size_bytes: u64,
    pub content_preview: String,
    pub line_count: usize,
}

#[async_trait]
impl Entity for FileEntity {
    fn metadata(&self) -> &EntityMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut EntityMetadata {
        &mut self.metadata
    }

    fn to_json(&self) -> EntityResult<String> {
        serde_json::to_string(self)
            .map_err(|e| crate::entities::EntityError::SerializationError(e.to_string()))
    }
}

impl FileEntity {
    const PREVIEW_LINES: usize = 50;

    pub fn from_path(path: PathBuf, workspace_root: &std::path::Path) -> std::io::Result<Self> {
        let metadata_fs = std::fs::metadata(&path)?;
        let size_bytes = metadata_fs.len();

        let relative_path = path
            .strip_prefix(workspace_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        let file_type = FileType::from_path(&path);

        let (content_preview, line_count) = if metadata_fs.is_file() {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    let lines: Vec<&str> = content.lines().collect();
                    let preview = lines
                        .iter()
                        .take(Self::PREVIEW_LINES)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join("\n");
                    (preview, lines.len())
                }
                Err(_) => ("(binary or unreadable)".to_string(), 0),
            }
        } else {
            ("(not a file)".to_string(), 0)
        };

        Ok(Self {
            metadata: EntityMetadata::new(EntityType::Ast),
            path,
            relative_path,
            file_type,
            size_bytes,
            content_preview,
            line_count,
        })
    }

    pub fn is_source_code(&self) -> bool {
        matches!(
            self.file_type,
            FileType::Rust
                | FileType::Python
                | FileType::JavaScript
                | FileType::TypeScript
                | FileType::Go
                | FileType::Java
        )
    }

    pub fn is_config(&self) -> bool {
        matches!(
            self.file_type,
            FileType::Toml | FileType::Json | FileType::Yaml | FileType::Dockerfile | FileType::Nix
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_type_from_extension() {
        assert_eq!(FileType::from_extension("rs"), FileType::Rust);
        assert_eq!(FileType::from_extension("py"), FileType::Python);
        assert_eq!(FileType::from_extension("ts"), FileType::TypeScript);
        assert_eq!(FileType::from_extension("toml"), FileType::Toml);
        assert_eq!(
            FileType::from_extension("xyz"),
            FileType::Other("xyz".to_string())
        );
    }

    /// Covers the JavaScript (`js`, `mjs`, `cjs`) and additional language
    /// extensions that are not exercised by the baseline test.
    #[test]
    fn test_file_type_from_extension_all_variants() {
        use std::path::Path;

        // JavaScript variants
        assert_eq!(FileType::from_extension("js"), FileType::JavaScript);
        assert_eq!(FileType::from_extension("mjs"), FileType::JavaScript);
        assert_eq!(FileType::from_extension("cjs"), FileType::JavaScript);

        // TypeScript variant
        assert_eq!(FileType::from_extension("tsx"), FileType::TypeScript);

        // Other languages
        assert_eq!(FileType::from_extension("go"), FileType::Go);
        assert_eq!(FileType::from_extension("java"), FileType::Java);

        // Data / config formats
        assert_eq!(FileType::from_extension("json"), FileType::Json);
        assert_eq!(FileType::from_extension("yaml"), FileType::Yaml);
        assert_eq!(FileType::from_extension("yml"), FileType::Yaml);
        assert_eq!(FileType::from_extension("md"), FileType::Markdown);
        assert_eq!(FileType::from_extension("markdown"), FileType::Markdown);

        // Shell
        assert_eq!(FileType::from_extension("sh"), FileType::Shell);
        assert_eq!(FileType::from_extension("bash"), FileType::Shell);
        assert_eq!(FileType::from_extension("zsh"), FileType::Shell);

        // Special names
        assert_eq!(FileType::from_extension("dockerfile"), FileType::Dockerfile);
        assert_eq!(FileType::from_extension("nix"), FileType::Nix);

        // Case-insensitive mapping
        assert_eq!(FileType::from_extension("RS"), FileType::Rust);
        assert_eq!(FileType::from_extension("PY"), FileType::Python);

        // Roundtrip path helper agrees with extension helper.
        assert_eq!(
            FileType::from_path(Path::new("index.js")),
            FileType::JavaScript
        );
        assert_eq!(FileType::from_path(Path::new("script.sh")), FileType::Shell);
    }

    #[test]
    fn test_file_type_from_path() {
        use std::path::Path;

        assert_eq!(
            FileType::from_path(Path::new("src/main.rs")),
            FileType::Rust
        );
        assert_eq!(
            FileType::from_path(Path::new("Dockerfile")),
            FileType::Dockerfile
        );
        assert_eq!(FileType::from_path(Path::new("flake.nix")), FileType::Nix);
    }

    /// `from_path` on a path with no extension should fall back to
    /// `Other("unknown")`.
    #[test]
    fn test_file_type_from_path_no_extension() {
        use std::path::Path;

        assert_eq!(
            FileType::from_path(Path::new("Makefile")),
            FileType::Other("unknown".to_string())
        );
        assert_eq!(
            FileType::from_path(Path::new("LICENSE")),
            FileType::Other("unknown".to_string())
        );
    }

    #[tokio::test]
    async fn test_file_entity_from_path() {
        let temp_dir = std::env::temp_dir().join("nanna_test_file_entity");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let test_file = temp_dir.join("test.rs");
        std::fs::write(&test_file, "fn main() {\n    println!(\"hello\");\n}").unwrap();

        let entity = FileEntity::from_path(test_file.clone(), &temp_dir).unwrap();

        assert_eq!(entity.relative_path, "test.rs");
        assert_eq!(entity.file_type, FileType::Rust);
        assert_eq!(entity.line_count, 3);
        assert!(entity.is_source_code());
        assert!(!entity.is_config());

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    /// `FileEntity` built from a config file must report `is_config()` true and
    /// `is_source_code()` false.
    #[tokio::test]
    async fn test_file_entity_is_config_true() {
        let temp_dir = std::env::temp_dir().join("nanna_test_file_entity_config");
        std::fs::create_dir_all(&temp_dir).unwrap();

        // JSON → is_config
        let json_file = temp_dir.join("config.json");
        std::fs::write(&json_file, r#"{"key": "value"}"#).unwrap();
        let entity = FileEntity::from_path(json_file, &temp_dir).unwrap();
        assert!(entity.is_config());
        assert!(!entity.is_source_code());

        // TOML → is_config
        let toml_file = temp_dir.join("Cargo.toml");
        std::fs::write(&toml_file, "[package]\nname = \"x\"\n").unwrap();
        let entity = FileEntity::from_path(toml_file, &temp_dir).unwrap();
        assert!(entity.is_config());

        // YAML → is_config
        let yaml_file = temp_dir.join("ci.yml");
        std::fs::write(&yaml_file, "key: value\n").unwrap();
        let entity = FileEntity::from_path(yaml_file, &temp_dir).unwrap();
        assert!(entity.is_config());

        // Dockerfile (by name) → is_config
        let dockerfile = temp_dir.join("Dockerfile");
        std::fs::write(&dockerfile, "FROM ubuntu\n").unwrap();
        let entity = FileEntity::from_path(dockerfile, &temp_dir).unwrap();
        assert!(entity.is_config());

        // Nix → is_config
        let nix_file = temp_dir.join("flake.nix");
        std::fs::write(&nix_file, "{ inputs = {}; }").unwrap();
        let entity = FileEntity::from_path(nix_file, &temp_dir).unwrap();
        assert!(entity.is_config());

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    /// `FileEntity::from_path` on a non-readable file falls back to
    /// `"(binary or unreadable)"` for the content preview and 0 for the line count.
    #[test]
    fn test_file_entity_from_path_unreadable_preview() {
        // Create a temp file, read its metadata, then check the struct.
        let temp_dir = std::env::temp_dir().join("nanna_test_unreadable");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let file_path = temp_dir.join("data.bin");
        // Write some valid bytes so the file exists.
        std::fs::write(&file_path, b"\x00\x01\x02").unwrap();

        // We can still create the entity even for binary files (the error
        // branch is triggered when `read_to_string` fails, e.g. on non-UTF-8).
        // Trigger the fallback by writing non-UTF-8 bytes.
        std::fs::write(&file_path, b"\xff\xfe invalid utf8 \x80").unwrap();

        let entity = FileEntity::from_path(file_path, &temp_dir).unwrap();
        // The fallback string must appear when the file is not valid UTF-8.
        assert_eq!(entity.content_preview, "(binary or unreadable)");
        assert_eq!(entity.line_count, 0);

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    /// `FileEntity::to_json` (from the `Entity` trait) must round-trip cleanly.
    #[test]
    fn test_file_entity_to_json() {
        let temp_dir = std::env::temp_dir().join("nanna_test_entity_json");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let file_path = temp_dir.join("main.py");
        std::fs::write(&file_path, "print('hello')\n").unwrap();

        let entity = FileEntity::from_path(file_path, &temp_dir).unwrap();
        let json = entity.to_json().expect("to_json must succeed");
        let back: FileEntity = serde_json::from_str(&json).expect("round-trip must succeed");

        assert_eq!(back.relative_path, entity.relative_path);
        assert_eq!(back.file_type, entity.file_type);
        assert_eq!(back.line_count, entity.line_count);
        assert!(back.is_source_code());

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }
}
