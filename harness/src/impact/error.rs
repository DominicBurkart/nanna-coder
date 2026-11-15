use super::change::{ActionError, DiffError};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ImpactError {
    #[error(transparent)]
    Assets(#[from] crate::assets::AssetError),
    #[error(transparent)]
    Deploy(Box<crate::deploy::DeployError>),
    #[error("git: {0}")]
    Git(#[from] git2::Error),
    #[error("failed to read diff: {0}")]
    Diff(String),
    #[error("malformed diff: {0}")]
    Parse(#[from] DiffError),
    #[error("tool call `{tool}`: {source}")]
    Action {
        tool: String,
        #[source]
        source: ActionError,
    },
    #[error("`{path}` is outside the task workspace")]
    OutsideWorkspace { path: String },
    #[error("failed to read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl From<crate::deploy::DeployError> for ImpactError {
    fn from(error: crate::deploy::DeployError) -> Self {
        Self::Deploy(Box::new(error))
    }
}
