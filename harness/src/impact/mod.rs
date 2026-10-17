//! Blast-radius analysis: which state assets a change or action reaches and
//! how much that matters to the business.
//!
//! An [`ImpactAnalyzer`] runs small [`Extractor`]s over a [`Change`] (a diff
//! plus actions), collects [`Evidence`] that names assets from the
//! [`AssetGraph`](crate::assets::AssetGraph), and scores the result as a
//! [`BlastRadius`].
//!
//! # Scoring
//!
//! Writes put an asset in `touched`; reads are recorded as evidence only.
//! Every asset that transitively reads a touched asset is `downstream`. The
//! score sums, over `touched ∪ downstream`, each asset's concern weights
//! (distinct concerns, summed per asset), halving the contribution for every
//! `reads` hop between the asset and the nearest touched asset. Depth 0 counts
//! in full, depth 1 half, depth 2 a quarter, rounding down. Adding edges or
//! touched assets can therefore never lower the score.
//!
//! # What scores zero without a marker
//!
//! The score only counts what the analysis could map to a declared asset, and
//! nothing in a [`BlastRadius`] says what it could not map. Each of these
//! contributes zero and leaves no trace: a changed path that no `owners` glob
//! or extractor attributes to an asset, evidence naming an asset the manifest
//! does not declare, a deploy to an environment outside the deploy template,
//! and a repository with no manifest (no radius at all). A score of zero
//! therefore means "nothing recognised", never "known to be safe". Tracked in
//! #847.
//!
//! Two further inputs are skipped quietly. A changed file that is deleted,
//! not valid UTF-8 or a directory has no content for the SQL and route
//! extractors (its path-based owners still apply; #894). A failed agent run
//! gets no task-level radius (#895).
//!
//! # Tool-call paths
//!
//! A path given to a workspace-writing tool call is resolved lexically against
//! the workspace root before it is matched: `.` components and repeated
//! separators are dropped, `..` removes the previous component, and a path
//! that climbs above the root, resolves to the root itself, or is absolute
//! outside the workspace is an [`ImpactError::OutsideWorkspace`] error, not a
//! score. Symlinks are not followed.

mod analyzer;
mod change;
mod error;
mod extract;
mod model;
mod runner;

pub use analyzer::{ExtractContext, Extractor, ImpactAnalyzer, DEPTH_DISCOUNT_SHIFT};
pub use change::{Action, ActionError, AddedLine, Change, ChangedFile, DiffError};
pub use error::ImpactError;
pub use extract::{
    function_spans, route_bindings, sql_accesses, ActionExtractor, ActixRouteExtractor,
    OwnerPathExtractor, RouteBinding, SqlAccess, SqlExtractor, TouchesExtractor,
};
pub use model::{Access, AssetId, BlastRadius, Evidence, Weight};
pub use runner::{diff_against, impact_of_diff, load_repo_context, TaskImpact};
