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

mod analyzer;
mod change;
mod extract;
mod model;

pub use analyzer::{ExtractContext, Extractor, ImpactAnalyzer, DEPTH_DISCOUNT_SHIFT};
pub use change::{Action, AddedLine, Change, ChangedFile};
pub use extract::{sql_accesses, OwnerPathExtractor, SqlAccess, SqlExtractor, TouchesExtractor};
pub use model::{Access, AssetId, BlastRadius, Evidence, Weight};
