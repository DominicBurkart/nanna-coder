//! State-asset manifest (`.nanna/effects.toml`).
//!
//! An [`EffectClass`](crate::effects::EffectClass) says *where* an effect
//! lands; the manifest says *what* it touches and *why that matters to the
//! business*. A repository declares its business [`Concern`]s (each with a
//! weight) and its state [`Asset`]s: tables, endpoints, jobs and external
//! services, each tagged with the concerns on the line, the code paths that
//! own it and the assets it reads. Loading yields an [`AssetGraph`].
//!
//! ```toml
//! [concern.revenue]
//! weight = 10
//! description = "Anything on the paid path."
//!
//! [concern.auth]
//! weight = 9
//!
//! [asset."db.users"]
//! kind = "table"
//! concerns = ["auth"]
//! owners = ["api/src/auth/**"]
//!
//! [asset."http.POST /api/v1/checkout"]
//! kind = "endpoint"
//! concerns = ["revenue"]
//! reads = ["db.users"]
//! ```
//!
//! Ownership can also be declared next to the code with
//! `nanna_effects::touches!("db.users")`; see
//! [`AssetGraph::merge_declarations`].
//!
//! The manifest lives under `.nanna/`, which is part of the protected path
//! set: [`PROTECTED_CONFIG_GLOB`] matches [`MANIFEST_REL_PATH`].

mod derive;
mod graph;
mod model;
mod render;

pub use derive::{propose, propose_in_repo, PROPOSAL_FILE_NAME};
pub use graph::{AssetGraph, CodeSite, Warning};
pub use model::{Asset, AssetKind, Concern, UnknownAssetKind, WeightedConcern};

use std::path::{Path, PathBuf};
use thiserror::Error;

pub const MANIFEST_FILE_NAME: &str = "effects.toml";

pub const MANIFEST_REL_PATH: &str = ".nanna/effects.toml";

pub const PROTECTED_CONFIG_GLOB: &str = ".nanna/**";

pub fn manifest_path_in(repo: &Path) -> PathBuf {
    repo.join(crate::deploy::DEPLOY_DIR)
        .join(MANIFEST_FILE_NAME)
}

#[derive(Debug, Error)]
pub enum AssetError {
    #[error("failed to read {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {}: {source}", file.display())]
    Parse {
        file: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("{}: field `{field}` is invalid: {reason}", file.display())]
    InvalidField {
        file: PathBuf,
        field: String,
        reason: String,
    },
    #[error("{}: duplicate asset `{asset}`", file.display())]
    DuplicateAsset { file: PathBuf, asset: String },
    #[error("{site}: touches undeclared asset `{asset}`")]
    UndeclaredAsset { site: String, asset: String },
    #[error("{} already exists; refusing to overwrite", path.display())]
    AlreadyExists { path: PathBuf },
    #[error("{} exists; a proposal is only written when there is no manifest", path.display())]
    ManifestExists { path: PathBuf },
}
