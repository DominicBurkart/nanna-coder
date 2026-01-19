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

#[deprecated(note = "Use FileEntity instead")]
pub type AstEntity = FileEntity;

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
}
