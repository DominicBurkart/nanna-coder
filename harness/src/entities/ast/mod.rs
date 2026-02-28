//! AST & Filesystem Entities
//!
//! This module implements file entities for representing workspace files
//! in a queryable format for RAG-based code understanding.
//!
//! See issue #23 and ARCHITECTURE.md for details.

pub mod types;

pub use types::*;

use crate::entities::{EntityStore, InMemoryEntityStore};
use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ScanError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Entity store error: {0}")]
    EntityStore(#[from] crate::entities::EntityError),
}

pub type ScanResult<T> = Result<T, ScanError>;

pub struct WorkspaceScanner {
    ignore_patterns: Vec<String>,
    max_file_size: u64,
}

impl Default for WorkspaceScanner {
    fn default() -> Self {
        Self {
            ignore_patterns: vec![
                ".git".to_string(),
                "target".to_string(),
                "node_modules".to_string(),
                ".idea".to_string(),
                ".vscode".to_string(),
                "__pycache__".to_string(),
                "*.pyc".to_string(),
                ".DS_Store".to_string(),
                "Thumbs.db".to_string(),
            ],
            max_file_size: 1024 * 1024,
        }
    }
}

impl WorkspaceScanner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_ignore_patterns(mut self, patterns: Vec<String>) -> Self {
        self.ignore_patterns = patterns;
        self
    }

    pub fn with_max_file_size(mut self, size: u64) -> Self {
        self.max_file_size = size;
        self
    }

    fn should_ignore(&self, name: &str) -> bool {
        for pattern in &self.ignore_patterns {
            if let Some(suffix) = pattern.strip_prefix('*') {
                if name.ends_with(suffix) {
                    return true;
                }
            } else if name == pattern {
                return true;
            }
        }
        name.starts_with('.')
    }

    pub async fn scan_workspace(
        &self,
        root: &Path,
        store: &mut InMemoryEntityStore,
    ) -> ScanResult<usize> {
        let mut count = 0;
        self.scan_directory(root, root, store, &mut count).await?;
        Ok(count)
    }

    fn scan_directory<'a>(
        &'a self,
        dir: &'a Path,
        root: &'a Path,
        store: &'a mut InMemoryEntityStore,
        count: &'a mut usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ScanResult<()>> + Send + 'a>> {
        Box::pin(async move {
            let entries = std::fs::read_dir(dir)?;

            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().to_string();

                if self.should_ignore(&name) {
                    continue;
                }

                let file_type = entry.file_type()?;

                if file_type.is_dir() {
                    self.scan_directory(&path, root, store, count).await?;
                } else if file_type.is_file() {
                    let metadata = std::fs::metadata(&path)?;
                    if metadata.len() > self.max_file_size {
                        continue;
                    }

                    if let Ok(entity) = FileEntity::from_path(path, root) {
                        store.store(Box::new(entity)).await?;
                        *count += 1;
                    }
                }
            }

            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::EntityQuery;

    #[tokio::test]
    async fn test_workspace_scanner() {
        let temp_dir = std::env::temp_dir().join("nanna_test_scanner");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(temp_dir.join("lib.rs"), "pub fn lib() {}").unwrap();
        std::fs::write(temp_dir.join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
        std::fs::create_dir_all(temp_dir.join("src")).unwrap();
        std::fs::write(temp_dir.join("src/util.rs"), "pub fn util() {}").unwrap();

        std::fs::create_dir_all(temp_dir.join(".git")).unwrap();
        std::fs::write(temp_dir.join(".git/config"), "ignored").unwrap();

        let mut store = InMemoryEntityStore::new();
        let scanner = WorkspaceScanner::new();

        let count = scanner.scan_workspace(&temp_dir, &mut store).await.unwrap();

        assert_eq!(count, 4);

        let all_entities = store.query(&EntityQuery::default()).await.unwrap();
        assert_eq!(all_entities.len(), 4);

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_scanner_respects_size_limit() {
        let temp_dir = std::env::temp_dir().join("nanna_test_scanner_size");
        std::fs::create_dir_all(&temp_dir).unwrap();

        std::fs::write(temp_dir.join("small.txt"), "small content").unwrap();

        let large_content = "x".repeat(2 * 1024 * 1024);
        std::fs::write(temp_dir.join("large.txt"), &large_content).unwrap();

        let mut store = InMemoryEntityStore::new();
        let scanner = WorkspaceScanner::new().with_max_file_size(1024 * 1024);

        let count = scanner.scan_workspace(&temp_dir, &mut store).await.unwrap();

        assert_eq!(count, 1);

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }
}
